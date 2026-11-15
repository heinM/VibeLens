//! Postgres engine.
//!
//! - Pool: `deadpool` with a custom `Manager` whose `create()` calls the [`CredentialFn`]
//!   for every new physical connection (IAM tokens are minted per connection; RDS only checks
//!   the token at connect time, so open connections are unaffected by expiry).
//! - TLS via [`crate::tls::client_config`] + `tokio-postgres-rustls`; SNI/hostname
//!   verification uses `tls_server_name` (the *real* host) even when dialing a tunnel.
//! - Streaming: `query_raw` row stream, batched into `QueryEvent::Rows` pages.
//! - Cancellation: `CancellationToken` → `client.cancel_token().cancel_query(..)`; a cancelled
//!   connection is discarded rather than returned to the pool. So is one whose run future is
//!   dropped (agent timeout, aborted task): a drop guard removes it from the pool and fires a
//!   best-effort cancel.
//! - Session hygiene: after every run the connection's session is reset in the background
//!   (`ROLLBACK`, `RESET ALL`, advisory locks, temp tables, user prepared statements, ...), and
//!   the pool's `recycle` repeats it for any connection that came back without it.
//! - Read-only runs use `BEGIN READ ONLY` (server-enforced), refuse transaction-control
//!   statements, and verify after every statement that the session is still in that very
//!   transaction (else `PolicyDenied`, rollback, connection discarded).
//!
//! Layout: [`connect`] (config, TLS, pool manager), [`catalog`] (introspection), [`exec`]
//! (execution), [`codec`] (binary value → JSON), [`error`] (error mapping).

mod catalog;
mod codec;
mod connect;
mod error;
mod exec;

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use deadpool::Runtime;
use deadpool::managed::Timeouts;

use crate::error::{CoreError, Result};
use crate::model::*;
use crate::ports::CredentialFn;

use connect::{Object, PgManager, Pool, Shared};
use error::{map_pg_error, map_pool_error};

pub use codec::numeric_to_string;

/// Checks a capitec/pg-proxy health listener: `GET <health_url>/ready` must answer 200.
/// 503 means the proxy runs but its database backend is unhealthy.
pub async fn pgproxy_ready(health_url: &str) -> Result<()> {
    let base = health_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Ok(());
    }
    let url = if base.ends_with("/ready") {
        base.to_string()
    } else {
        format!("{base}/ready")
    };
    let parsed = url::Url::parse(&url)
        .map_err(|e| CoreError::Invalid(format!("PGProxy health URL is not valid: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(CoreError::Invalid(
            "PGProxy health URL must start with http:// or https://".into(),
        ));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| CoreError::Other(format!("cannot build HTTP client: {e}")))?;
    let resp = client.get(parsed).send().await.map_err(|e| {
        CoreError::Database(format!(
            "cannot reach the PGProxy health endpoint {url}: {e}"
        ))
    })?;
    match resp.status().as_u16() {
        200..=299 => Ok(()),
        503 => Err(CoreError::Database(format!(
            "PGProxy is running but reports its database backend as unhealthy ({url} answered 503)"
        ))),
        code => Err(CoreError::Database(format!(
            "PGProxy health endpoint {url} answered HTTP {code}"
        ))),
    }
}

#[derive(Debug, Clone)]
pub struct PgConnectParams {
    /// Address actually dialed (`127.0.0.1` when tunneled).
    pub host: String,
    pub port: u16,
    /// Hostname used for TLS SNI + verification (the real server hostname).
    pub tls_server_name: String,
    pub database: String,
    pub tls: TlsConfig,
    pub options: PgOptions,
}

/// A pooled Postgres endpoint. Cheap to share (`Arc`), safe to use concurrently.
pub struct PgHandle {
    shared: Arc<Shared>,
    pool: Pool,
    created: Instant,
    last_used_ms: AtomicU64,
    active: AtomicUsize,
}

/// Marks the handle busy while an operation is in flight so the idle reaper never closes a
/// pool under a running query; refreshes the last-used time when dropped.
struct ActivityGuard<'a>(&'a PgHandle);

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}

