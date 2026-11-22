//! Profile rules for the remote engines, checked when a profile is saved and again when it is
//! used (`cleartext::check_profile` calls [`check_query_engine`]). Pure: no I/O.

use std::collections::BTreeMap;

use crate::cleartext::is_cleartext_remote;
use crate::error::{CoreError, Result};
use crate::model::{HttpEngineAuth, IcebergQueryEngine};

/// Upper bound for `sessionStartTimeoutSecs`: a session that is not idle within an hour is not
/// going to be.
pub(crate) const MAX_SESSION_START_TIMEOUT_SECS: u64 = 3600;

/// The only Spark session configuration keys a profile may set. Everything else is refused
/// with a message to configure it on the cluster: a profile file can be edited by hand, and a
/// deny-list of dangerous keys cannot keep up with Spark (container images, plugins, executor
/// environment, catalog URIs and storage endpoints all lead somewhere else).
///
/// Keys are compared exactly (case-sensitive) after trimming. `<C>` stands for the profile's
/// `catalog`. `spark.sql.catalog.<C>` itself accepts only a value from
/// [`SPARK_CATALOG_CLASSES`]. Values: no control characters, at most
/// [`MAX_SESSION_CONF_VALUE`] characters, nothing that looks like a credential and no `${`.
///
/// Not allowed, among others: `spark.sql.catalog.<C>.type` and `.warehouse` (a hand-edited
/// profile could repoint the catalog at `type=hadoop`, `warehouse=file:///` or another bucket
/// the cluster role can read), `catalog-impl`, `io-impl`, `uri`.
///
/// The UI mirrors this list; change both together.
pub const SPARK_SESSION_CONF_ALLOWED: &[&str] = &[
    "spark.sql.catalog.<C>",
    "spark.sql.catalog.<C>.default-namespace",
    "spark.sql.catalog.<C>.cache-enabled",
    "spark.sql.catalog.<C>.cache.expiration-interval-ms",
    "spark.sql.shuffle.partitions",
    "spark.sql.session.timeZone",
    "spark.sql.adaptive.enabled",
    "spark.sql.adaptive.coalescePartitions.enabled",
    "spark.executor.memory",
    "spark.executor.cores",
    "spark.executor.instances",
    "spark.driver.memory",
    "spark.driver.cores",
    "spark.dynamicAllocation.enabled",
    "spark.dynamicAllocation.minExecutors",
    "spark.dynamicAllocation.maxExecutors",
    "spark.dynamicAllocation.initialExecutors",
];

/// The values `spark.sql.catalog.<C>` may have.
pub const SPARK_CATALOG_CLASSES: &[&str] = &[
    "org.apache.iceberg.spark.SparkCatalog",
    "org.apache.iceberg.spark.SparkSessionCatalog",
];

/// Longest accepted `sessionConf` value, in characters.
pub const MAX_SESSION_CONF_VALUE: usize = 512;

fn invalid<T>(message: String) -> Result<T> {
    Err(CoreError::Invalid(message))
}

