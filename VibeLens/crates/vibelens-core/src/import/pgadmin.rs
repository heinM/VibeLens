//! pgAdmin 4 `servers.json` (Tools → Import/Export Servers, or `setup.py dump-servers`):
//!
//! ```json
//! { "Servers": { "1": { "Name": "orders", "Group": "Prod", "Host": "db", "Port": 5432,
//!   "MaintenanceDB": "postgres", "Username": "app", "SSLMode": "verify-full", "SSLRootCert": "...",
//!   "UseSSHTunnel": 1, "TunnelHost": "bastion", "TunnelPort": "22", "TunnelUsername": "ec2-user",
//!   "TunnelAuthentication": 1, "TunnelIdentityFile": "~/.ssh/id_rsa", "BGColor": "#ff0000" } } }
//! ```
//!
//! pgAdmin's live server list is in its SQLite database, which VibeLens does not read.

use serde_json::Value;

use super::{Draft, DraftAuth, SkippedEntry, SshDraft};
use crate::model::{EngineKind, SshAuth};

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
}

fn num(v: &Value, key: &str) -> Option<u64> {
    match v.get(key)? {
        Value::Number(n) => n.as_u64(),
        Value::String(t) => t.trim().parse().ok(),
        Value::Bool(b) => Some(u64::from(*b)),
        _ => None,
    }
}

pub(super) fn parse(text: &str) -> Result<(Vec<Draft>, Vec<SkippedEntry>), String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let servers = root
        .get("Servers")
        .and_then(Value::as_object)
        .ok_or("no \"Servers\" object (not a pgAdmin servers.json export)")?;
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    for (key, sv) in servers {
        let name = s(sv, "Name").unwrap_or(key).to_string();
        // `Host` may be empty when `HostAddr` (an IP) or a service is used.
        let host = s(sv, "Host").or_else(|| s(sv, "HostAddr"));
        let Some(host) = host else {
            let reason = if s(sv, "Service").is_some() {
                "uses a pg_service entry; import pg_service.conf instead"
            } else {
                "no host configured"
            };
            skipped.push(SkippedEntry {
                name,
                reason: reason.into(),
            });
            continue;
        };
        let mut warnings = Vec::new();
        let ssh = (num(sv, "UseSSHTunnel") == Some(1))
            .then(|| {
                let host = s(sv, "TunnelHost")?.to_string();
                let auth = if num(sv, "TunnelAuthentication") == Some(1) {
                    match s(sv, "TunnelIdentityFile") {
                        Some(k) => SshAuth::PrivateKey {
                            path: k.to_string(),
                        },
                        None => SshAuth::Agent,
                    }
                } else {
                    SshAuth::Password
                };
                Some(SshDraft {
                    host,
                    port: num(sv, "TunnelPort").and_then(|p| u16::try_from(p).ok()),
                    user: s(sv, "TunnelUsername").map(String::from),
                    auth,
                })
            })
            .flatten();
        if s(sv, "PassFile").is_some() {
            warnings
                .push("pgAdmin used a passfile; add the password in the connection editor".into());
        }
        if sv.get("KerberosAuthentication").and_then(Value::as_bool) == Some(true) {
            warnings.push(
                "Kerberos authentication is not supported; set a password or IAM auth instead"
                    .into(),
            );
        }
        let auth = DraftAuth::Password;
        drafts.push(Draft {
            name,
            engine: Some(EngineKind::Postgres),
            host: host.to_string(),
            port: num(sv, "Port").and_then(|p| u16::try_from(p).ok()),
            database: s(sv, "MaintenanceDB").map(String::from),
            user: s(sv, "Username").map(String::from),
            sslmode: s(sv, "SSLMode").map(String::from),
            ca_cert: s(sv, "SSLRootCert").map(String::from),
            ssh,
            environment: None,
            group: s(sv, "Group").filter(|g| *g != "Servers").map(String::from),
            color: s(sv, "BGColor").map(String::from),
            auth,
            warnings,
            password_saved: num(sv, "SavePassword") == Some(1),
        });
    }
    Ok((drafts, skipped))
}

#[cfg(test)]
mod tests {
    use super::super::{Extra, ImportSource, parse_text};
    use crate::model::*;

    #[test]
    fn imports_servers_with_tunnels() {
        let src = r##"{"Servers": {
  "1": {"Name": "orders prod", "Group": "Bank", "Host": "orders.db", "Port": 5432, "MaintenanceDB": "orders",
        "Username": "app", "SSLMode": "verify-full", "UseSSHTunnel": 1, "TunnelHost": "jump", "TunnelPort": "2200",
        "TunnelUsername": "ops", "TunnelAuthentication": 1, "TunnelIdentityFile": "~/.ssh/jump", "BGColor": "#336699"},
  "2": {"Name": "svc only", "Group": "Servers", "Service": "orders"}
}}"##;
        let r = parse_text(
            ImportSource::PgAdmin,
            src,
            "servers.json",
            &Extra::default(),
        );
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].reason.contains("pg_service"));
        let c = &r.candidates[0];
        assert_eq!(c.group.as_deref(), Some("Bank"));
        assert_eq!(c.profile.color.as_deref(), Some("#336699"));
        assert_eq!(c.profile.tls.mode, TlsMode::VerifyFull);
        match &c.profile.tunnel {
            Some(TunnelConfig::Ssh {
                host,
                port,
                username,
                auth,
                ..
            }) => {
                assert_eq!(
                    (host.as_str(), *port, username.as_str()),
                    ("jump", 2200, "ops")
                );
                assert_eq!(
                    auth,
                    &SshAuth::PrivateKey {
                        path: "~/.ssh/jump".into()
                    }
                );
            }
            other => panic!("{other:?}"),
        }
    }
}
