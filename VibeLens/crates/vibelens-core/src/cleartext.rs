//! Rules that keep secrets off cleartext channels and out of files that are not the vault.
//!
//! The UI checks most of this while a form is filled in, but the UI is not the boundary: the
//! profile and provider files can be edited by hand and IPC can be called directly. So the same
//! rules run here, when something is saved and again when it is used.

use std::collections::BTreeMap;

use crate::error::{CoreError, Result};
use crate::model::{AuthMethod, ConnectionProfile, EngineKind, IcebergCatalog, RestAuth, TlsMode};

/// Lower-case header names that carry credentials.
const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "x-portkey-api-key",
    "cookie",
];

/// Whether `host` names this machine: `localhost` or a loopback address (`127.0.0.0/8`, `::1`,
/// with or without the brackets a URL puts around IPv6). Other `*.localhost` names are not
/// accepted: whether they resolve to this machine depends on the resolver.
pub(crate) fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_ascii_lowercase();
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Whether a request to `url` crosses the network without encryption.
pub(crate) fn is_cleartext_remote(url: &url::Url) -> bool {
    url.scheme() == "http" && !url.host_str().is_some_and(is_loopback_host)
}

/// Whether an HTTP header is a credential, going by its name or by a `Bearer …` value. Such a
/// header belongs in the keychain, not in a config file that is written as plain text.
pub(crate) fn is_credential_header(name: &str, value: &str) -> bool {
    SECRET_HEADERS.contains(&name.trim().to_ascii_lowercase().as_str())
        || value
            .trim_start()
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("bearer "))
}

fn invalid<T>(message: String) -> Result<T> {
    Err(CoreError::Invalid(message))
}

/// Parses an `http://` or `https://` URL that must not carry credentials of its own.
fn http_url(text: &str, what: &str) -> Result<url::Url> {
    let url = url::Url::parse(text.trim())
        .map_err(|e| CoreError::Invalid(format!("the {what} is not valid: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return invalid(format!("the {what} must start with http:// or https://"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return invalid(format!(
            "the {what} must not contain a user name or password: the profile file is stored as \
             plain text. Use the connection's token or client-secret field, which is kept in the \
             keychain"
        ));
    }
    Ok(url)
}

/// The rules for an Iceberg REST catalog.
///
/// - A bearer token, or an OAuth2 client secret and the token it buys, is not sent over plain
///   `http://` to another machine. Loopback is fine (`http://localhost:8181` is what a new
///   connection starts with).
/// - Neither URI carries credentials, and no extra header is one: those are written to the
///   profile file.
pub(crate) fn check_rest_catalog(
    uri: &str,
    auth: &RestAuth,
    headers: &BTreeMap<String, String>,
) -> Result<()> {
    let catalog = http_url(uri, "catalog URI")?;
    let secret = match auth {
        RestAuth::None => None,
        RestAuth::Bearer => Some("bearer token"),
        RestAuth::OAuth2 { .. } => Some("OAuth2 client secret or access token"),
    };
    if let Some(secret) = secret
        && is_cleartext_remote(&catalog)
    {
        return invalid(format!(
            "the {secret} would be sent unencrypted to {}: use an https:// catalog URI. Plain \
             http:// is accepted only for a catalog on this machine",
            catalog.host_str().unwrap_or_default()
        ));
    }
    if let RestAuth::OAuth2 {
        server_uri: Some(server),
        ..
    } = auth
        && !server.trim().is_empty()
    {
        let server = http_url(server, "OAuth2 token server URI")?;
        if is_cleartext_remote(&server) {
            return invalid(format!(
                "the OAuth2 client secret would be sent unencrypted to {}: use an https:// \
                 token server URI",
                server.host_str().unwrap_or_default()
            ));
        }
    }
    for (name, value) in headers {
        if is_credential_header(name, value) {
            return invalid(format!(
                "catalog header '{name}' looks like a credential, and headers are stored as \
                 plain text in the profile file. Use the connection's token or client-secret \
                 field, which is kept in the keychain"
            ));
        }
    }
    Ok(())
}

/// A SQL catalog URI must not carry its password (`postgres://user:password@…` or
/// `?password=…`): the URI is written to the profile file. The password goes in the keychain
/// and is added to the URI at connect time.
///
/// A URI that does not parse is left to the database driver, which parses it the same way and
/// reports it.
pub(crate) fn check_sql_catalog_uri(uri: &str) -> Result<()> {
    let Ok(url) = url::Url::parse(uri.trim()) else {
        return Ok(());
    };
    let in_userinfo = url.password().is_some_and(|p| !p.is_empty());
    let in_query = url
        .query_pairs()
        .any(|(k, v)| k.eq_ignore_ascii_case("password") && !v.is_empty());
    if in_userinfo || in_query {
        return invalid(
            "the catalog URI contains a password, and the profile file is stored as plain text. \
             Remove it from the URI (for example postgres://user@host/db) and enter it in the \
             connection's password field, which is kept in the keychain"
                .into(),
        );
    }
    Ok(())
}

