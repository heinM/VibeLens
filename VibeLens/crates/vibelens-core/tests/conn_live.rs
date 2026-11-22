//! ConnectionManager against real servers, with the real JsonProfileStore and (optionally) the
//! real SshTunnelProvider. Each section is opt-in:
//!
//! - `VG_TEST_VALKEY=redis://127.0.0.1:6379`  Valkey/Redis tests (keys `vg_conn:*` in db 15)
//! - `VG_TEST_PG="host=… port=… user=… password=… dbname=…"`  Postgres tests
//! - `VG_TEST_SSH=1` (+ VG_TEST_VALKEY)       Valkey through a real SSH tunnel

mod services;

mod conn_common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use conn_common::{CuttableProxy, Sshd, valkey_profile};
use secrecy::SecretString;
use tempfile::TempDir;
use vibelens_core::connections::{AppToolBackend, ConnectionManager};
use vibelens_core::error::{CoreError, Result};
use vibelens_core::model::*;
use vibelens_core::ports::{
    CredentialSource, ProfileStore, ToolBackend, TunnelHandle, TunnelProvider,
};
use vibelens_core::profiles::{JsonProfileStore, trust_host_key};
use vibelens_core::tunnel::SshTunnelProvider;
use vibelens_core::vault::MemoryVault;
use vibelens_core::workspace::WorkspaceFs;

struct StaticCreds {
    username: Option<String>,
    password: Option<String>,
}

#[async_trait]
impl CredentialSource for StaticCreds {
    async fn resolve(&self, _p: &ConnectionProfile) -> Result<ResolvedCredentials> {
        Ok(ResolvedCredentials {
            username: self.username.clone(),
            password: self.password.clone().map(SecretString::from),
            ..ResolvedCredentials::anonymous()
        })
    }
}

struct NoTunnels;

#[async_trait]
impl TunnelProvider for NoTunnels {
    async fn open(&self, _: &str, _: &TunnelConfig, _: &str, _: u16) -> Result<TunnelHandle> {
        Err(CoreError::Tunnel("this test does not use tunnels".into()))
    }
}

struct Live {
    _dir: TempDir,
    profiles: Arc<JsonProfileStore>,
    mgr: Arc<ConnectionManager>,
    backend: Arc<AppToolBackend>,
}

fn live(
    creds: StaticCreds,
    tunnels: Arc<dyn TunnelProvider>,
    profiles: Option<(TempDir, Arc<JsonProfileStore>)>,
) -> Live {
    let (dir, profiles) = profiles.unwrap_or_else(|| {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JsonProfileStore::new(dir.path().to_path_buf()).unwrap());
        (dir, store)
    });
    let mgr = ConnectionManager::new(profiles.clone(), Arc::new(creds), tunnels);
    let ws = Arc::new(WorkspaceFs::new(profiles.path().parent().unwrap().join("ws")).unwrap());
    let backend = AppToolBackend::new(mgr.clone(), ws);
    Live {
        _dir: dir,
        profiles,
        mgr,
        backend,
    }
}

fn valkey_target() -> Option<(String, u16)> {
    let raw = std::env::var("VG_TEST_VALKEY")
        .ok()
        .filter(|s| !s.is_empty())?;
    let url = url::Url::parse(&raw).ok()?;
    Some((url.host_str()?.to_string(), url.port().unwrap_or(6379)))
}

