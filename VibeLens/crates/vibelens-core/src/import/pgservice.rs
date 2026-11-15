//! libpq connection service file (`~/.pg_service.conf`, `$PGSERVICEFILE`):
//!
//! ```ini
//! [orders-prod]
//! host=orders.db
//! port=5432
//! dbname=orders
//! user=app
//! sslmode=verify-full
//! ```
//!
//! Passwords in a service file are ignored on purpose (they belong in the keychain).

use super::{Draft, SkippedEntry};
use crate::model::EngineKind;

pub(super) fn parse(text: &str) -> Result<(Vec<Draft>, Vec<SkippedEntry>), String> {
    let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push((name.trim().to_string(), Vec::new()));
        } else if let Some((k, v)) = line.split_once('=')
            && let Some((_, kv)) = sections.last_mut()
        {
            kv.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    if sections.is_empty() {
        return Err("no [service] sections found".into());
    }
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    for (name, kv) in sections {
        let get = |k: &str| {
            kv.iter()
                .rev()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };
        let Some(host) = get("host").or_else(|| get("hostaddr")) else {
            skipped.push(SkippedEntry {
                name,
                reason: "no host (Unix-socket services are not supported)".into(),
            });
            continue;
        };
        let mut warnings = Vec::new();
        let host = match host.split_once(',') {
            Some((first, _)) => {
                warnings.push(format!("several hosts were listed; only {first} was kept"));
                first.trim().to_string()
            }
            None => host,
        };
        let password_saved = get("password").is_some();
        drafts.push(Draft {
            name,
            engine: Some(EngineKind::Postgres),
            host,
            port: get("port").and_then(|p| p.split(',').next()?.trim().parse().ok()),
            database: get("dbname"),
            user: get("user"),
            sslmode: get("sslmode"),
            ca_cert: get("sslrootcert"),
            warnings,
            password_saved,
            ..Default::default()
        });
    }
    Ok((drafts, skipped))
}

#[cfg(test)]
mod tests {
    use super::super::{Extra, ImportSource, parse_text};
    use crate::model::*;

    #[test]
    fn parses_services() {
        let src = "# comment\n[orders-prod]\nhost=orders.db\nport=6432\ndbname=orders\nuser=app\nsslmode=require\npassword=never-imported\n\n[socket]\ndbname=x\n";
        let r = parse_text(
            ImportSource::PgService,
            src,
            "~/.pg_service.conf",
            &Extra::default(),
        );
        assert_eq!(r.candidates.len(), 1);
        assert_eq!(r.skipped.len(), 1);
        let p = &r.candidates[0].profile;
        assert_eq!(
            (p.name.as_str(), p.host.as_str(), p.port),
            ("orders-prod", "orders.db", 6432)
        );
        assert_eq!(p.tls.mode, TlsMode::Require);
        assert_eq!(p.environment, Environment::Prod);
        assert!(!format!("{p:?}").contains("never-imported"));
    }
}
