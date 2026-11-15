//! Provider presets and `<config_dir>/providers.json` persistence (no secrets; API keys live in
//! the vault under `vault_keys::llm_api_key(id)`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::error::{CoreError, Result};
use crate::model::{ProviderConfig, ProviderKind};
use crate::profiles::atomic_write;

/// Built-in providers. The model id is deliberately empty for everything but Anthropic: model
/// names churn every few months, so the user sets them in Settings.
pub(super) fn presets() -> Vec<ProviderConfig> {
    let base =
        |id: &str, label: &str, kind, url: &str, model: &str, requires_api_key| ProviderConfig {
            id: id.into(),
            label: label.into(),
            kind,
            base_url: url.into(),
            model: model.into(),
            max_tokens: 8192,
            temperature: None,
            extra_headers: Default::default(),
            requires_api_key,
            portkey: None,
        };
    vec![
        base(
            "anthropic",
            "Anthropic",
            ProviderKind::Anthropic,
            "https://api.anthropic.com",
            "claude-sonnet-5-5",
            true,
        ),
        base(
            "xai",
            "xAI (Grok)",
            ProviderKind::OpenAiCompatible,
            "https://api.x.ai/v1",
            "",
            true,
        ),
        base(
            "gemini",
            "Google Gemini",
            ProviderKind::OpenAiCompatible,
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "",
            true,
        ),
        base(
            "ollama",
            "Ollama (local)",
            ProviderKind::OpenAiCompatible,
            "http://localhost:11434/v1",
            "",
            false,
        ),
        ProviderConfig {
            portkey: Some(crate::model::PortkeyOptions::default()),
            ..base(
                "portkey",
                "Portkey AI Gateway",
                ProviderKind::Portkey,
                super::providers::portkey::DEFAULT_BASE_URL,
                "",
                true,
            )
        },
    ]
}

/// Presets overlaid with saved configs (saved wins by id); saved-only ids follow, in file order.
pub(super) fn merge(saved: &[ProviderConfig]) -> Vec<ProviderConfig> {
    let mut out: Vec<ProviderConfig> = presets()
        .into_iter()
        .map(|p| saved.iter().find(|s| s.id == p.id).cloned().unwrap_or(p))
        .collect();
    for s in saved {
        if !out.iter().any(|o| o.id == s.id) {
            out.push(s.clone());
        }
    }
    out
}

pub(super) fn is_preset(id: &str) -> bool {
    presets().iter().any(|p| p.id == id)
}

/// Rejects configs that would be dangerous or useless to save.
pub(super) fn validate(cfg: &ProviderConfig) -> Result<()> {
    let bad = |m: &str| Err(CoreError::Invalid(m.to_string()));
    if cfg.id.is_empty()
        || cfg.id.len() > 64
        || !cfg
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return bad("provider id must be 1-64 characters: letters, digits, '-' or '_'");
    }
    if cfg.label.trim().is_empty() {
        return bad("provider needs a name");
    }
    if cfg.max_tokens == 0 || cfg.max_tokens > 1_000_000 {
        return bad("max tokens must be between 1 and 1,000,000");
    }
    if let Some(t) = cfg.temperature
        && (!t.is_finite() || !(0.0..=2.0).contains(&t))
    {
        return bad("temperature must be between 0 and 2");
    }
    let url = url::Url::parse(cfg.base_url.trim())
        .map_err(|e| CoreError::Invalid(format!("base URL is not valid: {e}")))?;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    match url.scheme() {
        "https" => {}
        "http" => {
            let loopback = host == "localhost"
                || host == "127.0.0.1"
                || host == "[::1]"
                || host.ends_with(".localhost");
            if cfg.requires_api_key && !loopback {
                return bad(
                    "an API key would be sent unencrypted: use an https:// base URL for remote providers",
                );
            }
        }
        _ => return bad("base URL must start with http:// or https://"),
    }
    if host.is_empty() {
        return bad("base URL has no host");
    }
    for (k, v) in &cfg.extra_headers {
        if reqwest::header::HeaderName::from_bytes(k.as_bytes()).is_err()
            || reqwest::header::HeaderValue::from_str(v).is_err()
        {
            return Err(CoreError::Invalid(format!(
                "extra header '{k}' is not a valid HTTP header"
            )));
        }
        // providers.json is not the vault: credentials must not end up in it.
        if SECRET_HEADERS.contains(&k.to_ascii_lowercase().as_str())
            || v.trim_start()
                .get(..7)
                .is_some_and(|p| p.eq_ignore_ascii_case("bearer "))
        {
            return Err(CoreError::Invalid(format!(
                "extra header '{k}' looks like a credential: put secrets in the API key field, not in extra headers"
            )));
        }
    }
    if cfg.kind == ProviderKind::Portkey {
        super::providers::portkey::validate(cfg)?;
    }
    Ok(())
}

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