/// Parses an engine base URL: `http` or `https`, a host, and no user, password, query or
/// fragment. `what` names the field in messages ("Trino URL").
pub(crate) fn parse_engine_url(text: &str, what: &str) -> Result<url::Url> {
    let url = url::Url::parse(text.trim())
        .map_err(|e| CoreError::Invalid(format!("the {what} is not valid: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return invalid(format!("the {what} must start with http:// or https://"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return invalid(format!("the {what} has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return invalid(format!(
            "the {what} must not contain a user name or password: the profile file is stored as \
             plain text. Choose an authentication mode and enter the secret in its field, which \
             is kept in the keychain"
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return invalid(format!(
            "the {what} must not have a query (?…) or fragment (#…): use the server's base URL, \
             for example https://host:8443"
        ));
    }
    Ok(url)
}

/// A name that goes into an HTTP header or a request body: not empty, no control characters.
fn plain_name(value: &str, what: &str) -> Result<()> {
    if value.trim().is_empty() {
        return invalid(format!("the {what} is empty"));
    }
    if value.chars().any(char::is_control) {
        return invalid(format!("the {what} contains a control character"));
    }
    if value.len() > 256 {
        return invalid(format!("the {what} is longer than 256 bytes"));
    }
    Ok(())
}

fn check_auth(url: &url::Url, auth: &HttpEngineAuth, engine: &str, what: &str) -> Result<()> {
    if let HttpEngineAuth::Basic { username } = auth {
        plain_name(username, &format!("{engine} user name"))?;
        if username.contains(':') {
            return invalid(format!(
                "the {engine} user name must not contain ':' (HTTP Basic cannot carry it)"
            ));
        }
    }
    if auth.uses_secret() && is_cleartext_remote(url) {
        let secret = match auth {
            HttpEngineAuth::Basic { .. } => "password",
            _ => "token",
        };
        return invalid(format!(
            "the {engine} {secret} would be sent unencrypted to {}: use an https:// {what}. Plain \
             http:// is accepted with a credential only for a server on this machine",
            url.host_str().unwrap_or_default()
        ));
    }
    Ok(())
}

/// Whether a configuration value looks like a credential.
fn is_secret_value(value: &str) -> bool {
    let v = value.trim_start();
    v.get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("bearer "))
        || v.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("basic "))
        || ["password=", "secret=", "token="]
            .iter()
            .any(|w| v.to_ascii_lowercase().contains(w))
}

/// The `sessionConf` rules: only keys of [`SPARK_SESSION_CONF_ALLOWED`] (for `catalog`), with
/// plain values. Credentials and anything else belong on the cluster, not in a plain-text
/// profile.
pub(crate) fn check_session_conf(conf: &BTreeMap<String, String>, catalog: &str) -> Result<()> {
    let catalog = catalog.trim();
    for (key, value) in conf {
        let k = key.trim();
        if k.is_empty() {
            return invalid("a Spark session configuration key is empty".into());
        }
        let shown: String = k.chars().filter(|c| !c.is_control()).take(128).collect();
        let allowed = SPARK_SESSION_CONF_ALLOWED
            .iter()
            .any(|a| a.replace("<C>", catalog) == k);
        if !allowed {
            return invalid(format!(
                "Spark session configuration '{shown}' is not one VibeLens sets from a connection \
                 profile (the profile file can be edited by hand). Configure it on the cluster \
                 (spark-defaults.conf or the Livy server) instead"
            ));
        }
        if value.chars().any(char::is_control) {
            return invalid(format!(
                "Spark session configuration '{shown}' contains a control character"
            ));
        }
        if value.chars().count() > MAX_SESSION_CONF_VALUE {
            return invalid(format!(
                "Spark session configuration '{shown}' is longer than {MAX_SESSION_CONF_VALUE} \
                 characters"
            ));
        }
        if value.contains("${") {
            return invalid(format!(
                "Spark session configuration '{shown}' contains '${{', which Spark would \
                 substitute; write the value out"
            ));
        }
        if is_secret_value(value) {
            return invalid(format!(
                "Spark session configuration '{shown}' looks like a credential, and the profile \
                 file is stored as plain text. Configure it on the cluster instead"
            ));
        }
        if k == format!("spark.sql.catalog.{catalog}")
            && !SPARK_CATALOG_CLASSES.contains(&value.trim())
        {
            return invalid(format!(
                "Spark session configuration '{shown}' must be one of {}",
                SPARK_CATALOG_CLASSES.join(", ")
            ));
        }
    }
    Ok(())
}

