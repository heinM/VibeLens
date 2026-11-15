//! The AWS CLI–compatible SSO token cache (`~/.aws/sso/cache/<sha1_hex(start_url)>.json`).
//!
//! This is the one place where credentials intentionally live on disk (see DESIGN.md §1 #8): the
//! AWS CLI and every AWS SDK read the same files, which is what lets `aws sso login` and VibeLens
//! share a session.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha1::{Digest, Sha1};

use crate::error::{CoreError, Result};
use crate::model::SsoSessionInfo;

/// Lower-case hex SHA-1 of the start URL: the CLI's cache file stem for `sso_start_url` profiles.
pub fn start_url_hash(start_url: &str) -> String {
    hex::encode(Sha1::digest(start_url.as_bytes()))
}

/// `<sha1_hex(start_url)>.json`
pub fn cache_file_name(start_url: &str) -> String {
    format!("{}.json", start_url_hash(start_url))
}

/// RFC 3339 UTC with a literal `Z` and whole seconds, which is what the CLI writes.
pub fn format_ts(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parses RFC 3339, plus the legacy `YYYY-MM-DDTHH:MM:SSUTC` form that AWS CLI v1 wrote.
pub fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Some(stripped) = s.strip_suffix("UTC") {
        let with_z = format!("{}Z", stripped.trim_end());
        return DateTime::parse_from_rfc3339(&with_z)
            .ok()
            .map(|d| d.with_timezone(&Utc));
    }
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

mod ts_opt {
    use super::*;

    pub fn serialize<S: Serializer>(
        v: &Option<DateTime<Utc>>,
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        match v {
            Some(t) => s.serialize_str(&format_ts(*t)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> std::result::Result<Option<DateTime<Utc>>, D::Error> {
        let raw: Option<String> = Option::deserialize(d)?;
        Ok(raw.as_deref().and_then(parse_ts))
    }
}

/// Contents of one cache file. Unknown fields are preserved on rewrite.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SsoCacheEntry {
    #[serde(default)]
    pub start_url: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default, with = "ts_opt", skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, with = "ts_opt", skip_serializing_if = "Option::is_none")]
    pub registration_expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl std::fmt::Debug for SsoCacheEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsoCacheEntry")
            .field("start_url", &self.start_url)
            .field("region", &self.region)
            .field("expires_at", &self.expires_at)
            .field("has_refresh_token", &self.refresh_token.is_some())
            .finish_non_exhaustive()
    }
}

impl SsoCacheEntry {
    /// True when the access token is past its expiry at `now`. A missing expiry counts as expired.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_none_or(|t| t <= now)
    }

    /// Whether a CreateToken(refresh_token) call is possible at `now`.
    pub fn can_refresh(&self, now: DateTime<Utc>) -> bool {
        let has = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.is_empty());
        has(&self.refresh_token)
            && has(&self.client_id)
            && has(&self.client_secret)
            && self.registration_expires_at.is_none_or(|t| t > now)
    }

    /// Public summary for the UI.
    pub fn info(&self) -> SsoSessionInfo {
        SsoSessionInfo {
            start_url: self.start_url.clone(),
            region: self.region.clone(),
            expires_at: self.expires_at,
        }
    }
}

/// A cache entry together with the file it came from (so a refresh rewrites the same file).
#[derive(Debug, Clone)]
pub struct LoadedEntry {
    pub path: PathBuf,
    pub entry: SsoCacheEntry,
}

/// Reads and parses one cache file. Returns `None` for files that are not SSO token files
/// (for example the CLI's `botocore-client-id-*.json` registration files) or are unreadable.
pub fn read_entry(path: &Path) -> Option<SsoCacheEntry> {
    let bytes = fs::read(path).ok()?;
    let entry: SsoCacheEntry = serde_json::from_slice(&bytes).ok()?;
    if entry.start_url.is_empty() || entry.access_token.is_empty() {
        return None;
    }
    Some(entry)
}

/// All usable token files in `dir` (any expiry), in file-name order. A missing dir is empty.
pub fn read_all(dir: &Path) -> Vec<LoadedEntry> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| read_entry(&path).map(|entry| LoadedEntry { path, entry }))
        .collect()
}

