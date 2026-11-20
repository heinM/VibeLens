//! Connection lifecycle: one lazily-created connection per logical database, a single
//! cluster connection, and the "rebuild once with fresh credentials" retry policy.
//!
//! A multiplexed connection shares `SELECT` state between everyone using it, so standalone
//! mode keeps **one connection per db index**, with the index fixed in the handshake. `SELECT`
//! is never sent through a shared connection.
//!
//! Why not `ConnectionManager`? It reconnects with the `ConnectionInfo` it was built with, so
//! after an ElastiCache IAM token expires (15 min) or the server drops the connection (12 h
//! forced disconnect) it would redial with stale credentials. Here every (re)build calls the
//! [`CredentialFn`] again.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use redis::aio::{ConnectionLike as AsyncConnectionLike, MultiplexedConnection};
use redis::cluster_async::ClusterConnection;
use redis::cluster_routing::{RoutingInfo, SingleNodeRoutingInfo};
use redis::{Cmd, ErrorKind, Pipeline, RedisError, RedisResult, Value};
use rustls::ClientConfig;
use tokio::sync::Mutex;

use super::{ValkeyConnectParams, transport};
use crate::error::{CoreError, Result};
use crate::ports::CredentialFn;

/// A live connection: standalone (bound to one db) or the cluster client.
#[derive(Clone)]
pub(super) enum Conn {
    Single(MultiplexedConnection),
    Cluster(ClusterConnection),
}

impl Conn {
    /// Sends one command. `route` only matters in cluster mode (pins the node); without it the
    /// cluster client routes by key / command policy. Top-level server errors become `Err`.
    pub async fn request(
        &mut self,
        cmd: &Cmd,
        route: Option<&SingleNodeRoutingInfo>,
    ) -> RedisResult<Value> {
        let value = match self {
            Conn::Single(c) => c.send_packed_command(cmd).await?,
            Conn::Cluster(c) => match route {
                Some(r) => {
                    c.route_command(cmd.clone(), RoutingInfo::SingleNode(r.clone()))
                        .await?
                }
                None => AsyncConnectionLike::req_packed_command(c, cmd).await?,
            },
        };
        match value {
            Value::ServerError(e) => Err(e.into()),
            v => Ok(v),
        }
    }

    /// Sends a pipeline; replies are returned verbatim (per-command errors are `Value::ServerError`).
    pub async fn pipeline(
        &mut self,
        p: &Pipeline,
        route: Option<&SingleNodeRoutingInfo>,
    ) -> RedisResult<Vec<Value>> {
        match self {
            Conn::Single(c) => c.send_packed_commands(p, 0, p.len()).await,
            Conn::Cluster(c) => match route {
                Some(r) => c.route_pipeline(p.clone(), 0, p.len(), r.clone()).await,
                None => AsyncConnectionLike::req_packed_commands(c, p, 0, p.len()).await,
            },
        }
    }
}

struct Entry {
    generation: u64,
    conn: Conn,
}

type Slot = Arc<Mutex<Option<Entry>>>;

pub(super) struct Session {
    pub params: ValkeyConnectParams,
    creds: CredentialFn,
    tls: Option<Arc<ClientConfig>>,
    slots: std::sync::Mutex<HashMap<u8, Slot>>,
    next_generation: AtomicU64,
    started: Instant,
    last_used_ms: AtomicU64,
    closed: AtomicBool,
}

impl Session {
    pub fn new(
        params: ValkeyConnectParams,
        creds: CredentialFn,
        tls: Option<Arc<ClientConfig>>,
    ) -> Self {
        Self {
            params,
            creds,
            tls,
            slots: std::sync::Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
            started: Instant::now(),
            last_used_ms: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        }
    }

    pub fn is_cluster(&self) -> bool {
        self.params.options.cluster
    }

    /// How many connections this session has built so far. It grows whenever a connection is
    /// rebuilt after a drop or an authentication error (and when another database is first
    /// used), so a change means "the server may see us differently now".
    pub fn connections_made(&self) -> u64 {
        self.next_generation.load(Ordering::Relaxed)
    }

    /// Seconds since the last call that used the connection.
    pub fn idle_secs(&self) -> u64 {
        let now = self.started.elapsed().as_millis() as u64;
        now.saturating_sub(self.last_used_ms.load(Ordering::Relaxed)) / 1000
    }

