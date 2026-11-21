//! Live connection registry (OWNER: connections agent).
//!
//! - One `PgHandle` per (profile, database), one `ValkeyHandle` and one `IcebergHandle` per
//!   profile, created lazily. Iceberg has no SSH tunnel and its services (AWS identity, vault)
//!   arrive once via [`ConnectionManager::set_iceberg_deps`].
//! - Tunnel opened first when configured; driver dials `tunnel.local_addr` but verifies TLS
//!   against `profile.host`. IAM tokens are signed for `profile.host:profile.port` (real endpoint).
//! - Idle reaper: every 30s closes the pooled *server sessions* of Postgres handles idle longer
//!   than the profile's `idle_disconnect_secs` so serverless compute can scale to zero, but keeps
//!   the handle, its tunnel and its status: the next query reopens a session transparently, and
//!   the connection does not vanish from the UI after five quiet minutes. Valkey and Iceberg
//!   handles are still closed outright. `0` means "never".
//! - `AppToolBackend` implements `ports::ToolBackend` over this + `WorkspaceFs`.
//! - Redshift: one `RedshiftEngine` per profile. It holds only a Data API client (no socket,
//!   no session, no tunnel), so the idle reaper leaves it alone; AWS identity comes from the
//!   same `AwsService` as Iceberg's.
//!
//! Concurrency rules:
//! - The registry lock is a plain `std::sync::Mutex` and is **never held across an `.await`**.
//! - Connecting is single-flight per key: a per-key `tokio::sync::Mutex` serialises callers, and
//!   the cache is re-checked after acquiring it, so N concurrent `pg()` calls open one pool.
//! - `disconnect()` bumps a per-profile epoch; a connect that was in flight notices the change
//!   when it finishes, throws its fresh handle away and reports `Cancelled`.
//! - A cached handle is reused only while the profile's *connection-relevant* settings are
//!   unchanged (so editing the host/auth/TLS of a profile takes effect on next use) and its
//!   tunnel is still alive. For Iceberg that covers the catalog, storage, query engine and
//!   default namespace (they are all part of the profile).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use secrecy::SecretString;

use crate::error::{CoreError, Result};
use crate::iceberg::{IcebergDeps, IcebergHandle};
use crate::model::*;
use crate::pg::{PgConnectParams, PgHandle};
use crate::ports::{
    CredentialFn, CredentialSource, FileVersion, ProfileStore, SecretVault, ToolBackend,
    TunnelHandle, TunnelProvider,
};
use crate::redshift::RedshiftEngine;
use crate::valkey::{ValkeyConnectParams, ValkeyHandle};
use crate::workspace::WorkspaceFs;
use tokio_util::sync::CancellationToken;

/// How often the idle reaper wakes up.
const REAP_INTERVAL: Duration = Duration::from_secs(30);
/// ElastiCache IAM connections are cut by the server after 12 hours; recycle a bit earlier so
/// the next use reconnects with a freshly minted token instead of failing mid-session.
const ELASTICACHE_IAM_MAX_AGE_SECS: i64 = 11 * 60 * 60;
/// Upper bound for an ephemeral `test()` (tunnel + auth + handshake).
const TEST_TIMEOUT: Duration = Duration::from_secs(120);
/// `ToolBackend::valkey_get` element cap.
const AGENT_VALKEY_MAX_ITEMS: usize = 200;
/// Statement timeout for agent-issued SQL.
const AGENT_SQL_TIMEOUT_MS: u64 = 30_000;
/// Athena queries are queued and run server-side, so they get a longer budget than a local scan.
/// The same budget applies to the other remote engines (Trino, Spark).
const AGENT_ATHENA_TIMEOUT_MS: u64 = 120_000;
/// Whole-script budget for agent SQL, below the agent's 180 s tool timeout so the engine is
/// cancelled (and stops the work on the server) before the agent gives up on the tool.
const AGENT_SCRIPT_DEADLINE: Duration = Duration::from_secs(150);
/// Redshift statements are queued and run server-side too (and a Serverless workgroup may have
/// to resume first), so they get the same budget as Athena.
const AGENT_REDSHIFT_TIMEOUT_MS: u64 = 120_000;
/// Safety valve for `valkey_scan`: stop after this many SCAN pages even if the cursor never ends.
const MAX_SCAN_PAGES: usize = 2_000;

struct PgEntry {
    handle: Arc<PgHandle>,
    tunnel: Option<TunnelHandle>,
    connected_at: DateTime<Utc>,
    signature: String,
    server_version: Option<String>,
}

struct ValkeyEntry {
    handle: Arc<ValkeyHandle>,
    tunnel: Option<TunnelHandle>,
    connected_at: DateTime<Utc>,
    signature: String,
    server_version: Option<String>,
}

struct IcebergEntry {
    handle: Arc<IcebergHandle>,
    connected_at: DateTime<Utc>,
    signature: String,
    /// `IcebergHandle::describe_target()` captured when the handle was created.
    target: String,
}

struct RedshiftEntry {
    engine: Arc<RedshiftEngine>,
    connected_at: DateTime<Utc>,
    signature: String,
    /// `RedshiftEngine::describe_target()`: `Redshift · {cluster or workgroup} · {db} · {region}`.
    target: String,
}

/// Non-steady states. "Connected" is derived from the live entries so it can never go stale.
#[derive(Debug, Clone)]
enum Transient {
    Connecting {
        engine: EngineKind,
        since: DateTime<Utc>,
        tunneled: bool,
    },
    Failed {
        engine: EngineKind,
        since: DateTime<Utc>,
        tunneled: bool,
        detail: String,
    },
}

type ConnectLockKey = (ProfileId, Option<String>);

#[derive(Default)]
struct State {
    pg: HashMap<(ProfileId, String), PgEntry>,
    valkey: HashMap<ProfileId, ValkeyEntry>,
    iceberg: HashMap<ProfileId, IcebergEntry>,
    redshift: HashMap<ProfileId, RedshiftEntry>,
    transient: HashMap<ProfileId, Transient>,
    epochs: HashMap<ProfileId, u64>,
    connect_locks: HashMap<ConnectLockKey, Arc<tokio::sync::Mutex<()>>>,
}

impl State {
    fn has_live(&self, profile_id: &str) -> bool {
        self.valkey.contains_key(profile_id)
            || self.iceberg.contains_key(profile_id)
            || self.redshift.contains_key(profile_id)
            || self.pg.keys().any(|(id, _)| id == profile_id)
    }
    fn epoch(&self, profile_id: &str) -> u64 {
        self.epochs.get(profile_id).copied().unwrap_or(0)
    }
}

pub struct ConnectionManager {
    profiles: Arc<dyn ProfileStore>,
    creds: Arc<dyn CredentialSource>,
    tunnels: Arc<dyn TunnelProvider>,
    state: Mutex<State>,
    reaper_started: AtomicBool,
    iceberg_deps: OnceLock<IcebergDeps>,
    /// Test wiring only: where Redshift Data API requests go instead of the regional endpoint.
    /// Never read from a profile (see `RedshiftEngine::with_endpoint_url`).
    redshift_endpoint: OnceLock<String>,
}

