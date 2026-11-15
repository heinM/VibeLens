//! Azure Data Studio `User/settings.json` (JSON with comments and trailing commas):
//!
//! ```jsonc
//! "datasource.connectionGroups": [ { "name": "ROOT", "id": "g0" }, { "name": "Prod", "id": "g1", "parentId": "g0", "color": "#A1634D" } ],
//! "datasource.connections": [ { "providerName": "PGSQL", "groupId": "g1", "savePassword": true,
//!     "options": { "connectionName": "orders", "host": "db", "port": "5432", "dbname": "orders",
//!                  "user": "app", "sslmode": "require", "authenticationType": "SqlLogin" } } ]
//! ```
//!
//! The PostgreSQL extension (`PGSQL`) is imported; SQL Server (`MSSQL`) and others are listed
//! as skipped. `AzureMFA` authentication becomes an access-token profile using the Azure CLI.

use serde_json::Value;

use super::{Draft, DraftAuth, SkippedEntry};
use crate::model::EngineKind;

/// Removes `//` and `/* */` comments and trailing commas, leaving strings intact.
pub(super) fn strip_jsonc(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' && i + 1 < b.len() && b[i + 1].is_ascii() {
                out.push_str(&src[i..i + 2]);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            let ch = src[i..].chars().next().unwrap_or('\0');
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                out.push('"');
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
            }
            b',' => {
                // Drop the comma when only whitespace/comments separate it from `}` or `]`.
                let mut j = i + 1;
                loop {
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if b.get(j) == Some(&b'/') && b.get(j + 1) == Some(&b'/') {
                        while j < b.len() && b[j] != b'\n' {
                            j += 1;
                        }
                    } else if b.get(j) == Some(&b'/') && b.get(j + 1) == Some(&b'*') {
                        j += 2;
                        while j + 1 < b.len() && !(b[j] == b'*' && b[j + 1] == b'/') {
                            j += 1;
                        }
                        j = (j + 2).min(b.len());
                    } else {
                        break;
                    }
                }
                if !matches!(b.get(j), Some(b'}' | b']')) {
                    out.push(',');
                }
                i += 1;
            }
            _ => {
                let ch = src[i..].chars().next().unwrap_or('\0');
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    out
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|x| !x.is_empty())
}

/// `Prod / Shop` style path of a group id.
fn group_path(groups: &[Value], id: &str) -> Option<String> {
    let mut names = Vec::new();
    let mut cur = Some(id.to_string());
    let mut guard = 0;
    while let Some(gid) = cur {
        guard += 1;
        if guard > 32 {
            break;
        }
        let g = groups.iter().find(|g| s(g, "id") == Some(gid.as_str()))?;
        let name = s(g, "name").unwrap_or("");
        if name != "ROOT" && !name.is_empty() {
            names.push(name.to_string());
        }
        cur = s(g, "parentId").map(String::from);
    }
    names.reverse();
    (!names.is_empty()).then(|| names.join(" / "))
}