/// Older builds wrote providers.json with default permissions; tighten it to match new writes.
#[cfg(unix)]
fn tighten_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if path.exists()
        && let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    {
        tracing::warn!("could not restrict providers.json permissions: {e}");
    }
}

#[cfg(not(unix))]
fn tighten_permissions(_path: &Path) {}

#[derive(Serialize, Deserialize, Default)]
struct FileFormat {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    providers: Vec<ProviderConfig>,
}

/// Saved provider configs. Read-modify-write cycles are serialised with a lock.
pub(super) struct ProviderStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl ProviderStore {
    pub fn new(config_dir: &Path) -> Self {
        let path = config_dir.join("providers.json");
        tighten_permissions(&path);
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    async fn read_locked(&self) -> Result<Vec<ProviderConfig>> {
        let text = match tokio::fs::read_to_string(&self.path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let parsed = serde_json::from_str::<FileFormat>(&text)
            .map(|f| f.providers)
            .or_else(|_| serde_json::from_str::<Vec<ProviderConfig>>(&text));
        match parsed {
            Ok(v) => Ok(v),
            Err(e) => {
                // Keep the broken file for inspection instead of failing every call forever.
                tracing::warn!("providers.json is unreadable ({e}); moving it aside");
                let aside = self.path.with_extension("json.corrupt");
                let _ = tokio::fs::rename(&self.path, &aside).await;
                Ok(Vec::new())
            }
        }
    }

    async fn write_locked(&self, providers: Vec<ProviderConfig>) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(&FileFormat {
            version: 1,
            providers,
        })?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || atomic_write(&path, &bytes, true))
            .await
            .map_err(|e| CoreError::Invalid(format!("providers.json write task failed: {e}")))?
    }

    /// Saved configs only.
    pub async fn saved(&self) -> Result<Vec<ProviderConfig>> {
        let _g = self.lock.lock().await;
        self.read_locked().await
    }

    /// Presets merged with saved configs.
    pub async fn merged(&self) -> Result<Vec<ProviderConfig>> {
        Ok(merge(&self.saved().await?))
    }

    /// Finds one provider (saved wins over preset).
    pub async fn find(&self, id: &str) -> Result<Option<ProviderConfig>> {
        Ok(self.merged().await?.into_iter().find(|p| p.id == id))
    }

    pub async fn upsert(&self, cfg: ProviderConfig) -> Result<()> {
        let _g = self.lock.lock().await;
        let mut saved = self.read_locked().await?;
        match saved.iter_mut().find(|s| s.id == cfg.id) {
            Some(slot) => *slot = cfg,
            None => saved.push(cfg),
        }
        self.write_locked(saved).await
    }