    fn touch(&self) {
        self.last_used_ms
            .store(self.started.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// Cluster has exactly one logical database.
    pub fn check_db(&self, db: u8) -> Result<u8> {
        if self.is_cluster() {
            if db != 0 {
                return Err(CoreError::Invalid(format!(
                    "cluster mode has only database 0 (requested {db})"
                )));
            }
            Ok(0)
        } else {
            Ok(db)
        }
    }

    fn slot(&self, db: u8) -> Slot {
        // The map lock is never held across an await; poisoning cannot leave it inconsistent.
        let mut map = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        map.entry(db).or_default().clone()
    }

    async fn establish(&self, db: u8) -> Result<Conn> {
        // Fresh credentials for every physical connection (IAM tokens are minted here).
        let creds = (self.creds)().await?;
        if self.is_cluster() {
            transport::connect_cluster(&self.params, &creds)
                .await
                .map(Conn::Cluster)
        } else {
            transport::connect_single(&self.params, self.tls.as_ref(), &creds, db)
                .await
                .map(Conn::Single)
        }
    }

    /// The connection for `db`, creating it on first use. Returns its generation so a failure
    /// can invalidate exactly this connection and not a newer one built by a concurrent caller.
    async fn conn_for(&self, db: u8) -> Result<(u64, Conn)> {
        if self.closed.load(Ordering::Acquire) {
            return Err(CoreError::NotConnected("connection was closed".into()));
        }
        let slot = self.slot(db);
        let mut guard = slot.lock().await;
        if let Some(e) = guard.as_ref() {
            return Ok((e.generation, e.conn.clone()));
        }
        let conn = self.establish(db).await?;
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        *guard = Some(Entry {
            generation,
            conn: conn.clone(),
        });
        Ok((generation, conn))
    }

    /// Forces the first use of `db` (connect, authenticate, select) so errors surface early.
    pub async fn warm_up(&self, db: u8) -> Result<()> {
        self.conn_for(self.check_db(db)?).await.map(|_| ())
    }

    async fn invalidate(&self, db: u8, generation: u64) {
        let slot = self.slot(db);
        let mut guard = slot.lock().await;
        if guard.as_ref().is_some_and(|e| e.generation == generation) {
            *guard = None;
        }
    }

    /// Runs `op` against the connection for `db`.
    ///
    /// - Auth errors (`NOAUTH`, `WRONGPASS`, ...): the connection is dropped and the call is
    ///   retried **once** on a new connection built with freshly resolved credentials. The
    ///   server rejected the command before running it, so this is safe for writes too.
    /// - Dropped connections: dropped and rebuilt; the call is retried once only when
    ///   `idempotent`, because a write may have been applied before the link died.
    /// - Anything else is returned as is.
    pub async fn run<T, F, Fut>(&self, db: u8, idempotent: bool, op: F) -> Result<T>
    where
        F: Fn(Conn) -> Fut,
        Fut: Future<Output = RedisResult<T>>,
    {
        let db = self.check_db(db)?;
        self.touch();
        let mut retried = false;
        loop {
            let (generation, conn) = self.conn_for(db).await?;
            match op(conn).await {
                Ok(v) => {
                    self.touch();
                    return Ok(v);
                }
                Err(e) => match classify_err(&e) {
                    ErrClass::Auth => {
                        self.invalidate(db, generation).await;
                        if !retried {
                            retried = true;
                            continue;
                        }
                        return Err(to_core(&e));
                    }
                    ErrClass::Dropped => {
                        self.invalidate(db, generation).await;
                        if idempotent && !retried {
                            retried = true;
                            continue;
                        }
                        let base = to_core(&e).to_string();
                        return Err(if idempotent {
                            CoreError::NotConnected(base)
                        } else {
                            CoreError::NotConnected(format!(
                                "{base}. The connection was reset and will be rebuilt on the next call; \
                                 the command may or may not have been applied, so check before retrying"
                            ))
                        });
                    }
                    ErrClass::Other => return Err(to_core(&e)),
                },
            }
        }
    }

    /// One command, as an idempotent read unless `idempotent` is false.
    pub async fn exec(
        &self,
        db: u8,
        cmd: &Cmd,
        route: Option<&SingleNodeRoutingInfo>,
        idempotent: bool,
    ) -> Result<Value> {
        self.run(db, idempotent, |mut c| async move {
            c.request(cmd, route).await
        })
        .await
    }

    /// A pipeline of idempotent reads.
    pub async fn pipeline(
        &self,
        db: u8,
        p: &Pipeline,
        route: Option<&SingleNodeRoutingInfo>,
    ) -> Result<Vec<Value>> {
        self.run(db, true, |mut c| async move { c.pipeline(p, route).await })
            .await
    }

    /// Drops every connection; later calls fail with `NotConnected`.
    pub async fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let slots: Vec<Slot> = {
            let mut map = self.slots.lock().unwrap_or_else(|e| e.into_inner());
            map.drain().map(|(_, s)| s).collect()
        };
        for s in slots {
            *s.lock().await = None;
        }
    }
}

// ----------------------------------------------------------------------------------------
// Error classification
// ----------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ErrClass {
    /// Credentials missing/expired/rejected.
    Auth,
    /// The socket is gone (reset, EOF, broken pipe, refused).
    Dropped,
    Other,
}

