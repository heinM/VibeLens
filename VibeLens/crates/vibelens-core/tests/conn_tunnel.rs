//! SshTunnelProvider against a real, throwaway `sshd` (127.0.0.1:2222).
//!
//! Opt-in: set `VG_TEST_SSH=1` (the setup script installs `openssh-server` if needed and starts
//! `sshd -D` with generated keys; see `conn_tunnel_setup.sh`). Without it every test returns
//! immediately. The forwarding target is the local Redis on 127.0.0.1:6379 (override with
//! `VG_TEST_REDIS_ADDR`) and a small in-process echo server.
//!
//! Tests are serialised: they share the sshd port and a few process-wide environment variables
//! (`SSH_AUTH_SOCK`, `HOME`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod conn_common;

use conn_common::{CuttableProxy, Sshd};
use secrecy::SecretString;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use vibelens_core::error::CoreError;
use vibelens_core::model::*;
use vibelens_core::ports::{SecretVault, TunnelHandle, TunnelProvider};
use vibelens_core::tunnel::SshTunnelProvider;
use vibelens_core::vault::MemoryVault;

static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn redis_addr() -> String {
    std::env::var("VG_TEST_REDIS_ADDR").unwrap_or_else(|_| "127.0.0.1:6379".to_string())
}

type Seen = Arc<Mutex<Vec<(String, String)>>>;

fn provider(vault: Arc<dyn SecretVault>) -> (Arc<SshTunnelProvider>, Seen) {
    let seen: Seen = Arc::default();
    let sink = seen.clone();
    let p = SshTunnelProvider::new(
        vault,
        Arc::new(move |id: &str, fp: &str| {
            sink.lock().unwrap().push((id.to_string(), fp.to_string()));
            Ok(())
        }),
    );
    (p, seen)
}

fn empty_vault() -> Arc<dyn SecretVault> {
    Arc::new(MemoryVault::new())
}

async fn secret_vault(entries: &[(String, &str)]) -> Arc<dyn SecretVault> {
    let v = MemoryVault::new();
    for (k, s) in entries {
        v.set(k, &SecretString::from(s.to_string())).await.unwrap();
    }
    Arc::new(v)
}

/// Writes `*1\r\n$4\r\nPING\r\n` to `addr` and returns what comes back.
async fn redis_ping(addr: SocketAddr) -> std::io::Result<String> {
    let mut s = TcpStream::connect(addr).await?;
    s.write_all(b"*1\r\n$4\r\nPING\r\n").await?;
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf))
        .await
        .map_err(|_| std::io::ErrorKind::TimedOut)??;
    Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
}

async fn open(
    p: &SshTunnelProvider,
    id: &str,
    cfg: &TunnelConfig,
    target: &str,
) -> Result<TunnelHandle, CoreError> {
    let (host, port) = target.rsplit_once(':').unwrap();
    p.open(id, cfg, host, port.parse().unwrap()).await
}

/// A TCP echo server that also propagates half-close: when the peer finishes sending, it
/// flushes everything back and closes.
async fn echo_server() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let (mut r, mut w) = s.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    addr
}

macro_rules! sshd_or_return {
    ($guard:ident, $sshd:ident) => {
        let $guard = SERIAL.lock().await;
        let Some($sshd) = Sshd::start() else { return };
    };
}

