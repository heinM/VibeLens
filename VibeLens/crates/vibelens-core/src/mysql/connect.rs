//! Connection establishment: driver options, TLS, the `deadpool` manager whose `create()` asks
//! the [`CredentialFn`] for fresh credentials every time, session reset, and `KILL QUERY`.

use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use deadpool::managed::{self, Metrics, RecycleError, RecycleResult};
use mysql_async::prelude::Queryable;
use mysql_async::{Conn, OptsBuilder, SslOpts};
use secrecy::ExposeSecret;

use super::MysqlConnectParams;
use super::error::map_error;
use crate::error::{CoreError, Result};
use crate::model::TlsMode;
use crate::ports::CredentialFn;

/// Largest packet (one row, one value) read from the server: the server's own default
/// `max_allowed_packet` (64 MiB). A bigger row fails the statement instead of growing memory
/// without limit (R6). Set explicitly so the driver never adopts a value the server reports.
pub(super) const MAX_PACKET_BYTES: usize = 64 * 1024 * 1024;
/// Connections that were reset when they were returned but then sat idle this long are pinged.
const RECYCLE_PING_AFTER: Duration = Duration::from_secs(30);
/// Upper bound for one session reset.
const RESET_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound for delivering a `KILL QUERY` (connect + authenticate + run).
pub(super) const KILL_TIMEOUT: Duration = Duration::from_secs(10);

/// State shared between the handle and the pool manager.
pub(super) struct Shared {
    pub params: MysqlConnectParams,
    pub creds: CredentialFn,
    /// DER CA certificates for `VerifyFull` (OS roots, RDS bundle, CA file), loaded once.
    pub roots: Option<Vec<Vec<u8>>>,
    pub server_version: OnceLock<String>,
}

pub(super) type Pool = managed::Pool<MysqlManager>;
pub(super) type Object = managed::Object<MysqlManager>;

/// A pooled connection plus whether its session was already reset when it was returned.
pub(super) struct MyConn {
    pub conn: Conn,
    pub reset_done: bool,
    /// Which server this session is on (`@@server_uuid`), read when it was opened. A
    /// `KILL QUERY` for it is sent only over a connection that lands on the same server.
    pub server: Option<String>,
}

/// Identifies one server instance; reader endpoints and proxies may route two connections to
/// the same host name onto different instances, where the same connection id is someone else.
const SERVER_ID_SQL: &str = "SELECT @@server_uuid";

async fn server_id(conn: &mut Conn) -> Option<String> {
    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        conn.query_first::<Option<String>, _>(SERVER_ID_SQL),
    )
    .await
    {
        Ok(Ok(Some(Some(v)))) if !v.is_empty() => Some(v),
        _ => None,
    }
}

pub(super) struct MysqlManager {
    pub shared: Arc<Shared>,
}

impl managed::Manager for MysqlManager {
    type Type = MyConn;
    type Error = CoreError;

    fn create(&self) -> impl Future<Output = Result<MyConn>> + Send {
        let shared = Arc::clone(&self.shared);
        async move {
            let mut conn = shared.connect_new().await?;
            let server = server_id(&mut conn).await;
            Ok(MyConn {
                conn,
                reset_done: false,
                server,
            })
        }
    }

    /// Last line of defence against state a previous run left behind: an open transaction,
    /// `SET`s, user variables, locks, temporary tables, `USE`. Normally the engine has already
    /// reset the session right after the run; anything that bypassed that is reset here.
    fn recycle(
        &self,
        c: &mut MyConn,
        metrics: &Metrics,
    ) -> impl Future<Output = RecycleResult<CoreError>> + Send {
        let shared = Arc::clone(&self.shared);
        let needs_reset = !c.reset_done;
        c.reset_done = false;
        let stale = metrics.last_used() > RECYCLE_PING_AFTER;
        async move {
            if c.conn.is_disconnected() {
                return Err(RecycleError::Message("connection closed".into()));
            }
            if needs_reset {
                shared
                    .reset_session(&mut c.conn)
                    .await
                    .map_err(|e| RecycleError::Message(e.into()))
            } else if stale {
                match tokio::time::timeout(Duration::from_secs(5), c.conn.ping()).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(e)) => Err(RecycleError::Message(
                        format!("ping failed: {}", map_error(&e)).into(),
                    )),
                    Err(_) => Err(RecycleError::Message("ping timed out".into())),
                }
            } else {
                Ok(())
            }
        }
    }
}

