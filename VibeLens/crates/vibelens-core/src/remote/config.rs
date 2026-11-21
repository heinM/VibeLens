//! Profile rules for the remote engines, checked when a profile is saved and again when it is
//! used (`cleartext::check_profile` calls [`check_query_engine`]). Pure: no I/O.

use std::collections::BTreeMap;

use crate::cleartext::is_cleartext_remote;
use crate::error::{CoreError, Result};
use crate::model::{HttpEngineAuth, IcebergQueryEngine};

/// Upper bound for `sessionStartTimeoutSecs`: a session that is not idle within an hour is not
/// going to be.
pub(crate) const MAX_SESSION_START_TIMEOUT_SECS: u64 = 3600;

/// Session configuration keys that load code into the cluster. A profile file can be edited by
/// hand, so a key like this would let whoever can write the file run code as the Spark user.
const CODE_LOADING_KEYS: &[&str] = &[
    "spark.jars",
    "spark.jars.packages",
    "spark.jars.repositories",
    "spark.jars.ivy",
    "spark.jars.ivySettings",
    "spark.jars.excludes",
    "spark.files",
    "spark.archives",
    "spark.submit.pyFiles",
    "spark.driver.extraJavaOptions",
    "spark.executor.extraJavaOptions",
    "spark.driver.extraClassPath",
    "spark.executor.extraClassPath",
    "spark.driver.extraLibraryPath",
    "spark.executor.extraLibraryPath",
    "spark.driver.defaultJavaOptions",
    "spark.executor.defaultJavaOptions",
];

/// Words in a configuration key that mark it as a credential.
const SECRET_KEY_WORDS: &[&str] = &["secret", "password", "token", "credential"];

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

/// Whether a Spark configuration entry looks like a credential.
fn is_secret_conf(key: &str, value: &str) -> bool {
    let k = key.trim().to_ascii_lowercase();
    SECRET_KEY_WORDS.iter().any(|w| k.contains(w))
        || k.ends_with(".key")
        || k.ends_with("access.key")
        || value
            .trim_start()
            .get(..7)
            .is_some_and(|p| p.eq_ignore_ascii_case("bearer "))
}

/// The `sessionConf` rules: no credentials (they belong on the cluster, not in a plain-text
/// profile) and nothing that loads code.
pub(crate) fn check_session_conf(conf: &BTreeMap<String, String>) -> Result<()> {
    for (key, value) in conf {
        let k = key.trim();
        if k.is_empty() {
            return invalid("a Spark session configuration key is empty".into());
        }
        if k.chars().any(char::is_control) || value.chars().any(char::is_control) {
            return invalid(format!(
                "Spark session configuration '{k}' contains a control character"
            ));
        }
        if is_secret_conf(k, value) {
            return invalid(format!(
                "Spark session configuration '{k}' looks like a credential, and the profile file \
                 is stored as plain text. Configure it on the cluster (spark-defaults.conf or the \
                 Livy server) instead"
            ));
        }
        if CODE_LOADING_KEYS.iter().any(|c| c.eq_ignore_ascii_case(k)) {
            return invalid(format!(
                "Spark session configuration '{k}' loads code into the cluster and is not \
                 accepted from a connection profile. Configure it on the cluster instead"
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
            if let Some(p) = proxy_user.as_deref().filter(|p| !p.trim().is_empty()) {
                plain_name(p, "Livy proxy user")?;
            }
            check_auth(&url, auth, "Livy", "Livy URL")?;
            check_session_conf(session_conf)?;
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

    /// Without this rule a key or token typed into the session configuration would be written
    /// to the profile file in plain text, and a code-loading key would let whoever can edit
    /// that file run code on the cluster.
    #[test]
    fn session_conf_holds_no_secrets_and_loads_no_code() {
        for (k, v) in [
            ("spark.hadoop.fs.s3a.secret.key", "x"),
            ("spark.hadoop.fs.s3a.access.key", "x"),
            ("spark.sql.catalog.lake.token", "x"),
            ("spark.sql.catalog.lake.credential", "id:secret"),
            ("spark.hadoop.javax.jdo.option.ConnectionPassword", "x"),
            ("spark.sql.catalog.lake.header.Authorization", "Bearer abc"),
            ("spark.ssl.keyStore.key", "x"),
        ] {
            let m = refused(check_query_engine(&spark(
                "https://l",
                HttpEngineAuth::None,
                &[(k, v)],
            )));
            assert!(m.contains("credential") && !m.contains("abc"), "{m}");
        }
        for k in [
            "spark.jars",
            "spark.jars.packages",
            "spark.files",
            "spark.submit.pyFiles",
            "spark.driver.extraJavaOptions",
            "spark.executor.extraJavaOptions",
            "spark.driver.extraClassPath",
            "spark.executor.extraClassPath",
            "SPARK.JARS",
        ] {
            let m = refused(check_query_engine(&spark(
                "https://l",
                HttpEngineAuth::None,
                &[(k, "s3://bucket/evil.jar")],
            )));
            assert!(m.contains("loads code"), "{m}");
        }
        check_query_engine(&spark(
            "https://l",
            HttpEngineAuth::None,
            &[
                (
                    "spark.sql.catalog.lake",
                    "org.apache.iceberg.spark.SparkCatalog",
                ),
                ("spark.sql.catalog.lake.type", "rest"),
                ("spark.sql.catalog.lake.uri", "https://catalog.example"),
                ("spark.sql.shuffle.partitions", "8"),
            ],
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