/// Where SQL of a Trino or Spark engine runs, for the agent's approval text and prompt:
/// "Trino at trino.example:8443, catalog 'lake'". Host and port only: never a user name,
/// credential or path. `None` for the other engines.
pub(crate) fn engine_target(engine: &IcebergQueryEngine) -> Option<String> {
    let (name, url, catalog) = match engine {
        IcebergQueryEngine::Trino { url, catalog, .. } => ("Trino", url, catalog),
        IcebergQueryEngine::Spark { url, catalog, .. } => ("Spark (Livy)", url, catalog),
        IcebergQueryEngine::Local | IcebergQueryEngine::Athena { .. } => return None,
    };
    let place = url::Url::parse(url.trim())
        .ok()
        .and_then(|u| {
            let host = u.host_str()?.to_string();
            Some(match u.port() {
                Some(p) => format!("{host}:{p}"),
                None => host,
            })
        })
        .unwrap_or_else(|| "an unparsable URL".to_string());
    Some(format!("{name} at {place}, catalog '{}'", catalog.trim()))
}

/// The endpoint part of an engine secret's origin:
/// `"{kind}|{scheme}://{host}:{port}{path prefix}|{caCertPath or empty}"`. Pure; the full origin
/// (`remote::secret::engine_origin`) adds a hash of the CA bundle's contents. `None` for
/// engines without an HTTP endpoint, or a URL that does not parse.
pub fn endpoint_origin(engine: &IcebergQueryEngine) -> Option<String> {
    let (kind, url, ca) = match engine {
        IcebergQueryEngine::Trino {
            url, ca_cert_path, ..
        } => ("trino", url, ca_cert_path),
        IcebergQueryEngine::Spark {
            url, ca_cert_path, ..
        } => ("spark", url, ca_cert_path),
        IcebergQueryEngine::Local | IcebergQueryEngine::Athena { .. } => return None,
    };
    let u = url::Url::parse(url.trim()).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    let port = u.port_or_known_default()?;
    let path = u.path().trim_end_matches('/');
    let ca = ca.as_deref().map(str::trim).unwrap_or("");
    Some(format!("{kind}|{}://{host}:{port}{path}|{ca}", u.scheme()))
}

/// What the approval card shows under the connection for a Trino or Spark engine: engine,
/// host:port, URL path prefix, catalog, default namespace and the acting user. Never a
/// credential. `None` for the other engines.
pub fn engine_detail(opts: &crate::model::IcebergOptions) -> Option<String> {
    let (name, url, catalog, user) = match &opts.query_engine {
        IcebergQueryEngine::Trino {
            url, catalog, user, ..
        } => ("Trino", url, catalog, Some(user.trim().to_string())),
        IcebergQueryEngine::Spark {
            url,
            catalog,
            proxy_user,
            ..
        } => (
            "Spark (Livy)",
            url,
            catalog,
            proxy_user
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| format!("{p} (proxy user)")),
        ),
        IcebergQueryEngine::Local | IcebergQueryEngine::Athena { .. } => return None,
    };
    let mut parts = vec![name.to_string()];
    if let Ok(u) = url::Url::parse(url.trim()) {
        if let (Some(h), Some(p)) = (u.host_str(), u.port_or_known_default()) {
            parts.push(format!("{h}:{p}"));
        }
        let path = u.path().trim_end_matches('/');
        if !path.is_empty() {
            parts.push(format!("path {path}"));
        }
    }
    parts.push(format!("catalog {}", catalog.trim()));
    if let Some(ns) = opts
        .default_namespace
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        parts.push(format!("namespace {ns}"));
    }
    if let Some(u) = user.filter(|u| !u.is_empty()) {
        parts.push(format!("as {u}"));
    }
    Some(parts.join(" · "))
}