pub(super) fn parse(text: &str) -> Result<(Vec<Draft>, Vec<SkippedEntry>), String> {
    let root: Value =
        serde_json::from_str(&strip_jsonc(text)).map_err(|e| format!("not valid JSON: {e}"))?;
    let conns = root
        .get("datasource.connections")
        .and_then(Value::as_array)
        .ok_or("no \"datasource.connections\" in this settings file")?;
    let groups: Vec<Value> = root
        .get("datasource.connectionGroups")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    for c in conns {
        let o = c.get("options").cloned().unwrap_or(Value::Null);
        let host_raw = s(&o, "host").or_else(|| s(&o, "server")).unwrap_or("");
        let name = s(&o, "connectionName")
            .map(String::from)
            .unwrap_or_else(|| host_raw.to_string());
        let provider = s(c, "providerName").unwrap_or("").to_ascii_uppercase();
        if provider != "PGSQL" {
            skipped.push(SkippedEntry {
                name,
                reason: format!(
                    "{} connections are not supported by VibeLens (Postgres and Valkey/Redis only)",
                    if provider == "MSSQL" {
                        "SQL Server"
                    } else {
                        provider.as_str()
                    }
                ),
            });
            continue;
        }
        if host_raw.is_empty() {
            skipped.push(SkippedEntry {
                name,
                reason: "no host configured".into(),
            });
            continue;
        }
        let port = match o.get("port") {
            Some(Value::Number(n)) => n.as_u64().and_then(|p| u16::try_from(p).ok()),
            Some(Value::String(p)) => p.trim().parse().ok(),
            _ => None,
        };
        let auth = match s(&o, "authenticationType")
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "azuremfa" | "azuremfaanduser" | "azureactivedirectory" => DraftAuth::Entra,
            _ => DraftAuth::Password,
        };
        let group_id = s(c, "groupId").or_else(|| s(&o, "groupId"));
        let group_color = group_id.and_then(|gid| {
            groups
                .iter()
                .find(|g| s(g, "id") == Some(gid))
                .and_then(|g| s(g, "color"))
                .map(String::from)
        });
        drafts.push(Draft {
            name,
            engine: Some(EngineKind::Postgres),
            host: host_raw.to_string(),
            port,
            database: s(&o, "dbname")
                .or_else(|| s(&o, "database"))
                .map(String::from),
            user: s(&o, "user").map(String::from),
            sslmode: s(&o, "sslmode").map(String::from),
            ca_cert: s(&o, "sslrootcert").map(String::from),
            ssh: None,
            environment: None,
            group: group_id.and_then(|g| group_path(&groups, g)),
            color: group_color,
            auth,
            warnings: Vec::new(),
            password_saved: c
                .get("savePassword")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok((drafts, skipped))
}

#[cfg(test)]
mod tests {
    use super::super::{Extra, ImportSource, parse_text};
    use super::*;
    use crate::model::*;

    #[test]
    fn strips_comments_and_trailing_commas_but_not_strings() {
        let src = r#"{
  // a comment
  "a": "http://x/y", /* block */
  "b": [1, 2, ],
  "c": "has, comma }",
}"#;
        let v: Value = serde_json::from_str(&strip_jsonc(src)).expect("json");
        assert_eq!(v["a"], "http://x/y");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        assert_eq!(v["c"], "has, comma }");
    }

    #[test]
    fn imports_pgsql_and_skips_mssql() {
        let src = r##"{
  "workbench.colorTheme": "Default Dark Modern",
  "datasource.connectionGroups": [
    { "name": "ROOT", "id": "g0" },
    { "name": "Azure", "id": "g1", "parentId": "g0", "color": "#A1634D" },
  ],
  "datasource.connections": [
    { "providerName": "PGSQL", "groupId": "g1", "savePassword": true, "id": "c1",
      "options": { "connectionName": "flex prod", "host": "flex.postgres.database.azure.com", "port": "5432",
                   "dbname": "app", "user": "hein@bank.example", "sslmode": "require", "authenticationType": "AzureMFA" } },
    { "providerName": "PGSQL", "id": "c2",
      "options": { "host": "localhost", "dbname": "postgres", "user": "postgres", "authenticationType": "SqlLogin" } },
    { "providerName": "MSSQL", "id": "c3", "options": { "server": "sql01", "connectionName": "erp" } }, // trailing comment
  ]
}"##;
        let r = parse_text(
            ImportSource::AzureDataStudio,
            src,
            "settings.json",
            &Extra::default(),
        );
        assert!(r.error.is_none(), "{:?}", r.error);
        assert_eq!(r.candidates.len(), 2);
        assert_eq!(r.skipped.len(), 1);
        assert!(r.skipped[0].reason.contains("SQL Server"));

        let flex = &r.candidates[0];
        assert_eq!(flex.group.as_deref(), Some("Azure"));
        assert_eq!(flex.profile.color.as_deref(), Some("#a1634d"));
        assert_eq!(flex.profile.environment, Environment::Prod);
        match &flex.profile.auth {
            AuthMethod::AccessToken { username, command } => {
                assert_eq!(username, "hein@bank.example");
                assert!(command.as_deref().unwrap().contains("oss-rdbms"));
            }
            other => panic!("{other:?}"),
        }
        assert!(!flex.needs_secret);

        let local = &r.candidates[1];
        assert_eq!(local.profile.name, "localhost");
        assert_eq!(local.profile.tls.mode, TlsMode::Disable);
    }
}