impl ConnectionManager {
    pub fn new(
        profiles: Arc<dyn ProfileStore>,
        creds: Arc<dyn CredentialSource>,
        tunnels: Arc<dyn TunnelProvider>,
    ) -> Arc<Self> {
        Arc::new(Self {
            profiles,
            creds,
            tunnels,
            state: Mutex::new(State::default()),
            reaper_started: AtomicBool::new(false),
            iceberg_deps: OnceLock::new(),
            redshift_endpoint: OnceLock::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // No code path leaves `State` half-updated across a panic, so poisoning is harmless.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Spawns the idle reaper on the current tokio runtime. Calling it twice is a no-op.
    /// The task holds only a `Weak` reference, so it ends when the manager is dropped.
    pub fn start_idle_reaper(self: &Arc<Self>) {
        if self.reaper_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let weak: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(REAP_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticker.tick().await; // the first tick fires immediately
            loop {
                ticker.tick().await;
                let Some(manager) = weak.upgrade() else { break };
                let closed = manager.reap_idle().await;
                if closed > 0 {
                    tracing::info!(closed, "closed idle connections");
                }
            }
        });
    }

    /// One reaper pass; returns how many handles were closed. Public so it can be driven
    /// directly (tests, "reconnect now" style features).
    ///
    /// Postgres: releases the pooled server sessions of a handle idle longer than its profile's
    /// `idle_disconnect_secs` (`0` = never); the handle itself is only closed when its profile no
    /// longer exists. Valkey/Iceberg: closes the handle when idle too long, and ElastiCache IAM
    /// connections approaching the server's 12-hour cut-off.
    pub async fn reap_idle(&self) -> usize {
        // Snapshot under the lock; decide with the lock released (profile lookups hit the store).
        let pg: Vec<_> = self
            .lock()
            .pg
            .iter()
            .map(|(k, e)| (k.clone(), e.handle.clone()))
            .collect();
        let valkey: Vec<_> = self
            .lock()
            .valkey
            .iter()
            .map(|(k, e)| (k.clone(), e.handle.clone(), e.connected_at))
            .collect();
        let iceberg: Vec<_> = self
            .lock()
            .iceberg
            .iter()
            .map(|(k, e)| (k.clone(), e.handle.clone()))
            .collect();

        let mut closed = 0;
        for (key, handle) in pg {
            let verdict = match self.profiles.get(&key.0) {
                Ok(p) => {
                    if idle_exceeded(handle.idle_secs(), p.pg.idle_disconnect_secs) {
                        let released = handle.release_idle_connections();
                        if released > 0 {
                            tracing::debug!(profile_id = %key.0, database = %key.1, released, "released idle Postgres sessions");
                            closed += released;
                        }
                    }
                    false
                }
                Err(CoreError::NotFound(_)) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "idle reaper could not read profile; leaving connection open");
                    false
                }
            };
            if verdict {
                let removed = {
                    let mut st = self.lock();
                    match st.pg.get(&key) {
                        Some(e) if Arc::ptr_eq(&e.handle, &handle) => st.pg.remove(&key),
                        _ => None,
                    }
                };
                if let Some(entry) = removed {
                    entry.handle.close().await;
                    drop(entry.tunnel);
                    closed += 1;
                    self.forget_if_idle(&key.0);
                }
            }
        }
        for (id, handle, connected_at) in valkey {
            let verdict = match self.profiles.get(&id) {
                Ok(p) => {
                    let too_old = matches!(p.auth, AuthMethod::ElastiCacheIam { .. })
                        && (Utc::now() - connected_at).num_seconds() > ELASTICACHE_IAM_MAX_AGE_SECS;
                    too_old || idle_exceeded(handle.idle_secs(), p.valkey.idle_disconnect_secs)
                }
                Err(CoreError::NotFound(_)) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "idle reaper could not read profile; leaving connection open");
                    false
                }
            };
            if verdict {
                let removed = {
                    let mut st = self.lock();
                    match st.valkey.get(&id) {
                        Some(e) if Arc::ptr_eq(&e.handle, &handle) => st.valkey.remove(&id),
                        _ => None,
                    }
                };
                if let Some(entry) = removed {
                    entry.handle.close().await;
                    drop(entry.tunnel);
                    closed += 1;
                    self.forget_if_idle(&id);
                }
            }
        }
        for (id, handle) in iceberg {
            // Iceberg has no idle setting of its own: it shares the Postgres default (300 s) that
            // every profile carries, so an Iceberg connection does not hold catalog/AWS state forever.
            let verdict = match self.profiles.get(&id) {
                Ok(p) => idle_exceeded(handle.idle_secs(), p.pg.idle_disconnect_secs),
                Err(CoreError::NotFound(_)) => true,
                Err(e) => {
                    tracing::warn!(error = %e, "idle reaper could not read profile; leaving connection open");
                    false
                }
            };
            if verdict {
                let removed = {
                    let mut st = self.lock();
                    match st.iceberg.get(&id) {
                        Some(e) if Arc::ptr_eq(&e.handle, &handle) => st.iceberg.remove(&id),
                        _ => None,
                    }
                };
                if let Some(entry) = removed {
                    entry.handle.close().await;
                    closed += 1;
                    self.forget_if_idle(&id);
                }
            }
        }
        closed
    }

    pub fn profiles(&self) -> Arc<dyn ProfileStore> {
        self.profiles.clone()
    }

    /// Connects (or reuses the existing connection) to the profile's default database and
    /// reports the status. A failure is returned as `Err` *and* recorded as the profile's
    /// `Error` status (with detail) for `status()`.
    pub async fn connect(&self, profile_id: &str) -> Result<ConnectionStatus> {
        let profile = self.profiles.get(profile_id)?;
        match profile.engine {
            EngineKind::Postgres => {
                let handle = self.pg(profile_id, None).await?;
                let version = match handle.server_version().await {
                    Ok(v) => Some(v),
                    Err(e) => {
                        tracing::warn!(profile_id, error = %e, "connected, but could not read the server version");
                        None
                    }
                };
                let db = profile.effective_database(None);
                if let Some(entry) = self.lock().pg.get_mut(&(profile.id.clone(), db))
                    && Arc::ptr_eq(&entry.handle, &handle)
                {
                    entry.server_version = version;
                }
            }
            EngineKind::Valkey => {
                let handle = self.valkey(profile_id).await?;
                let version = match handle.server_info().await {
                    Ok(info) => info.version,
                    Err(e) => {
                        tracing::warn!(profile_id, error = %e, "connected, but could not read the server info");
                        None
                    }
                };
                if let Some(entry) = self.lock().valkey.get_mut(&profile.id)
                    && Arc::ptr_eq(&entry.handle, &handle)
                {
                    entry.server_version = version;
                }
            }
            EngineKind::Iceberg => {
                // The entry already carries the target description, so there is nothing more to
                // record: `iceberg()` registered Connecting/Connected/Error like pg/valkey do.
                self.iceberg(profile_id).await?;
            }
            EngineKind::Redshift => {
                // `redshift()` verified the target answers and recorded the status.
                self.redshift(profile_id).await?;
            }
        }
        let st = self.lock();
        status_of(&st, &profile.id).ok_or_else(|| {
            CoreError::NotConnected(format!("{} disconnected while connecting", profile.name))
        })
    }

    /// Closes every handle and tunnel of the profile. Idempotent, and works for profiles that
    /// no longer exist in the store (the delete-profile flow calls it first).
    pub async fn disconnect(&self, profile_id: &str) -> Result<()> {
        let (pg, valkey, iceberg, redshift) = {
            let mut st = self.lock();
            *st.epochs.entry(profile_id.to_string()).or_insert(0) += 1;
            st.transient.remove(profile_id);
            st.connect_locks.retain(|(id, _), _| id != profile_id);
            let keys: Vec<_> = st
                .pg
                .keys()
                .filter(|(id, _)| id == profile_id)
                .cloned()
                .collect();
            let pg: Vec<PgEntry> = keys.iter().filter_map(|k| st.pg.remove(k)).collect();
            (
                pg,
                st.valkey.remove(profile_id),
                st.iceberg.remove(profile_id),
                st.redshift.remove(profile_id),
            )
        };
        // A Redshift engine holds no connection; dropping it is all there is to close.
        drop(redshift);
        for entry in pg {
            entry.handle.close().await;
            drop(entry.tunnel);
        }
        if let Some(entry) = valkey {
            entry.handle.close().await;
            drop(entry.tunnel);
        }
        if let Some(entry) = iceberg {
            entry.handle.close().await;
        }
        Ok(())
    }

    /// Ephemeral connect + version check; nothing is cached. `password_override` lets the
    /// wizard test a password before it is saved to the keychain; it replaces credential
    /// resolution for `Password`/`None` auth and is ignored for IAM / Secrets Manager auth.
    ///
    /// Connection-level failures (unreachable, bad password, TLS, tunnel…) come back as
    /// `Ok(TestResult { ok: false, message })`; only invalid input is an `Err`.
    ///
    /// A test writes nothing: when the SSH host's key is not pinned it reports the key the
    /// server presented in `host_key` and leaves pinning to the caller.
    pub async fn test(
        &self,
        profile: &ConnectionProfile,
        password_override: Option<SecretString>,
    ) -> Result<TestResult> {
        self.test_with_engine_secret(profile, password_override, None)
            .await
    }

    /// [`test`](Self::test) with the not-yet-saved password or token of an Iceberg
    /// connection's remote query engine (Trino, Spark through Livy). Like the catalog secret it
    /// is served from an overlay on the vault for this test only; `None` uses the stored one.
    pub async fn test_with_engine_secret(
        &self,
        profile: &ConnectionProfile,
        password_override: Option<SecretString>,
        engine_secret_override: Option<SecretString>,
    ) -> Result<TestResult> {
        if profile.engine == EngineKind::Iceberg {
            // `host`/`port` are unused for Iceberg; the catalog options are what matter.
            if profile.iceberg.is_none() {
                return Err(CoreError::Invalid(
                    "Iceberg catalog options are required".into(),
                ));
            }
        } else if profile.engine == EngineKind::Redshift {
            // `host`/`port` are unused; the options are validated by `check_profile` below.
            if profile.redshift.is_none() {
                return Err(CoreError::Invalid(
                    "Redshift connection options are required".into(),
                ));
            }
        } else {
            if profile.host.trim().is_empty() {
                return Err(CoreError::Invalid("host is required".into()));
            }
            if profile.port == 0 {
                return Err(CoreError::Invalid(
                    "port must be between 1 and 65535".into(),
                ));
            }
        }
        // A draft that would send a secret in the clear is invalid input, not a failed test.
        crate::cleartext::check_profile(profile)?;
        let started = Instant::now();
        let outcome = match tokio::time::timeout(
            TEST_TIMEOUT,
            self.test_inner(profile, password_override, engine_secret_override),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(CoreError::Timeout(format!(
                "connection test took longer than {}s",
                TEST_TIMEOUT.as_secs()
            ))),
        };
        let latency_ms = started.elapsed().as_millis() as u64;
        match outcome {
            Ok(version) => Ok(TestResult {
                ok: true,
                message: match &version {
                    Some(v) => format!("Connected: {v}"),
                    None => "Connected".to_string(),
                },
                server_version: version,
                latency_ms,
                host_key: None,
            }),
            Err(e @ (CoreError::Invalid(_) | CoreError::Cancelled)) => Err(e),
            Err(e) => Ok(TestResult {
                ok: false,
                server_version: None,
                latency_ms,
                message: e.to_string(),
                host_key: match e {
                    CoreError::UnknownHostKey(key) => Some(key),
                    _ => None,
                },
            }),
        }
    }

    async fn test_inner(
        &self,
        profile: &ConnectionProfile,
        password_override: Option<SecretString>,
        engine_secret_override: Option<SecretString>,
    ) -> Result<Option<String>> {
        if profile.engine == EngineKind::Iceberg {
            return self
                .test_iceberg(profile, password_override, engine_secret_override)
                .await;
        }
        if profile.engine == EngineKind::Redshift {
            // No password exists for Redshift (IAM-signed Data API), so there is nothing to override.
            return self.test_redshift(profile).await;
        }
        let creds = self.credential_fn(profile, password_override);
        match profile.engine {
            EngineKind::Postgres => {
                // A proxy whose backend is down otherwise looks like a refused login or a hang.
                if let Some(url) = profile
                    .pg
                    .pgproxy
                    .as_ref()
                    .and_then(|p| p.health_url.as_deref())
                    .filter(|u| !u.trim().is_empty())
                    && profile.tunnel.is_none()
                {
                    crate::pg::pgproxy_ready(url).await?;
                }
                let db = profile.effective_database(None);
                let (handle, tunnel) = self.dial_pg(profile, &db, creds).await?;
                let version = handle.server_version().await;
                handle.close().await;
                drop(tunnel);
                Ok(Some(version?))
            }
            EngineKind::Valkey => {
                let (handle, tunnel) = self.dial_valkey(profile, creds).await?;
                let info = handle.server_info().await;
                handle.close().await;
                drop(tunnel);
                Ok(info?.version)
            }
            // Handled above (it needs no credential closure).
            EngineKind::Iceberg => Err(CoreError::Other(
                "Iceberg connections are tested by test_iceberg".into(),
            )),
            EngineKind::Redshift => Err(CoreError::Other(
                "Redshift connections are tested by test_redshift".into(),
            )),
        }
    }

    /// Ephemeral Iceberg connect + target description. `secret_override` (the wizard's
    /// not-yet-saved REST token / client secret / catalog password) is served from an overlay on
    /// the vault, only for this profile's `iceberg_secret` key; nothing is written anywhere.
    async fn test_iceberg(
        &self,
        profile: &ConnectionProfile,
        secret_override: Option<SecretString>,
        engine_secret_override: Option<SecretString>,
    ) -> Result<Option<String>> {
        let mut deps = self.iceberg_deps()?;
        if let Some(secret) = secret_override {
            deps.vault = Arc::new(OverlayVault {
                inner: deps.vault.clone(),
                key: vault_keys::iceberg_secret(&profile.id),
                value: secret,
            });
        }
        // A secret typed for the test is bound to the endpoint being tested, and only there.
        if let Some(secret) = engine_secret_override
            && let Some(origin) = profile
                .iceberg
                .as_ref()
                .and_then(|o| crate::remote::config::engine_origin(&o.query_engine))
        {
            deps.vault = Arc::new(OverlayVault {
                inner: deps.vault.clone(),
                key: vault_keys::engine_secret(&profile.id),
                value: secret,
            });
            deps.vault = Arc::new(OverlayVault {
                inner: deps.vault.clone(),
                key: vault_keys::engine_secret_origin(&profile.id),
                value: SecretString::from(origin),
            });
        }
        let handle = IcebergHandle::connect(profile.clone(), deps).await?;
        let target = handle.describe_target();
        let probed = handle.probe_engine().await;
        handle.close().await;
        probed?;
        Ok(Some(target).filter(|t| !t.is_empty()))
    }

    /// Ephemeral Redshift engine + one `ListDatabases` call, which signs in with the profile's
    /// identity and authentication and so checks all of it. Nothing is cached.
    async fn test_redshift(&self, profile: &ConnectionProfile) -> Result<Option<String>> {
        let engine = self.build_redshift(profile).await?;
        engine.ping().await?;
        Ok(Some(engine.describe_target()))
    }

    /// One entry per profile that is connecting, connected or failed. Disconnected profiles
    /// are simply absent. Sorted by profile id.
    pub fn status(&self) -> Vec<ConnectionStatus> {
        let st = self.lock();
        let mut ids: Vec<&ProfileId> = st
            .valkey
            .keys()
            .chain(st.iceberg.keys())
            .chain(st.redshift.keys())
            .chain(st.pg.keys().map(|(id, _)| id))
            .chain(st.transient.keys())
            .collect();
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .filter_map(|id| status_of(&st, id))
            .collect()
    }

    /// Pool for `(profile, database)`, connecting lazily. `database` defaults to the profile's
    /// database, then `"postgres"` ([`ConnectionProfile::effective_database`], which is also what
    /// the agent's approval card shows).
    pub async fn pg(&self, profile_id: &str, database: Option<&str>) -> Result<Arc<PgHandle>> {
        let profile = self.profiles.get(profile_id)?;
        require_engine(&profile, EngineKind::Postgres)?;
        let db = profile.effective_database(database);
        let key = (profile.id.clone(), db.clone());
        let signature = connection_signature(&profile);

        if let Some(handle) = self.cached_pg(&key, &signature).await {
            return Ok(handle);
        }
        let lock = self.connect_lock((profile.id.clone(), Some(db.clone())));
        let _single_flight = lock.lock().await;
        if let Some(handle) = self.cached_pg(&key, &signature).await {
            return Ok(handle);
        }

        let epoch = self.lock().epoch(&profile.id);
        let mut attempt = Attempt::begin(self, &profile);
        let dialed = self
            .dial_pg(&profile, &db, self.credential_fn(&profile, None))
            .await;
        let (handle, tunnel) = match dialed {
            Ok(x) => x,
            Err(e) => {
                attempt.fail(&e);
                return Err(e);
            }
        };
        let entry = PgEntry {
            handle: handle.clone(),
            tunnel,
            connected_at: Utc::now(),
            signature,
            server_version: None,
        };
        let inserted = {
            let mut st = self.lock();
            if st.epoch(&profile.id) != epoch {
                Err(entry)
            } else {
                st.transient.remove(&profile.id);
                Ok(st.pg.insert(key, entry))
            }
        };
        match inserted {
            Err(discarded) => {
                // `disconnect()` ran while we were connecting: throw the new connection away.
                handle.close().await;
                drop(discarded.tunnel);
                attempt.abandon();
                Err(CoreError::Cancelled)
            }
            Ok(replaced) => {
                attempt.succeeded();
                if let Some(old) = replaced {
                    old.handle.close().await;
                }
                Ok(handle)
            }
        }
    }

    /// Services Iceberg connections need (AWS identity and the keychain for REST/S3 secrets).
    /// Called once at startup by the app shell; a second call is ignored (the first set wins).
    pub fn set_iceberg_deps(&self, deps: IcebergDeps) {
        if self.iceberg_deps.set(deps).is_err() {
            tracing::debug!("Iceberg dependencies were already set; ignoring the second call");
        }
    }

    fn iceberg_deps(&self) -> Result<IcebergDeps> {
        self.iceberg_deps.get().cloned().ok_or_else(|| {
            CoreError::Invalid(
                "Iceberg support is not initialised: the application never called set_iceberg_deps"
                    .into(),
            )
        })
    }

    /// Lazily connected Iceberg handle for the profile (catalog + query engine). Single-flight
    /// per profile; the cached handle is reused while the profile's connection-relevant settings
    /// (catalog, storage, engine, default namespace) are unchanged.
    pub async fn iceberg(&self, profile_id: &str) -> Result<Arc<IcebergHandle>> {
        let profile = self.profiles.get(profile_id)?;
        require_engine(&profile, EngineKind::Iceberg)?;
        if profile.iceberg.is_none() {
            return Err(CoreError::Invalid(format!(
                "connection \"{}\" has no Iceberg catalog options",
                profile.name
            )));
        }
        let deps = self.iceberg_deps()?;
        let signature = connection_signature(&profile);

        if let Some(handle) = self.cached_iceberg(&profile.id, &signature).await {
            return Ok(handle);
        }
        let lock = self.connect_lock((profile.id.clone(), None));
        let _single_flight = lock.lock().await;
        if let Some(handle) = self.cached_iceberg(&profile.id, &signature).await {
            return Ok(handle);
        }

        let epoch = self.lock().epoch(&profile.id);
        let mut attempt = Attempt::begin(self, &profile);
        let handle = match IcebergHandle::connect(profile.clone(), deps).await {
            Ok(h) => h,
            Err(e) => {
                attempt.fail(&e);
                return Err(e);
            }
        };
        let entry = IcebergEntry {
            handle: handle.clone(),
            connected_at: Utc::now(),
            signature,
            target: handle.describe_target(),
        };
        let inserted = {
            let mut st = self.lock();
            if st.epoch(&profile.id) != epoch {
                Err(())
            } else {
                st.transient.remove(&profile.id);
                Ok(st.iceberg.insert(profile.id.clone(), entry))
            }
        };
        match inserted {
            Err(()) => {
                // `disconnect()` ran while we were connecting: throw the new connection away.
                handle.close().await;
                attempt.abandon();
                Err(CoreError::Cancelled)
            }
            Ok(replaced) => {
                attempt.succeeded();
                if let Some(old) = replaced {
                    old.handle.close().await;
                }
                Ok(handle)
            }
        }
    }

    /// Where Redshift Data API requests go instead of the regional endpoint. For tests that wire
    /// the manager to a mock; the app never calls it and no profile field reaches it. A second
    /// call is ignored.
    #[doc(hidden)]
    pub fn set_redshift_endpoint_url(&self, url: impl Into<String>) {
        if self.redshift_endpoint.set(url.into()).is_err() {
            tracing::debug!("Redshift endpoint override was already set; ignoring the second call");
        }
    }

    /// A Redshift engine for `profile`: options validated, credentials resolved for its AWS
    /// target. Makes no request.
    async fn build_redshift(&self, profile: &ConnectionProfile) -> Result<RedshiftEngine> {
        let opts = profile.redshift.clone().ok_or_else(|| {
            CoreError::Invalid(format!(
                "connection \"{}\" has no Redshift options",
                profile.name
            ))
        })?;
        // The stored profile may have been edited by hand; the rules a save applies run again.
        crate::cleartext::check_profile(profile)?;
        let aws = self
            .iceberg_deps
            .get()
            .map(|d| d.aws.clone())
            .ok_or_else(|| {
                CoreError::Invalid(
                    "AWS support is not initialised: the application never called set_iceberg_deps"
                        .into(),
                )
            })?;
        let creds = aws.credentials_provider(&opts.aws).await?;
        let engine = RedshiftEngine::new(opts, creds)?;
        Ok(match self.redshift_endpoint.get() {
            Some(url) => engine.with_endpoint_url(url.clone()),
            None => engine,
        })
    }

    /// Lazily created Redshift engine for the profile. The first creation checks that the
    /// cluster or workgroup answers with the profile's identity (one `ListDatabases` call).
    /// Single-flight per profile; reused while the profile's Redshift options are unchanged.
    pub async fn redshift(&self, profile_id: &str) -> Result<Arc<RedshiftEngine>> {
        let profile = self.profiles.get(profile_id)?;
        require_engine(&profile, EngineKind::Redshift)?;
        let signature = connection_signature(&profile);

        if let Some(engine) = self.cached_redshift(&profile.id, &signature) {
            return Ok(engine);
        }
        let lock = self.connect_lock((profile.id.clone(), None));
        let _single_flight = lock.lock().await;
        if let Some(engine) = self.cached_redshift(&profile.id, &signature) {
            return Ok(engine);
        }

        let epoch = self.lock().epoch(&profile.id);
        let mut attempt = Attempt::begin(self, &profile);
        let built = match self.build_redshift(&profile).await {
            Ok(e) => e.ping().await.map(|()| e),
            Err(e) => Err(e),
        };
        let engine = match built {
            Ok(e) => Arc::new(e),
            Err(e) => {
                attempt.fail(&e);
                return Err(e);
            }
        };
        let entry = RedshiftEntry {
            engine: engine.clone(),
            connected_at: Utc::now(),
            signature,
            target: engine.describe_target(),
        };
        let inserted = {
            let mut st = self.lock();
            if st.epoch(&profile.id) != epoch {
                false
            } else {
                st.transient.remove(&profile.id);
                st.redshift.insert(profile.id.clone(), entry);
                true
            }
        };
        if inserted {
            attempt.succeeded();
            Ok(engine)
        } else {
            // `disconnect()` ran while we were connecting: throw the new engine away.
            attempt.abandon();
            Err(CoreError::Cancelled)
        }
    }

    fn cached_redshift(&self, profile_id: &str, signature: &str) -> Option<Arc<RedshiftEngine>> {
        let mut st = self.lock();
        match st.redshift.get(profile_id) {
            None => None,
            Some(e) if e.signature == signature => Some(e.engine.clone()),
            Some(_) => {
                tracing::info!(profile_id, "discarding stale Redshift engine");
                st.redshift.remove(profile_id);
                None
            }
        }
    }

    /// Valkey handle for the profile, connecting lazily.
    pub async fn valkey(&self, profile_id: &str) -> Result<Arc<ValkeyHandle>> {
        let profile = self.profiles.get(profile_id)?;
        require_engine(&profile, EngineKind::Valkey)?;
        let signature = connection_signature(&profile);

        if let Some(handle) = self.cached_valkey(&profile.id, &signature).await {
            return Ok(handle);
        }
        let lock = self.connect_lock((profile.id.clone(), None));
        let _single_flight = lock.lock().await;
        if let Some(handle) = self.cached_valkey(&profile.id, &signature).await {
            return Ok(handle);
        }

        let epoch = self.lock().epoch(&profile.id);
        let mut attempt = Attempt::begin(self, &profile);
        let dialed = self
            .dial_valkey(&profile, self.credential_fn(&profile, None))
            .await;
        let (handle, tunnel) = match dialed {
            Ok(x) => x,
            Err(e) => {
                attempt.fail(&e);
                return Err(e);
            }
        };
        let entry = ValkeyEntry {
            handle: handle.clone(),
            tunnel,
            connected_at: Utc::now(),
            signature,
            server_version: None,
        };
        let inserted = {
            let mut st = self.lock();
            if st.epoch(&profile.id) != epoch {
                Err(entry)
            } else {
                st.transient.remove(&profile.id);
                Ok(st.valkey.insert(profile.id.clone(), entry))
            }
        };
        match inserted {
            Err(discarded) => {
                // `disconnect()` ran while we were connecting: throw the new connection away.
                handle.close().await;
                drop(discarded.tunnel);
                attempt.abandon();
                Err(CoreError::Cancelled)
            }
            Ok(replaced) => {
                attempt.succeeded();
                if let Some(old) = replaced {
                    old.handle.close().await;
                }
                Ok(handle)
            }
        }
    }

    // ---- internals -------------------------------------------------------------------------

    fn connect_lock(&self, key: ConnectLockKey) -> Arc<tokio::sync::Mutex<()>> {
        self.lock().connect_locks.entry(key).or_default().clone()
    }

    /// Returns the cached pool if it is still valid for `signature`; stale entries are removed
    /// (and closed) and `None` is returned.
    async fn cached_pg(&self, key: &(ProfileId, String), signature: &str) -> Option<Arc<PgHandle>> {
        let stale = {
            let mut st = self.lock();
            match st.pg.get(key) {
                None => return None,
                Some(e) if e.signature == signature && !tunnel_dead(&e.tunnel) => {
                    return Some(e.handle.clone());
                }
                Some(_) => st.pg.remove(key),
            }
        };
        if let Some(entry) = stale {
            tracing::info!(profile_id = %key.0, database = %key.1, "discarding stale Postgres connection");
            entry.handle.close().await;
            drop(entry.tunnel);
        }
        None
    }

    async fn cached_valkey(&self, profile_id: &str, signature: &str) -> Option<Arc<ValkeyHandle>> {
        let stale = {
            let mut st = self.lock();
            match st.valkey.get(profile_id) {
                None => return None,
                Some(e) if e.signature == signature && !tunnel_dead(&e.tunnel) => {
                    return Some(e.handle.clone());
                }
                Some(_) => st.valkey.remove(profile_id),
            }
        };
        if let Some(entry) = stale {
            tracing::info!(profile_id, "discarding stale Valkey connection");
            entry.handle.close().await;
            drop(entry.tunnel);
        }
        None
    }

    async fn cached_iceberg(
        &self,
        profile_id: &str,
        signature: &str,
    ) -> Option<Arc<IcebergHandle>> {
        let stale = {
            let mut st = self.lock();
            match st.iceberg.get(profile_id) {
                None => return None,
                Some(e) if e.signature == signature => return Some(e.handle.clone()),
                Some(_) => st.iceberg.remove(profile_id),
            }
        };
        if let Some(entry) = stale {
            tracing::info!(profile_id, "discarding stale Iceberg connection");
            entry.handle.close().await;
        }
        None
    }

    /// Drops the transient record of a profile with nothing live (after the reaper closed it).
    fn forget_if_idle(&self, profile_id: &str) {
        let mut st = self.lock();
        if !st.has_live(profile_id) {
            st.transient.remove(profile_id);
        }
    }

    /// Credentials closure handed to the drivers; called once per *physical* connection so IAM
    /// tokens are minted fresh each time. With an override (wizard test) nothing is resolved.
    fn credential_fn(
        &self,
        profile: &ConnectionProfile,
        password_override: Option<SecretString>,
    ) -> CredentialFn {
        let profile = profile.clone();
        if let Some(password) = password_override {
            let username = match &profile.auth {
                AuthMethod::Password { username } if !username.is_empty() => {
                    Some(Some(username.clone()))
                }
                AuthMethod::Password { .. } | AuthMethod::None => Some(None),
                // A pasted (not yet saved) access token: the override *is* the token.
                AuthMethod::AccessToken { username, .. } if profile.auth.uses_stored_password() => {
                    let u = username.trim();
                    Some(Some(if u.is_empty() {
                        "vibelens".to_string()
                    } else {
                        u.to_string()
                    }))
                }
                _ => None, // IAM / Secrets Manager / token command: resolve normally
            };
            if let Some(username) = username {
                return Arc::new(move || {
                    let creds = ResolvedCredentials {
                        username: username.clone(),
                        password: Some(password.clone()),
                        ..ResolvedCredentials::anonymous()
                    };
                    Box::pin(async move { Ok(creds) })
                });
            }
        }
        let source = self.creds.clone();
        Arc::new(move || {
            let source = source.clone();
            let profile = profile.clone();
            Box::pin(async move { source.resolve(&profile).await })
        })
    }

    async fn open_tunnel(&self, profile: &ConnectionProfile) -> Result<Option<TunnelHandle>> {
        match &profile.tunnel {
            Some(cfg) => Ok(Some(
                self.tunnels
                    .open(&profile.id, cfg, &profile.host, profile.port)
                    .await?,
            )),
            None => Ok(None),
        }
    }

    async fn dial_pg(
        &self,
        profile: &ConnectionProfile,
        database: &str,
        creds: CredentialFn,
    ) -> Result<(Arc<PgHandle>, Option<TunnelHandle>)> {
        // Before the tunnel and before any credential is fetched. The profile may have been
        // edited by hand, so the rules a save applies run again here.
        crate::cleartext::check_profile(profile)?;
        let tunnel = self.open_tunnel(profile).await?;
        let (host, port) = dial_target(profile, &tunnel);
        let params = PgConnectParams {
            host,
            port,
            tls_server_name: profile.host.clone(),
            database: database.to_string(),
            tls: profile.tls.clone(),
            options: profile.pg.clone(),
        };
        // On error `tunnel` is dropped here, which tears it down.
        let handle = PgHandle::connect(params, creds).await?;
        Ok((handle, tunnel))
    }

    async fn dial_valkey(
        &self,
        profile: &ConnectionProfile,
        creds: CredentialFn,
    ) -> Result<(Arc<ValkeyHandle>, Option<TunnelHandle>)> {
        crate::cleartext::check_profile(profile)?;
        if profile.tunnel.is_some() && profile.valkey.cluster {
            tracing::warn!(profile = %profile.name, "Valkey cluster mode through an SSH tunnel will not follow MOVED redirects (see docs/DESIGN.md §8)");
        }
        let tunnel = self.open_tunnel(profile).await?;
        let (host, port) = dial_target(profile, &tunnel);
        let params = ValkeyConnectParams {
            host,
            port,
            tls_server_name: profile.host.clone(),
            tls: profile.tls.clone(),
            options: profile.valkey.clone(),
        };
        let handle = ValkeyHandle::connect(params, creds).await?;
        Ok((handle, tunnel))
    }
}