fn valkey_profile_for(name: &str, host: &str, port: u16) -> ConnectionProfile {
    let mut p = valkey_profile(name, host, port);
    p.valkey.connect_timeout_secs = 5;
    p.valkey.scan_count = 5; // small pages so valkey_scan has to loop
    p
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

// ---- Valkey -------------------------------------------------------------------------------

#[tokio::test]
async fn conn_valkey_lifecycle_reuse_invalidation_and_reaping() {
    let Some((host, port)) = valkey_target() else {
        services::skip("valkey", "VG_TEST_VALKEY is not set");
        return;
    };
    let l = live(
        StaticCreds {
            username: None,
            password: None,
        },
        Arc::new(NoTunnels),
        None,
    );
    let p = l
        .profiles
        .upsert(valkey_profile_for("live-vk", &host, port))
        .unwrap();

    // connect() reports a full status.
    let st = l.mgr.connect(&p.id).await.expect("connect");
    assert_eq!(
        (st.state, st.engine, st.tunneled),
        (ConnectionState::Connected, EngineKind::Valkey, false)
    );
    assert!(st.server_version.is_some() && st.since.is_some() && st.detail.is_none());
    assert_eq!(l.mgr.status(), vec![st.clone()]);

    // Reuse.
    let a = l.mgr.valkey(&p.id).await.unwrap();
    let b = l.mgr.valkey(&p.id).await.unwrap();
    assert!(Arc::ptr_eq(&a, &b));

    // Cosmetic edits keep the handle...
    let mut renamed = l.profiles.get(&p.id).unwrap();
    renamed.name = "live-vk renamed".into();
    renamed.color = Some("#123456".into());
    l.profiles.upsert(renamed).unwrap();
    assert!(Arc::ptr_eq(&a, &l.mgr.valkey(&p.id).await.unwrap()));

    // ...but a connection-relevant edit (here: a dead port) takes effect immediately instead of
    // silently reusing the old connection, and fixing it reconnects.
    let mut moved = l.profiles.get(&p.id).unwrap();
    moved.port = 1;
    l.profiles.upsert(moved).unwrap();
    assert!(l.mgr.valkey(&p.id).await.is_err());
    let mut fixed = l.profiles.get(&p.id).unwrap();
    fixed.port = port;
    l.profiles.upsert(fixed).unwrap();
    let c = l.mgr.valkey(&p.id).await.unwrap();
    assert!(!Arc::ptr_eq(&a, &c), "a fresh connection after the edit");

    // Disconnect then 8 concurrent callers share one handle.
    l.mgr.disconnect(&p.id).await.unwrap();
    assert!(l.mgr.status().is_empty());
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let mgr = l.mgr.clone();
        let id = p.id.clone();
        tasks.push(tokio::spawn(async move { mgr.valkey(&id).await.unwrap() }));
    }
    let mut handles = Vec::new();
    for t in tasks {
        handles.push(t.await.unwrap());
    }
    assert!(
        handles.windows(2).all(|w| Arc::ptr_eq(&w[0], &w[1])),
        "single-flight: one handle for all callers"
    );

    // Idle reaping: 1s limit, wait it out.
    let mut quick = l.profiles.get(&p.id).unwrap();
    quick.valkey.idle_disconnect_secs = 1;
    l.profiles.upsert(quick).unwrap();
    assert_eq!(l.mgr.reap_idle().await, 0, "not idle yet");
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert_eq!(l.mgr.reap_idle().await, 1, "idle past the limit");
    assert!(l.mgr.status().is_empty());
    // And it comes back on demand.
    l.mgr.valkey(&p.id).await.unwrap();
    assert_eq!(l.mgr.status().len(), 1);

    // A deleted profile's connection is reaped too.
    l.profiles.delete(&p.id).unwrap();
    assert_eq!(l.mgr.reap_idle().await, 1);
    assert!(l.mgr.status().is_empty());
}

