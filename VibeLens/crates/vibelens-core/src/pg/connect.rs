//! Connection establishment: `tokio_postgres::Config` construction, TLS, and the `deadpool`
//! manager whose `create()` asks the [`CredentialFn`] for fresh credentials every time.

use std::future::Future;
use std::net::IpAddr;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use deadpool::managed::{self, Metrics, RecycleError, RecycleResult};
use secrecy::ExposeSecret;
use tokio_postgres::config::SslMode;
use tokio_postgres::{Client, Config, NoTls};
use tokio_postgres_rustls::MakeRustlsConnect;

use super::PgConnectParams;
use super::error::{error_chain, map_pg_error};
use crate::error::{CoreError, Result};
use crate::model::ResolvedCredentials;
use crate::ports::CredentialFn;

/// Connections that were reset when they were returned but then sat idle this long are pinged
/// (`SELECT 1`) when checked out again.
const RECYCLE_PING_AFTER: Duration = Duration::from_secs(30);
/// Upper bound for one session reset round trip.
const RESET_TIMEOUT: Duration = Duration::from_secs(5);
/// How often to retry while the server answers "starting up" (Aurora/Neon resuming).
const STARTUP_RETRIES: u32 = 8;

/// State shared between the handle and the pool manager.
pub(super) struct Shared {
    pub params: PgConnectParams,
    pub creds: CredentialFn,
    /// `None` when `TlsMode::Disable`.
    pub tls: Option<rustls::ClientConfig>,
    /// Database actually used (profile value, or the credentials' fallback); set on first connect.
    pub database: OnceLock<String>,
    pub server_version: OnceLock<String>,
    /// Cleared once the server rejected the session reset as unsupported (some
    /// Postgres-compatible services); resets then only roll back.
    pub full_reset: AtomicBool,
}

impl Shared {
    /// The endpoint is a capitec/pg-proxy in shunt mode: `BEGIN`, `COMMIT` and `ROLLBACK` are
    /// refused, so read-only runs and session resets must work without them.
    pub(super) fn no_transactions(&self) -> bool {
        self.params
            .options
            .pgproxy
            .as_ref()
            .is_some_and(|p| p.forbids_transactions())
    }
}

pub(super) type Pool = managed::Pool<PgManager>;
pub(super) type Object = managed::Object<PgManager>;

/// A pooled connection plus what the pool needs to know about its session.
pub(super) struct PgConn {
    pub client: Client,
    /// The session was reset when the connection was last returned (see
    /// [`Shared::reset_session`]), so checking it out needs no further round trip. Cleared on
    /// every checkout; a connection that comes back without the flag (dropped future, error
    /// path, introspection query) is reset by [`PgManager::recycle`] instead.
    pub reset_done: bool,
}

impl Deref for PgConn {
    type Target = Client;
    fn deref(&self) -> &Client {
        &self.client
    }
}

pub(super) struct PgManager {
    pub shared: Arc<Shared>,
}

