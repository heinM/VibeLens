//! MySQL engine (MySQL 8.x, Aurora MySQL, RDS MySQL). Design and the security rules it meets:
//! `docs/REMOTE_ENGINES_AND_REDSHIFT.md`, section "MySQL".
//!
//! - Driver: `mysql_async` with rustls, patched in `vendor/mysql_async` (no `LOCAL INFILE`, no
//!   multi-statement capability, no password over an unencrypted channel; see its `PATCH.md`).
//! - Pool: `deadpool` with a custom manager whose `create()` calls the [`CredentialFn`] for every
//!   new physical connection (RDS IAM tokens are minted per connection), one pool per
//!   (profile, database), as for Postgres.
//! - TLS: [`crate::tls`] roots (OS, RDS bundle, CA file) and the real host name for
//!   verification even through an SSH tunnel.
//! - Execution: the script is split on the client ([`check::split_script`]); each statement is
//!   one `COM_QUERY` (text protocol), streamed into `QueryEvent::Rows` pages with the row, cell
//!   and page bounds of the Postgres engine. Read-only runs are checked statement by statement
//!   before anything is sent ([`check::check_read_only`]) and run inside
//!   `START TRANSACTION READ ONLY`, whose status the server reports after every statement.
//! - Cancel, timeout, a dropped run: `KILL QUERY <id>` from a separate connection, and the
//!   connection is closed, never returned to the pool. `max_execution_time` backs up the timeout
//!   for `SELECT`s on the server.
//!
//! Layout: [`connect`] (options, TLS, pool manager, kill), [`exec`] (execution), [`catalog`]
//! (navigator, schema metadata), [`cells`] (value mapping), [`check`] and [`lex`] (statement
//! classification), [`validate`] (profile rules), [`secret`] (password bound to its endpoint).

mod catalog;
mod cells;
pub mod check;
mod connect;
mod error;
mod exec;
mod lex;
pub mod secret;
mod validate;

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use deadpool::Runtime;
use deadpool::managed::{PoolError, Timeouts};

use crate::error::{CoreError, Result};
use crate::model::*;
use crate::ports::CredentialFn;

pub use check::{agent_refusal, check_read_only, classify, prod_guard_reason, split_script};
pub use validate::{check_database_name, check_profile};

use connect::{MysqlManager, Object, Pool, Shared};

/// Default cap of `my_schema_metadata` (tables per call), as for Postgres.
pub const DEFAULT_MAX_TABLES: usize = 2000;

#[derive(Debug, Clone)]
pub struct MysqlConnectParams {
    /// Address actually dialed (`127.0.0.1` when tunneled).
    pub host: String,
    pub port: u16,
    /// Host name used for TLS verification (the real server host name).
    pub tls_server_name: String,
    /// The database this pool's sessions use.
    pub database: String,
    pub tls: TlsConfig,
    pub options: MysqlOptions,
    /// Answer an auth switch to `mysql_clear_password` (RDS IAM tokens over verified TLS only).
    pub cleartext_plugin: bool,
}

/// What the approval card shows besides the connection and database: `MySQL · host:port · as
/// user`, plus the tunnel. From the profile, never from the model; no secrets.
pub fn approval_detail(p: &ConnectionProfile) -> String {
    let user = match &p.auth {
        AuthMethod::Password { username } => format!("as {username}"),
        AuthMethod::RdsIam { username, .. } => format!("as {username} (RDS IAM)"),
        _ => String::new(),
    };
    let mut parts = vec!["MySQL".to_string(), format!("{}:{}", p.host.trim(), p.port)];
    if !user.is_empty() {
        parts.push(user);
    }
    if let Some(TunnelConfig::Ssh { host, port, .. }) = &p.tunnel {
        parts.push(format!("via SSH {}:{port}", host.trim()));
    }
    parts.join(" · ")
}

/// A pooled MySQL endpoint (one database). Cheap to share (`Arc`), safe to use concurrently.
pub struct MysqlHandle {
    shared: Arc<Shared>,
    pool: Pool,
    created: Instant,
    last_used_ms: AtomicU64,
    active: AtomicUsize,
}

/// Marks the handle busy while an operation is in flight so the idle reaper never closes a
/// pool under a running query; refreshes the last-used time when dropped.
struct ActivityGuard<'a>(&'a MysqlHandle);

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}

fn map_pool_error(e: PoolError<CoreError>) -> CoreError {
    match e {
        PoolError::Backend(e) => e,
        PoolError::Timeout(_) => {
            CoreError::Timeout("waiting for a free MySQL connection timed out".into())
        }
        PoolError::Closed => CoreError::NotConnected("the MySQL connection was closed".into()),
        other => CoreError::Other(format!("MySQL connection pool: {other}")),
    }
}

