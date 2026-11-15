//! Bearer access tokens used as database passwords ([`crate::model::AuthMethod::AccessToken`]):
//! capitec/pg-proxy JWT/OIDC auth and Microsoft Entra auth for Azure Database for PostgreSQL.
//!
//! - [`jwt_expiry`] reads a JWT's `exp` claim (no signature check: the server verifies the token;
//!   we only want to know when to stop reusing it).
//! - [`run_token_command`] runs the profile's token command through the user's login shell, so a
//!   GUI-launched app still finds `az`, `kubectl oidc-login`, `gcloud`, Homebrew tools, ...
//! - [`TokenCache`] keeps command output until shortly before it expires, so a pool of four
//!   connections does not run the command four times.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::Engine;
use chrono::{DateTime, TimeZone, Utc};
use secrecy::{ExposeSecret, SecretString};

use crate::error::{CoreError, Result};

/// Upper bound for one token command (interactive logins such as a browser SSO prompt included).
pub const TOKEN_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// A cached token is not handed out when it expires sooner than this.
const EXPIRY_MARGIN: chrono::Duration = chrono::Duration::seconds(60);
/// Command output without a readable expiry is reused for this long.
const OPAQUE_TOKEN_TTL: Duration = Duration::from_secs(5 * 60);
/// How much of stderr goes into an error message.
const MAX_STDERR: usize = 600;

/// The `exp` claim of a JWT (`header.payload.signature`), if the token is one and has it.
pub fn jwt_expiry(token: &str) -> Option<DateTime<Utc>> {
    let mut parts = token.trim().split('.');
    let (_header, payload, _sig) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let payload = payload.trim_end_matches('=');
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let exp = claims.get("exp")?;
    let secs = exp
        .as_i64()
        .or_else(|| exp.as_f64().map(|f| f as i64))
        .or_else(|| exp.as_str().and_then(|s| s.trim().parse().ok()))?;
    Utc.timestamp_opt(secs, 0).single()
}

/// Replaces anything shaped like a JWT with `[token]` (stderr can echo what a tool received).
fn scrub_tokens(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|w| {
            let t = w.trim_end();
            if t.len() > 40 && t.split('.').count() == 3 && !t.contains('/') {
                format!("[token]{}", &w[t.len()..])
            } else {
                w.to_string()
            }
        })
        .collect()
}