/// Where the driver actually dials: the tunnel's local end when tunnelled, else the profile.
fn dial_target(profile: &ConnectionProfile, tunnel: &Option<TunnelHandle>) -> (String, u16) {
    match tunnel {
        Some(t) => (t.local_addr.ip().to_string(), t.local_addr.port()),
        None => (profile.host.clone(), profile.port),
    }
}

/// Iceberg and Redshift connections never use the SSH tunnel, whatever a stale profile field says.
fn is_tunneled(profile: &ConnectionProfile) -> bool {
    !matches!(profile.engine, EngineKind::Iceberg | EngineKind::Redshift)
        && profile.tunnel.is_some()
}

/// A vault that answers one key from memory and defers everything else (and all writes) to the
/// real one. Lets the wizard test a not-yet-saved secret without touching the keychain.
struct OverlayVault {
    inner: Arc<dyn SecretVault>,
    key: String,
    value: SecretString,
}

#[async_trait]
impl SecretVault for OverlayVault {
    async fn get(&self, key: &str) -> Result<Option<SecretString>> {
        if key == self.key {
            return Ok(Some(self.value.clone()));
        }
        self.inner.get(key).await
    }
    async fn set(&self, key: &str, value: &SecretString) -> Result<()> {
        self.inner.set(key, value).await
    }
    async fn delete(&self, key: &str) -> Result<()> {
        self.inner.delete(key).await
    }
}