impl MysqlHandle {
    /// Opens the pool and validates one connection.
    pub async fn connect(
        params: MysqlConnectParams,
        creds: CredentialFn,
    ) -> Result<Arc<MysqlHandle>> {
        check_database_name(&params.database)?;
        let roots = match params.tls.mode {
            TlsMode::VerifyFull => Some(
                crate::tls::verify_full_root_certs(&params.tls)?
                    .into_iter()
                    .map(|c| c.as_ref().to_vec())
                    .collect(),
            ),
            _ => None,
        };
        let connect_timeout = Duration::from_secs(params.options.connect_timeout_secs.max(1));
        let max_size = params.options.max_pool_size.max(1);
        let shared = Arc::new(Shared {
            params,
            creds,
            roots,
            server_version: OnceLock::new(),
        });
        let pool = Pool::builder(MysqlManager {
            shared: Arc::clone(&shared),
        })
        .max_size(max_size)
        .timeouts(Timeouts {
            wait: Some(Duration::from_secs(60)),
            create: Some(connect_timeout + Duration::from_secs(30)),
            recycle: Some(Duration::from_secs(10)),
        })
        .runtime(Runtime::Tokio1)
        .build()
        .map_err(|e| CoreError::Other(format!("cannot build connection pool: {e}")))?;
        let handle = Arc::new(MysqlHandle {
            shared,
            pool,
            created: Instant::now(),
            last_used_ms: AtomicU64::new(0),
            active: AtomicUsize::new(0),
        });
        handle.touch();
        {
            let _g = handle.activity();
            let obj = handle.pool.get().await.map_err(map_pool_error)?;
            drop(obj);
        }
        Ok(handle)
    }

    /// The database this pool connects to.
    pub fn database(&self) -> &str {
        &self.shared.params.database
    }

    /// The server version from the handshake (`8.0.39`).
    pub fn server_version(&self) -> Option<String> {
        self.touch();
        self.shared.server_version.get().cloned()
    }

    /// Seconds since the pool was last used (idle reaper). Always 0 while an operation runs.
    pub fn idle_secs(&self) -> u64 {
        if self.active.load(Ordering::SeqCst) > 0 {
            return 0;
        }
        self.elapsed_ms()
            .saturating_sub(self.last_used_ms.load(Ordering::SeqCst))
            / 1000
    }

    /// An operation is running on this pool.
    pub fn in_use(&self) -> bool {
        self.active.load(Ordering::SeqCst) > 0
    }

    /// Milliseconds since the pool was last used (0 while in use).
    pub fn idle_ms(&self) -> u64 {
        if self.in_use() {
            return 0;
        }
        self.elapsed_ms()
            .saturating_sub(self.last_used_ms.load(Ordering::SeqCst))
    }

    /// Closes the server sessions sitting idle in the pool and returns how many. The handle
    /// stays usable: the next operation opens a fresh session.
    pub fn release_idle_connections(&self) -> usize {
        let removed = self.pool.retain(|_, _| false).removed;
        let n = removed.len();
        for obj in removed {
            tokio::spawn(connect::close_hard(obj.conn));
        }
        n
    }

    /// Server sessions currently open (idle in the pool or in use).
    pub fn open_sessions(&self) -> usize {
        self.pool.status().size
    }

    /// Closes the pool; later calls fail with `NotConnected`.
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
    async fn conn(&self) -> Result<(Object, ActivityGuard<'_>)> {
        let guard = self.activity();
        let obj = self.pool.get().await.map_err(map_pool_error)?;
        Ok((obj, guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_detail_names_the_endpoint_and_user_but_no_secret() {
        let mut p: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "p", "name": "shop", "engine": "mysql", "host": "db.internal", "port": 3306,
            "database": "shop", "auth": {"kind": "password", "username": "app"},
            "tunnel": null, "color": null, "createdAt": null, "updatedAt": null,
        }))
        .unwrap();
        assert_eq!(approval_detail(&p), "MySQL · db.internal:3306 · as app");
        p.tunnel = Some(TunnelConfig::Ssh {
            host: "bastion".into(),
            port: 2222,
            username: "ops".into(),
            auth: SshAuth::Agent,
            known_host_fingerprint: None,
        });
        assert_eq!(
            approval_detail(&p),
            "MySQL · db.internal:3306 · as app · via SSH bastion:2222"
        );
    }
}