/// Picks the token out of a command's stdout: the whole (trimmed) text, or, for JSON output,
/// the first of `accessToken`, `access_token`, `token`, `id_token`.
pub fn token_from_output(stdout: &str) -> Option<String> {
    let t = stdout.trim();
    if t.is_empty() {
        return None;
    }
    if t.starts_with('{')
        && let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(t)
    {
        return ["accessToken", "access_token", "token", "id_token"]
            .iter()
            .find_map(|k| map.get(*k).and_then(|v| v.as_str()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
    // Tools sometimes print a banner first; the token is the last non-empty line.
    t.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(String::from)
}

/// Runs `command` through the login shell (`$SHELL -lc`, `/bin/sh` as fallback; `cmd /C` on
/// Windows) and returns the token it printed.
pub async fn run_token_command(command: &str, timeout: Duration) -> Result<SecretString> {
    let command = command.trim();
    if command.is_empty() {
        return Err(CoreError::Invalid("the token command is empty".into()));
    }
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| s.starts_with('/'))
            .unwrap_or_else(|| "/bin/sh".to_string());
        let mut c = tokio::process::Command::new(shell);
        c.arg("-lc").arg(command);
        c
    };
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd
        .spawn()
        .map_err(|e| CoreError::Auth(format!("could not start the token command: {e}")))?;
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| {
            CoreError::Timeout(format!(
                "the token command did not finish within {}s",
                timeout.as_secs()
            ))
        })?
        .map_err(|e| CoreError::Auth(format!("the token command failed: {e}")))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut stderr = scrub_tokens(stderr.trim());
    if stderr.chars().count() > MAX_STDERR {
        stderr = stderr.chars().take(MAX_STDERR).collect::<String>() + "…";
    }
    if !out.status.success() {
        let code = out
            .status
            .code()
            .map_or_else(|| "a signal".to_string(), |c| format!("status {c}"));
        return Err(CoreError::Auth(if stderr.is_empty() {
            format!("the token command exited with {code}")
        } else {
            format!("the token command exited with {code}: {stderr}")
        }));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    token_from_output(&stdout)
        .map(SecretString::from)
        .ok_or_else(|| CoreError::Auth("the token command printed no token".into()))
}

struct Cached {
    token: SecretString,
    expires_at: Option<DateTime<Utc>>,
    fetched: Instant,
}

impl Cached {
    fn usable(&self, now: DateTime<Utc>) -> bool {
        match self.expires_at {
            Some(exp) => exp - EXPIRY_MARGIN > now,
            None => self.fetched.elapsed() < OPAQUE_TOKEN_TTL,
        }
    }
}

/// Token-command output per (profile, command). Lookups for the same key are serialised, so
/// several connections opening at once run the command once.
#[derive(Default)]
pub struct TokenCache {
    entries: tokio::sync::Mutex<HashMap<(String, String), Cached>>,
}

impl TokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cached, still-valid token, or a fresh one from `command`.
    pub async fn get_or_run(
        &self,
        profile_id: &str,
        command: &str,
    ) -> Result<(SecretString, Option<DateTime<Utc>>)> {
        let key = (profile_id.to_string(), command.trim().to_string());
        let mut entries = self.entries.lock().await;
        let now = Utc::now();
        if let Some(c) = entries.get(&key)
            && c.usable(now)
        {
            return Ok((c.token.clone(), c.expires_at));
        }
        let token = run_token_command(command, TOKEN_COMMAND_TIMEOUT).await?;
        let expires_at = jwt_expiry(token.expose_secret());
        if let Some(exp) = expires_at
            && exp <= now
        {
            return Err(CoreError::Auth(format!(
                "the token command returned a token that already expired ({exp})"
            )));
        }
        entries.retain(|(id, _), _| id != profile_id);
        entries.insert(
            key,
            Cached {
                token: token.clone(),
                expires_at,
                fetched: Instant::now(),
            },
        );
        Ok((token, expires_at))
    }

    /// Forgets the cached token of a profile (its settings changed or the server rejected it).
    pub async fn forget(&self, profile_id: &str) {
        self.entries
            .lock()
            .await
            .retain(|(id, _), _| id != profile_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: &str) -> String {
        let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        format!(
            "{}.{}.{}",
            e.encode(r#"{"alg":"RS256"}"#),
            e.encode(claims),
            e.encode("sig")
        )
    }

    #[test]
    fn reads_exp_from_jwts_only() {
        assert_eq!(
            jwt_expiry(&jwt(r#"{"exp":1800000000,"email":"a@b"}"#)),
            Utc.timestamp_opt(1_800_000_000, 0).single()
        );
        assert_eq!(
            jwt_expiry(&jwt(r#"{"exp":1800000000.5}"#)).map(|d| d.timestamp()),
            Some(1_800_000_000)
        );
        assert_eq!(jwt_expiry(&jwt(r#"{"sub":"x"}"#)), None);
        assert_eq!(jwt_expiry("not-a-jwt"), None);
        assert_eq!(jwt_expiry("a.b.c.d"), None);
        assert_eq!(jwt_expiry("a.%%%.c"), None);
    }

    #[test]
    fn extracts_tokens_from_command_output() {
        assert_eq!(
            token_from_output("  abc.def.ghi \n").as_deref(),
            Some("abc.def.ghi")
        );
        assert_eq!(
            token_from_output("Logging in...\n\neyJ.x.y\n").as_deref(),
            Some("eyJ.x.y")
        );
        assert_eq!(
            token_from_output(r#"{"accessToken":"tok","expiresOn":"2026-01-01"}"#).as_deref(),
            Some("tok")
        );
        assert_eq!(
            token_from_output(r#"{"access_token":"t2"}"#).as_deref(),
            Some("t2")
        );
        assert_eq!(token_from_output(r#"{"nothing":"here"}"#), None);
        assert_eq!(token_from_output("   \n "), None);
    }

    #[test]
    fn scrubs_jwts_from_stderr() {
        let t = jwt(r#"{"exp":1800000000,"email":"someone@example.com"}"#);
        let s = scrub_tokens(&format!("rejected token {t} because"));
        assert!(!s.contains(&t));
        assert!(s.contains("[token] because"));
        assert_eq!(
            scrub_tokens("see https://a.b/c.d.e"),
            "see https://a.b/c.d.e"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_commands_and_caches_until_expiry() {
        let cache = TokenCache::new();
        let dir = tempfile::tempdir().expect("tmp");
        let counter = dir.path().join("n");
        let far = jwt(r#"{"exp":4102444800}"#); // 2100-01-01
        let cmd = format!("echo x >> '{}'; echo '{far}'", counter.display());
        let (a, exp) = cache.get_or_run("p", &cmd).await.expect("token");
        assert_eq!(a.expose_secret(), far);
        assert!(exp.is_some());
        let (b, _) = cache.get_or_run("p", &cmd).await.expect("cached");
        assert_eq!(b.expose_secret(), far);
        let runs = std::fs::read_to_string(&counter).expect("counter");
        assert_eq!(
            runs.lines().count(),
            1,
            "second lookup must come from the cache"
        );
        cache.forget("p").await;
        cache.get_or_run("p", &cmd).await.expect("again");
        let runs = std::fs::read_to_string(&counter).expect("counter");
        assert_eq!(runs.lines().count(), 2);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reports_failures_and_expired_tokens() {
        let err = run_token_command("echo boom >&2; exit 3", Duration::from_secs(10))
            .await
            .expect_err("fails");
        assert!(err.to_string().contains("status 3"), "{err}");
        assert!(err.to_string().contains("boom"), "{err}");
        let err = run_token_command("true", Duration::from_secs(10))
            .await
            .expect_err("no output");
        assert!(err.to_string().contains("no token"), "{err}");
        let past = jwt(r#"{"exp":1000}"#);
        let err = TokenCache::new()
            .get_or_run("p", &format!("echo '{past}'"))
            .await
            .expect_err("expired");
        assert!(err.to_string().contains("already expired"), "{err}");
    }
}