/// A Spark catalog name: `^[A-Za-z_][A-Za-z0-9_]*$`. It is put into `USE` and into the
/// `spark.sql.catalog.<C>` keys of the allow-list, where a `.` would widen the match (a catalog
/// called `lake.rest.auth` would allow `spark.sql.catalog.lake.rest.auth`).
pub fn is_spark_catalog_name(name: &str) -> bool {
    let mut b = name.bytes();
    b.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// [`check_query_engine`] plus the rules on the connection's default namespace that the
/// engine uses: Spark puts it into `USE`, Trino into `X-Trino-Schema`. No control characters,
/// and no `${` (Spark substitutes it before parsing, and the value would come back in the error
/// for a namespace that does not exist). Called by `cleartext::check_profile`.
pub(crate) fn check_iceberg_engine(opts: &crate::model::IcebergOptions) -> Result<()> {
    check_query_engine(&opts.query_engine)?;
    if matches!(
        opts.query_engine,
        IcebergQueryEngine::Trino { .. } | IcebergQueryEngine::Spark { .. }
    ) && let Some(ns) = opts.default_namespace.as_deref()
    {
        if ns.chars().any(char::is_control) {
            return invalid("the default namespace contains a control character".into());
        }
        if ns.contains("${") {
            return invalid(
                "the default namespace contains '${', which Spark would substitute; write the \
                 name out"
                    .into(),
            );
        }
    }
    Ok(())
}

/// The rules for the query engine of an Iceberg connection. Only Trino and Spark have any.
pub(crate) fn check_query_engine(engine: &IcebergQueryEngine) -> Result<()> {
    match engine {
        IcebergQueryEngine::Local | IcebergQueryEngine::Athena { .. } => Ok(()),
        IcebergQueryEngine::Trino {
            url,
            catalog,
            user,
            auth,
            ca_cert_path: _,
        } => {
            let url = parse_engine_url(url, "Trino URL")?;
            plain_name(catalog, "Trino catalog")?;
            plain_name(user, "Trino user")?;
            check_auth(&url, auth, "Trino", "Trino URL")
        }
        IcebergQueryEngine::Spark {
            url,
            catalog,
            auth,
            ca_cert_path: _,
            proxy_user,
            session_conf,
            session_start_timeout_secs,
        } => {
            let url = parse_engine_url(url, "Livy URL")?;
            plain_name(catalog, "Spark catalog")?;
            if !is_spark_catalog_name(catalog.trim()) {
                return invalid(
                    "the Spark catalog name may hold only letters, digits and '_', and must not \
                     start with a digit"
                        .into(),
                );
            }
            if let Some(p) = proxy_user.as_deref().filter(|p| !p.trim().is_empty()) {
                plain_name(p, "Livy proxy user")?;
            }
            check_auth(&url, auth, "Livy", "Livy URL")?;
            check_session_conf(session_conf, catalog)?;
            if let Some(secs) = session_start_timeout_secs
                && !(1..=MAX_SESSION_START_TIMEOUT_SECS).contains(secs)
            {
                return invalid(format!(
                    "the Spark session start timeout must be between 1 and \
                     {MAX_SESSION_START_TIMEOUT_SECS} seconds"
                ));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(r: Result<()>) -> String {
        match r {
            Err(CoreError::Invalid(m)) => m,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    fn trino(url: &str, auth: HttpEngineAuth) -> IcebergQueryEngine {
        IcebergQueryEngine::Trino {
            url: url.into(),
            catalog: "iceberg".into(),
            user: "ana".into(),
            auth,
            ca_cert_path: None,
        }
    }

    fn spark(url: &str, auth: HttpEngineAuth, conf: &[(&str, &str)]) -> IcebergQueryEngine {
        IcebergQueryEngine::Spark {
            url: url.into(),
            catalog: "lake".into(),
            auth,
            ca_cert_path: None,
            proxy_user: None,
            session_conf: conf
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            session_start_timeout_secs: None,
        }
    }

    fn basic() -> HttpEngineAuth {
        HttpEngineAuth::Basic {
            username: "ana".into(),
        }
    }

    #[test]
    fn urls_are_http_or_https_without_credentials_query_or_fragment() {
        for url in [
            "ftp://trino.example",
            "file:///etc/passwd",
            "https://ana:hunter2@trino.example",
            "https://trino.example/?x=1",
            "https://trino.example/#f",
            "not a url",
            "https://",
        ] {
            let m = refused(check_query_engine(&trino(url, HttpEngineAuth::None)));
            assert!(!m.contains("hunter2"), "{m}");
            refused(check_query_engine(&spark(url, HttpEngineAuth::None, &[])));
        }
        check_query_engine(&trino("https://trino.example:8443", basic())).unwrap();
        check_query_engine(&trino("https://gw.example/trino/", basic())).unwrap();
    }

    /// Without this rule a password or token would cross the network readable by anyone on the
    /// path. `none` over http:// carries no secret and stays the user's call.
    #[test]
    fn a_credential_is_not_sent_in_cleartext_to_another_machine() {
        for auth in [basic(), HttpEngineAuth::Bearer] {
            let m = refused(check_query_engine(&trino(
                "http://trino.internal:8080",
                auth.clone(),
            )));
            assert!(m.contains("unencrypted") && m.contains("https://"), "{m}");
            refused(check_query_engine(&spark(
                "http://livy.internal:8998",
                auth.clone(),
                &[],
            )));
            // Loopback (a port-forward to the cluster) is fine.
            check_query_engine(&trino("http://localhost:8080", auth.clone())).unwrap();
            check_query_engine(&spark("http://127.0.0.1:8998", auth.clone(), &[])).unwrap();
            check_query_engine(&trino("https://trino.internal", auth)).unwrap();
        }
        check_query_engine(&trino("http://trino.internal:8080", HttpEngineAuth::None)).unwrap();
        check_query_engine(&spark("http://livy.internal", HttpEngineAuth::None, &[])).unwrap();
    }

    #[test]
    fn names_are_required_and_plain() {
        let mut e = trino("https://t", HttpEngineAuth::None);
        if let IcebergQueryEngine::Trino { user, .. } = &mut e {
            user.clear();
        }
        refused(check_query_engine(&e));
        let mut e = trino("https://t", HttpEngineAuth::None);
        if let IcebergQueryEngine::Trino { catalog, .. } = &mut e {
            *catalog = "ice\r\nX-Evil: 1".into();
        }
        refused(check_query_engine(&e));
        refused(check_query_engine(&trino(
            "https://t",
            HttpEngineAuth::Basic {
                username: "a:b".into(),
            },
        )));
    }

    /// Audit 2026-11-21, item 4: the deny-list was bypassable. Without the allow-list each of
    /// these keys, written into a hand-edited profile, would change what the cluster runs or
    /// where its data and credentials go.
    #[test]
    fn session_conf_is_an_allow_list() {
        for (k, v) in [
            ("spark.hadoop.fs.s3a.secret.key", "x"),
            ("spark.kubernetes.container.image", "evil/image:latest"),
            ("spark.plugins", "com.evil.Plugin"),
            ("spark.sql.extensions", "com.evil.Ext"),
            ("spark.executorEnv.AWS_ACCESS_KEY_ID", "x"),
            ("spark.yarn.dist.jars", "s3://b/e.jar"),
            ("spark.sql.catalog.lake.uri", "https://attacker.example"),
            ("spark.sql.catalog.lake.catalog-impl", "x"),
            ("spark.sql.catalog.lake.io-impl", "x"),
            // Re-audit item 4.
            ("spark.sql.catalog.lake.type", "hadoop"),
            ("spark.sql.catalog.lake.warehouse", "file:///"),
            ("spark.hadoop.fs.s3a.endpoint", "https://attacker.example"),
            ("spark.jars", "s3://bucket/evil.jar"),
            ("spark.driver.extraJavaOptions", "-javaagent:x"),
            (
                "spark.sql.catalog.other",
                "org.apache.iceberg.spark.SparkCatalog",
            ),
            ("SPARK.SQL.SHUFFLE.PARTITIONS", "8"),
        ] {
            let m = refused(check_query_engine(&spark(
                "https://l",
                HttpEngineAuth::None,
                &[(k, v)],
            )));
            assert!(m.contains("on the cluster"), "{k}: {m}");
        }
        for (k, v) in [
            ("spark.sql.catalog.lake", "com.evil.Catalog"),
            ("spark.sql.catalog.lake.warehouse", "${env:SECRET}"),
            ("spark.sql.shuffle.partitions", "8\n9"),
            ("spark.driver.memory", "Bearer abc"),
        ] {
            let m = refused(check_query_engine(&spark(
                "https://l",
                HttpEngineAuth::None,
                &[(k, v)],
            )));
            assert!(!m.contains("abc"), "{m}");
        }
        let long = "1".repeat(MAX_SESSION_CONF_VALUE + 1);
        refused(check_query_engine(&spark(
            "https://l",
            HttpEngineAuth::None,
            &[("spark.sql.shuffle.partitions", long.as_str())],
        )));
        let all: Vec<(String, &str)> = SPARK_SESSION_CONF_ALLOWED
            .iter()
            .map(|k| {
                let key = k.replace("<C>", "lake");
                let v = if key == "spark.sql.catalog.lake" {
                    "org.apache.iceberg.spark.SparkSessionCatalog"
                } else {
                    "8"
                };
                (key, v)
            })
            .collect();
        let pairs: Vec<(&str, &str)> = all.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        check_query_engine(&spark("https://l", HttpEngineAuth::None, &pairs)).unwrap();
        check_query_engine(&spark(
            "https://l",
            HttpEngineAuth::None,
            &[(" spark.sql.shuffle.partitions ", "8")],
        ))
        .unwrap();
    }

    /// The rules run through `cleartext::check_profile`, which saving and connecting call.
    #[test]
    fn check_profile_applies_the_engine_rules() {
        let profile = |engine: serde_json::Value| -> crate::model::ConnectionProfile {
            serde_json::from_value(serde_json::json!({
                "id": "p", "name": "p", "engine": "iceberg", "host": "", "port": 0,
                "database": null, "auth": {"kind": "none"}, "tunnel": null, "color": null,
                "createdAt": null, "updatedAt": null,
                "iceberg": {"catalog": {"kind": "hms", "address": "h:9083", "warehouse": "w"},
                            "queryEngine": engine, "defaultNamespace": null},
            }))
            .unwrap()
        };
        let m = refused(crate::cleartext::check_profile(&profile(
            serde_json::json!({
            "kind": "trino", "url": "http://trino.internal:8080", "catalog": "c", "user": "u",
            "auth": {"kind": "bearer"}}),
        )));
        assert!(m.contains("unencrypted"), "{m}");
        refused(crate::cleartext::check_profile(&profile(
            serde_json::json!({
            "kind": "spark", "url": "https://livy", "catalog": "c", "auth": {"kind": "none"},
            "sessionConf": {"spark.jars": "s3://x.jar"}}),
        )));
        crate::cleartext::check_profile(&profile(serde_json::json!({
            "kind": "trino", "url": "https://trino.example", "catalog": "c", "user": "u",
            "auth": {"kind": "basic", "username": "u"}})))
        .unwrap();
        // An unknown auth kind is rejected, not defaulted (R9).
        let bad = serde_json::from_value::<IcebergQueryEngine>(serde_json::json!({
            "kind": "trino", "url": "https://t", "catalog": "c", "user": "u",
            "auth": {"kind": "kerberos"}}));
        assert!(bad.is_err());
    }

    #[test]
    fn endpoint_origins_change_with_endpoint_ca_and_kind() {
        let o = endpoint_origin(&trino("https://T.example/trino/", basic())).unwrap();
        assert_eq!(o, "trino|https://t.example:443/trino|");
        assert_ne!(
            Some(o.clone()),
            endpoint_origin(&trino("https://t.example/other/", basic()))
        );
        assert_ne!(
            Some(o.clone()),
            endpoint_origin(&trino("https://t.example:8443/trino", basic()))
        );
        let mut with_ca = trino("https://t.example/trino", basic());
        if let IcebergQueryEngine::Trino { ca_cert_path, .. } = &mut with_ca {
            *ca_cert_path = Some("/tmp/evil.pem".into());
        }
        assert_ne!(Some(o.clone()), endpoint_origin(&with_ca));
        assert_ne!(
            Some(o),
            endpoint_origin(&spark("https://t.example/trino", basic(), &[]))
        );
        assert_eq!(endpoint_origin(&IcebergQueryEngine::Local), None);
    }

    #[test]
    fn engine_detail_names_endpoint_catalog_namespace_and_user() {
        let opts = |engine| crate::model::IcebergOptions {
            catalog: crate::model::IcebergCatalog::Hms {
                address: "h".into(),
                warehouse: "w".into(),
            },
            storage: Default::default(),
            query_engine: engine,
            default_namespace: Some("shop".into()),
        };
        assert_eq!(
            engine_detail(&opts(trino("https://gw.example/trino/", basic()))).as_deref(),
            Some(
                "Trino · gw.example:443 · path /trino · catalog iceberg · namespace shop · as ana"
            )
        );
        let mut s = spark("https://livy.example:8998", HttpEngineAuth::None, &[]);
        if let IcebergQueryEngine::Spark { proxy_user, .. } = &mut s {
            *proxy_user = Some("etl".into());
        }
        assert_eq!(
            engine_detail(&opts(s)).as_deref(),
            Some(
                "Spark (Livy) · livy.example:8998 · catalog lake · namespace shop · as etl (proxy user)"
            )
        );
    }

    /// Re-audit item 2: the profile's catalog and namespace go into `USE`, which Spark
    /// substitutes `${…}` in; a hand-edited namespace `${env:AWS_SECRET_ACCESS_KEY}` came back
    /// in the "could not switch" error. Item 4 (catalog shape): a `.` would widen the
    /// sessionConf allow-list.
    #[test]
    fn spark_catalog_and_namespace_are_plain() {
        let with = |catalog: &str, ns: Option<&str>| {
            let mut e = spark("https://l", HttpEngineAuth::None, &[]);
            if let IcebergQueryEngine::Spark { catalog: c, .. } = &mut e {
                *c = catalog.into();
            }
            check_iceberg_engine(&crate::model::IcebergOptions {
                catalog: crate::model::IcebergCatalog::Hms {
                    address: "h".into(),
                    warehouse: "w".into(),
                },
                storage: Default::default(),
                query_engine: e,
                default_namespace: ns.map(String::from),
            })
        };
        for c in ["lake.rest.auth", "1lake", "la-ke", "${env:X}", "la ke", ""] {
            refused(with(c, None));
        }
        refused(with("lake", Some("${env:AWS_SECRET_ACCESS_KEY}")));
        refused(with("lake", Some("shop\nx")));
        with("lake", Some("shop.daily")).unwrap();
        with("_Lake_2", None).unwrap();
        assert!(is_spark_catalog_name("glue") && !is_spark_catalog_name("a.b"));
    }

    #[test]
    fn engine_targets_name_engine_host_and_catalog_only() {
        let t = engine_target(&trino("https://trino.example:8443/x", basic())).unwrap();
        assert_eq!(t, "Trino at trino.example:8443, catalog 'iceberg'");
        let s = engine_target(&spark("https://livy.example", HttpEngineAuth::None, &[])).unwrap();
        assert_eq!(s, "Spark (Livy) at livy.example, catalog 'lake'");
        assert_eq!(engine_target(&IcebergQueryEngine::Local), None);
    }

    #[test]
    fn session_start_timeout_is_bounded() {
        let with = |secs| {
            let mut e = spark("https://l", HttpEngineAuth::None, &[]);
            if let IcebergQueryEngine::Spark {
                session_start_timeout_secs,
                ..
            } = &mut e
            {
                *session_start_timeout_secs = Some(secs);
            }
            check_query_engine(&e)
        };
        refused(with(0));
        refused(with(MAX_SESSION_START_TIMEOUT_SECS + 1));
        with(300).unwrap();
    }
}