/// Sessions for the UI: one per start URL (the one expiring last wins), sorted by start URL.
pub fn sessions(dir: &Path) -> Vec<SsoSessionInfo> {
    let mut best: BTreeMap<String, SsoSessionInfo> = BTreeMap::new();
    for loaded in read_all(dir) {
        let info = loaded.entry.info();
        match best.get(&info.start_url) {
            Some(existing) if existing.expires_at >= info.expires_at => {}
            _ => {
                best.insert(info.start_url.clone(), info);
            }
        }
    }
    best.into_values().collect()
}

/// Finds the token for `start_url`: the canonical `<sha1(url)>.json` first, then any file whose
/// `startUrl` matches (the CLI keys files for `sso-session` profiles by *session name*).
/// When several match, the one expiring last wins.
pub fn find_for_start_url(dir: &Path, start_url: &str) -> Option<LoadedEntry> {
    let canonical = dir.join(cache_file_name(start_url));
    let mut candidates: Vec<LoadedEntry> = Vec::new();
    if let Some(entry) = read_entry(&canonical)
        && entry.start_url == start_url
    {
        candidates.push(LoadedEntry {
            path: canonical.clone(),
            entry,
        });
    }
    for loaded in read_all(dir) {
        if loaded.entry.start_url == start_url && loaded.path != canonical {
            candidates.push(loaded);
        }
    }
    // Prefer the canonical file on ties: `max_by_key` returns the last max, so iterate reversed.
    candidates
        .into_iter()
        .rev()
        .max_by_key(|c| c.entry.expires_at)
}