pub(super) fn is_auth_error(e: &RedisError) -> bool {
    if e.kind() == ErrorKind::AuthenticationFailed {
        return true;
    }
    if matches!(e.code(), Some("NOAUTH" | "WRONGPASS")) {
        return true;
    }
    let text = e.to_string().to_ascii_lowercase();
    [
        "noauth",
        "wrongpass",
        "invalid username-password",
        "authentication required",
        "invalid password",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub(super) fn classify_err(e: &RedisError) -> ErrClass {
    if is_auth_error(e) {
        ErrClass::Auth
    } else if e.is_timeout() {
        ErrClass::Other
    } else if e.is_connection_dropped() || e.is_io_error() {
        ErrClass::Dropped
    } else {
        ErrClass::Other
    }
}

/// Maps a driver error to the right [`CoreError`] variant. Never includes credentials: the
/// `redis` error types carry server replies and I/O errors only.
pub(super) fn to_core(e: &RedisError) -> CoreError {
    let text = describe(e);
    match classify_err(e) {
        ErrClass::Auth => CoreError::Auth(text),
        ErrClass::Dropped => CoreError::NotConnected(text),
        ErrClass::Other if e.is_timeout() => CoreError::Timeout(text),
        ErrClass::Other => CoreError::Valkey(text),
    }
}

/// `CODE detail` for server errors (what `redis-cli` would show), the library text otherwise.
fn describe(e: &RedisError) -> String {
    if matches!(e.kind(), ErrorKind::Server(_) | ErrorKind::Extension)
        && let Some(code) = e.code()
    {
        return format!("{code} {}", e.detail().unwrap_or(""))
            .trim_end()
            .to_owned();
    }
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn io_err(kind: io::ErrorKind) -> RedisError {
        RedisError::from(io::Error::new(kind, "boom"))
    }

    #[test]
    fn auth_errors_are_recognised() {
        let auth = RedisError::from((ErrorKind::AuthenticationFailed, "bad"));
        assert_eq!(classify_err(&auth), ErrClass::Auth);
        for text in [
            "NOAUTH Authentication required.",
            "WRONGPASS invalid username-password pair or user is disabled.",
            "ERR invalid username-password pair",
        ] {
            let (code, detail) = text.split_once(' ').unwrap();
            let e = redis::make_extension_error(code.to_owned(), Some(detail.to_owned()));
            assert_eq!(classify_err(&e), ErrClass::Auth, "{text}");
            assert!(matches!(to_core(&e), CoreError::Auth(_)), "{text}");
        }
    }

    #[test]
    fn dropped_connections_and_timeouts() {
        for k in [
            io::ErrorKind::BrokenPipe,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::ConnectionRefused,
        ] {
            let e = io_err(k);
            assert_eq!(classify_err(&e), ErrClass::Dropped, "{k:?}");
            assert!(matches!(to_core(&e), CoreError::NotConnected(_)));
        }
        let t = io_err(io::ErrorKind::TimedOut);
        assert_eq!(classify_err(&t), ErrClass::Other);
        assert!(matches!(to_core(&t), CoreError::Timeout(_)));
    }

    #[test]
    fn ordinary_server_errors_are_not_retried() {
        let e = redis::make_extension_error(
            "WRONGTYPE".into(),
            Some("Operation against a key holding the wrong kind of value".into()),
        );
        assert_eq!(classify_err(&e), ErrClass::Other);
        let core = to_core(&e);
        assert!(matches!(core, CoreError::Valkey(_)));
        assert!(core.to_string().contains("WRONGTYPE"));
        let Ok(Value::ServerError(noperm)) =
            redis::parse_redis_value(b"-NOPERM no permissions\r\n")
        else {
            panic!("parse")
        };
        assert_eq!(classify_err(&RedisError::from(noperm)), ErrClass::Other);
    }
}