#[tokio::test]
async fn conn_valkey_tool_backend_scan_get_and_write_detection() {
    let Some((host, port)) = valkey_target() else {
        services::skip("valkey", "VG_TEST_VALKEY is not set");
        return;
    };
    let l = live(
        StaticCreds {
            username: None,
            password: None,
        },
        Arc::new(NoTunnels),
        None,
    );
    let p = l
        .profiles
        .upsert(valkey_profile_for("live-vk-tools", &host, port))
        .unwrap();
    let be = &l.backend;
    const DB: u8 = 15;

    for i in 0..30 {
        let out = be
            .valkey_command(
                &p.id,
                DB,
                argv(&["SET", &format!("vg_conn:k{i:02}"), &format!("v{i}")]),
                true,
            )
            .await
            .unwrap();
        assert!(matches!(out, CommandOutcome::Ok { .. }));
    }

    // Limit smaller than the keyspace: loops over several 5-key pages and trims to the limit.
    let some = be.valkey_scan(&p.id, DB, "vg_conn:*", 12).await.unwrap();
    assert_eq!(some.len(), 12);
    assert!(some.iter().all(|k| k.key.starts_with("vg_conn:")));
    // Limit larger than the keyspace: walks to the end of the cursor.
    let all = be.valkey_scan(&p.id, DB, "vg_conn:*", 1000).await.unwrap();
    assert_eq!(all.len(), 30);
    assert!(
        be.valkey_scan(&p.id, DB, "vg_conn:*", 0)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        be.valkey_scan(&p.id, DB, "vg_conn_nothing:*", 10)
            .await
            .unwrap()
            .is_empty()
    );

    let v = be.valkey_get(&p.id, DB, "vg_conn:k07").await.unwrap();
    assert_eq!(v.key_type, KeyType::String);
    assert_eq!(v.value, serde_json::json!("v7"));

    assert!(
        be.valkey_is_write(&p.id, &argv(&["SET", "k", "v"]))
            .await
            .unwrap()
    );
    assert!(
        !be.valkey_is_write(&p.id, &argv(&["GET", "k"]))
            .await
            .unwrap()
    );

    // Without allow_write a write is not executed.
    let blocked = be
        .valkey_command(
            &p.id,
            DB,
            argv(&["SET", "vg_conn:k00", "overwritten"]),
            false,
        )
        .await
        .unwrap();
    assert!(
        matches!(blocked, CommandOutcome::RequiresConfirmation { .. }),
        "{blocked:?}"
    );
    assert_eq!(
        be.valkey_get(&p.id, DB, "vg_conn:k00").await.unwrap().value,
        serde_json::json!("v0")
    );

    // Clean up.
    for i in 0..30 {
        be.valkey_command(&p.id, DB, argv(&["DEL", &format!("vg_conn:k{i:02}")]), true)
            .await
            .unwrap();
    }
    assert!(
        be.valkey_scan(&p.id, DB, "vg_conn:*", 100)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn conn_valkey_test_connection_reports_version_and_failures() {
    let Some((host, port)) = valkey_target() else {
        services::skip("valkey", "VG_TEST_VALKEY is not set");
        return;
    };
    let l = live(
        StaticCreds {
            username: None,
            password: None,
        },
        Arc::new(NoTunnels),
        None,
    );
    let ok = l
        .mgr
        .test(&valkey_profile_for("t", &host, port), None)
        .await
        .unwrap();
    assert!(ok.ok, "{}", ok.message);
    assert!(ok.server_version.is_some());
    let bad = l
        .mgr
        .test(&valkey_profile_for("t", &host, 1), None)
        .await
        .unwrap();
    assert!(!bad.ok && !bad.message.is_empty());
    assert!(l.mgr.status().is_empty());
}

// ---- Postgres -----------------------------------------------------------------------------

struct Dsn {
    host: String,
    port: u16,
    user: String,
    password: String,
    dbname: String,
}

fn pg_dsn() -> Option<Dsn> {
    let raw = std::env::var("VG_TEST_PG").ok().filter(|s| !s.is_empty())?;
    let mut d = Dsn {
        host: "127.0.0.1".into(),
        port: 5432,
        user: "postgres".into(),
        password: String::new(),
        dbname: "postgres".into(),
    };
    for kv in raw.split_whitespace() {
        let (k, v) = kv.split_once('=')?;
        match k {
            "host" => d.host = v.into(),
            "port" => d.port = v.parse().ok()?,
            "user" => d.user = v.into(),
            "password" => d.password = v.into(),
            "dbname" => d.dbname = v.into(),
            _ => {}
        }
    }
    Some(d)
}

fn pg_profile_for(d: &Dsn) -> ConnectionProfile {
    let mut p = conn_common::pg_profile("live-pg");
    p.host = d.host.clone();
    p.port = d.port;
    p.database = Some(d.dbname.clone());
    p.auth = AuthMethod::Password {
        username: d.user.clone(),
    };
    // The CI Postgres service uses a self-signed certificate: encrypt, don't verify.
    p.tls = TlsConfig {
        mode: TlsMode::Require,
        ca_cert_path: None,
        use_rds_bundle: false,
    };
    p.pg.connect_timeout_secs = 5;
    p
}

#[tokio::test]
async fn conn_postgres_connect_reuse_databases_and_tool_backend() {
    let Some(d) = pg_dsn() else {
        services::skip("pg", "VG_TEST_PG is not set");
        return;
    };
    let l = live(
        StaticCreds {
            username: Some(d.user.clone()),
            password: Some(d.password.clone()),
        },
        Arc::new(NoTunnels),
        None,
    );
    let p = l.profiles.upsert(pg_profile_for(&d)).unwrap();

    let st = l.mgr.connect(&p.id).await.expect("connect");
    assert_eq!(
        (st.state, st.engine),
        (ConnectionState::Connected, EngineKind::Postgres)
    );
    assert!(
        st.server_version
            .as_deref()
            .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit())),
        "{:?}",
        st.server_version
    );

    let default = l.mgr.pg(&p.id, None).await.unwrap();
    assert_eq!(default.database(), d.dbname);
    assert!(
        Arc::ptr_eq(&default, &l.mgr.pg(&p.id, Some(&d.dbname)).await.unwrap()),
        "explicit == default database"
    );
    let other = l.mgr.pg(&p.id, Some("postgres")).await.unwrap();
    if d.dbname != "postgres" {
        assert!(!Arc::ptr_eq(&default, &other));
        assert_eq!(other.database(), "postgres");
    }
    assert_eq!(
        l.mgr.status().len(),
        1,
        "one status per profile, however many databases"
    );

    let r = l
        .backend
        .execute_sql(
            &p.id,
            None,
            "select 1 as one, 'x'::text as t",
            true,
            100,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.row_count, 1);
    assert_eq!(r.columns.len(), 2);
    assert!(!r.truncated);

    let capped = l
        .backend
        .execute_sql(
            &p.id,
            None,
            "select * from generate_series(1, 50)",
            true,
            10,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!((capped.rows.len(), capped.truncated), (10, true));

    // read_only really is read only.
    let denied = l
        .backend
        .execute_sql(
            &p.id,
            None,
            "create table vg_conn_must_not_exist(i int)",
            true,
            10,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(
        denied.is_err(),
        "DDL inside a read-only transaction must fail"
    );

    let meta = l
        .backend
        .schema_metadata(&p.id, None, None, 5)
        .await
        .unwrap();
    assert_eq!(meta.database, d.dbname);

    // The agent's list_databases: names of databases that accept connections, no templates.
    let names = l.backend.list_databases(&p.id).await.unwrap();
    assert!(names.contains(&d.dbname), "{names:?}");
    assert!(names.contains(&"postgres".to_string()), "{names:?}");
    assert!(
        !names.iter().any(|n| n.starts_with("template")),
        "{names:?}"
    );

    l.mgr.disconnect(&p.id).await.unwrap();
    assert!(l.mgr.status().is_empty());
}

#[tokio::test]
async fn conn_postgres_test_connection_with_password_override() {
    let Some(d) = pg_dsn() else {
        services::skip("pg", "VG_TEST_PG is not set");
        return;
    };
    // The credential source would hand out a WRONG password; only the override can succeed.
    let l = live(
        StaticCreds {
            username: Some(d.user.clone()),
            password: Some("not-it".into()),
        },
        Arc::new(NoTunnels),
        None,
    );
    let p = pg_profile_for(&d);

    let good = l
        .mgr
        .test(&p, Some(SecretString::from(d.password.clone())))
        .await
        .unwrap();
    assert!(good.ok, "{}", good.message);
    assert!(
        good.server_version
            .as_deref()
            .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
    );

    let bad = l
        .mgr
        .test(
            &p,
            Some(SecretString::from("typed-wrong-password".to_string())),
        )
        .await
        .unwrap();
    assert!(!bad.ok);
    assert!(
        !bad.message.contains("typed-wrong-password"),
        "{}",
        bad.message
    );

    let no_override = l.mgr.test(&p, None).await.unwrap();
    assert!(
        !no_override.ok,
        "falls back to the (wrong) resolved password"
    );
    assert!(l.mgr.status().is_empty());
}

// ---- Valkey through a real SSH tunnel -----------------------------------------------------

fn ssh_profile(
    sshd: &Sshd,
    ssh_port: u16,
    host: &str,
    port: u16,
    pinned: Option<String>,
) -> ConnectionProfile {
    let mut p = valkey_profile_for("live-vk-ssh", host, port);
    p.tunnel = Some(TunnelConfig::Ssh {
        host: "127.0.0.1".into(),
        port: ssh_port,
        username: sshd.user(),
        auth: SshAuth::PrivateKey {
            path: sshd.get("CLIENT_KEY").to_string(),
        },
        known_host_fingerprint: pinned,
    });
    p
}

/// The sshd-backed tests share one port, so they take turns.
static SSHD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The pin stored for a saved profile's SSH tunnel.
fn stored_pin(store: &JsonProfileStore, id: &str) -> Option<String> {
    match store.get(id).unwrap().tunnel {
        Some(TunnelConfig::Ssh {
            known_host_fingerprint,
            ..
        }) => known_host_fingerprint,
        None => panic!("profile has no tunnel"),
    }
}

/// Review finding M2. Pressing Test (while editing, the draft carries the saved profile's id)
/// must leave the saved profile's pinned host key exactly as it was.
#[tokio::test]
async fn conn_test_never_writes_a_host_key_to_the_saved_profile() {
    let Some((host, port)) = valkey_target() else {
        services::skip("valkey", "VG_TEST_VALKEY is not set");
        return;
    };
    let _one_sshd = SSHD.lock().await;
    let Some(sshd) = Sshd::start() else { return };

    // The wiring src-tauri/state.rs uses.
    let l = live(
        StaticCreds {
            username: None,
            password: None,
        },
        SshTunnelProvider::new(Arc::new(MemoryVault::new())),
        None,
    );

    // Saved without a pin; the form's draft is the same profile.
    let saved = l
        .profiles
        .upsert(ssh_profile(&sshd, sshd.port(), &host, port, None))
        .unwrap();
    let r = l.mgr.test(&saved, None).await.unwrap();
    assert_eq!(
        stored_pin(&l.profiles, &saved.id),
        None,
        "Test pinned a host key on the saved profile"
    );
    // It reports the key the server presented instead, for the user to check.
    assert!(!r.ok && r.server_version.is_none(), "{r:?}");
    assert_eq!(
        r.host_key,
        Some(PresentedHostKey {
            host: "127.0.0.1".into(),
            port: sshd.port(),
            fingerprint: sshd.host_fp(),
        })
    );
    assert!(r.message.contains(&sshd.host_fp()), "{}", r.message);

    // With that key on the draft (the wizard's "trust"), the test goes through, and still
    // writes nothing.
    let mut trusted = saved.clone();
    if let Some(TunnelConfig::Ssh {
        known_host_fingerprint,
        ..
    }) = trusted.tunnel.as_mut()
    {
        *known_host_fingerprint = r.host_key.map(|k| k.fingerprint);
    }
    let r = l.mgr.test(&trusted, None).await.unwrap();
    assert!(r.ok && r.host_key.is_none(), "{r:?}");
    assert_eq!(stored_pin(&l.profiles, &saved.id), None);

    // Saved with a pin; the user pressed Forget (not yet saved) and then Test.
    let mut pinned = saved.clone();
    if let Some(TunnelConfig::Ssh {
        known_host_fingerprint,
        ..
    }) = pinned.tunnel.as_mut()
    {
        *known_host_fingerprint = Some(sshd.other_fp());
    }
    let pinned = l.profiles.upsert(pinned).unwrap();
    let r = l.mgr.test(&saved, None).await.unwrap();
    assert!(!r.ok && r.host_key.is_some(), "{r:?}");
    assert_eq!(
        stored_pin(&l.profiles, &pinned.id),
        Some(sshd.other_fp()),
        "Test replaced the saved pin before Save"
    );
    assert!(
        l.mgr.status().is_empty(),
        "Test leaves no connection behind"
    );
}

#[tokio::test]
async fn conn_valkey_over_a_real_ssh_tunnel_with_explicit_trust_pinning_and_recovery() {
    let Some((host, port)) = valkey_target() else {
        services::skip("valkey", "VG_TEST_VALKEY is not set");
        return;
    };
    let _one_sshd = SSHD.lock().await;
    let Some(sshd) = Sshd::start() else { return };

    // The wiring src-tauri/state.rs uses: the tunnel provider can read secrets and nothing else.
    let l = live(
        StaticCreds {
            username: None,
            password: None,
        },
        SshTunnelProvider::new(Arc::new(MemoryVault::new())),
        None,
    );

    // Bastion reached through a proxy we can "cut" later. The redis host is the *bastion-side* address.
    let proxy = CuttableProxy::start(sshd.port()).await;
    let p = l
        .profiles
        .upsert(ssh_profile(&sshd, proxy.addr.port(), &host, port, None))
        .unwrap();

    // No pin yet: connecting stops with the key the server presented, pins nothing, and the
    // agent's tools (which connect lazily) fail the same way.
    let err = l.mgr.connect(&p.id).await.expect_err("unknown host key");
    let presented = match &err {
        CoreError::UnknownHostKey(key) => key.clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(
        presented,
        PresentedHostKey {
            host: "127.0.0.1".into(),
            port: proxy.addr.port(),
            fingerprint: sshd.host_fp(),
        }
    );
    let status = l.mgr.status().into_iter().next().unwrap();
    assert_eq!(status.state, ConnectionState::Error);
    assert!(status.detail.unwrap().contains(&sshd.host_fp()));
    let err = l
        .backend
        .valkey_command(&p.id, 0, argv(&["PING"]), false)
        .await
        .expect_err("the agent cannot connect either");
    assert_eq!(err.code(), "unknown_host_key");
    assert_eq!(
        stored_pin(&l.profiles, &p.id),
        None,
        "connecting never pins"
    );

    // The user's explicit trust is the only thing that pins.
    trust_host_key(
        &*l.profiles,
        &p.id,
        &presented.host,
        presented.port,
        &presented.fingerprint,
    )
    .unwrap();
    assert_eq!(stored_pin(&l.profiles, &p.id), Some(sshd.host_fp()));

    let st = l.mgr.connect(&p.id).await.expect("connect over ssh");
    assert!(
        st.tunneled && st.state == ConnectionState::Connected,
        "{st:?}"
    );

    // Traffic really flows through the tunnel.
    let first = l.mgr.valkey(&p.id).await.unwrap();
    assert!(Arc::ptr_eq(&first, &l.mgr.valkey(&p.id).await.unwrap()));
    let pong = l
        .backend
        .valkey_command(&p.id, 0, argv(&["PING"]), false)
        .await
        .unwrap();
    assert!(matches!(pong, CommandOutcome::Ok { .. }), "{pong:?}");

    // Network blip: the SSH session dies, the tunnel handle notices, the next use reconnects
    // against the pinned key.
    proxy.cut_connections();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let second = l
        .mgr
        .valkey(&p.id)
        .await
        .expect("reconnects after the tunnel died");
    assert!(!Arc::ptr_eq(&first, &second), "stale handle was replaced");
    let pong = l
        .backend
        .valkey_command(&p.id, 0, argv(&["PING"]), false)
        .await
        .unwrap();
    assert!(matches!(pong, CommandOutcome::Ok { .. }));

    // A changed host key is refused and surfaced as the profile's error status.
    l.mgr.disconnect(&p.id).await.unwrap();
    let mut tampered = l.profiles.get(&p.id).unwrap();
    if let Some(TunnelConfig::Ssh {
        known_host_fingerprint,
        ..
    }) = tampered.tunnel.as_mut()
    {
        *known_host_fingerprint = Some(sshd.other_fp());
    }
    l.profiles.upsert(tampered).unwrap();
    let err = l.mgr.connect(&p.id).await.expect_err("host key mismatch");
    assert!(
        matches!(&err, CoreError::Tunnel(m) if m.contains("host key mismatch")),
        "{err:?}"
    );
    let status = l.mgr.status().into_iter().next().unwrap();
    assert_eq!(status.state, ConnectionState::Error);
    assert!(status.tunneled);
    assert!(status.detail.unwrap().contains("host key mismatch"));

    // test() through the tunnel works too and leaves nothing behind.
    l.mgr.disconnect(&p.id).await.unwrap();
    let mut good = l.profiles.get(&p.id).unwrap();
    if let Some(TunnelConfig::Ssh {
        known_host_fingerprint,
        ..
    }) = good.tunnel.as_mut()
    {
        *known_host_fingerprint = Some(sshd.host_fp());
    }
    let r = l.mgr.test(&good, None).await.unwrap();
    assert!(r.ok, "{}", r.message);
    assert!(l.mgr.status().is_empty());
}

#[tokio::test]
async fn conn_postgres_idle_reaper_releases_sessions_but_stays_connected() {
    let Some(d) = pg_dsn() else {
        services::skip("pg", "VG_TEST_PG is not set");
        return;
    };
    let l = live(
        StaticCreds {
            username: Some(d.user.clone()),
            password: Some(d.password.clone()),
        },
        Arc::new(NoTunnels),
        None,
    );
    let mut p = pg_profile_for(&d);
    p.pg.idle_disconnect_secs = 1;
    let p = l.profiles.upsert(p).unwrap();
    l.mgr.connect(&p.id).await.expect("connect");
    let h = l.mgr.pg(&p.id, None).await.expect("pg");
    assert!(h.open_sessions() >= 1);

    assert_eq!(l.mgr.reap_idle().await, 0, "not idle yet");
    tokio::time::sleep(Duration::from_millis(2300)).await;
    assert!(l.mgr.reap_idle().await >= 1, "idle sessions are released");
    assert_eq!(h.open_sessions(), 0);

    // Still connected (the UI must not see a disconnect), and says why it is quiet.
    let st = l.mgr.status();
    assert_eq!(st.len(), 1);
    assert_eq!(st[0].state, ConnectionState::Connected);
    assert!(
        st[0].detail.as_deref().is_some_and(|d| d.contains("idle")),
        "{:?}",
        st[0].detail
    );

    // The same handle serves the next query on a fresh session.
    let again = l.mgr.pg(&p.id, None).await.expect("pg again");
    assert!(Arc::ptr_eq(&h, &again), "the handle survives the reaper");
    let v = again.server_version().await.expect("version");
    assert!(!v.is_empty());
    let r = again
        .execute_collect("SELECT 41 + 1", ExecOptions::default())
        .await
        .expect("query after release");
    assert_eq!(r.rows[0][0], serde_json::json!(42));
    assert!(again.open_sessions() >= 1);
    assert!(l.mgr.status()[0].detail.is_none());

    // A deleted profile's handle is still closed for good.
    l.profiles.delete(&p.id).unwrap();
    assert_eq!(l.mgr.reap_idle().await, 1);
    assert!(l.mgr.status().is_empty());
}
