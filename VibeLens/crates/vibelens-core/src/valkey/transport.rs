//! Connection establishment: TCP, TLS (dial address separate from the verified name), RESP3
//! handshake and cluster client construction.
//!
//! ## Why we open the socket ourselves (standalone)
//! `redis` derives the TLS server name from the host it dials and offers no hook to change one
//! without the other, and it builds its own rustls config (so our shared [`crate::tls`] config,
//! with the RDS bundle and the `Require` no-verify mode, cannot be injected). We need
//! "dial `127.0.0.1:<tunnel port>`, verify `<real ElastiCache host>`", so for standalone we
//! open the TCP stream, wrap it with our `rustls::ClientConfig` and hand the finished stream to
//! [`redis::aio::MultiplexedConnection::new_with_config`], which runs the RESP3 handshake
//! (`HELLO 3 AUTH ...`, `SELECT`) on it. Reconnection is ours too (see `session.rs`), which is
//! what lets every rebuild mint fresh IAM credentials.
//!
//! ## Cluster
//! The cluster client cannot take a caller-supplied stream, so it uses the crate's own TLS. The
//! seed node is addressed by the TLS name and a pinned DNS resolver maps that name to the real
//! dial host, which keeps dial and verification separate for the seed. Limitations are listed
//! in the module docs of [`super`].

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use redis::aio::MultiplexedConnection;
use redis::cluster::ClusterClientBuilder;
use redis::cluster_async::ClusterConnection;
use redis::io::AsyncDNSResolver;
use redis::{
    AsyncConnectionConfig, ConnectionAddr, ProtocolVersion, RedisConnectionInfo, RedisError,
    RedisFuture, TlsCertificates,
};
use rustls::ClientConfig;
use rustls::pki_types::ServerName;
use secrecy::ExposeSecret;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use super::ValkeyConnectParams;
use super::session::to_core;
use crate::error::{CoreError, Result};
use crate::model::{ResolvedCredentials, TlsMode};

/// How long a single command may wait for its reply. Generous: console users run slow commands.
pub(super) const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// The name certificates are verified against: `tls_server_name`, falling back to the host.
pub(super) fn verify_name(p: &ValkeyConnectParams) -> &str {
    let n = p.tls_server_name.trim();
    if n.is_empty() { &p.host } else { n }
}

fn connect_timeout(p: &ValkeyConnectParams) -> Duration {
    Duration::from_secs(p.options.connect_timeout_secs.max(1))
}

/// Handshake parameters: RESP3, the logical db, and credentials when there is a password.
fn connection_info(creds: &ResolvedCredentials, db: u8) -> RedisConnectionInfo {
    let mut info = RedisConnectionInfo::default()
        .set_protocol(ProtocolVersion::RESP3)
        .set_db(i64::from(db))
        .set_lib_name("VibeLens", env!("CARGO_PKG_VERSION"));
    if let Some(password) = &creds.password {
        if let Some(user) = creds.username.as_deref().filter(|u| !u.is_empty()) {
            info = info.set_username(user);
        }
        info = info.set_password(password.expose_secret());
    }
    info
}

async fn with_deadline<T>(
    p: &ValkeyConnectParams,
    what: &str,
    fut: impl Future<Output = Result<T>>,
) -> Result<T> {
    let limit = connect_timeout(p);
    match tokio::time::timeout(limit, fut).await {
        Ok(r) => r,
        Err(_) => Err(CoreError::Timeout(format!(
            "{what} {}:{} timed out after {}s",
            p.host,
            p.port,
            limit.as_secs()
        ))),
    }
}

fn dial_error(p: &ValkeyConnectParams, e: std::io::Error) -> CoreError {
    CoreError::NotConnected(format!("cannot connect to {}:{}: {e}", p.host, p.port))
}

