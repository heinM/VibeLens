//! Shared helpers for the `conn_*` integration tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;
use vibelens_core::model::*;

/// A minimal valid Postgres profile with password auth and no tunnel.
pub fn pg_profile(name: &str) -> ConnectionProfile {
    ConnectionProfile {
        id: String::new(),
        name: name.to_string(),
        engine: EngineKind::Postgres,
        host: "db.internal.example".into(),
        port: 5432,
        database: Some("app".into()),
        auth: AuthMethod::Password {
            username: "app_ro".into(),
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
    }
}

/// A minimal valid Valkey profile (no auth, no tunnel).
pub fn valkey_profile(name: &str, host: &str, port: u16) -> ConnectionProfile {
    ConnectionProfile {
        engine: EngineKind::Valkey,
        host: host.into(),
        port,
        database: None,
        auth: AuthMethod::None,
        tls: TlsConfig {
            mode: TlsMode::Disable,
            ca_cert_path: None,
            use_rds_bundle: false,
        },
        ..pg_profile(name)
    }
}

/// A running throwaway sshd. Stopped on drop.
pub struct Sshd {
    pub dir: TempDir,
    pub env: HashMap<String, String>,
}

impl Sshd {
    pub fn start() -> Option<Sshd> {
        if std::env::var("VG_TEST_SSH").as_deref() != Ok("1") {
            eprintln!("skipping: set VG_TEST_SSH=1 to run the sshd-backed tunnel tests");
            return None;
        }
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conn_tunnel_setup.sh");
        let dir = tempfile::Builder::new()
            .prefix("vg-sshd-")
            .tempdir()
            .expect("tempdir");
        let out = Command::new("bash")
            .arg(&script)
            .arg("start")
            .arg(dir.path())
            .stderr(Stdio::inherit())
            .output()
            .expect("run setup script");
        assert!(
            out.status.success(),
            "sshd setup failed: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let env = String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter_map(|l| {
                l.split_once('=')
                    .map(|(k, v)| (k.to_string(), v.to_string()))
            })
            .collect();
        Some(Sshd { dir, env })
    }

    pub fn get(&self, key: &str) -> &str {
        self.env
            .get(key)
            .unwrap_or_else(|| panic!("setup script did not print {key}"))
    }
    pub fn port(&self) -> u16 {
        self.get("SSH_PORT").parse().unwrap()
    }
    pub fn host_fp(&self) -> String {
        self.get("HOST_FINGERPRINT").to_string()
    }
    pub fn other_fp(&self) -> String {
        self.get("OTHER_HOST_FINGERPRINT").to_string()
    }
    pub fn user(&self) -> String {
        self.get("SSH_USER").to_string()
    }

    pub fn stop(&self) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/conn_tunnel_setup.sh");
        let _ = Command::new("bash")
            .arg(script)
            .arg("stop")
            .arg(self.dir.path())
            .status();
    }

    pub fn cfg(&self, auth: SshAuth, pinned: Option<String>) -> TunnelConfig {
        TunnelConfig::Ssh {
            host: "127.0.0.1".into(),
            port: self.port(),
            username: self.user(),
            auth,
            known_host_fingerprint: pinned,
        }
    }
    /// [`cfg`](Self::cfg) with this sshd's own host key pinned, which is what a connection
    /// needs to get as far as authentication.
    pub fn trusted(&self, auth: SshAuth) -> TunnelConfig {
        self.cfg(auth, Some(self.host_fp()))
    }
    pub fn key_auth(&self) -> SshAuth {
        SshAuth::PrivateKey {
            path: self.get("CLIENT_KEY").to_string(),
        }
    }
}

impl Drop for Sshd {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A TCP proxy in front of a local port whose live connections can be cut on demand while it
/// keeps accepting new ones: a deterministic stand-in for "the network to the bastion blipped".
pub struct CuttableProxy {
    pub addr: std::net::SocketAddr,
    current: std::sync::Arc<std::sync::Mutex<tokio_util::sync::CancellationToken>>,
}

impl CuttableProxy {
    pub async fn start(upstream_port: u16) -> Self {
        use std::sync::{Arc, Mutex};
        use tokio::net::{TcpListener, TcpStream};
        use tokio_util::sync::CancellationToken;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let current = Arc::new(Mutex::new(CancellationToken::new()));
        let shared = current.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    break;
                };
                let token = shared.lock().unwrap().clone();
                tokio::spawn(async move {
                    let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", upstream_port)).await
                    else {
                        return;
                    };
                    tokio::select! {
                        _ = token.cancelled() => {}
                        _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
                    }
                });
            }
        });
        Self { addr, current }
    }

    /// Drops every connection currently going through the proxy; new connections still work.
    pub fn cut_connections(&self) {
        let mut guard = self.current.lock().unwrap();
        guard.cancel();
        *guard = tokio_util::sync::CancellationToken::new();
    }
}