impl PgManager {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

/// Everything [`RESET_SESSION`] does after its `ROLLBACK`.
macro_rules! reset_body {
    () => {
        "SET SESSION AUTHORIZATION DEFAULT; RESET ALL; CLOSE ALL; UNLISTEN *; \
         SELECT pg_catalog.pg_advisory_unlock_all(); \
         DISCARD PLANS; DISCARD TEMP; DISCARD SEQUENCES"
    };
}

/// `DISCARD ALL` minus `DEALLOCATE ALL`, behind a `ROLLBACK` (so it also ends an open or aborted
/// transaction). `DEALLOCATE ALL` would also drop the statements `tokio-postgres` keeps per client
/// for custom-type look-ups, and the driver offers no way to forget them; the next query touching
/// an enum or composite would fail with "prepared statement does not exist". Statements the
/// *user* prepared are removed separately by [`DROP_USER_PREPARED`].
///
/// Through capitec/pg-proxy the whole string is classified by its first keyword: `ROLLBACK` and
/// `SET` are on its skip list, so the reset never costs an LLM analysis round trip.
const RESET_SESSION: &str = concat!("ROLLBACK; ", reset_body!());

/// [`RESET_SESSION`] for a pg-proxy in shunt mode, which refuses every transaction-control
/// statement (`ROLLBACK` included). No transaction can be open there, so nothing is lost.
const RESET_SESSION_NO_TX: &str = reset_body!();

/// Deallocates every prepared statement whose name is not one of the driver's own (`s<digits>`).
const DROP_USER_PREPARED: &str = "DO $vg$ DECLARE r record; BEGIN \
    FOR r IN SELECT name FROM pg_catalog.pg_prepared_statements WHERE name !~ '^s[0-9]+$' LOOP \
    EXECUTE format('DEALLOCATE %I', r.name); END LOOP; END $vg$";

/// The reset was rejected because the server does not implement part of it (SQLSTATE class 0A or
/// 42, e.g. a Postgres-compatible service without `UNLISTEN` or `pg_advisory_unlock_all`).
fn reset_unsupported(e: &tokio_postgres::Error) -> bool {
    e.code()
        .is_some_and(|c| c.code().starts_with("0A") || c.code().starts_with("42"))
}

impl managed::Manager for PgManager {
    type Type = PgConn;
    type Error = CoreError;

    fn create(&self) -> impl Future<Output = Result<PgConn>> + Send {
        let shared = Arc::clone(&self.shared);
        async move {
            Ok(PgConn {
                client: shared.connect_new().await?,
                reset_done: false,
            })
        }
    }

    /// Runs every time a pooled connection is handed out again, and so is the last line of
    /// defence against state a previous user left behind: an open or aborted transaction (a
    /// dropped agent query, a script that never ended its `BEGIN`), `SET` values, advisory locks,
    /// temp tables, cursors, `LISTEN`s and prepared statements.
    ///
    /// Normally the query engine has already reset the session in the background right after the
    /// run ended, and this is free (a liveness ping for connections idle a while). Anything that
    /// bypassed that gets the reset here, in one pipelined round trip.
    fn recycle(
        &self,
        conn: &mut PgConn,
        metrics: &Metrics,
    ) -> impl Future<Output = RecycleResult<CoreError>> + Send {
        let shared = &self.shared;
        let needs_reset = !conn.reset_done;
        conn.reset_done = false;
        let stale = metrics.last_used() > RECYCLE_PING_AFTER;
        let client = &conn.client;
        async move {
            if client.is_closed() {
                return Err(RecycleError::Message("connection closed".into()));
            }
            if needs_reset {
                shared
                    .reset_session(client)
                    .await
                    .map_err(|e| RecycleError::Message(e.into()))
            } else if stale {
                match tokio::time::timeout(Duration::from_secs(5), client.simple_query("SELECT 1"))
                    .await
                {
                    Ok(Ok(_)) => Ok(()),
                    Ok(Err(e)) => Err(RecycleError::Message(
                        format!("ping failed: {}", error_chain(&e)).into(),
                    )),
                    Err(_) => Err(RecycleError::Message("ping timed out".into())),
                }
            } else {
                Ok(())
            }
        }
    }
}

/// A resolved dial target: `host` is what TLS verifies, `addr` (if any) is what is dialed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    pub host: String,
    pub addr: Option<IpAddr>,
}

/// Effective endpoint after applying the credentials' fallbacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub tls_server_name: String,
}

pub(super) fn endpoint(params: &PgConnectParams, creds: &ResolvedCredentials) -> Result<Endpoint> {
    let host = if params.host.trim().is_empty() {
        creds.host_override.clone().unwrap_or_default()
    } else {
        params.host.clone()
    };
    if host.trim().is_empty() {
        return Err(CoreError::Invalid("no database host configured".into()));
    }
    let port = if params.port == 0 {
        creds.port_override.unwrap_or(5432)
    } else {
        params.port
    };
    let database = if params.database.trim().is_empty() {
        creds
            .database_override
            .clone()
            .filter(|d| !d.trim().is_empty())
            .unwrap_or_else(|| "postgres".to_string())
    } else {
        params.database.clone()
    };
    let tls_server_name = if params.tls_server_name.trim().is_empty() {
        host.clone()
    } else {
        params.tls_server_name.clone()
    };
    Ok(Endpoint {
        host,
        port,
        database,
        tls_server_name,
    })
}

