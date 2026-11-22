//! TLS + password-auth behaviour of `ValkeyHandle`, against a throw-away `redis-server`
//! started with TLS on a free loopback port (killed when the test ends).
//!
//! Skipped unless `VG_TEST_VALKEY` is set (the same switch as the other Valkey tests) and both
//! `redis-server` and `openssl` are on `PATH` (service `valkey-tls` in `VG_TEST_REQUIRE`, apart
//! from `valkey`). Does not touch the shared server on :6379.

mod services;

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use secrecy::SecretString;
use serde_json::json;
use vibelens_core::CoreError;
use vibelens_core::model::*;
use vibelens_core::ports::CredentialFn;
use vibelens_core::valkey::{ValkeyConnectParams, ValkeyHandle};

const CERT_NAME: &str = "cache.vg-test.example";

struct TlsServer {
    child: Child,
    port: u16,
    ca: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn have(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .expect("free port")
}

fn start(password: &str) -> Option<TlsServer> {
    if std::env::var("VG_TEST_VALKEY")
        .map(|v| v.is_empty())
        .unwrap_or(true)
    {
        services::skip("valkey", "VG_TEST_VALKEY is not set");
        return None;
    }
    if let Some(missing) = ["redis-server", "openssl"].into_iter().find(|b| !have(b)) {
        services::skip(
            "valkey-tls",
            format!("`{missing}` is not on PATH, and these tests start their own TLS server"),
        );
        return None;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let (cert, key) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "2",
            "-subj",
            &format!("/CN={CERT_NAME}"),
        ])
        .args([
            "-addext",
            &format!("subjectAltName=DNS:{CERT_NAME}"),
            "-addext",
            "basicConstraints=critical,CA:FALSE",
        ])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    assert!(status.success(), "openssl failed");