impl PgHandle {
    /// Opens the pool and validates one connection (`SELECT version()`).
    pub async fn connect(params: PgConnectParams, creds: CredentialFn) -> Result<Arc<PgHandle>> {
        let tls = crate::tls::client_config(&params.tls)?.map(|c| (*c).clone());
        let connect_timeout = Duration::from_secs(params.options.connect_timeout_secs.max(1));
        let max_size = params.options.max_pool_size.max(1);
        let shared = Arc::new(Shared {
            params,
            creds,
            tls,
            database: OnceLock::new(),
            server_version: OnceLock::new(),
            full_reset: AtomicBool::new(true),
        });
        let pool = Pool::builder(PgManager::new(Arc::clone(&shared)))
            .max_size(max_size)
            .timeouts(Timeouts {
                // Waiting for a free connection while others run long queries.
                wait: Some(Duration::from_secs(60)),
                // TCP connect + TLS + auth + credential minting, with slack for scale-to-zero resume.
                create: Some(connect_timeout + Duration::from_secs(30)),
                recycle: Some(Duration::from_secs(10)),
            })
            .runtime(Runtime::Tokio1)
            .build()
            .map_err(|e| CoreError::Other(format!("cannot build connection pool: {e}")))?;
        let handle = Arc::new(PgHandle {
            shared,
            pool,
            created: Instant::now(),
            last_used_ms: AtomicU64::new(0),
            active: AtomicUsize::new(0),
        });
        handle.touch();

        {
            let (client, _g) = handle.client().await?;
            let row = client
                .query_one("SELECT version(), current_setting('server_version')", &[])
                .await
                .map_err(|e| map_pg_error(&e))?;
            if let Ok(v) = row.try_get::<_, String>(1) {
                let _ = handle.shared.server_version.set(v);
            }
            if let Ok(full) = row.try_get::<_, String>(0) {
                tracing::debug!(version = %full, "postgres connected");
            }
        }
        Ok(handle)
    }

    /// The database this pool connects to.
    pub fn database(&self) -> &str {
        self.shared
            .database
            .get()
            .map_or(self.shared.params.database.as_str(), String::as_str)
    }

    /// `SHOW server_version` (cached after the first connection).
    pub async fn server_version(&self) -> Result<String> {
        if let Some(v) = self.shared.server_version.get() {
            self.touch();
            return Ok(v.clone());
        }
        let (c, _g) = self.client().await?;
        let row = c
            .query_one("SHOW server_version", &[])
            .await
            .map_err(|e| map_pg_error(&e))?;
        let v: String = row.try_get(0).map_err(|e| map_pg_error(&e))?;
        let _ = self.shared.server_version.set(v.clone());
        Ok(v)
    }

    /// Seconds since the pool was last used (idle reaper). Always 0 while an operation runs.
    pub fn idle_secs(&self) -> u64 {
        if self.active.load(Ordering::SeqCst) > 0 {
            return 0;
        }
        let now = self.elapsed_ms();
        now.saturating_sub(self.last_used_ms.load(Ordering::SeqCst)) / 1000
    }

    /// Closes the server sessions sitting idle in the pool (none is in use by definition) and
    /// returns how many. The handle stays usable: the next operation opens a fresh session.
    /// This is what the idle reaper does, so serverless compute (Aurora Serverless v2, Neon)
    /// can pause without the connection disappearing from the UI.
    pub fn release_idle_connections(&self) -> usize {
        let removed = self.pool.retain(|_, _| false).removed;
        let n = removed.len();
        drop(removed);
        n
    }

    /// Server sessions currently open (idle in the pool or in use).
    pub fn open_sessions(&self) -> usize {
        self.pool.status().size
    }

    /// Closes the pool; idle connections are dropped, and later calls fail with `NotConnected`.
    pub async fn close(&self) {
        self.pool.close();
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.created.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn touch(&self) {
        self.last_used_ms.store(self.elapsed_ms(), Ordering::SeqCst);
    }

    fn activity(&self) -> ActivityGuard<'_> {
        self.active.fetch_add(1, Ordering::SeqCst);
        self.touch();
        ActivityGuard(self)
    }

    /// Checks a connection out of the pool (creating one, with fresh credentials, if needed).
    async fn client(&self) -> Result<(Object, ActivityGuard<'_>)> {
        let guard = self.activity();
        let obj = self.pool.get().await.map_err(map_pool_error)?;
        Ok((obj, guard))
    }
}