/// `` `name` `` with backticks doubled.
pub(super) fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// The driver options for one physical connection. The password is exposed here and nowhere
/// else.
pub(super) fn build_opts(
    p: &MysqlConnectParams,
    creds: &crate::model::ResolvedCredentials,
    roots: Option<&[Vec<u8>]>,
) -> Result<OptsBuilder> {
    let user = creds
        .username
        .as_deref()
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            CoreError::Invalid("no database user resolved for this connection".into())
        })?;
    let mut attrs = std::collections::HashMap::new();
    attrs.insert("program_name".to_string(), "VibeLens".to_string());
    let mut b = OptsBuilder::default()
        .ip_or_hostname(p.host.clone())
        .tcp_port(p.port)
        .user(Some(user))
        .pass(
            creds
                .password
                .as_ref()
                .map(|s| s.expose_secret().to_string()),
        )
        .db_name(Some(p.database.clone()))
        // Never switch to a Unix socket the server names (`@@socket`): through a tunnel that
        // path would be a socket of whatever server runs on this machine.
        .prefer_socket(false)
        .max_allowed_packet(Some(MAX_PACKET_BYTES))
        // Known up front, so the driver does not ask the server for it.
        .wait_timeout(Some(28_800))
        .stmt_cache_size(0)
        .tcp_keepalive(Some(Duration::from_secs(60)))
        // `mysql_old_password` is refused.
        .secure_auth(true)
        // The cleartext plugin answers only for an RDS IAM token over verified TLS (the profile
        // validator guarantees that pairing; the patched driver also requires TLS).
        .enable_cleartext_plugin(p.cleartext_plugin)
        .connect_attributes(attrs);
    b = match p.tls.mode {
        TlsMode::Disable => b.ssl_opts(None::<SslOpts>),
        // Encrypted, not verified: the UI warns, and the validator refuses it for any credential
        // sent to another machine.
        TlsMode::Require => b.ssl_opts(Some(
            SslOpts::default()
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true),
        )),
        TlsMode::VerifyFull => {
            let roots = roots
                .ok_or_else(|| CoreError::Invalid("no trusted CA certificates available".into()))?;
            b.ssl_opts(Some(
                SslOpts::default()
                    .with_root_certs(roots.iter().map(|c| c.clone().into()).collect())
                    // Only the anchors above: not the driver's built-in Mozilla list.
                    .with_disable_built_in_roots(true)
                    // Verify the real server name even when dialing a tunnel's local end.
                    .with_danger_tls_hostname_override(Some(p.tls_server_name.clone())),
            ))
        }
    };
    Ok(b)
}

impl Shared {
    /// Opens one new physical connection with freshly resolved credentials.
    pub(super) async fn connect_new(&self) -> Result<Conn> {
        crate::tls::ensure_provider();
        let creds = (self.creds)().await?;
        let opts = build_opts(&self.params, &creds, self.roots.as_deref())?;
        let timeout = Duration::from_secs(self.params.options.connect_timeout_secs.max(1));
        match tokio::time::timeout(timeout, Conn::new(opts)).await {
            Ok(Ok(conn)) => {
                let (a, b, c) = conn.server_version();
                let _ = self.server_version.set(format!("{a}.{b}.{c}"));
                Ok(conn)
            }
            Ok(Err(e)) => Err(map_error(&e)),
            Err(_) => Err(CoreError::Timeout(format!(
                "connecting to MySQL took longer than {}s",
                timeout.as_secs()
            ))),
        }
    }

    /// Returns the session to the state of a new connection: `COM_RESET_CONNECTION` (rolls back,
    /// drops temporary tables, releases locks and `GET_LOCK`s, resets session variables and user
    /// variables), then `USE` of the pool's database, which a reset does not restore.
    pub(super) async fn reset_session(&self, conn: &mut Conn) -> std::result::Result<(), String> {
        let db = quote_ident(&self.params.database);
        let reset = async {
            let supported = conn.reset().await.map_err(|e| map_error(&e).to_string())?;
            if !supported {
                // Without COM_RESET_CONNECTION, session state (sql_mode such as ANSI_QUOTES,
                // variables, locks) would carry over to the next run: close instead.
                return Err("the server cannot reset sessions (COM_RESET_CONNECTION)".to_string());
            }
            conn.query_drop(format!("USE {db}"))
                .await
                .map_err(|e| map_error(&e).to_string())
        };
        match tokio::time::timeout(RESET_TIMEOUT, reset).await {
            Ok(r) => r,
            Err(_) => Err("connection reset timed out".into()),
        }
    }