fn tunnel_dead(tunnel: &Option<TunnelHandle>) -> bool {
    tunnel.as_ref().is_some_and(|t| t.cancel.is_cancelled())
}

/// The agent's `schema` argument as namespace levels (`"a.b"` is the nested namespace `a`/`b`).
/// Empty segments are dropped, and an empty filter means "the whole catalog".
fn namespace_levels(filter: Option<&str>) -> Option<Vec<String>> {
    let levels: Vec<String> = filter?
        .split('.')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    (!levels.is_empty()).then_some(levels)
}

fn idle_exceeded(idle_secs: u64, limit_secs: u64) -> bool {
    limit_secs > 0 && idle_secs > limit_secs
}

fn require_engine(profile: &ConnectionProfile, wanted: EngineKind) -> Result<()> {
    if profile.engine == wanted {
        return Ok(());
    }
    let is = match profile.engine {
        EngineKind::Postgres => "a Postgres",
        EngineKind::Valkey => "a Valkey",
        EngineKind::Iceberg => "an Iceberg",
        EngineKind::Redshift => "a Redshift",
    };
    let wants = match wanted {
        EngineKind::Postgres => "Postgres",
        EngineKind::Valkey => "Valkey",
        EngineKind::Iceberg => "Iceberg",
        EngineKind::Redshift => "Redshift",
    };
    Err(CoreError::Invalid(format!(
        "connection \"{}\" is {is} connection, not {wants}",
        profile.name
    )))
}

