//! Agent runtime (OWNER: agent agent).
//!
//! - Providers: `Anthropic` (Messages API, SSE, tool_use blocks) and `OpenAiCompatible`
//!   (Chat Completions, SSE, tool_calls) over `reqwest`. Model id is config, never code.
//! - Loop: user prompt → stream → for each tool call: assess risk (policy.rs) → maybe
//!   `ApprovalRequired` and await `resolve_approval` → run via `ToolBackend` → feed result
//!   back. Max 16 tool rounds per turn; cancellable.
//! - Untrusted output: tool results are wrapped as data, truncated to a byte budget, and
//!   obey the profile's `AiDataAccess`. Prompt injection from DB content can at most cause
//!   a tool *request*, which still hits the approval gate.
//! - Providers config: `<config_dir>/providers.json`; API keys in the vault.
//!
//! Internal layout: `sse` (event-stream parser), `providers/{anthropic,openai}` (request
//! builders + stream decoders), `tools` (specs, argument parsing, execution, result framing),
//! `policy` (pure risk function), `prompt` (system prompt), `session` (turn loop, history
//! budget), `store` (presets + `providers.json`), `diff` (approval previews).

mod diff;
mod iceberg_fmt;
mod policy;
mod prompt;
mod providers;
mod redact;
mod session;
mod sse;
mod store;
mod tools;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use self::providers::ProviderClient;
use self::session::{Sessions, TurnRunner, lock};
use self::store::ProviderStore;
use crate::error::{CoreError, Result};
use crate::model::*;
use crate::ports::{SecretVault, ToolBackend};

pub struct AgentService {
    vault: Arc<dyn SecretVault>,
    tools: Arc<dyn ToolBackend>,
    store: ProviderStore,
    http: Mutex<Option<reqwest::Client>>,
    sessions: Sessions,
}

impl AgentService {
    pub fn new(
        vault: Arc<dyn SecretVault>,
        tools: Arc<dyn ToolBackend>,
        config_dir: PathBuf,
    ) -> Arc<Self> {
        Arc::new(Self {
            vault,
            tools,
            store: ProviderStore::new(&config_dir),
            http: Mutex::new(None),
            sessions: Arc::new(Mutex::new(Default::default())),
        })
    }