/// Writes `entry` to `path` atomically (temp file in the same directory + rename), with mode 0600
/// on unix. Creates the directory if needed.
pub fn write_entry(path: &Path, entry: &SsoCacheEntry) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| CoreError::Invalid("SSO cache path has no parent directory".into()))?;
    fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(entry)?;

    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("sso"),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> std::io::Result<()> {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2030, 1, 2, h, 4, 5).unwrap()
    }

    fn entry(url: &str, exp: Option<DateTime<Utc>>) -> SsoCacheEntry {
        SsoCacheEntry {
            start_url: url.into(),
            region: "eu-west-1".into(),
            access_token: "tok".into(),
            expires_at: exp,
            client_id: Some("cid".into()),
            client_secret: Some("sec".into()),
            registration_expires_at: Some(t(23)),
            refresh_token: Some("ref".into()),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn file_name_is_lower_hex_sha1_of_start_url() {
        // Known vector: sha1("https://example.awsapps.com/start")
        let name = cache_file_name("abc");
        assert_eq!(name, "a9993e364706816aba3e25717850c26c9cd0d89d.json");
        let name = cache_file_name("https://d-1234567890.awsapps.com/start");
        assert_eq!(name.len(), 40 + ".json".len());
        assert!(
            name.trim_end_matches(".json")
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        );
    }

    #[test]
    fn json_round_trip_uses_rfc3339_z_and_cli_field_names() {
        let e = entry("https://x.awsapps.com/start", Some(t(10)));
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["startUrl"], "https://x.awsapps.com/start");
        assert_eq!(v["accessToken"], "tok");
        assert_eq!(v["expiresAt"], "2030-01-02T10:04:05Z");
        assert_eq!(v["registrationExpiresAt"], "2030-01-02T23:04:05Z");
        assert_eq!(v["clientId"], "cid");
        assert_eq!(v["clientSecret"], "sec");
        assert_eq!(v["refreshToken"], "ref");
        let back: SsoCacheEntry = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn refresh_token_omitted_when_absent_and_unknown_fields_survive() {
        let mut e = entry("u", Some(t(1)));
        e.refresh_token = None;
        let v = serde_json::to_value(&e).unwrap();
        assert!(v.get("refreshToken").is_none());

        let raw = r#"{"startUrl":"u","region":"r","accessToken":"a","expiresAt":"2030-01-02T01:04:05Z","somethingNew":{"x":1}}"#;
        let parsed: SsoCacheEntry = serde_json::from_str(raw).unwrap();
        let again = serde_json::to_value(&parsed).unwrap();
        assert_eq!(again["somethingNew"]["x"], 1);
    }

    #[test]
    fn parses_legacy_utc_suffix_and_offsets() {
        assert_eq!(parse_ts("2030-01-02T10:04:05UTC"), Some(t(10)));
        assert_eq!(parse_ts("2030-01-02T12:04:05+02:00"), Some(t(10)));
        assert_eq!(parse_ts("garbage"), None);
    }

    #[test]
    fn write_is_atomic_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("nested/sso/cache");
        let e = entry("https://x.awsapps.com/start", Some(t(10)));
        let path = cache.join(cache_file_name(&e.start_url));
        write_entry(&path, &e).unwrap();
        assert_eq!(read_entry(&path).unwrap(), e);
        // Overwrite works and leaves no temp files behind.
        let mut e2 = e.clone();
        e2.access_token = "tok2".into();
        write_entry(&path, &e2).unwrap();
        assert_eq!(read_entry(&path).unwrap().access_token, "tok2");
        let leftovers: Vec<_> = fs::read_dir(&cache)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(leftovers.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn sessions_lists_valid_files_including_expired_and_skips_junk() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path();
        let a = entry("https://a.awsapps.com/start", Some(t(10)));
        let b = entry(
            "https://b.awsapps.com/start",
            Some(Utc.with_ymd_and_hms(2001, 1, 1, 0, 0, 0).unwrap()),
        ); // long expired
        write_entry(&c.join(cache_file_name(&a.start_url)), &a).unwrap();
        write_entry(&c.join(cache_file_name(&b.start_url)), &b).unwrap();
        // CLI client-registration file: no startUrl/accessToken.
        fs::write(
            c.join("botocore-client-id-eu-west-1.json"),
            r#"{"clientId":"x","clientSecret":"y"}"#,
        )
        .unwrap();
        fs::write(c.join("broken.json"), "{not json").unwrap();
        fs::write(c.join("notes.txt"), "hello").unwrap();
        // Same start URL under a session-name file name: the later expiry wins.
        let a2 = entry("https://a.awsapps.com/start", Some(t(20)));
        write_entry(&c.join("my-session.json"), &a2).unwrap();

        let s = sessions(c);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].start_url, "https://a.awsapps.com/start");
        assert_eq!(s[0].expires_at, Some(t(20)));
        assert_eq!(s[1].start_url, "https://b.awsapps.com/start");
        assert!(s[1].expires_at.unwrap() < t(0));
        assert!(sessions(&c.join("missing")).is_empty());
    }

    #[test]
    fn find_prefers_canonical_file_then_scans_for_session_name_files() {
        let dir = tempfile::tempdir().unwrap();
        let c = dir.path();
        let url = "https://a.awsapps.com/start";
        let named = entry(url, Some(t(10)));
        write_entry(&c.join("corp-session.json"), &named).unwrap();
        assert_eq!(
            find_for_start_url(c, url).unwrap().path,
            c.join("corp-session.json")
        );

        let canonical = entry(url, Some(t(10)));
        write_entry(&c.join(cache_file_name(url)), &canonical).unwrap();
        assert_eq!(
            find_for_start_url(c, url).unwrap().path,
            c.join(cache_file_name(url)),
            "tie goes to the canonical file"
        );

        let newer = entry(url, Some(t(22)));
        write_entry(&c.join("corp-session.json"), &newer).unwrap();
        assert_eq!(
            find_for_start_url(c, url).unwrap().path,
            c.join("corp-session.json")
        );
        assert!(find_for_start_url(c, "https://other/start").is_none());
    }

    #[test]
    fn expiry_and_refreshability() {
        let e = entry("u", Some(t(10)));
        assert!(!e.is_expired(t(9)));
        assert!(e.is_expired(t(10)));
        assert!(entry("u", None).is_expired(t(0)));
        assert!(e.can_refresh(t(10)));
        assert!(!e.can_refresh(t(23)), "registration expired");
        let mut no_secret = e.clone();
        no_secret.client_secret = None;
        assert!(!no_secret.can_refresh(t(1)));
    }

    #[test]
    fn debug_does_not_leak_tokens() {
        let e = entry("u", Some(t(1)));
        let d = format!("{e:?}");
        assert!(
            !d.contains("\"tok\"") && !d.contains("\"sec\"") && !d.contains("\"ref\""),
            "{d}"
        );
    }
}