/// Serialised form of everything that affects *how we connect*. Cached handles are reused only
/// while this is unchanged. Cosmetic fields, the idle timeouts (the reaper reads the live
/// profile) and the pinned SSH host key are excluded: a live tunnel was checked against the pin
/// it was opened with, and the next one is checked against whatever is pinned then.
fn connection_signature(profile: &ConnectionProfile) -> String {
    let mut p = profile.clone();
    p.name.clear();
    p.color = None;
    p.created_at = None;
    p.updated_at = None;
    p.environment = Environment::default();
    p.ai_data_access = AiDataAccess::default();
    p.pg.idle_disconnect_secs = 0;
    p.valkey.idle_disconnect_secs = 0;
    // Everything under `iceberg` (catalog, storage, query engine, default namespace) changes how
    // we connect, so it stays in the signature. Secrets live in the vault and are not part of it.
    if let Some(TunnelConfig::Ssh {
        known_host_fingerprint,
        ..
    }) = p.tunnel.as_mut()
    {
        *known_host_fingerprint = None;
    }
    serde_json::to_string(&p).unwrap_or_default()
}

/// Builds the externally visible status for one profile from the registry.
fn status_of(st: &State, profile_id: &str) -> Option<ConnectionStatus> {
    let pg: Vec<&PgEntry> = st
        .pg
        .iter()
        .filter(|((id, _), _)| id == profile_id)
        .map(|(_, e)| e)
        .collect();
    let valkey = st.valkey.get(profile_id);
    let iceberg = st.iceberg.get(profile_id);
    let redshift = st.redshift.get(profile_id);
    if !pg.is_empty() || valkey.is_some() || iceberg.is_some() || redshift.is_some() {
        let engine = if valkey.is_some() {
            EngineKind::Valkey
        } else if iceberg.is_some() {
            EngineKind::Iceberg
        } else if redshift.is_some() {
            EngineKind::Redshift
        } else {
            EngineKind::Postgres
        };
        let since = pg
            .iter()
            .map(|e| e.connected_at)
            .chain(valkey.map(|e| e.connected_at))
            .chain(iceberg.map(|e| e.connected_at))
            .chain(redshift.map(|e| e.connected_at))
            .min();
        let server_version = pg
            .iter()
            .filter_map(|e| e.server_version.clone())
            .chain(valkey.and_then(|e| e.server_version.clone()))
            .chain(iceberg.map(|e| e.target.clone()).filter(|t| !t.is_empty()))
            .chain(redshift.map(|e| e.target.clone()))
            .next();
        let tunneled =
            pg.iter().any(|e| e.tunnel.is_some()) || valkey.is_some_and(|e| e.tunnel.is_some());
        // Postgres handles whose sessions the reaper released are still connected, just asleep.
        let detail = (engine == EngineKind::Postgres
            && pg.iter().all(|e| e.handle.open_sessions() == 0))
        .then(|| "idle: no open server sessions; the next query reconnects".to_string());
        return Some(ConnectionStatus {
            profile_id: profile_id.to_string(),
            engine,
            state: ConnectionState::Connected,
            since,
            server_version,
            detail,
            tunneled,
        });
    }
    let status = match st.transient.get(profile_id)? {
        Transient::Connecting {
            engine,
            since,
            tunneled,
        } => ConnectionStatus {
            profile_id: profile_id.to_string(),
            engine: *engine,
            state: ConnectionState::Connecting,
            since: Some(*since),
            server_version: None,
            detail: None,
            tunneled: *tunneled,
        },
        Transient::Failed {
            engine,
            since,
            tunneled,
            detail,
        } => ConnectionStatus {
            profile_id: profile_id.to_string(),
            engine: *engine,
            state: ConnectionState::Error,
            since: Some(*since),
            server_version: None,
            detail: Some(detail.clone()),
            tunneled: *tunneled,
        },
    };
    Some(status)
}