/// Opens one standalone connection bound to logical database `db`.
pub(super) async fn connect_single(
    p: &ValkeyConnectParams,
    tls: Option<&Arc<ClientConfig>>,
    creds: &ResolvedCredentials,
    db: u8,
) -> Result<MultiplexedConnection> {
    with_deadline(p, "connecting to", async {
        let tcp = TcpStream::connect((p.host.as_str(), p.port))
            .await
            .map_err(|e| dial_error(p, e))?;
        let _ = tcp.set_nodelay(true);

        let io: Box<dyn Io> = match tls {
            None => Box::new(tcp),
            Some(cfg) => {
                let name = ServerName::try_from(verify_name(p).to_owned()).map_err(|e| {
                    CoreError::Invalid(format!("invalid TLS server name {:?}: {e}", verify_name(p)))
                })?;
                let stream = TlsConnector::from(cfg.clone())
                    .connect(name, tcp)
                    .await
                    .map_err(|e| {
                        CoreError::NotConnected(format!(
                            "TLS handshake with {}:{} failed: {e}",
                            p.host, p.port
                        ))
                    })?;
                Box::new(stream)
            }
        };

        let config = AsyncConnectionConfig::new()
            .set_response_timeout(Some(RESPONSE_TIMEOUT))
            .set_connection_timeout(Some(connect_timeout(p)));
        let (conn, driver) =
            MultiplexedConnection::new_with_config(&connection_info(creds, db), io, config)
                .await
                .map_err(|e| handshake_error(p, e))?;
        // The driver ends on its own once every clone of `conn` is dropped.
        tokio::spawn(driver);
        Ok(conn)
    })
    .await
}

fn handshake_error(p: &ValkeyConnectParams, e: RedisError) -> CoreError {
    if e.kind() == redis::ErrorKind::RESP3NotSupported {
        return CoreError::Valkey(format!(
            "{}:{} does not support RESP3; VibeLens needs Redis OSS 6+ or Valkey",
            p.host, p.port
        ));
    }
    to_core(&e)
}

// ----------------------------------------------------------------------------------------
// Cluster
// ----------------------------------------------------------------------------------------

/// Maps one hostname to a different dial host; everything else resolves normally.
pub(super) struct PinnedResolver {
    pub name: String,
    pub dial_host: String,
}

impl AsyncDNSResolver for PinnedResolver {
    fn resolve<'a, 'b: 'a>(
        &'a self,
        host: &'b str,
        port: u16,
    ) -> RedisFuture<'a, Box<dyn Iterator<Item = SocketAddr> + Send + 'a>> {
        let target = if host.eq_ignore_ascii_case(&self.name) {
            self.dial_host.as_str()
        } else {
            host
        };
        async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((target, port))
                .await
                .map_err(RedisError::from)?
                .collect();
            if addrs.is_empty() {
                return Err(RedisError::from((
                    redis::ErrorKind::InvalidClientConfig,
                    "no addresses found for host",
                )));
            }
            Ok(Box::new(addrs.into_iter()) as Box<dyn Iterator<Item = SocketAddr> + Send>)
        }
        .boxed()
    }
}

/// PEM bundle of trust anchors for cluster TLS: the OS roots plus the user's extra CA file.
/// `None` when the user supplied no extra CA (the crate then uses the OS roots itself).
///
/// The crate *replaces* its roots with a supplied bundle, so the OS roots must be included to
/// keep public CAs working. Roots rustls rejects are skipped, as in [`crate::tls`].
pub(super) fn cluster_roots_pem(cfg: &crate::model::TlsConfig) -> Result<Option<Vec<u8>>> {
    let Some(path) = cfg.ca_cert_path.as_deref().filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let extra = std::fs::read(path)
        .map_err(|e| CoreError::Invalid(format!("cannot read CA file {path}: {e}")))?;
    let mut pem = Vec::new();
    let mut scratch = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        if scratch.add(cert.clone()).is_ok() {
            pem.extend_from_slice(&der_to_pem(cert.as_ref()));
        }
    }
    pem.extend_from_slice(&extra);
    if !pem.ends_with(b"\n") {
        pem.push(b'\n');
    }
    Ok(Some(pem))
}

