//! DBeaver `data-sources*.json`:
//!
//! ```json
//! { "folders": { "Prod": {}, "Prod/Shop": { "parent": "Prod" } },
//!   "connections": { "postgres-jdbc-18c...": {
//!       "provider": "postgresql", "driver": "postgres-jdbc", "name": "orders", "folder": "Prod",
//!       "save-password": true, "color": "255,128,0",
//!       "configuration": { "host": "db", "port": "5432", "database": "orders",
//!         "url": "jdbc:postgresql://db:5432/orders", "type": "prod", "auth-model": "native",
//!         "handlers": { "ssh_tunnel": { "enabled": true, "properties": { "host": "bastion", ... } },
//!                       "postgre_ssl": { "enabled": true, "properties": { "sslMode": "verify-full" } } } } } } }
//! ```
//!
//! User names and passwords normally live in the encrypted `credentials-config.json`; VibeLens
//! does not decrypt it, so only a plaintext `configuration.user` (older versions) is imported.

use serde_json::Value;

use super::{Draft, DraftAuth, SkippedEntry, SshDraft, jdbc};
use crate::model::{EngineKind, Environment, SshAuth};

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
}

fn port_of(v: &Value, key: &str) -> Option<u16> {
    match v.get(key)? {
        Value::Number(n) => n.as_u64().and_then(|p| u16::try_from(p).ok()),
        Value::String(t) => t.trim().parse().ok(),
        _ => None,
    }
}

/// DBeaver colors are `"r,g,b"` strings.
fn rgb(c: &str) -> Option<String> {
    let parts: Vec<u8> = c
        .split(',')
        .map(|p| p.trim().parse::<u8>())
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    match parts.as_slice() {
        [r, g, b] => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        _ => None,
    }
}

fn engine_of(
    provider: &str,
    driver: &str,
    url_scheme: Option<&str>,
) -> Option<(EngineKind, Option<&'static str>)> {
    let p = provider.to_ascii_lowercase();
    let d = driver.to_ascii_lowercase();
    if p == "redis" || d.contains("redis") || url_scheme == Some("redis") {
        return Some((EngineKind::Valkey, None));
    }
    if p == "redshift" || d.contains("redshift") {
        return Some((
            EngineKind::Postgres,
            Some(
                "Amazon Redshift speaks the Postgres protocol but not every catalog query VibeLens runs works there",
            ),
        ));
    }
    let pg_family = [
        "postgresql",
        "postgres",
        "greenplum",
        "yugabyte",
        "timescale",
        "edb",
        "cockroach",
        "babelfish",
        "alloydb",
        "neon",
    ];
    if pg_family.iter().any(|f| p.contains(f) || d.contains(f)) || url_scheme == Some("postgresql")
    {
        return Some((EngineKind::Postgres, None));
    }
    None
}

