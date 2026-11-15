//! Tolerant parser for `~/.aws/config` and `~/.aws/credentials`, used to list profiles in the UI.
//!
//! We deliberately do **not** use `aws-config`'s profile loader here: it is strict (a single malformed
//! line fails the whole file) and does not expose "what kind of profile is this". The SDK still does
//! the real credential resolution; this module only classifies what is on disk.
//!
//! Rules (matching the CLI/SDK):
//! - config file sections: `[default]`, `[profile name]`, `[sso-session name]`. Other headers
//!   (for example a bare `[name]`, `[services x]`) are ignored, because the SDK ignores them too and
//!   listing them would offer profiles that cannot be used.
//! - credentials file sections: `[name]` (raw profile names).
//! - full-line comments start with `#` or `;`; trailing ` #`/` ;` comments are stripped.
//! - indented lines below `key =` (empty value) are a nested block (e.g. `s3 =`) and are skipped.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::Result;
use crate::model::AwsProfileSummary;

/// One INI section with lower-cased keys.
type Section = BTreeMap<String, String>;

#[derive(Debug, Default)]
struct ParsedConfig {
    profiles: BTreeMap<String, Section>,
    sso_sessions: BTreeMap<String, Section>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Config,
    Credentials,
}

enum Header {
    Profile(String),
    SsoSession(String),
    /// A section we recognise syntactically but intentionally ignore.
    Ignored,
}

fn classify_header(raw: &str, kind: FileKind) -> Header {
    let raw = raw.trim();
    match kind {
        FileKind::Credentials => {
            // Tolerate `[profile x]` in the credentials file (some tools write it).
            let name = raw.strip_prefix("profile ").map(str::trim).unwrap_or(raw);
            if name.is_empty() {
                Header::Ignored
            } else {
                Header::Profile(name.to_string())
            }
        }
        FileKind::Config => {
            if raw == "default" {
                return Header::Profile("default".to_string());
            }
            let mut parts = raw.splitn(2, char::is_whitespace);
            let first = parts.next().unwrap_or("");
            let rest = parts.next().map(str::trim).unwrap_or("");
            match first {
                "profile" if !rest.is_empty() => Header::Profile(rest.to_string()),
                "sso-session" if !rest.is_empty() => Header::SsoSession(rest.to_string()),
                _ => Header::Ignored,
            }
        }
    }
}

fn strip_inline_comment(value: &str) -> &str {
    let bytes = value.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if (b == b'#' || b == b';') && i > 0 && bytes[i - 1].is_ascii_whitespace() {
            return value[..i].trim_end();
        }
    }
    value
}

enum Target {
    None,
    Profile(String),
    SsoSession(String),
}

fn parse_into(text: &str, kind: FileKind, out: &mut ParsedConfig) {
    let mut target = Target::None;
    // Set when the previous key had an empty value: following indented lines belong to a nested block.
    let mut in_nested = false;

    for line in text.lines() {
        let line = line.trim_start_matches('\u{feff}');
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') {
            in_nested = false;
            // `[name] # comment` is tolerated.
            let Some(end) = trimmed.find(']') else {
                target = Target::None;
                continue;
            };
            target = match classify_header(&trimmed[1..end], kind) {
                Header::Profile(n) => {
                    out.profiles.entry(n.clone()).or_default();
                    Target::Profile(n)
                }
                Header::SsoSession(n) => {
                    out.sso_sessions.entry(n.clone()).or_default();
                    Target::SsoSession(n)
                }
                Header::Ignored => Target::None,
            };
            continue;
        }
        let indented = line.starts_with(char::is_whitespace);
        if indented && in_nested {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let value = strip_inline_comment(v.trim()).trim().to_string();
        if key.is_empty() {
            continue;
        }
        in_nested = value.is_empty();
        match &target {
            Target::Profile(n) => {
                out.profiles
                    .entry(n.clone())
                    .or_default()
                    .insert(key, value);
            }
            Target::SsoSession(n) => {
                out.sso_sessions
                    .entry(n.clone())
                    .or_default()
                    .insert(key, value);
            }
            Target::None => {}
        }
    }
}