/// Tracks one connect attempt so the `Connecting` marker can never outlive it, even when the
/// caller's future is dropped mid-connect.
struct Attempt<'a> {
    manager: &'a ConnectionManager,
    profile_id: ProfileId,
    engine: EngineKind,
    tunneled: bool,
    resolved: bool,
}

impl<'a> Attempt<'a> {
    fn begin(manager: &'a ConnectionManager, profile: &ConnectionProfile) -> Self {
        {
            let mut st = manager.lock();
            // A profile that is already connected (another database, say) keeps showing
            // "Connected"; only the first connection shows "Connecting".
            if !st.has_live(&profile.id) {
                st.transient.insert(
                    profile.id.clone(),
                    Transient::Connecting {
                        engine: profile.engine,
                        since: Utc::now(),
                        tunneled: is_tunneled(profile),
                    },
                );
            }
        }
        Self {
            manager,
            profile_id: profile.id.clone(),
            engine: profile.engine,
            tunneled: is_tunneled(profile),
            resolved: false,
        }
    }

    fn succeeded(&mut self) {
        self.resolved = true;
    }

    /// The attempt was discarded on purpose (disconnect raced it): leave the status alone.
    fn abandon(&mut self) {
        self.resolved = true;
    }

    fn fail(&mut self, err: &CoreError) {
        self.resolved = true;
        let mut st = self.manager.lock();
        if st.has_live(&self.profile_id) {
            // A failed extra connection must not flip a healthy profile to Error.
            if matches!(
                st.transient.get(&self.profile_id),
                Some(Transient::Connecting { .. })
            ) {
                st.transient.remove(&self.profile_id);
            }
        } else {
            st.transient.insert(
                self.profile_id.clone(),
                Transient::Failed {
                    engine: self.engine,
                    since: Utc::now(),
                    tunneled: self.tunneled,
                    detail: err.to_string(),
                },
            );
        }
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if self.resolved {
            return;
        }
        // Dropped mid-flight (cancelled caller): don't leave "Connecting" behind.
        let mut st = self.manager.lock();
        if matches!(
            st.transient.get(&self.profile_id),
            Some(Transient::Connecting { .. })
        ) {
            st.transient.remove(&self.profile_id);
        }
    }
}

/// [`ToolBackend`] over the connection manager and the workspace jail. Policy (approval,
/// `AiDataAccess`) is enforced by the agent layer before these are called; they still honour
/// `read_only`, the row cap and the statement timeout.
pub struct AppToolBackend {
    pub connections: Arc<ConnectionManager>,
    pub workspace: Arc<WorkspaceFs>,
}

impl AppToolBackend {
    pub fn new(connections: Arc<ConnectionManager>, workspace: Arc<WorkspaceFs>) -> Arc<Self> {
        Arc::new(Self {
            connections,
            workspace,
        })
    }

    /// `execute_sql` for one engine, stopped by `cancel`.
    async fn execute_sql_inner(
        &self,
        profile_id: &str,
        database: Option<&str>,
        sql: &str,
        read_only: bool,
        max_rows: usize,
        cancel: &CancellationToken,
    ) -> Result<QueryResult> {
        let profile = self.connections.profiles().get(profile_id)?;
        let mut opts = ExecOptions {
            read_only,
            max_rows,
            page_size: max_rows.max(1),
            timeout_ms: Some(AGENT_SQL_TIMEOUT_MS),
        };
        if profile.engine == EngineKind::Iceberg {
            // `database` has no meaning for Iceberg; tables are addressed as `namespace.table`.
            if matches!(
                profile.iceberg.as_ref().map(|o| &o.query_engine),
                Some(
                    IcebergQueryEngine::Athena { .. }
                        | IcebergQueryEngine::Trino { .. }
                        | IcebergQueryEngine::Spark { .. }
                )
            ) {
                opts.timeout_ms = Some(AGENT_ATHENA_TIMEOUT_MS);
            }
            let handle = self.connections.iceberg(profile_id).await?;
            return handle
                .execute_collect_cancellable(sql, opts, cancel.clone())
                .await;
        }
        if profile.engine == EngineKind::Redshift {
            opts.timeout_ms = Some(AGENT_REDSHIFT_TIMEOUT_MS);
            let engine = self.connections.redshift(profile_id).await?;
            return engine
                .execute_collect(sql, database, opts, cancel.clone())
                .await;
        }
        // Everything else goes through the Postgres path, which rejects other engines with
        // a clear `Invalid`.
        let pg = self.connections.pg(profile_id, database).await?;
        pg.execute_collect_cancellable(sql, opts, cancel.clone())
            .await
    }
}