/// Decides what to dial. With TLS on and `host != tls_server_name` (tunnels, CNAMEs) we dial the
/// IP(s) but hand `tls_server_name` to the driver so SNI and certificate verification use it.
pub(super) async fn resolve_targets(ep: &Endpoint, tls_enabled: bool) -> Result<Vec<Target>> {
    let same = ep.host.eq_ignore_ascii_case(&ep.tls_server_name);
    if !tls_enabled || same || ep.host.starts_with('/') {
        return Ok(vec![Target {
            host: ep.host.clone(),
            addr: None,
        }]);
    }
    if let Ok(ip) = ep.host.parse::<IpAddr>() {
        return Ok(vec![Target {
            host: ep.tls_server_name.clone(),
            addr: Some(ip),
        }]);
    }
    let mut ips: Vec<IpAddr> = Vec::new();
    let addrs = tokio::net::lookup_host((ep.host.as_str(), ep.port))
        .await
        .map_err(|e| CoreError::Database(format!("cannot resolve {}: {e}", ep.host)))?;
    for a in addrs {
        if !ips.contains(&a.ip()) {
            ips.push(a.ip());
        }
    }
    if ips.is_empty() {
        return Err(CoreError::Database(format!(
            "cannot resolve {}: no addresses",
            ep.host
        )));
    }
    Ok(ips
        .into_iter()
        .map(|ip| Target {
            host: ep.tls_server_name.clone(),
            addr: Some(ip),
        })
        .collect())
}

/// Builds the driver config. The password is exposed here and nowhere else.
pub(super) fn build_config(
    params: &PgConnectParams,
    creds: &ResolvedCredentials,
    ep: &Endpoint,
    targets: &[Target],
    tls_enabled: bool,
) -> Result<Config> {
    let user = creds
        .username
        .as_deref()
        .filter(|u| !u.is_empty())
        .ok_or_else(|| {
            CoreError::Invalid("no database user resolved for this connection".into())
        })?;
    let mut cfg = Config::new();
    cfg.user(user);
    if let Some(pw) = &creds.password {
        cfg.password(pw.expose_secret());
    }
    cfg.dbname(&ep.database);
    cfg.application_name(&params.options.application_name);
    cfg.connect_timeout(Duration::from_secs(
        params.options.connect_timeout_secs.max(1),
    ));
    // TCP keepalives stop NAT gateways, load balancers (AWS NLB: 350 s) and corporate firewalls
    // from silently dropping idle sessions, and detect a dead peer in ~2 minutes instead of the
    // OS default (8-9 probes) so a stale pooled connection is found by the driver, not by a query.
    cfg.keepalives(true);
    cfg.keepalives_idle(Duration::from_secs(60));
    cfg.keepalives_interval(Duration::from_secs(20));
    cfg.keepalives_retries(3);
    cfg.ssl_mode(if tls_enabled {
        SslMode::Require
    } else {
        SslMode::Disable
    });
    cfg.port(ep.port);
    for t in targets {
        cfg.host(t.host.clone());
        if let Some(ip) = t.addr {
            cfg.hostaddr(ip);
        }
    }
    Ok(cfg)
}

fn spawn_driver<F>(conn: F)
where
    F: Future<Output = std::result::Result<(), tokio_postgres::Error>> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!(error = %error_chain(&e), "postgres connection ended with error");
        }
    });
}