fn non_empty(section: &Section, key: &str) -> Option<String> {
    section
        .get(key)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Parses the text of an AWS config file and (optionally) credentials file into profile summaries,
/// sorted by name. Never fails: malformed lines are skipped.
pub fn parse_profiles(config_text: &str, credentials_text: &str) -> Vec<AwsProfileSummary> {
    let mut cfg = ParsedConfig::default();
    parse_into(config_text, FileKind::Config, &mut cfg);

    // Credentials file: only profile sections, merged on top of the config file's values.
    let mut creds = ParsedConfig::default();
    parse_into(credentials_text, FileKind::Credentials, &mut creds);

    let mut merged: BTreeMap<String, Section> = cfg.profiles;
    for (name, section) in creds.profiles {
        merged.entry(name).or_default().extend(section);
    }

    merged
        .into_iter()
        .map(|(name, p)| {
            // sso_start_url directly on the profile, or via the referenced sso-session block.
            let session = non_empty(&p, "sso_session").and_then(|s| cfg.sso_sessions.get(&s));
            let sso_start_url = non_empty(&p, "sso_start_url")
                .or_else(|| session.and_then(|s| non_empty(s, "sso_start_url")));
            let is_sso = sso_start_url.is_some() || non_empty(&p, "sso_session").is_some();

            let kind = if is_sso {
                "sso"
            } else if non_empty(&p, "role_arn").is_some() {
                "assumeRole"
            } else if non_empty(&p, "credential_process").is_some() {
                "process"
            } else if non_empty(&p, "aws_access_key_id").is_some() {
                "static"
            } else {
                "other"
            };

            AwsProfileSummary {
                name,
                kind: kind.to_string(),
                region: non_empty(&p, "region"),
                sso_start_url,
                sso_account_id: non_empty(&p, "sso_account_id"),
                sso_role_name: non_empty(&p, "sso_role_name"),
            }
        })
        .collect()
}

/// Reads the two files (missing or unreadable files count as empty) and parses them.
pub fn list_profiles_from(
    config_file: Option<&Path>,
    credentials_file: Option<&Path>,
) -> Result<Vec<AwsProfileSummary>> {
    Ok(parse_profiles(
        &read_lossy(config_file),
        &read_lossy(credentials_file),
    ))
}

fn read_lossy(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(path = %path.display(), error = %e, "could not read AWS profile file");
            }
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
# a comment
[default]
region = eu-west-1
output = json

[profile sso-direct]
sso_start_url = https://d-123.awsapps.com/start
sso_region = eu-west-1
sso_account_id = 111111111111
sso_role_name = ReadOnly
region = eu-central-1

[profile sso-via-session]
sso_session = corp
sso_account_id = 222222222222
sso_role_name = Admin
region = af-south-1

[profile sso-missing-session]
sso_session = nope
sso_account_id = 333333333333

[sso-session corp]
sso_start_url = https://corp.awsapps.com/start ; trailing comment
sso_region = eu-west-1
sso_registration_scopes = sso:account:access

[profile assume]
role_arn = arn:aws:iam::444444444444:role/Deploy
source_profile = sso-direct
region = us-east-1

[profile proc]
credential_process = /usr/local/bin/get-creds --flag # note
region = us-west-2

[profile  spaced ]
region = ap-southeast-2

[profile nested]
region = eu-north-1
s3 =
  max_concurrent_requests = 10
  region = should-not-leak

[profile other-only]
output = text

[not-a-profile]
region = zz-fake-1

[services foo]
dynamodb =
  endpoint_url = http://localhost:4566
"#;

    const CREDENTIALS: &str = r#"
[default]
aws_access_key_id = AKIAEXAMPLE
aws_secret_access_key = secret

[ci]
aws_access_key_id = AKIACI
aws_secret_access_key = secret2
region = eu-west-3

[proc]
aws_access_key_id = AKIA_OVERRIDES_NOTHING
"#;

    fn by_name<'a>(v: &'a [AwsProfileSummary], n: &str) -> &'a AwsProfileSummary {
        v.iter()
            .find(|p| p.name == n)
            .unwrap_or_else(|| panic!("missing profile {n}"))
    }

    #[test]
    fn classifies_all_kinds_and_resolves_sso_sessions() {
        let v = parse_profiles(CONFIG, CREDENTIALS);

        let direct = by_name(&v, "sso-direct");
        assert_eq!(direct.kind, "sso");
        assert_eq!(
            direct.sso_start_url.as_deref(),
            Some("https://d-123.awsapps.com/start")
        );
        assert_eq!(direct.sso_account_id.as_deref(), Some("111111111111"));
        assert_eq!(direct.sso_role_name.as_deref(), Some("ReadOnly"));
        assert_eq!(direct.region.as_deref(), Some("eu-central-1"));

        let via = by_name(&v, "sso-via-session");
        assert_eq!(via.kind, "sso");
        assert_eq!(
            via.sso_start_url.as_deref(),
            Some("https://corp.awsapps.com/start"),
            "inline comment stripped"
        );
        assert_eq!(via.sso_account_id.as_deref(), Some("222222222222"));
        assert_eq!(via.region.as_deref(), Some("af-south-1"));

        // sso_session pointing at a missing block is still an SSO profile, just without a start URL.
        let missing = by_name(&v, "sso-missing-session");
        assert_eq!(missing.kind, "sso");
        assert_eq!(missing.sso_start_url, None);

        let assume = by_name(&v, "assume");
        assert_eq!(assume.kind, "assumeRole");
        assert_eq!(assume.region.as_deref(), Some("us-east-1"));

        let proc = by_name(&v, "proc");
        assert_eq!(proc.kind, "process", "credential_process beats static keys");

        assert_eq!(by_name(&v, "ci").kind, "static");
        assert_eq!(by_name(&v, "ci").region.as_deref(), Some("eu-west-3"));
        assert_eq!(by_name(&v, "default").kind, "static");
        assert_eq!(
            by_name(&v, "default").region.as_deref(),
            Some("eu-west-1"),
            "region comes from config, keys from credentials"
        );
        assert_eq!(by_name(&v, "other-only").kind, "other");
    }

    #[test]
    fn sorted_and_ignores_foreign_sections_and_nested_blocks() {
        let v = parse_profiles(CONFIG, CREDENTIALS);
        let names: Vec<&str> = v.iter().map(|p| p.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        assert!(!names.contains(&"not-a-profile"));
        assert!(
            !names.contains(&"corp"),
            "sso-session blocks are not profiles"
        );
        assert!(!names.iter().any(|n| n.starts_with("services")));
        assert!(names.contains(&"spaced"), "header whitespace is trimmed");
        assert_eq!(
            by_name(&v, "nested").region.as_deref(),
            Some("eu-north-1"),
            "nested s3 block must not leak keys"
        );
    }

    #[test]
    fn tolerates_garbage_bom_and_crlf() {
        let text = "\u{feff}[default]\r\nregion = eu-west-1\r\nthis line has no equals\r\n[broken\r\nregion=x\r\n[profile ok]\r\nregion=eu-west-2\r\n";
        let v = parse_profiles(text, "");
        assert_eq!(by_name(&v, "default").region.as_deref(), Some("eu-west-1"));
        assert_eq!(by_name(&v, "ok").region.as_deref(), Some("eu-west-2"));
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn empty_and_missing_files_are_empty_list() {
        assert!(parse_profiles("", "").is_empty());
        let dir = tempfile::tempdir().unwrap();
        let v = list_profiles_from(
            Some(&dir.path().join("nope")),
            Some(&dir.path().join("nope2")),
        )
        .unwrap();
        assert!(v.is_empty());
        assert!(list_profiles_from(None, None).unwrap().is_empty());
    }

    #[test]
    fn credentials_file_profile_prefix_tolerated() {
        let v = parse_profiles("", "[profile weird]\naws_access_key_id = A\n");
        assert_eq!(by_name(&v, "weird").kind, "static");
    }
}