pub(super) fn parse(text: &str) -> Result<(Vec<Draft>, Vec<SkippedEntry>), String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let conns = root
        .get("connections")
        .and_then(Value::as_object)
        .ok_or("no \"connections\" object (not a DBeaver data-sources file)")?;
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    for (id, c) in conns {
        let name = s(c, "name").unwrap_or(id).to_string();
        let cfg = c.get("configuration").cloned().unwrap_or(Value::Null);
        let url = s(&cfg, "url").and_then(jdbc::parse);
        let provider = s(c, "provider").unwrap_or("");
        let driver = s(c, "driver").unwrap_or("");
        let Some((engine, note)) =
            engine_of(provider, driver, url.as_ref().map(|u| u.scheme.as_str()))
        else {
            let what = if provider.is_empty() {
                driver
            } else {
                provider
            };
            skipped.push(SkippedEntry {
                name,
                reason: format!(
                    "{what} is not a database VibeLens supports (Postgres and Valkey/Redis only)"
                ),
            });
            continue;
        };
        let mut warnings: Vec<String> = note.map(String::from).into_iter().collect();

        // Manual settings win; URL-only connections (configurationType "URL") fall back to it.
        let host = s(&cfg, "host")
            .map(String::from)
            .or_else(|| url.as_ref().map(|u| u.host.clone()))
            .unwrap_or_default();
        if host.is_empty() {
            skipped.push(SkippedEntry {
                name,
                reason: "no host configured".into(),
            });
            continue;
        }
        let port = port_of(&cfg, "port").or_else(|| url.as_ref().and_then(|u| u.port));
        let database = s(&cfg, "database")
            .map(String::from)
            .or_else(|| url.as_ref().and_then(|u| u.database.clone()));
        let user = s(&cfg, "user")
            .map(String::from)
            .or_else(|| url.as_ref().and_then(|u| u.user.clone()));
        let password_saved = c
            .get("save-password")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if user.is_none() && engine == EngineKind::Postgres {
            warnings.push(
                "DBeaver keeps the user name in its encrypted credentials store; add it in the editor".into(),
            );
        }

        let environment = match s(&cfg, "type").map(str::to_ascii_lowercase).as_deref() {
            Some("prod") => Some(Environment::Prod),
            Some("test") => Some(Environment::Test),
            Some("dev") => Some(Environment::Dev),
            _ => None,
        };

        let handlers = cfg.get("handlers").cloned().unwrap_or(Value::Null);
        let enabled = |h: &Value| h.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        let ssh = handlers
            .get("ssh_tunnel")
            .filter(|h| enabled(h))
            .and_then(|h| {
                let p = h.get("properties")?;
                let host = s(p, "host")?.to_string();
                let auth = match s(p, "authType").unwrap_or("").to_ascii_uppercase().as_str() {
                    "PASSWORD" => SshAuth::Password,
                    "PUBLIC_KEY" => match s(p, "keyPath") {
                        Some(k) => SshAuth::PrivateKey {
                            path: k.to_string(),
                        },
                        None => SshAuth::Agent,
                    },
                    _ => SshAuth::Agent,
                };
                Some(SshDraft {
                    host,
                    port: port_of(p, "port"),
                    // Usually in the encrypted store too.
                    user: s(p, "userName").or_else(|| s(p, "user")).map(String::from),
                    auth,
                })
            });
        if ssh.as_ref().is_some_and(|t| t.user.is_none()) {
            warnings
                .push("SSH user name is in DBeaver's encrypted store; add it in the editor".into());
        }
        if handlers.as_object().is_some_and(|m| {
            m.iter()
                .any(|(k, h)| k != "ssh_tunnel" && k.contains("proxy") && enabled(h))
        }) {
            warnings.push("a proxy handler was configured in DBeaver; VibeLens does not support SOCKS/HTTP proxies".into());
        }
        let ssl = handlers
            .as_object()
            .and_then(|m| m.iter().find(|(k, h)| k.contains("ssl") && enabled(h)))
            .and_then(|(_, h)| h.get("properties").cloned());
        let sslmode = ssl
            .as_ref()
            .and_then(|p| {
                s(p, "sslMode")
                    .or_else(|| s(p, "ssl.mode"))
                    .map(String::from)
            })
            .or_else(|| ssl.as_ref().map(|_| "require".to_string()))
            .or_else(|| url.as_ref().and_then(|u| u.params.get("sslmode").cloned()));
        let ca_cert = ssl.as_ref().and_then(|p| {
            ["ssl.ca.cert", "sslRootCert", "sslrootcert", "ssl.ca"]
                .iter()
                .find_map(|k| s(p, k))
                .map(String::from)
        });

        let auth_model = s(&cfg, "auth-model")
            .unwrap_or("native")
            .to_ascii_lowercase();
        let auth = if auth_model.contains("aws") || auth_model.contains("iam") {
            DraftAuth::RdsIam
        } else if auth_model.contains("azure") || auth_model.contains("entra") {
            DraftAuth::Entra
        } else {
            if auth_model.contains("pgpass") {
                warnings.push(
                    "DBeaver read the password from ~/.pgpass; add it in the connection editor"
                        .into(),
                );
            }
            DraftAuth::Password
        };

        let folder = s(c, "folder").map(String::from);
        drafts.push(Draft {
            name,
            engine: Some(engine),
            host,
            port,
            database,
            user,
            sslmode,
            ca_cert,
            ssh,
            environment,
            group: folder,
            color: s(c, "color")
                .and_then(rgb)
                .or_else(|| s(&cfg, "color").and_then(rgb)),
            auth,
            warnings,
            password_saved,
        });
    }
    Ok((drafts, skipped))
}