    /// Removes a saved config. Returns whether one existed.
    pub async fn remove(&self, id: &str) -> Result<bool> {
        let _g = self.lock.lock().await;
        let mut saved = self.read_locked().await?;
        let before = saved.len();
        saved.retain(|s| s.id != id);
        let removed = saved.len() != before;
        if removed {
            self.write_locked(saved).await?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom(id: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            label: "Mine".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: "https://llm.example.com/v1".into(),
            model: "m".into(),
            max_tokens: 4096,
            temperature: Some(0.3),
            extra_headers: Default::default(),
            requires_api_key: true,
            portkey: None,
        }
    }

    #[test]
    fn presets_match_spec() {
        let p = presets();
        let ids: Vec<_> = p.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["anthropic", "xai", "gemini", "ollama", "portkey"]);
        assert_eq!(p[0].model, "claude-sonnet-5-5");
        assert_eq!(p[0].max_tokens, 8192);
        assert_eq!(p[0].kind, ProviderKind::Anthropic);
        assert_eq!(p[0].base_url, "https://api.anthropic.com");
        assert!(p[1..3].iter().all(|x| x.model.is_empty()
            && x.requires_api_key
            && x.kind == ProviderKind::OpenAiCompatible));
        assert_eq!(p[1].base_url, "https://api.x.ai/v1");
        assert_eq!(
            p[2].base_url,
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
        assert_eq!(p[3].base_url, "http://localhost:11434/v1");
        assert!(!p[3].requires_api_key);
        assert_eq!(p[4].kind, ProviderKind::Portkey);
        assert_eq!(p[4].base_url, "https://api.portkey.ai/v1");
        assert!(p[4].requires_api_key && p[4].model.is_empty() && p[4].portkey.is_some());
        for x in &p {
            validate(x).unwrap();
        }
    }

    #[test]
    fn merge_saved_wins_and_custom_appended() {
        let mut a = presets().remove(0);
        a.model = "other".into();
        let m = merge(&[custom("zzz"), a.clone()]);
        assert_eq!(m.len(), 6);
        assert_eq!(m[0], a, "saved overrides preset in place");
        assert_eq!(m[5].id, "zzz");
    }

    #[test]
    fn validation() {
        assert!(validate(&custom("ok_id-1")).is_ok());
        for bad in ["", "has space", "a/b", &"x".repeat(65)] {
            assert!(validate(&custom(bad)).is_err(), "{bad}");
        }
        let mut c = custom("a");
        c.base_url = "ftp://x".into();
        assert!(validate(&c).is_err());
        c.base_url = "not a url".into();
        assert!(validate(&c).is_err());
        c.base_url = "http://remote.example.com/v1".into();
        assert!(
            validate(&c)
                .unwrap_err()
                .to_string()
                .contains("unencrypted")
        );
        c.requires_api_key = false;
        assert!(validate(&c).is_ok(), "no key to leak");
        c.requires_api_key = true;
        c.base_url = "http://127.0.0.1:8080/v1".into();
        assert!(validate(&c).is_ok());
        c = custom("a");
        c.max_tokens = 0;
        assert!(validate(&c).is_err());
        c = custom("a");
        c.temperature = Some(5.0);
        assert!(validate(&c).is_err());
        c = custom("a");
        c.extra_headers.insert("bad header".into(), "v".into());
        assert!(validate(&c).is_err());
        c = custom("a");
        c.label = "  ".into();
        assert!(validate(&c).is_err());
    }

    #[test]
    fn extra_headers_reject_credentials() {
        for (k, v) in [
            ("Authorization", "abc"),
            ("api-key", "abc"),
            ("X-Goog-Api-Key", "abc"),
            ("Cookie", "a=b"),
            ("x-custom", "Bearer abc"),
        ] {
            let mut c = custom("a");
            c.extra_headers.insert(k.into(), v.into());
            let e = validate(&c).unwrap_err().to_string();
            assert!(e.contains("API key field"), "{k}: {e}");
            assert!(!e.contains("abc"), "the value must not be echoed: {e}");
        }
        let mut c = custom("a");
        c.extra_headers.insert("x-title".into(), "VibeLens".into());
        assert!(validate(&c).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn providers_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;

        // A file left behind by an older build is tightened on startup.
        std::fs::write(&path, "[]").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let store = ProviderStore::new(dir.path());
        assert_eq!(mode(&path), 0o600);

        store.upsert(custom("one")).await.unwrap();
        assert_eq!(mode(&path), 0o600);
    }

    #[tokio::test]
    async fn persistence_roundtrip_and_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProviderStore::new(dir.path());
        assert!(store.saved().await.unwrap().is_empty());
        store.upsert(custom("one")).await.unwrap();
        let mut two = custom("two");
        two.model = "m2".into();
        store.upsert(two.clone()).await.unwrap();
        let mut one = custom("one");
        one.model = "changed".into();
        store.upsert(one.clone()).await.unwrap();
        let saved = ProviderStore::new(dir.path()).saved().await.unwrap();
        assert_eq!(saved, vec![one, two]);
        assert!(store.remove("one").await.unwrap());
        assert!(!store.remove("one").await.unwrap());
        assert_eq!(store.find("two").await.unwrap().unwrap().model, "m2");
        assert_eq!(
            store.find("anthropic").await.unwrap().unwrap().id,
            "anthropic"
        );
        assert!(store.find("nope").await.unwrap().is_none());
        // No temp files left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["providers.json"]);

        // Corrupt file: moved aside, store keeps working.
        std::fs::write(dir.path().join("providers.json"), "{ not json").unwrap();
        assert!(store.saved().await.unwrap().is_empty());
        assert!(dir.path().join("providers.json.corrupt").exists());
        store.upsert(custom("three")).await.unwrap();
        assert_eq!(store.saved().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn accepts_bare_array_format() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("providers.json"),
            serde_json::to_string(&vec![custom("x")]).unwrap(),
        )
        .unwrap();
        assert_eq!(
            ProviderStore::new(dir.path()).saved().await.unwrap()[0].id,
            "x"
        );
    }
}