fn der_to_pem(der: &[u8]) -> Vec<u8> {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::with_capacity(b64.len() + b64.len() / 64 + 64);
    out.push_str("-----BEGIN CERTIFICATE-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        // base64 output is ASCII, so this cannot fail.
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out.into_bytes()
}

/// Opens a cluster connection from the single configured seed node.
pub(super) async fn connect_cluster(
    p: &ValkeyConnectParams,
    creds: &ResolvedCredentials,
) -> Result<ClusterConnection> {
    let tls_on = p.tls.mode != TlsMode::Disable;
    let insecure = p.tls.mode == TlsMode::Require;
    // With TLS the seed is addressed by the verified name; the resolver below dials the real host.
    let seed_host = if tls_on {
        verify_name(p).to_owned()
    } else {
        p.host.clone()
    };
    let addr = if tls_on {
        ConnectionAddr::TcpTls {
            host: seed_host.clone(),
            port: p.port,
            insecure,
            tls_params: None,
        }
    } else {
        ConnectionAddr::Tcp(seed_host.clone(), p.port)
    };

    let mut builder = ClusterClientBuilder::new(vec![addr])
        .use_protocol(ProtocolVersion::RESP3)
        .connection_timeout(connect_timeout(p))
        .response_timeout(RESPONSE_TIMEOUT);
    if tls_on {
        builder = builder.tls(if insecure {
            redis::TlsMode::Insecure
        } else {
            redis::TlsMode::Secure
        });
        if !insecure {
            if p.tls.use_rds_bundle {
                tracing::warn!(
                    "valkey cluster mode cannot use the bundled RDS CA set; trusting OS roots and ca_cert_path only"
                );
            }
            if let Some(pem) = cluster_roots_pem(&p.tls)? {
                builder = builder.certs(TlsCertificates {
                    client_tls: None,
                    root_cert: Some(pem),
                });
            }
        }
    }
    if !seed_host.eq_ignore_ascii_case(&p.host) {
        builder = builder.async_dns_resolver(PinnedResolver {
            name: seed_host,
            dial_host: p.host.clone(),
        });
    }
    if let Some(password) = &creds.password {
        if let Some(user) = creds.username.as_deref().filter(|u| !u.is_empty()) {
            builder = builder.username(user);
        }
        builder = builder.password(password.expose_secret());
    }

    let client = builder.build().map_err(|e| cluster_error(p, &e))?;
    with_deadline(p, "connecting to cluster at", async {
        client
            .get_async_connection()
            .await
            .map_err(|e| cluster_error(p, &e))
    })
    .await
}

fn cluster_error(p: &ValkeyConnectParams, e: &RedisError) -> CoreError {
    let msg = e.to_string();
    if msg.contains("tls-rustls-insecure") {
        return CoreError::Invalid(
            "TLS mode \"require\" (encrypt without verifying) is not supported for Valkey cluster connections in this build; \
             use verify-full with a CA file"
                .into(),
        );
    }
    match to_core(e) {
        // Dial failures surface as bare I/O errors; say where we were dialling.
        CoreError::NotConnected(m) => {
            CoreError::NotConnected(format!("cluster {}:{}: {m}", p.host, p.port))
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{CertificateDer, pem::PemObject};

    #[tokio::test]
    async fn pinned_resolver_maps_only_the_pinned_name() {
        let r = PinnedResolver {
            name: "cache.example.invalid".into(),
            dial_host: "127.0.0.1".into(),
        };
        let mapped: Vec<_> = r
            .resolve("cache.example.invalid", 6380)
            .await
            .unwrap()
            .collect();
        assert!(
            mapped
                .iter()
                .all(|a| a.ip().is_loopback() && a.port() == 6380),
            "{mapped:?}"
        );
        let other: Vec<_> = r.resolve("127.0.0.2", 7000).await.unwrap().collect();
        assert_eq!(other[0].ip().to_string(), "127.0.0.2");
        // case-insensitive match
        assert!(r.resolve("CACHE.EXAMPLE.INVALID", 1).await.is_ok());
    }

    #[test]
    fn pem_encoding_is_parseable() {
        let der = vec![0x30, 0x03, 0x02, 0x01, 0x05];
        let pem = String::from_utf8(der_to_pem(&der)).unwrap();
        assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(pem.ends_with("-----END CERTIFICATE-----\n"));
        let parsed: Vec<_> = CertificateDer::pem_slice_iter(pem.as_bytes()).collect();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].as_ref().unwrap().as_ref(), der.as_slice());
    }

    #[test]
    fn verify_name_falls_back_to_host() {
        let mut p = ValkeyConnectParams {
            host: "127.0.0.1".into(),
            port: 1,
            tls_server_name: "".into(),
            tls: Default::default(),
            options: Default::default(),
        };
        assert_eq!(verify_name(&p), "127.0.0.1");
        p.tls_server_name = "real.cache.amazonaws.com".into();
        assert_eq!(verify_name(&p), "real.cache.amazonaws.com");
    }

    #[test]
    fn no_extra_ca_means_no_custom_roots() {
        assert!(cluster_roots_pem(&Default::default()).unwrap().is_none());
    }

    #[test]
    fn missing_ca_file_is_invalid() {
        let cfg = crate::model::TlsConfig {
            ca_cert_path: Some("/nonexistent/ca.pem".into()),
            ..Default::default()
        };
        assert!(matches!(
            cluster_roots_pem(&cfg),
            Err(CoreError::Invalid(_))
        ));
    }
}