#[tokio::test]
async fn conn_forwards_to_redis_and_captures_the_host_key_fingerprint_on_first_use() {
    sshd_or_return!(_g, sshd);
    let (p, seen) = provider(empty_vault());

    let handle = open(
        &p,
        "profile-1",
        &sshd.cfg(sshd.key_auth(), None),
        &redis_addr(),
    )
    .await
    .expect("tunnel opens");
    assert!(handle.local_addr.ip().is_loopback());
    assert_ne!(handle.local_addr.port(), 0);

    assert_eq!(redis_ping(handle.local_addr).await.unwrap(), "+PONG\r\n");
    // A second connection over the same SSH session.
    assert_eq!(redis_ping(handle.local_addr).await.unwrap(), "+PONG\r\n");

    // TOFU: reported exactly once, for this profile, in `SHA256:<base64 no padding>` form that
    // matches what `ssh-keygen -lf` prints for the host key.
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen, vec![("profile-1".to_string(), sshd.host_fp())]);
    assert!(seen[0].1.starts_with("SHA256:") && !seen[0].1.contains('='));

    // Dropping the handle closes the listener.
    let addr = handle.local_addr;
    drop(handle);
    let mut closed = false;
    for _ in 0..50 {
        if TcpStream::connect(addr).await.is_err() {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        closed,
        "listener still accepting after the handle was dropped"
    );
}

#[tokio::test]
async fn conn_correctly_pinned_fingerprint_connects_and_is_not_reported_again() {
    sshd_or_return!(_g, sshd);
    let (p, seen) = provider(empty_vault());
    let cfg = sshd.cfg(sshd.key_auth(), Some(sshd.host_fp()));
    let handle = open(&p, "p", &cfg, &redis_addr())
        .await
        .expect("pinned connect");
    assert_eq!(redis_ping(handle.local_addr).await.unwrap(), "+PONG\r\n");
    assert!(
        seen.lock().unwrap().is_empty(),
        "no TOFU callback when a fingerprint is already pinned"
    );
}