impl Shared {
    /// Opens one new physical connection with freshly resolved credentials.
    pub(super) async fn connect_new(&self) -> Result<Client> {
        let creds = (self.creds)().await?;
        let ep = endpoint(&self.params, &creds)?;
        let _ = self.database.set(ep.database.clone());
        let tls_enabled = self.tls.is_some();
        let targets = resolve_targets(&ep, tls_enabled).await?;
        let cfg = build_config(&self.params, &creds, &ep, &targets, tls_enabled)?;
        let mut attempt = 0;
        loop {
            let res =
                match &self.tls {
                    Some(c) => cfg.connect(MakeRustlsConnect::new(c.clone())).await.map(
                        |(client, conn)| {
                            spawn_driver(conn);
                            client
                        },
                    ),
                    None => cfg.connect(NoTls).await.map(|(client, conn)| {
                        spawn_driver(conn);
                        client
                    }),
                };
            match res {
                Ok(client) => return Ok(client),
                Err(e) => {
                    let starting =
                        e.code() == Some(&tokio_postgres::error::SqlState::CANNOT_CONNECT_NOW);
                    attempt += 1;
                    if starting && attempt < STARTUP_RETRIES {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        continue;
                    }
                    return Err(map_pg_error(&e));
                }
            }
        }
    }