    /// Stops the statement running on connection `id` of server `server` from a separate
    /// connection (`KILL QUERY`), so it stops on the server rather than only on this side. The
    /// kill is sent only when the new connection reports the same `@@server_uuid` (a load
    /// balancer may route it to another instance, where `id` is another session); up to three
    /// connections are tried. Without a known server nothing is killed. Best effort, bounded by
    /// [`KILL_TIMEOUT`]; returns whether the server accepted it.
    pub(super) async fn kill_query(&self, id: u32, server: Option<&str>) -> bool {
        let Some(server) = server else {
            tracing::warn!("MySQL KILL QUERY skipped: the server's identity is unknown");
            return false;
        };
        let fut = async {
            for _ in 0..3 {
                let mut c = self.connect_new().await?;
                if server_id(&mut c).await.as_deref() != Some(server) {
                    let _ = c.disconnect().await;
                    continue;
                }
                let r = c
                    .query_drop(format!("KILL QUERY {id}"))
                    .await
                    .map_err(|e| map_error(&e));
                let _ = c.disconnect().await;
                return r;
            }
            Err(CoreError::Database(
                "no connection reached the server the statement runs on".into(),
            ))
        };
        match tokio::time::timeout(KILL_TIMEOUT, fut).await {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "MySQL KILL QUERY failed");
                false
            }
            Err(_) => {
                tracing::warn!("MySQL KILL QUERY timed out");
                false
            }
        }
    }
}

/// Closes a connection without waiting for whatever result is still arriving on it: `COM_QUIT`
/// is attempted for five seconds, then the socket is dropped.
pub(super) async fn close_hard(conn: Conn) {
    let _ = tokio::time::timeout(Duration::from_secs(5), conn.disconnect()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MysqlOptions, ResolvedCredentials, TlsConfig};
    use secrecy::SecretString;

    fn params(tls: TlsMode) -> MysqlConnectParams {
        MysqlConnectParams {
            host: "127.0.0.1".into(),
            port: 54000,
            tls_server_name: "db.cluster-x.eu-west-1.rds.amazonaws.com".into(),
            database: "shop".into(),
            tls: TlsConfig {
                mode: tls,
                ..Default::default()
            },
            options: MysqlOptions::default(),
            cleartext_plugin: false,
        }
    }

    fn creds() -> ResolvedCredentials {
        ResolvedCredentials {
            username: Some("app".into()),
            password: Some(SecretString::from("pw")),
            ..ResolvedCredentials::anonymous()
        }
    }

    #[test]
    fn options_never_allow_sockets_local_files_or_server_chosen_limits() {
        let p = params(TlsMode::VerifyFull);
        let roots = vec![vec![0u8; 4]];
        let opts: mysql_async::Opts = build_opts(&p, &creds(), Some(&roots)).unwrap().into();
        assert!(!opts.prefer_socket());
        assert!(opts.socket().is_none());
        assert_eq!(opts.max_allowed_packet(), Some(MAX_PACKET_BYTES));
        assert!(opts.local_infile_handler().is_none());
        assert!(!opts.enable_cleartext_plugin());
        assert!(opts.secure_auth());
        assert_eq!(opts.ip_or_hostname(), "127.0.0.1");
        assert_eq!(opts.db_name(), Some("shop"));
        let ssl = opts.ssl_opts().expect("TLS on");
        assert!(ssl.disable_built_in_roots());
        assert!(!ssl.accept_invalid_certs());
        assert!(!ssl.skip_domain_validation());
        // Verified against the real host, not the tunnel's local address.
        assert_eq!(
            ssl.tls_hostname_override(),
            Some("db.cluster-x.eu-west-1.rds.amazonaws.com")
        );
        // Opts' Debug output must not hold the password.
        assert!(!format!("{:?}", p).contains("pw"));
    }

    #[test]
    fn tls_modes() {
        let opts: mysql_async::Opts = build_opts(&params(TlsMode::Disable), &creds(), None)
            .unwrap()
            .into();
        assert!(opts.ssl_opts().is_none());
        let opts: mysql_async::Opts = build_opts(&params(TlsMode::Require), &creds(), None)
            .unwrap()
            .into();
        assert!(opts.ssl_opts().unwrap().accept_invalid_certs());
        assert!(build_opts(&params(TlsMode::VerifyFull), &creds(), None).is_err());
        let mut c = creds();
        c.username = None;
        assert!(build_opts(&params(TlsMode::Disable), &c, None).is_err());
    }

    #[test]
    fn identifiers_are_quoted() {
        assert_eq!(quote_ident("shop"), "`shop`");
        assert_eq!(quote_ident("a`b"), "`a``b`");
    }
}