#[tokio::test]
async fn conn_wrong_pinned_fingerprint_is_refused_before_authentication() {
    sshd_or_return!(_g, sshd);
    // Auth is Password with nothing in the vault: if the host key check did not run first we
    // would see an Auth error instead of the mismatch.
    let (p, seen) = provider(empty_vault());
    for wrong in [
        sshd.other_fp(),
        "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string(),
        "garbage".to_string(),
    ] {
        let cfg = sshd.cfg(SshAuth::Password, Some(wrong.clone()));
        let err = open(&p, "p", &cfg, &redis_addr())
            .await
            .err()
            .expect("must fail");
        match &err {
            CoreError::Tunnel(msg) => {
                assert!(msg.contains("host key mismatch"), "{msg}");
                assert!(
                    msg.contains(&wrong),
                    "message names the expected key: {msg}"
                );
                assert!(
                    msg.contains(&sshd.host_fp()),
                    "message names the presented key: {msg}"
                );
            }
            other => panic!("expected Tunnel error, got {other:?}"),
        }
    }
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn conn_pinned_fingerprint_survives_whitespace() {
    sshd_or_return!(_g, sshd);
    let (p, _) = provider(empty_vault());
    let cfg = sshd.cfg(sshd.key_auth(), Some(format!("  {}\n", sshd.host_fp())));
    open(&p, "p", &cfg, &redis_addr())
        .await
        .expect("padded pin still matches");
}

#[tokio::test]
async fn conn_tilde_in_key_path_is_expanded() {
    sshd_or_return!(_g, sshd);
    let fake_home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(fake_home.path().join(".ssh")).unwrap();
    std::fs::copy(
        sshd.get("CLIENT_KEY"),
        fake_home.path().join(".ssh/id_test"),
    )
    .unwrap();
    let old = std::env::var_os("HOME");
    // SAFETY: tests in this file are serialised by SERIAL and nothing else here reads HOME.
    unsafe { std::env::set_var("HOME", fake_home.path()) };
    let (p, _) = provider(empty_vault());
    let result = open(
        &p,
        "p",
        &sshd.cfg(
            SshAuth::PrivateKey {
                path: "~/.ssh/id_test".into(),
            },
            None,
        ),
        &redis_addr(),
    )
    .await;
    // SAFETY: as above.
    unsafe {
        match old {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
    result.expect("~ expands to $HOME");
}

#[tokio::test]
async fn conn_encrypted_key_uses_the_passphrase_from_the_vault() {
    sshd_or_return!(_g, sshd);
    let auth = SshAuth::PrivateKey {
        path: sshd.get("CLIENT_KEY_ENCRYPTED").to_string(),
    };

    // Right passphrase.
    let vault = secret_vault(&[(
        vault_keys::ssh_passphrase("p"),
        sshd.get("CLIENT_KEY_PASSPHRASE"),
    )])
    .await;
    let (p, _) = provider(vault);
    let h = open(&p, "p", &sshd.cfg(auth.clone(), None), &redis_addr())
        .await
        .expect("decrypts and authenticates");
    assert_eq!(redis_ping(h.local_addr).await.unwrap(), "+PONG\r\n");
    drop(h);

    // No passphrase saved.
    let (p, _) = provider(empty_vault());
    match open(&p, "p", &sshd.cfg(auth.clone(), None), &redis_addr())
        .await
        .err()
        .unwrap()
    {
        CoreError::Auth(m) => assert!(m.to_lowercase().contains("passphrase"), "{m}"),
        other => panic!("{other:?}"),
    }

    // Wrong passphrase; the message must not echo it.
    let vault = secret_vault(&[(vault_keys::ssh_passphrase("p"), "definitely-wrong")]).await;
    let (p, _) = provider(vault);
    match open(&p, "p", &sshd.cfg(auth, None), &redis_addr())
        .await
        .err()
        .unwrap()
    {
        CoreError::Auth(m) => assert!(!m.contains("definitely-wrong"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn conn_missing_or_unreadable_key_file_is_an_auth_error() {
    sshd_or_return!(_g, sshd);
    let (p, _) = provider(empty_vault());
    let cfg = sshd.cfg(
        SshAuth::PrivateKey {
            path: "/nonexistent/id_ed25519".into(),
        },
        None,
    );
    let err = open(&p, "p", &cfg, &redis_addr()).await.err().unwrap();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("/nonexistent/id_ed25519")),
        "{err:?}"
    );
}

#[tokio::test]
async fn conn_password_authentication() {
    sshd_or_return!(_g, sshd);
    let pw_user = sshd.get("PASSWORD_USER").to_string();
    if pw_user.is_empty() {
        eprintln!(
            "skipping password auth: setup script could not create a password user (not root?)"
        );
        return;
    }
    let cfg = TunnelConfig::Ssh {
        host: "127.0.0.1".into(),
        port: sshd.port(),
        username: pw_user,
        auth: SshAuth::Password,
        known_host_fingerprint: None,
    };

    let vault = secret_vault(&[(vault_keys::ssh_password("p"), sshd.get("PASSWORD"))]).await;
    let (p, _) = provider(vault);
    let h = open(&p, "p", &cfg, &redis_addr())
        .await
        .expect("password auth");
    assert_eq!(redis_ping(h.local_addr).await.unwrap(), "+PONG\r\n");
    drop(h);

    let vault = secret_vault(&[(vault_keys::ssh_password("p"), "not-the-password")]).await;
    let (p, _) = provider(vault);
    let err = open(&p, "p", &cfg, &redis_addr()).await.err().unwrap();
    assert!(
        matches!(&err, CoreError::Auth(m) if !m.contains("not-the-password")),
        "{err:?}"
    );

    let (p, _) = provider(empty_vault());
    let err = open(&p, "p", &cfg, &redis_addr()).await.err().unwrap();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("no SSH password")),
        "{err:?}"
    );
}

#[tokio::test]
async fn conn_unknown_user_is_rejected_as_an_auth_error() {
    sshd_or_return!(_g, sshd);
    let (p, seen) = provider(empty_vault());
    let cfg = TunnelConfig::Ssh {
        host: "127.0.0.1".into(),
        port: sshd.port(),
        username: "no-such-user-xyz".into(),
        auth: sshd.key_auth(),
        known_host_fingerprint: None,
    };
    let err = open(&p, "p", &cfg, &redis_addr()).await.err().unwrap();
    assert!(matches!(err, CoreError::Auth(_)), "{err:?}");
    assert!(
        seen.lock().unwrap().is_empty(),
        "a host key is only pinned after authentication succeeds"
    );
}

#[tokio::test]
async fn conn_ssh_agent_authentication() {
    sshd_or_return!(_g, sshd);
    let agent_dir = tempfile::tempdir().unwrap();
    let sock: PathBuf = agent_dir.path().join("agent.sock");
    let mut agent = Command::new("ssh-agent")
        .arg("-D")
        .arg("-a")
        .arg(&sock)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("ssh-agent");
    for _ in 0..50 {
        if sock.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let old = std::env::var_os("SSH_AUTH_SOCK");

    // No identities loaded yet.
    // SAFETY: serialised by SERIAL; only the agent code reads SSH_AUTH_SOCK.
    unsafe { std::env::set_var("SSH_AUTH_SOCK", &sock) };
    let (p, _) = provider(empty_vault());
    let empty = open(&p, "p", &sshd.cfg(SshAuth::Agent, None), &redis_addr())
        .await
        .err();

    let added = Command::new("ssh-add")
        .arg(sshd.get("CLIENT_KEY"))
        .env("SSH_AUTH_SOCK", &sock)
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(added.success());
    let loaded = open(&p, "p", &sshd.cfg(SshAuth::Agent, None), &redis_addr()).await;
    let ping = match &loaded {
        Ok(h) => Some(redis_ping(h.local_addr).await),
        Err(_) => None,
    };

    // SSH_AUTH_SOCK unset.
    unsafe { std::env::remove_var("SSH_AUTH_SOCK") };
    let unset = open(&p, "p", &sshd.cfg(SshAuth::Agent, None), &redis_addr())
        .await
        .err();

    // Restore before asserting so a failure doesn't leak state into other tests.
    unsafe {
        match old {
            Some(v) => std::env::set_var("SSH_AUTH_SOCK", v),
            None => std::env::remove_var("SSH_AUTH_SOCK"),
        }
    }
    let _ = agent.kill();
    let _ = agent.wait();

    assert!(
        matches!(&empty, Some(CoreError::Auth(m)) if m.contains("no keys")),
        "{empty:?}"
    );
    assert!(loaded.is_ok(), "agent auth failed: {:?}", loaded.err());
    assert_eq!(ping.unwrap().unwrap(), "+PONG\r\n");
    assert!(
        matches!(&unset, Some(CoreError::Auth(m)) if m.contains("SSH_AUTH_SOCK")),
        "{unset:?}"
    );
}

#[tokio::test]
async fn conn_unreachable_bastion_is_a_tunnel_error() {
    let _g = SERIAL.lock().await;
    if std::env::var("VG_TEST_SSH").as_deref() != Ok("1") {
        return;
    }
    let free = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = TunnelConfig::Ssh {
        host: "127.0.0.1".into(),
        port: free,
        username: "x".into(),
        auth: SshAuth::Agent,
        known_host_fingerprint: None,
    };
    let (p, _) = provider(empty_vault());
    let err = open(&p, "p", &cfg, "127.0.0.1:6379").await.err().unwrap();
    assert!(
        matches!(&err, CoreError::Tunnel(m) if m.contains("cannot reach")),
        "{err:?}"
    );
}

#[tokio::test]
async fn conn_invalid_config_is_rejected_up_front() {
    let (p, _) = provider(empty_vault());
    let bad_user = TunnelConfig::Ssh {
        host: "h".into(),
        port: 22,
        username: " ".into(),
        auth: SshAuth::Agent,
        known_host_fingerprint: None,
    };
    assert!(matches!(
        open(&p, "p", &bad_user, "db:5432").await,
        Err(CoreError::Invalid(_))
    ));
    let bad_host = TunnelConfig::Ssh {
        host: "".into(),
        port: 22,
        username: "u".into(),
        auth: SshAuth::Agent,
        known_host_fingerprint: None,
    };
    assert!(matches!(
        open(&p, "p", &bad_host, "db:5432").await,
        Err(CoreError::Invalid(_))
    ));
    let ok = TunnelConfig::Ssh {
        host: "h".into(),
        port: 22,
        username: "u".into(),
        auth: SshAuth::Agent,
        known_host_fingerprint: None,
    };
    assert!(matches!(
        p.open("p", &ok, "", 5432).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        p.open("p", &ok, "db", 0).await,
        Err(CoreError::Invalid(_))
    ));
}

#[tokio::test]
async fn conn_moves_a_megabyte_both_ways_with_half_close_and_parallel_connections() {
    sshd_or_return!(_g, sshd);
    let echo = echo_server().await;
    let (p, _) = provider(empty_vault());
    let h = open(&p, "p", &sshd.cfg(sshd.key_auth(), None), &echo.to_string())
        .await
        .unwrap();
    let addr = h.local_addr;

    let payload: Vec<u8> = (0..1_048_576u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let payload = payload.clone();
        tasks.push(tokio::spawn(async move {
            let s = TcpStream::connect(addr).await.unwrap();
            let (mut r, mut w) = s.into_split();
            let sender = tokio::spawn({
                let payload = payload.clone();
                async move {
                    w.write_all(&payload).await.unwrap();
                    w.shutdown().await.unwrap(); // half-close: the echo server then closes its side
                }
            });
            let mut got = Vec::new();
            tokio::time::timeout(Duration::from_secs(30), r.read_to_end(&mut got))
                .await
                .expect("echo finished")
                .unwrap();
            sender.await.unwrap();
            assert_eq!(got.len(), payload.len());
            assert!(got == payload, "payload corrupted in transit");
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
}

#[tokio::test]
async fn conn_refused_target_closes_the_client_socket_instead_of_hanging() {
    sshd_or_return!(_g, sshd);
    let free = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (p, _) = provider(empty_vault());
    let h = open(
        &p,
        "p",
        &sshd.cfg(sshd.key_auth(), None),
        &format!("127.0.0.1:{free}"),
    )
    .await
    .unwrap();

    let mut s = TcpStream::connect(h.local_addr).await.unwrap();
    let mut buf = [0u8; 16];
    let read = tokio::time::timeout(Duration::from_secs(10), s.read(&mut buf))
        .await
        .expect("closed promptly");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");

    // The tunnel itself is still healthy for a good target afterwards.
    let (p2, _) = provider(empty_vault());
    let good = open(&p2, "p", &sshd.cfg(sshd.key_auth(), None), &redis_addr())
        .await
        .unwrap();
    assert_eq!(redis_ping(good.local_addr).await.unwrap(), "+PONG\r\n");
}

#[tokio::test]
async fn conn_tunnel_notices_when_the_bastion_goes_away() {
    sshd_or_return!(_g, sshd);
    let proxy = CuttableProxy::start(sshd.port()).await;
    let cfg = TunnelConfig::Ssh {
        host: "127.0.0.1".into(),
        port: proxy.addr.port(),
        username: sshd.user(),
        auth: sshd.key_auth(),
        known_host_fingerprint: Some(sshd.host_fp()),
    };
    let (p, _) = provider(empty_vault());
    let h = open(&p, "p", &cfg, &redis_addr()).await.unwrap();
    assert_eq!(redis_ping(h.local_addr).await.unwrap(), "+PONG\r\n");
    assert!(!h.cancel.is_cancelled());

    proxy.cut_connections(); // the "network" drops every connection to the bastion

    let mut dead = false;
    for _ in 0..40 {
        if h.cancel.is_cancelled() {
            dead = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        dead,
        "the handle's token must fire once the SSH session is gone, so the ConnectionManager can reconnect"
    );
    // ...and the local port stops accepting.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(TcpStream::connect(h.local_addr).await.is_err());
}