    /// Returns the session to a pristine state: ends any open or aborted transaction and
    /// discards session state (see [`RESET_SESSION`]). The statements are pipelined, so this is
    /// one round trip. Errors mean the connection must not be reused.
    pub(super) async fn reset_session(&self, client: &Client) -> std::result::Result<(), String> {
        let full = self.full_reset.load(Ordering::Relaxed);
        let no_tx = self.no_transactions();
        let reset = async {
            if full {
                let sql = if no_tx {
                    RESET_SESSION_NO_TX
                } else {
                    RESET_SESSION
                };
                let (reset, prepared) = tokio::join!(
                    client.simple_query(sql),
                    client.simple_query(DROP_USER_PREPARED)
                );
                // Removing the user's prepared statements is best effort; the reset itself is not.
                if let Err(e) = prepared
                    && e.as_db_error().is_none()
                {
                    return Err(e);
                }
                reset.map(|_| ())
            } else if no_tx {
                client
                    .simple_query("SET SESSION AUTHORIZATION DEFAULT; RESET ALL")
                    .await
                    .map(|_| ())
            } else {
                client.simple_query("ROLLBACK").await.map(|_| ())
            }
        };
        match tokio::time::timeout(RESET_TIMEOUT, reset).await {
            Err(_) => Err("connection reset timed out".into()),
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) if full && reset_unsupported(&e) => {
                self.full_reset.store(false, Ordering::Relaxed);
                tracing::warn!(
                    error = %error_chain(&e),
                    "server rejected the session reset; pooled connections are only rolled back"
                );
                if no_tx {
                    // The proxy refuses ROLLBACK; the failed string's implicit transaction was
                    // already ended by the error.
                    return Ok(());
                }
                // The failed multi-statement string ran in an implicit transaction.
                client
                    .simple_query("ROLLBACK")
                    .await
                    .map(|_| ())
                    .map_err(|e| format!("rollback failed: {}", error_chain(&e)))
            }
            Ok(Err(e)) => Err(format!("connection reset failed: {}", error_chain(&e))),
        }
    }

    /// Asks the server to cancel the query running on `token`'s connection (new TCP connection,
    /// same TLS settings). Best effort, bounded to five seconds.
    pub(super) async fn cancel_backend(&self, token: tokio_postgres::CancelToken) {
        let fut = async {
            match &self.tls {
                Some(c) => token.cancel_query(MakeRustlsConnect::new(c.clone())).await,
                None => token.cancel_query(NoTls).await,
            }
        };
        match tokio::time::timeout(Duration::from_secs(5), fut).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(error = %error_chain(&e), "postgres cancel request failed")
            }
            Err(_) => tracing::warn!("postgres cancel request timed out"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{PgOptions, TlsConfig, TlsMode};
    use secrecy::SecretString;
    use tokio_postgres::config::Host;

    fn params(host: &str, tls_name: &str) -> PgConnectParams {
        PgConnectParams {
            host: host.into(),
            port: 54123,
            tls_server_name: tls_name.into(),
            database: "app".into(),
            tls: TlsConfig {
                mode: TlsMode::VerifyFull,
                ..Default::default()
            },
            options: PgOptions::default(),
        }
    }

    fn creds() -> ResolvedCredentials {
        ResolvedCredentials {
            username: Some("u".into()),
            password: Some(SecretString::from("pw")),
            ..ResolvedCredentials::anonymous()
        }
    }

    #[tokio::test]
    async fn tunnel_uses_real_hostname_for_tls_and_ip_for_dialing() {
        let p = params("127.0.0.1", "mydb.cluster-x.eu-west-1.rds.amazonaws.com");
        let c = creds();
        let ep = endpoint(&p, &c).expect("endpoint");
        let targets = resolve_targets(&ep, true).await.expect("targets");
        let cfg = build_config(&p, &c, &ep, &targets, true).expect("config");
        assert_eq!(
            cfg.get_hosts(),
            &[Host::Tcp(
                "mydb.cluster-x.eu-west-1.rds.amazonaws.com".into()
            )]
        );
        assert_eq!(
            cfg.get_hostaddrs(),
            &["127.0.0.1".parse::<IpAddr>().expect("ip")]
        );
        assert_eq!(cfg.get_ports(), &[54123]);
        assert_eq!(cfg.get_ssl_mode(), SslMode::Require);
        assert_eq!(cfg.get_user(), Some("u"));
        assert_eq!(cfg.get_dbname(), Some("app"));
    }

    #[tokio::test]
    async fn direct_connection_dials_the_host_itself() {
        let p = params("db.example.com", "db.example.com");
        let c = creds();
        let ep = endpoint(&p, &c).expect("endpoint");
        let targets = resolve_targets(&ep, true).await.expect("targets");
        let cfg = build_config(&p, &c, &ep, &targets, true).expect("config");
        assert_eq!(cfg.get_hosts(), &[Host::Tcp("db.example.com".into())]);
        assert!(cfg.get_hostaddrs().is_empty());
    }

    #[tokio::test]
    async fn disabled_tls_ignores_tls_name() {
        let p = params("127.0.0.1", "mydb.example.com");
        let c = creds();
        let ep = endpoint(&p, &c).expect("endpoint");
        let targets = resolve_targets(&ep, false).await.expect("targets");
        let cfg = build_config(&p, &c, &ep, &targets, false).expect("config");
        assert_eq!(cfg.get_hosts(), &[Host::Tcp("127.0.0.1".into())]);
        assert_eq!(cfg.get_ssl_mode(), SslMode::Disable);
    }

    #[tokio::test]
    async fn localhost_cname_is_resolved_and_verified_under_tls_name() {
        let p = params("localhost", "db.internal.example");
        let c = creds();
        let ep = endpoint(&p, &c).expect("endpoint");
        let targets = resolve_targets(&ep, true).await.expect("targets");
        assert!(!targets.is_empty());
        assert!(
            targets
                .iter()
                .all(|t| t.host == "db.internal.example" && t.addr.is_some())
        );
    }

    #[test]
    fn credential_overrides_are_fallbacks_only() {
        let mut p = params("", "");
        p.port = 0;
        p.database = String::new();
        let mut c = creds();
        c.host_override = Some("secret-host".into());
        c.port_override = Some(6543);
        c.database_override = Some("from_secret".into());
        let ep = endpoint(&p, &c).expect("endpoint");
        assert_eq!(
            (
                ep.host.as_str(),
                ep.port,
                ep.database.as_str(),
                ep.tls_server_name.as_str()
            ),
            ("secret-host", 6543, "from_secret", "secret-host")
        );

        let p = params("profile-host", "profile-host");
        let ep = endpoint(&p, &c).expect("endpoint");
        assert_eq!(
            (ep.host.as_str(), ep.port, ep.database.as_str()),
            ("profile-host", 54123, "app")
        );
    }

    #[test]
    fn missing_user_or_host_is_an_error() {
        let p = params("h", "h");
        let ep = endpoint(&p, &creds()).expect("endpoint");
        let mut c = creds();
        c.username = None;
        assert!(matches!(
            build_config(&p, &c, &ep, &[], true),
            Err(CoreError::Invalid(_))
        ));
        assert!(matches!(
            endpoint(&params("", ""), &creds()),
            Err(CoreError::Invalid(_))
        ));
    }
}