    let port = free_port();
    let child = Command::new("redis-server")
        .args([
            "--port",
            "0",
            "--bind",
            "127.0.0.1",
            "--save",
            "",
            "--appendonly",
            "no",
            "--tls-auth-clients",
            "no",
        ])
        .args(["--tls-port", &port.to_string()])
        .arg("--tls-cert-file")
        .arg(&cert)
        .arg("--tls-key-file")
        .arg(&key)
        .arg("--tls-ca-cert-file")
        .arg(&cert)
        .args(["--requirepass", password])
        .current_dir(dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let server = TlsServer {
        child,
        port,
        ca: cert,
        _dir: dir,
    };

    let deadline = Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            Instant::now() < deadline,
            "redis-server did not start (built without TLS?)"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Some(server)
}

fn creds(password: Option<&'static str>) -> CredentialFn {
    Arc::new(move || {
        let c = ResolvedCredentials {
            password: password.map(|p| SecretString::from(p.to_owned())),
            ..ResolvedCredentials::anonymous()
        };
        async move { Ok(c) }.boxed()
    })
}

fn params(s: &TlsServer, mode: TlsMode, server_name: &str, ca: bool) -> ValkeyConnectParams {
    ValkeyConnectParams {
        // Dial loopback, verify the certificate's real name: what an SSH tunnel looks like.
        host: "127.0.0.1".into(),
        port: s.port,
        tls_server_name: server_name.into(),
        tls: TlsConfig {
            mode,
            ca_cert_path: ca.then(|| s.ca.to_string_lossy().into_owned()),
            use_rds_bundle: false,
        },
        options: ValkeyOptions {
            connect_timeout_secs: 5,
            ..Default::default()
        },
    }
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn dials_loopback_but_verifies_the_real_server_name() {
    let Some(s) = start("s3cret") else { return };
    let h = ValkeyHandle::connect(
        params(&s, TlsMode::VerifyFull, CERT_NAME, true),
        creds(Some("s3cret")),
    )
    .await
    .expect("verify-full against the pinned CA with the right name");
    match h.command(0, argv(&["PING"]), false).await.unwrap() {
        CommandOutcome::Ok { result, .. } => assert_eq!(result, json!("PONG")),
        other => panic!("{other:?}"),
    }
    assert!(h.server_info().await.unwrap().version.is_some());
    h.close().await;
}

#[tokio::test]
async fn wrong_server_name_fails_verification() {
    let Some(s) = start("s3cret") else { return };
    let err = ValkeyHandle::connect(
        params(&s, TlsMode::VerifyFull, "other.example", true),
        creds(Some("s3cret")),
    )
    .await
    .err()
    .expect("hostname mismatch must be rejected");
    assert!(
        matches!(err, CoreError::NotConnected(ref m) if m.contains("TLS")),
        "{err:?}"
    );
}

#[tokio::test]
async fn untrusted_certificate_fails_verification() {
    let Some(s) = start("s3cret") else { return };
    // Right name, but the self-signed cert is not trusted (no ca_cert_path, not in OS roots).
    let err = ValkeyHandle::connect(
        params(&s, TlsMode::VerifyFull, CERT_NAME, false),
        creds(Some("s3cret")),
    )
    .await
    .err();
    assert!(matches!(err, Some(CoreError::NotConnected(_))), "{err:?}");
}

#[tokio::test]
async fn require_mode_encrypts_without_verifying() {
    let Some(s) = start("s3cret") else { return };
    let h = ValkeyHandle::connect(
        params(&s, TlsMode::Require, "anything.example", false),
        creds(Some("s3cret")),
    )
    .await
    .expect("require = no verification");
    assert!(h.is_write_command(&argv(&["SET", "a", "b"])).await.unwrap());
}

#[tokio::test]
async fn plain_text_client_cannot_talk_to_a_tls_port() {
    let Some(s) = start("s3cret") else { return };
    let r = ValkeyHandle::connect(
        params(&s, TlsMode::Disable, CERT_NAME, false),
        creds(Some("s3cret")),
    )
    .await;
    assert!(r.is_err());
}

#[tokio::test]
async fn missing_or_wrong_password_is_an_auth_error() {
    let Some(s) = start("s3cret") else { return };
    for c in [creds(None), creds(Some("wrong"))] {
        match ValkeyHandle::connect(params(&s, TlsMode::Require, CERT_NAME, false), c).await {
            Err(CoreError::Auth(m)) => {
                assert!(!m.contains("wrong") || m.contains("WRONGPASS"), "{m}")
            }
            Err(other) => panic!("expected Auth, got {other:?}"),
            Ok(_) => panic!("must not connect"),
        }
    }
}

/// The IAM case: the password rotates while we are connected and the server drops our
/// connection. The rebuild must call the credential function again and carry on.
#[tokio::test]
async fn rebuild_after_password_rotation_uses_fresh_credentials() {
    let Some(s) = start("old-token") else { return };
    let calls = Arc::new(AtomicUsize::new(0));
    let rotated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let creds: CredentialFn = {
        let (calls, rotated) = (calls.clone(), rotated.clone());
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            let pw = if rotated.load(Ordering::SeqCst) {
                "new-token"
            } else {
                "old-token"
            };
            let c = ResolvedCredentials {
                password: Some(SecretString::from(pw.to_owned())),
                ..ResolvedCredentials::anonymous()
            };
            async move { Ok(c) }.boxed()
        })
    };
    let h = ValkeyHandle::connect(params(&s, TlsMode::Require, CERT_NAME, false), creds)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Rotate the password and drop every client (what ElastiCache's 12h disconnect does).
    let admin = ValkeyHandle::connect(
        params(&s, TlsMode::Require, CERT_NAME, false),
        self::creds(Some("old-token")),
    )
    .await
    .unwrap();
    admin
        .command(
            0,
            argv(&["CONFIG", "SET", "requirepass", "new-token"]),
            true,
        )
        .await
        .unwrap();
    rotated.store(true, Ordering::SeqCst);
    admin
        .command(
            0,
            argv(&["CLIENT", "KILL", "TYPE", "normal", "SKIPME", "no"]),
            true,
        )
        .await
        .ok();
    tokio::time::sleep(Duration::from_millis(150)).await;

    match h.command(0, argv(&["PING"]), false).await.unwrap() {
        CommandOutcome::Ok { result, .. } => assert_eq!(result, json!("PONG")),
        other => panic!("{other:?}"),
    }
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "credential function must be called again, was {}",
        calls.load(Ordering::SeqCst)
    );
}