#[cfg(test)]
mod tests {
    use super::super::{Extra, ImportSource, parse_text};
    use crate::model::*;

    const FILE: &str = r#"{
  "folders": { "Prod": {} },
  "connections": {
    "postgres-jdbc-1": {
      "provider": "postgresql", "driver": "postgres-jdbc", "name": "orders",
      "folder": "Prod", "save-password": true, "color": "255,0,0",
      "configuration": {
        "host": "orders.internal", "port": "5433", "database": "orders",
        "url": "jdbc:postgresql://orders.internal:5433/orders",
        "type": "prod", "auth-model": "native",
        "handlers": {
          "ssh_tunnel": { "type": "TUNNEL", "enabled": true,
            "properties": { "host": "bastion", "port": 22, "authType": "PUBLIC_KEY", "keyPath": "/Users/h/.ssh/id_rsa" } },
          "postgre_ssl": { "type": "CONFIG", "enabled": true, "properties": { "sslMode": "verify-ca", "ssl.ca.cert": "/certs/ca.pem" } }
        }
      }
    },
    "postgres-jdbc-2": {
      "provider": "postgresql", "driver": "postgres-jdbc", "name": "rds iam",
      "configuration": { "host": "db.cluster-a.eu-west-1.rds.amazonaws.com", "port": 5432, "database": "app",
        "user": "iam_user", "auth-model": "aws_rds" }
    },
    "url-only": {
      "provider": "postgresql", "driver": "postgres-jdbc", "name": "by url",
      "configuration": { "configurationType": "URL", "url": "jdbc:postgresql://u.example.com:6000/udb?sslmode=require&user=uu" }
    },
    "mysql8-1": { "provider": "mysql", "driver": "mysql8", "name": "shop mysql", "configuration": { "host": "m" } }
  }
}"#;

    #[test]
    fn maps_connections_folders_types_ssh_and_tls() {
        let r = parse_text(
            ImportSource::DBeaver,
            FILE,
            "data-sources.json",
            &Extra::default(),
        );
        assert!(r.error.is_none(), "{:?}", r.error);
        assert_eq!(r.candidates.len(), 3, "{r:?}");
        assert_eq!(r.skipped.len(), 1);

        let o = r
            .candidates
            .iter()
            .find(|c| c.profile.name == "orders")
            .unwrap();
        let p = &o.profile;
        assert_eq!((p.host.as_str(), p.port), ("orders.internal", 5433));
        assert_eq!(p.environment, Environment::Prod);
        assert_eq!(p.color.as_deref(), Some("#ff0000"));
        assert_eq!(o.group.as_deref(), Some("Prod"));
        assert_eq!(p.tls.mode, TlsMode::VerifyFull);
        assert_eq!(p.tls.ca_cert_path.as_deref(), Some("/certs/ca.pem"));
        assert!(
            matches!(&p.tunnel, Some(TunnelConfig::Ssh { host, auth: SshAuth::PrivateKey { .. }, .. }) if host == "bastion")
        );
        assert!(
            o.warnings.iter().any(|w| w.contains("encrypted")),
            "{:?}",
            o.warnings
        );
        assert!(o.needs_secret);

        let iam = r
            .candidates
            .iter()
            .find(|c| c.profile.name == "rds iam")
            .unwrap();
        match &iam.profile.auth {
            AuthMethod::RdsIam { username, aws } => {
                assert_eq!(username, "iam_user");
                assert_eq!(aws.region, "eu-west-1");
            }
            other => panic!("{other:?}"),
        }
        assert!(!iam.needs_secret);

        let u = r
            .candidates
            .iter()
            .find(|c| c.profile.name == "by url")
            .unwrap();
        assert_eq!(
            (u.profile.host.as_str(), u.profile.port),
            ("u.example.com", 6000)
        );
        assert_eq!(u.profile.database.as_deref(), Some("udb"));
        assert_eq!(u.profile.tls.mode, TlsMode::Require);
        assert_eq!(
            u.profile.auth,
            AuthMethod::Password {
                username: "uu".into()
            }
        );
    }

    #[test]
    fn rejects_other_json() {
        let r = parse_text(ImportSource::DBeaver, r#"{"x":1}"#, "f", &Extra::default());
        assert!(r.error.is_some());
    }
}