#[async_trait]
impl ToolBackend for AppToolBackend {
    fn profile(&self, profile_id: &str) -> Result<ConnectionProfile> {
        self.connections.profiles().get(profile_id)
    }

    fn list_profiles(&self) -> Result<Vec<ConnectionProfile>> {
        self.connections.profiles().list()
    }

    async fn execute_sql(
        &self,
        profile_id: &str,
        database: Option<&str>,
        sql: &str,
        read_only: bool,
        max_rows: usize,
        cancel: CancellationToken,
    ) -> Result<QueryResult> {
        // One deadline for the whole script, below the agent's tool timeout: when it passes,
        // the engine is cancelled and stops the statement on the server before returning.
        let script = cancel.child_token();
        let timer = {
            let script = script.clone();
            tokio::spawn(async move {
                tokio::time::sleep(AGENT_SCRIPT_DEADLINE).await;
                script.cancel();
            })
        };
        let res = self
            .execute_sql_inner(profile_id, database, sql, read_only, max_rows, &script)
            .await;
        timer.abort();
        match res {
            Err(CoreError::Cancelled) if !cancel.is_cancelled() && script.is_cancelled() => {
                Err(CoreError::Timeout(format!(
                    "the script did not finish within {} seconds; it was cancelled",
                    AGENT_SCRIPT_DEADLINE.as_secs()
                )))
            }
            other => other,
        }
    }

    async fn schema_metadata(
        &self,
        profile_id: &str,
        database: Option<&str>,
        schema_filter: Option<&str>,
        max_tables: usize,
    ) -> Result<SchemaMetadata> {
        let profile = self.connections.profiles().get(profile_id)?;
        if profile.engine == EngineKind::Iceberg {
            let handle = self.connections.iceberg(profile_id).await?;
            return handle
                .schema_metadata(namespace_levels(schema_filter), max_tables)
                .await;
        }
        if profile.engine == EngineKind::Redshift {
            let engine = self.connections.redshift(profile_id).await?;
            return engine
                .schema_metadata(database, schema_filter, max_tables)
                .await;
        }
        let pg = self.connections.pg(profile_id, database).await?;
        pg.schema_metadata(schema_filter, max_tables).await
    }