    fn http(&self) -> Result<reqwest::Client> {
        let mut guard = self.http.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = guard.as_ref() {
            return Ok(c.clone());
        }
        // Idempotent: errors only mean a provider is already installed.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        // Never follow a redirect. Every request carries the API key and the conversation, and
        // a redirect would pass them on: custom headers such as `x-api-key` survive a redirect
        // to another host, and a 307/308 re-sends the body. `ProviderClient::post` reports the
        // 3xx with its target instead.
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .user_agent(concat!("VibeLens/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CoreError::Llm(format!("could not initialise the HTTP client: {e}")))?;
        *guard = Some(client.clone());
        Ok(client)
    }

    async fn api_key(&self, provider_id: &str) -> Result<Option<SecretString>> {
        self.vault.get(&vault_keys::llm_api_key(provider_id)).await
    }

    /// Built-in presets merged with saved configs (saved wins by id).
    pub async fn list_providers(&self) -> Result<Vec<ProviderSummary>> {
        let configs = self.store.merged().await?;
        let mut out = Vec::with_capacity(configs.len());
        for config in configs {
            let has_api_key = match self.api_key(&config.id).await {
                Ok(k) => k.is_some_and(|k| !k.expose_secret().is_empty()),
                Err(e) => {
                    // A broken keychain must not hide the provider list from Settings.
                    tracing::warn!(
                        "could not check the API key for provider {}: {e}",
                        config.id
                    );
                    false
                }
            };
            out.push(ProviderSummary {
                config,
                has_api_key,
            });
        }
        Ok(out)
    }

    /// Saves a provider config. `api_key`: `None` keeps the stored key, `Some("")` deletes it,
    /// `Some(k)` replaces it. The key goes to the vault, never to `providers.json`.
    pub async fn save_provider(
        &self,
        mut config: ProviderConfig,
        api_key: Option<SecretString>,
    ) -> Result<()> {
        config.id = config.id.trim().to_string();
        config.label = config.label.trim().to_string();
        config.base_url = config.base_url.trim().to_string();
        config.model = config.model.trim().to_string();
        store::validate(&config)?;
        // The key this provider will have once saved: the new one, or the stored one when kept.
        let will_have_key = match &api_key {
            Some(k) => !k.expose_secret().trim().is_empty(),
            // An unreadable keychain is not a reason to refuse the save; the same rule runs
            // again when the provider is used.
            None => self
                .api_key(&config.id)
                .await
                .ok()
                .flatten()
                .is_some_and(|k| !k.expose_secret().trim().is_empty()),
        };
        if will_have_key {
            store::check_key_is_not_sent_in_cleartext(&config)?;
        }
        let key_name = vault_keys::llm_api_key(&config.id);
        if let Some(k) = &api_key {
            let k = k.expose_secret().trim();
            if k.is_empty() {
                self.vault.delete(&key_name).await?;
            } else {
                if k.chars().any(char::is_control) {
                    return Err(CoreError::Invalid("the API key contains invalid characters (line breaks or control characters); paste it again".into()));
                }
                self.vault
                    .set(&key_name, &SecretString::from(k.to_string()))
                    .await?;
            }
        }
        self.store.upsert(config).await
    }

    /// Removes a saved config and its key. Deleting a built-in preset's override restores the
    /// preset defaults; the preset itself always remains.
    pub async fn delete_provider(&self, id: &str) -> Result<()> {
        let removed = self.store.remove(id).await?;
        if !removed && !store::is_preset(id) {
            return Err(CoreError::NotFound(format!("provider '{id}'")));
        }
        self.vault.delete(&vault_keys::llm_api_key(id)).await
    }

    /// Spawns the turn and returns its id immediately. Events stream into `tx`;
    /// the last event is always `TurnFinished` or `Failed`.
    ///
    /// Fails synchronously (nothing is spawned) when the provider has no model or key, the id
    /// is unknown, or the session already has a turn running.
    pub async fn start_turn(
        self: &Arc<Self>,
        session_id: String,
        provider_id: String,
        prompt: String,
        ctx: AgentContext,
        tx: mpsc::Sender<AgentEvent>,
    ) -> Result<String> {
        if prompt.trim().is_empty() {
            return Err(CoreError::Invalid("the prompt is empty".into()));
        }
        let cfg = self
            .store
            .find(&provider_id)
            .await?
            .ok_or_else(|| CoreError::NotFound(format!("provider '{provider_id}'")))?;
        let key = match self.api_key(&cfg.id).await {
            Ok(k) => k,
            Err(e) if cfg.requires_api_key => return Err(e),
            Err(e) => {
                tracing::warn!("could not read the API key for provider {}: {e}", cfg.id);
                None
            }
        };
        let provider = ProviderClient::new(cfg, key, self.http()?)?;

        let turn_id = uuid::Uuid::new_v4().to_string();
        let (history, cancel, generation) = {
            let mut map = lock(&self.sessions);
            let s = map.entry(session_id.clone()).or_default();
            if s.running {
                return Err(CoreError::Invalid(
                    "a turn is already running in this session; cancel it or wait for it to finish"
                        .into(),
                ));
            }
            s.running = true;
            s.cancel = CancellationToken::new();
            s.pending.clear();
            (s.history.clone(), s.cancel.clone(), s.generation)
        };

        let runner = TurnRunner {
            sessions: self.sessions.clone(),
            session_id,
            generation,
            turn_id: turn_id.clone(),
            provider,
            backend: self.tools.clone(),
            tx,
            cancel,
            ctx,
            history,
            usage: Usage::default(),
            done: false,
        };
        tokio::spawn(runner.run(prompt));
        Ok(turn_id)
    }

    /// Answers a pending `ApprovalRequired`. Unknown session or call → `NotFound`.
    pub async fn resolve_approval(
        &self,
        session_id: &str,
        call_id: &str,
        decision: ApprovalDecision,
    ) -> Result<()> {
        let sender = {
            let mut map = lock(&self.sessions);
            let s = map
                .get_mut(session_id)
                .ok_or_else(|| CoreError::NotFound(format!("session '{session_id}'")))?;
            s.pending
                .remove(call_id)
                .ok_or_else(|| CoreError::NotFound(format!("pending approval '{call_id}'")))?
        };
        sender.send(decision).map_err(|_| {
            CoreError::NotFound(format!("pending approval '{call_id}' (the turn has ended)"))
        })
    }

    /// Cancels the running turn (if any) and rejects everything awaiting approval. Idempotent.
    pub async fn cancel(&self, session_id: &str) -> Result<()> {
        let mut map = lock(&self.sessions);
        if let Some(s) = map.get_mut(session_id) {
            s.cancel.cancel();
            s.pending.clear();
        }
        Ok(())
    }

    /// Forgets the conversation. A running turn is cancelled and will not write back.
    pub async fn reset(&self, session_id: &str) -> Result<()> {
        let mut map = lock(&self.sessions);
        if let Some(s) = map.get_mut(session_id) {
            s.cancel.cancel();
            s.pending.clear();
            s.history.clear();
            s.generation += 1;
            s.running = false;
        }
        Ok(())
    }
}