/// A token that anyone who reads it can replay (an access token, an IAM token) is not sent
/// without TLS to another machine. An ordinary password is left to the user's TLS choice, as
/// libpq does: refusing it would break databases on private networks.
fn check_bearer_auth_has_tls(profile: &ConnectionProfile) -> Result<()> {
    let token = match &profile.auth {
        AuthMethod::AccessToken { .. } => "access token",
        AuthMethod::RdsIam { .. } | AuthMethod::ElastiCacheIam { .. } => "IAM token",
        AuthMethod::None | AuthMethod::Password { .. } | AuthMethod::SecretsManager { .. } => {
            return Ok(());
        }
    };
    if profile.tls.mode != TlsMode::Disable
        || profile.tunnel.is_some()
        || is_loopback_host(&profile.host)
    {
        return Ok(());
    }
    invalid(format!(
        "TLS is off, so the {token} would be sent unencrypted to {}, and anyone who reads it \
         on the way can use it. Turn TLS on for this connection, or reach the server through an \
         SSH tunnel",
        profile.host.trim()
    ))
}

/// Everything above that applies to `profile`. Called when a profile is saved and again before
/// it is connected to or tested.
pub fn check_profile(profile: &ConnectionProfile) -> Result<()> {
    // Remote query engines (Trino, Spark through Livy): URL shape, credentials over cleartext,
    // session configuration. See `remote::config`.
    if let (EngineKind::Iceberg, Some(o)) = (profile.engine, profile.iceberg.as_ref()) {
        crate::remote::config::check_query_engine(&o.query_engine)?;
    }
    match profile.engine {
        EngineKind::Postgres | EngineKind::Valkey => check_bearer_auth_has_tls(profile),
        EngineKind::Iceberg => match profile.iceberg.as_ref().map(|o| &o.catalog) {
            Some(IcebergCatalog::Rest {
                uri, auth, headers, ..
            }) => check_rest_catalog(uri, auth, headers),
            Some(IcebergCatalog::Sql { uri, .. }) => check_sql_catalog_uri(uri),
            Some(
                IcebergCatalog::Glue { .. }
                | IcebergCatalog::S3Tables { .. }
                | IcebergCatalog::Hms { .. },
            )
            | None => Ok(()),
        },
        // No secret is stored or sent (IAM-signed calls to the regional HTTPS endpoint), but
        // every identifier that is signed and sent to AWS is validated on save and on use.
        EngineKind::Redshift => match &profile.redshift {
            Some(o) => crate::redshift::validate_options(o),
            None => invalid("Redshift connection options are required".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AiDataAccess, AwsIdentity, AwsTarget, Environment, IcebergOptions, SshAuth, TlsConfig,
        TunnelConfig,
    };

    fn refused(r: Result<()>) -> String {
        match r {
            Err(CoreError::Invalid(m)) => m,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn loopback_hosts() {
        for h in [
            "localhost",
            "LOCALHOST",
            "127.0.0.1",
            "127.8.9.1",
            "::1",
            "[::1]",
        ] {
            assert!(is_loopback_host(h), "{h}");
        }
        for h in [
            "",
            "example.com",
            // Audit 2026-11-21, item 12: resolver-dependent.
            "db.localhost",
            "localhost.example.com",
            "10.0.0.1",
            "192.168.1.5",
            "0.0.0.0",
            "::",
            "notlocalhost",
        ] {
            assert!(!is_loopback_host(h), "{h}");
        }
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(is_cleartext_remote(&u("http://catalog.internal:8181/api")));
        assert!(is_cleartext_remote(&u("http://10.0.0.7")));
        assert!(!is_cleartext_remote(&u("https://catalog.internal")));
        assert!(!is_cleartext_remote(&u("http://localhost:8181")));
        assert!(!is_cleartext_remote(&u("http://127.0.0.1:11434/v1")));
        assert!(!is_cleartext_remote(&u("http://[::1]:8181")));
    }

    #[test]
    fn credential_headers_are_recognised_by_name_and_by_value() {
        for (k, v) in [
            ("Authorization", "x"),
            ("authorization", "x"),
            ("Proxy-Authorization", "x"),
            ("X-Api-Key", "x"),
            ("api-key", "x"),
            ("x-goog-api-key", "x"),
            ("X-Portkey-Api-Key", "x"),
            ("Cookie", "x"),
            ("X-Custom", "Bearer abc"),
            ("X-Custom", "  bearer abc"),
        ] {
            assert!(is_credential_header(k, v), "{k}: {v}");
        }
        for (k, v) in [
            ("X-Tenant", "acme"),
            ("X-Iceberg-Access-Delegation", "vended-credentials"),
            ("X-Note", "bearer"),
        ] {
            assert!(!is_credential_header(k, v), "{k}: {v}");
        }
    }

    fn oauth(server_uri: Option<&str>) -> RestAuth {
        RestAuth::OAuth2 {
            client_id: "cid".into(),
            scope: None,
            server_uri: server_uri.map(String::from),
        }
    }

    #[test]
    fn rest_catalog_secrets_never_travel_in_cleartext() {
        let none = BTreeMap::new();
        // A token or client secret to a remote http:// catalog.
        for auth in [
            RestAuth::Bearer,
            oauth(None),
            oauth(Some("https://idp.example/token")),
        ] {
            let m = refused(check_rest_catalog(
                "http://catalog.internal:8181",
                &auth,
                &none,
            ));
            assert!(m.contains("unencrypted") && m.contains("https://"), "{m}");
        }
        // The catalog is fine, the token endpoint is not.
        let m = refused(check_rest_catalog(
            "https://catalog.example",
            &oauth(Some("http://idp.internal/token")),
            &none,
        ));
        assert!(
            m.contains("client secret") && m.contains("unencrypted"),
            "{m}"
        );

        // Loopback keeps working with every auth mode: the default new connection is
        // http://localhost:8181, and local catalogs with a token are common in development.
        for auth in [
            RestAuth::None,
            RestAuth::Bearer,
            oauth(None),
            oauth(Some("http://127.0.0.1:8080/token")),
        ] {
            check_rest_catalog("http://localhost:8181", &auth, &none).unwrap();
            check_rest_catalog("http://127.0.0.1:8181/", &auth, &none).unwrap();
        }
        // No secret, no objection.
        check_rest_catalog("http://catalog.internal:8181", &RestAuth::None, &none).unwrap();
        check_rest_catalog("https://catalog.example/api", &RestAuth::Bearer, &none).unwrap();
        check_rest_catalog(
            "https://catalog.example",
            &oauth(Some("https://idp.example/t")),
            &none,
        )
        .unwrap();
    }

    #[test]
    fn rest_catalog_uris_and_headers_hold_no_credentials() {
        let none = BTreeMap::new();
        let m = refused(check_rest_catalog(
            "https://user:hunter2@catalog.example",
            &RestAuth::None,
            &none,
        ));
        assert!(m.contains("password") && !m.contains("hunter2"), "{m}");
        let m = refused(check_rest_catalog(
            "https://catalog.example",
            &oauth(Some("https://cid:hunter2@idp.example/token")),
            &none,
        ));
        assert!(!m.contains("hunter2"), "{m}");

        for (k, v) in [
            ("Authorization", "Basic abc"),
            ("X-Api-Key", "abc"),
            ("X-Custom", "Bearer abc"),
        ] {
            let mut h = BTreeMap::new();
            h.insert(k.to_string(), v.to_string());
            let m = refused(check_rest_catalog(
                "https://catalog.example",
                &RestAuth::None,
                &h,
            ));
            assert!(
                m.contains(k) && m.contains("credential") && !m.contains("abc"),
                "{m}"
            );
        }
        let mut h = BTreeMap::new();
        h.insert("X-Tenant".to_string(), "acme".to_string());
        check_rest_catalog("https://catalog.example", &RestAuth::Bearer, &h).unwrap();

        assert!(matches!(
            check_rest_catalog("not a url", &RestAuth::None, &none),
            Err(CoreError::Invalid(_))
        ));
    }

    #[test]
    fn sql_catalog_uris_hold_no_password() {
        let m = refused(check_sql_catalog_uri(
            "postgres://iceberg:hunter2@db.internal:5432/catalog",
        ));
        assert!(
            m.contains("password field") && !m.contains("hunter2"),
            "{m}"
        );
        let m = refused(check_sql_catalog_uri(
            "postgres://iceberg@db.internal/catalog?sslmode=require&password=hunter2",
        ));
        assert!(!m.contains("hunter2"), "{m}");
        // An empty password is no secret.
        check_sql_catalog_uri("postgresql://u:@db/catalog").unwrap();
        check_sql_catalog_uri("postgres://u@db/catalog?sslmode=require").unwrap();
        check_sql_catalog_uri("postgres://iceberg@db.internal:5432/catalog").unwrap();
        check_sql_catalog_uri("sqlite:///home/me/catalog.db").unwrap();
        check_sql_catalog_uri("  postgres://iceberg@db/catalog  ").unwrap();
    }

    fn pg(host: &str, auth: AuthMethod, tls: TlsMode) -> ConnectionProfile {
        ConnectionProfile {
            id: "p".into(),
            name: "p".into(),
            engine: EngineKind::Postgres,
            host: host.into(),
            port: 5432,
            database: Some("app".into()),
            auth,
            tls: TlsConfig {
                mode: tls,
                ..Default::default()
            },
            tunnel: None,
            environment: Environment::Dev,
            ai_data_access: AiDataAccess::default(),
            color: None,
            pg: Default::default(),
            valkey: Default::default(),
            created_at: None,
            updated_at: None,
            iceberg: None,
            redshift: None,
        }
    }

    fn aws() -> AwsTarget {
        AwsTarget {
            identity: AwsIdentity::Profile {
                profile_name: "dev".into(),
            },
            region: "eu-west-1".into(),
        }
    }

    fn token() -> AuthMethod {
        AuthMethod::AccessToken {
            username: "me".into(),
            command: None,
        }
    }

    fn iam() -> AuthMethod {
        AuthMethod::RdsIam {
            username: "app".into(),
            aws: aws(),
        }
    }

    /// The open point of finding M7: a bearer token is replayable by whoever reads it off the
    /// wire, so it is not sent without TLS to another machine. Ordinary passwords are left to
    /// the user's TLS choice, as with libpq.
    #[test]
    fn bearer_credentials_are_not_sent_to_a_remote_database_without_tls() {
        for auth in [token(), iam()] {
            let m = refused(check_profile(&pg(
                "db.internal",
                auth.clone(),
                TlsMode::Disable,
            )));
            assert!(m.contains("TLS") && m.contains("unencrypted"), "{m}");
            // With TLS, on this machine, or through the SSH tunnel: fine.
            check_profile(&pg("db.internal", auth.clone(), TlsMode::Require)).unwrap();
            check_profile(&pg("db.internal", auth.clone(), TlsMode::VerifyFull)).unwrap();
            check_profile(&pg("localhost", auth.clone(), TlsMode::Disable)).unwrap();
            check_profile(&pg("127.0.0.1", auth.clone(), TlsMode::Disable)).unwrap();
            let mut tunnelled = pg("db.internal", auth, TlsMode::Disable);
            tunnelled.tunnel = Some(TunnelConfig::Ssh {
                host: "bastion".into(),
                port: 22,
                username: "ops".into(),
                auth: SshAuth::Agent,
                known_host_fingerprint: None,
            });
            check_profile(&tunnelled).unwrap();
        }
        // Password authentication without TLS stays the user's call.
        for auth in [
            AuthMethod::None,
            AuthMethod::Password {
                username: "app".into(),
            },
            AuthMethod::SecretsManager {
                secret_arn: "arn".into(),
                aws: aws(),
            },
        ] {
            check_profile(&pg("db.internal", auth, TlsMode::Disable)).unwrap();
        }
        // ElastiCache IAM tokens are bearer credentials too.
        let mut cache = pg(
            "cache.internal",
            AuthMethod::ElastiCacheIam {
                username: "app".into(),
                cache_name: "c".into(),
                serverless: false,
                aws: aws(),
            },
            TlsMode::Disable,
        );
        cache.engine = EngineKind::Valkey;
        cache.database = None;
        refused(check_profile(&cache));
        cache.tls.mode = TlsMode::VerifyFull;
        check_profile(&cache).unwrap();
    }

    #[test]
    fn iceberg_profiles_are_checked_through_check_profile() {
        let ice = |catalog: IcebergCatalog| {
            let mut p = pg("", AuthMethod::None, TlsMode::VerifyFull);
            p.engine = EngineKind::Iceberg;
            p.iceberg = Some(IcebergOptions {
                catalog,
                storage: Default::default(),
                query_engine: Default::default(),
                default_namespace: None,
            });
            p
        };
        let rest = |uri: &str, auth: RestAuth| IcebergCatalog::Rest {
            uri: uri.into(),
            warehouse: None,
            auth,
            headers: BTreeMap::new(),
        };
        refused(check_profile(&ice(rest(
            "http://catalog.internal",
            RestAuth::Bearer,
        ))));
        check_profile(&ice(rest("http://localhost:8181", RestAuth::None))).unwrap();
        check_profile(&ice(rest("https://catalog.example", RestAuth::Bearer))).unwrap();
        let sql = |uri: &str| IcebergCatalog::Sql {
            uri: uri.into(),
            warehouse: "file:///w".into(),
            catalog_name: "c".into(),
        };
        refused(check_profile(&ice(sql("postgres://u:pw@db/catalog"))));
        check_profile(&ice(sql("postgres://u@db/catalog"))).unwrap();
    }
}