    async fn valkey_scan(
        &self,
        profile_id: &str,
        db: u8,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<KeyEntry>> {
        let profile = self.connections.profiles().get(profile_id)?;
        let handle = self.connections.valkey(profile_id).await?;
        let count = profile.valkey.scan_count.max(1);
        let mut keys: Vec<KeyEntry> = Vec::new();
        if limit == 0 {
            return Ok(keys);
        }
        let mut cursor = "0".to_string();
        for _ in 0..MAX_SCAN_PAGES {
            let page = handle.scan(db, pattern, &cursor, count).await?;
            keys.extend(page.keys);
            if page.done || keys.len() >= limit {
                break;
            }
            cursor = page.cursor;
        }
        keys.truncate(limit);
        Ok(keys)
    }

    async fn valkey_get(&self, profile_id: &str, db: u8, key: &str) -> Result<ValkeyValue> {
        let handle = self.connections.valkey(profile_id).await?;
        handle.get_value(db, key, AGENT_VALKEY_MAX_ITEMS).await
    }

    async fn valkey_is_write(&self, profile_id: &str, argv: &[String]) -> Result<bool> {
        let handle = self.connections.valkey(profile_id).await?;
        handle.is_write_command(argv).await
    }

    async fn valkey_command(
        &self,
        profile_id: &str,
        db: u8,
        argv: Vec<String>,
        allow_write: bool,
    ) -> Result<CommandOutcome> {
        let handle = self.connections.valkey(profile_id).await?;
        handle.command(db, argv, allow_write).await
    }

    async fn describe_iceberg_table(
        &self,
        profile_id: &str,
        namespace: Vec<String>,
        name: &str,
    ) -> Result<IcebergTableInfo> {
        let handle = self.connections.iceberg(profile_id).await?;
        handle.describe_table(namespace, name).await
    }

    async fn list_files(&self) -> Result<Vec<WorkspaceFile>> {
        self.workspace.list().await
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        self.workspace.read(path).await
    }

    async fn write_file(&self, path: &str, contents: &str, expected: &FileVersion) -> Result<()> {
        self.workspace
            .write_if_unchanged(path, contents, expected)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unused_vault_keys_follow_the_saved_configuration() {
        use crate::model::{IcebergCatalog, IcebergStorage, RestAuth, SshAuth, TunnelConfig};
        let keys = |p: &ConnectionProfile| p.unused_vault_keys();
        let ssh = |auth| TunnelConfig::Ssh {
            host: "h".into(),
            port: 22,
            username: "u".into(),
            auth,
            known_host_fingerprint: None,
        };

        let mut p = profile(EngineKind::Postgres);
        p.auth = AuthMethod::Password {
            username: "u".into(),
        };
        p.tunnel = Some(ssh(SshAuth::Password));
        let unused = keys(&p);
        assert!(!unused.contains(&vault_keys::password(&p.id)));
        assert!(!unused.contains(&vault_keys::ssh_password(&p.id)));
        assert!(unused.contains(&vault_keys::ssh_passphrase(&p.id)));
        assert!(unused.contains(&vault_keys::iceberg_secret(&p.id)));
        assert!(unused.contains(&vault_keys::s3_access_key_id(&p.id)));

        // Moving off password auth and to a key-file tunnel orphans both old secrets.
        p.auth = AuthMethod::None;
        p.tunnel = Some(ssh(SshAuth::PrivateKey { path: "k".into() }));
        let unused = keys(&p);
        assert!(unused.contains(&vault_keys::password(&p.id)));
        assert!(unused.contains(&vault_keys::ssh_password(&p.id)));
        assert!(!unused.contains(&vault_keys::ssh_passphrase(&p.id)));

        let mut p = iceberg_profile();
        let opts = p.iceberg.as_mut().unwrap();
        opts.catalog = IcebergCatalog::Rest {
            uri: "http://c".into(),
            warehouse: None,
            auth: RestAuth::Bearer,
            headers: Default::default(),
        };
        opts.storage = IcebergStorage::S3Static {
            endpoint: None,
            region: None,
            path_style: true,
        };
        let unused = keys(&p);
        assert!(!unused.contains(&vault_keys::iceberg_secret(&p.id)));
        assert!(!unused.contains(&vault_keys::s3_access_key_id(&p.id)));
        assert!(!unused.contains(&vault_keys::s3_secret_access_key(&p.id)));

        let opts = p.iceberg.as_mut().unwrap();
        opts.catalog = IcebergCatalog::Rest {
            uri: "http://c".into(),
            warehouse: None,
            auth: RestAuth::None,
            headers: Default::default(),
        };
        opts.storage = IcebergStorage::Auto;
        let unused = keys(&p);
        assert!(unused.contains(&vault_keys::iceberg_secret(&p.id)));
        assert!(unused.contains(&vault_keys::s3_access_key_id(&p.id)));
        assert!(unused.contains(&vault_keys::s3_secret_access_key(&p.id)));
    }

    fn profile(engine: EngineKind) -> ConnectionProfile {
        ConnectionProfile {
            id: "p1".into(),
            name: "Prod".into(),
            engine,
            host: "db.example".into(),
            port: 5432,
            database: None,
            auth: AuthMethod::Password {
                username: "u".into(),
            },
            tls: TlsConfig::default(),
            tunnel: None,
            environment: Environment::Dev,
            ai_data_access: AiDataAccess::default(),
            color: None,
            pg: PgOptions::default(),
            valkey: ValkeyOptions::default(),
            created_at: None,
            updated_at: None,
            iceberg: None,
            redshift: None,
        }
    }

    #[test]
    fn default_database_precedence() {
        let mut p = profile(EngineKind::Postgres);
        assert_eq!(p.effective_database(None), "postgres");
        p.database = Some("app".into());
        assert_eq!(p.effective_database(None), "app");
        assert_eq!(p.effective_database(Some("other")), "other");
        assert_eq!(p.effective_database(Some(" other ")), "other");
        assert_eq!(p.effective_database(Some("  ")), "app");
        p.database = Some(String::new());
        assert_eq!(p.effective_database(None), "postgres");
    }

    #[test]
    fn idle_limit_semantics() {
        assert!(!idle_exceeded(10_000, 0), "0 means never");
        assert!(!idle_exceeded(300, 300), "strictly greater");
        assert!(idle_exceeded(301, 300));
    }

    #[test]
    fn signature_ignores_cosmetics_and_pinned_host_key_but_not_connection_fields() {
        let mut a = profile(EngineKind::Postgres);
        a.tunnel = Some(TunnelConfig::Ssh {
            host: "bastion".into(),
            port: 22,
            username: "me".into(),
            auth: SshAuth::Agent,
            known_host_fingerprint: None,
        });
        let base = connection_signature(&a);

        let mut b = a.clone();
        b.name = "Renamed".into();
        b.color = Some("#fff".into());
        b.updated_at = Some(Utc::now());
        b.environment = Environment::Prod;
        b.ai_data_access = AiDataAccess::None;
        if let Some(TunnelConfig::Ssh {
            known_host_fingerprint,
            ..
        }) = b.tunnel.as_mut()
        {
            *known_host_fingerprint = Some("SHA256:abc".into());
        }
        assert_eq!(connection_signature(&b), base);

        let mut c = a.clone();
        c.host = "other.example".into();
        assert_ne!(connection_signature(&c), base);
        let mut d = a.clone();
        d.tls.mode = TlsMode::Disable;
        assert_ne!(connection_signature(&d), base);
        let mut e = a.clone();
        e.auth = AuthMethod::None;
        assert_ne!(connection_signature(&e), base);
    }

    fn iceberg_profile() -> ConnectionProfile {
        ConnectionProfile {
            engine: EngineKind::Iceberg,
            host: String::new(),
            port: 0,
            iceberg: Some(IcebergOptions {
                catalog: IcebergCatalog::Rest {
                    uri: "https://catalog.example/api".into(),
                    warehouse: Some("wh".into()),
                    auth: RestAuth::Bearer,
                    headers: Default::default(),
                },
                storage: IcebergStorage::Auto,
                query_engine: IcebergQueryEngine::Local,
                default_namespace: Some("shop".into()),
            }),
            ..profile(EngineKind::Iceberg)
        }
    }

    #[test]
    fn iceberg_signature_tracks_catalog_storage_engine_and_namespace_only() {
        let a = iceberg_profile();
        let base = connection_signature(&a);

        // Cosmetic fields and idle timeouts do not invalidate a live connection.
        let mut cosmetic = a.clone();
        cosmetic.name = "Lake".into();
        cosmetic.color = Some("#abc".into());
        cosmetic.environment = Environment::Prod;
        cosmetic.ai_data_access = AiDataAccess::None;
        cosmetic.updated_at = Some(Utc::now());
        cosmetic.pg.idle_disconnect_secs = 5;
        cosmetic.valkey.idle_disconnect_secs = 5;
        assert_eq!(connection_signature(&cosmetic), base);

        let edit = |f: &dyn Fn(&mut IcebergOptions)| {
            let mut p = a.clone();
            f(p.iceberg.as_mut().unwrap());
            connection_signature(&p)
        };
        assert_ne!(
            edit(&|o| {
                if let IcebergCatalog::Rest { uri, .. } = &mut o.catalog {
                    *uri = "https://other.example/api".into();
                }
            }),
            base,
            "catalog uri"
        );
        assert_ne!(
            edit(&|o| {
                if let IcebergCatalog::Rest { auth, .. } = &mut o.catalog {
                    *auth = RestAuth::None;
                }
            }),
            base,
            "catalog auth"
        );
        assert_ne!(
            edit(&|o| o.storage = IcebergStorage::Local),
            base,
            "storage"
        );
        assert_ne!(
            edit(&|o| o.storage = IcebergStorage::S3 {
                aws: None,
                endpoint: Some("http://minio:9000".into()),
                region: None,
                path_style: true,
            }),
            base,
            "storage endpoint"
        );
        assert_ne!(
            edit(&|o| o.query_engine = IcebergQueryEngine::Athena {
                workgroup: "primary".into(),
                output_location: None,
                aws: None,
            }),
            base,
            "query engine"
        );
        assert_ne!(
            edit(&|o| o.default_namespace = None),
            base,
            "default namespace"
        );
        assert_ne!(
            edit(&|o| o.default_namespace = Some("other".into())),
            base,
            "default namespace value"
        );
    }

    #[test]
    fn iceberg_connections_are_never_reported_as_tunnelled() {
        let mut p = iceberg_profile();
        assert!(!is_tunneled(&p));
        p.tunnel = Some(TunnelConfig::Ssh {
            host: "bastion".into(),
            port: 22,
            username: "me".into(),
            auth: SshAuth::Agent,
            known_host_fingerprint: None,
        });
        assert!(!is_tunneled(&p), "a stale tunnel setting is ignored");
        p.engine = EngineKind::Postgres;
        assert!(is_tunneled(&p));
    }

    #[test]
    fn schema_argument_becomes_namespace_levels() {
        assert_eq!(namespace_levels(None), None);
        assert_eq!(namespace_levels(Some("")), None);
        assert_eq!(namespace_levels(Some("  ")), None);
        assert_eq!(namespace_levels(Some(".")), None);
        assert_eq!(
            namespace_levels(Some("shop")),
            Some(vec!["shop".to_string()])
        );
        assert_eq!(
            namespace_levels(Some(" analytics . daily ")),
            Some(vec!["analytics".to_string(), "daily".to_string()])
        );
        assert_eq!(
            namespace_levels(Some("a..b.")),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn iceberg_requirement_messages() {
        let ice = iceberg_profile();
        assert!(require_engine(&ice, EngineKind::Iceberg).is_ok());
        let err = require_engine(&ice, EngineKind::Postgres).unwrap_err();
        assert!(err.to_string().contains("an Iceberg") && err.to_string().contains("not Postgres"));
        let err = require_engine(&profile(EngineKind::Postgres), EngineKind::Iceberg).unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert!(err.to_string().contains("not Iceberg"));
    }

    #[tokio::test]
    async fn overlay_vault_serves_one_key_and_defers_the_rest() {
        let inner = Arc::new(crate::vault::MemoryVault::new());
        inner
            .set("other", &SecretString::from("real".to_string()))
            .await
            .unwrap();
        inner
            .set(
                &vault_keys::iceberg_secret("p1"),
                &SecretString::from("saved".to_string()),
            )
            .await
            .unwrap();
        let overlay = OverlayVault {
            inner: inner.clone(),
            key: vault_keys::iceberg_secret("p1"),
            value: SecretString::from("typed-in-wizard".to_string()),
        };
        use secrecy::ExposeSecret;
        let got = overlay
            .get(&vault_keys::iceberg_secret("p1"))
            .await
            .unwrap();
        assert_eq!(got.unwrap().expose_secret(), "typed-in-wizard");
        let got = overlay.get("other").await.unwrap();
        assert_eq!(got.unwrap().expose_secret(), "real");
        assert!(overlay.get("missing").await.unwrap().is_none());
        // Writes pass straight through; the overlay never stores anything itself.
        overlay
            .set("new", &SecretString::from("v".to_string()))
            .await
            .unwrap();
        assert!(inner.get("new").await.unwrap().is_some());
        overlay.delete("new").await.unwrap();
        assert!(inner.get("new").await.unwrap().is_none());
        // The saved secret in the real vault is untouched by the overlay.
        let saved = inner.get(&vault_keys::iceberg_secret("p1")).await.unwrap();
        assert_eq!(saved.unwrap().expose_secret(), "saved");
    }

    #[test]
    fn engine_requirement_message() {
        let p = profile(EngineKind::Valkey);
        let err = require_engine(&p, EngineKind::Postgres).unwrap_err();
        assert_eq!(err.code(), "invalid");
        assert!(err.to_string().contains("Prod"));
        assert!(require_engine(&p, EngineKind::Valkey).is_ok());
    }
}
