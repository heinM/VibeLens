//! ConnectionManager + AppToolBackend against fakes (credential source, tunnel provider,
//! in-memory profile store). Everything asserted here is independent of a real database:
//! connections point at a closed local port so engine connects fail fast, which is exactly
//! what lets us check status transitions, tunnel lifecycle and error mapping.

mod conn_common;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use conn_common::{pg_profile, valkey_profile};
use secrecy::SecretString;
use tempfile::tempdir;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use vibelens_core::connections::{AppToolBackend, ConnectionManager};
use vibelens_core::error::{CoreError, Result};
use vibelens_core::model::*;
use vibelens_core::ports::{
    CredentialSource, ProfileStore, ToolBackend, TunnelHandle, TunnelProvider,
};
use vibelens_core::workspace::WorkspaceFs;

// ---- fakes --------------------------------------------------------------------------------

#[derive(Default)]
struct MemProfiles {
    inner: Mutex<Vec<ConnectionProfile>>,
}

impl ProfileStore for MemProfiles {
    fn list(&self) -> Result<Vec<ConnectionProfile>> {
        Ok(self.inner.lock().unwrap().clone())
    }
    fn get(&self, id: &str) -> Result<ConnectionProfile> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("profile {id}")))
    }
    fn upsert(&self, mut p: ConnectionProfile) -> Result<ConnectionProfile> {
        let mut all = self.inner.lock().unwrap();
        if p.id.is_empty() {
            p.id = format!("id-{}", all.len() + 1);
        }
        match all.iter_mut().find(|x| x.id == p.id) {
            Some(slot) => *slot = p.clone(),
            None => all.push(p.clone()),
        }
        Ok(p)
    }
    fn delete(&self, id: &str) -> Result<()> {
        self.inner.lock().unwrap().retain(|p| p.id != id);
        Ok(())
    }
}

#[derive(Default)]
struct FakeCreds {
    resolves: AtomicUsize,
}

#[async_trait]
impl CredentialSource for FakeCreds {
    async fn resolve(&self, _profile: &ConnectionProfile) -> Result<ResolvedCredentials> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        Ok(ResolvedCredentials {
            username: Some("u".into()),
            password: Some(SecretString::from("pw".to_string())),
            ..ResolvedCredentials::anonymous()
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenCall {
    profile_id: String,
    target_host: String,
    target_port: u16,
}

struct FakeTunnels {
    calls: Mutex<Vec<OpenCall>>,
    tokens: Mutex<Vec<CancellationToken>>,
    fail_with: Mutex<Option<String>>,
    /// When set, `open` blocks until a permit is available.
    gate: Option<Arc<Semaphore>>,
    entered: Notify,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    local: SocketAddr,
}

impl FakeTunnels {
    fn new(local_port: u16) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::default(),
            tokens: Mutex::default(),
            fail_with: Mutex::default(),
            gate: None,
            entered: Notify::new(),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            local: SocketAddr::from((Ipv4Addr::LOCALHOST, local_port)),
        })
    }
    fn gated(local_port: u16) -> (Arc<Self>, Arc<Semaphore>) {
        let gate = Arc::new(Semaphore::new(0));
        let t = Arc::new(Self {
            calls: Mutex::default(),
            tokens: Mutex::default(),
            fail_with: Mutex::default(),
            gate: Some(gate.clone()),
            entered: Notify::new(),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            local: SocketAddr::from((Ipv4Addr::LOCALHOST, local_port)),
        });
        (t, gate)
    }
    fn calls(&self) -> Vec<OpenCall> {
        self.calls.lock().unwrap().clone()
    }
    fn tokens(&self) -> Vec<CancellationToken> {
        self.tokens.lock().unwrap().clone()
    }
}

#[async_trait]
impl TunnelProvider for FakeTunnels {
    async fn open(
        &self,
        profile_id: &str,
        _cfg: &TunnelConfig,
        target_host: &str,
        target_port: u16,
    ) -> Result<TunnelHandle> {
        self.calls.lock().unwrap().push(OpenCall {
            profile_id: profile_id.into(),
            target_host: target_host.into(),
            target_port,
        });
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.entered.notify_one();
        struct Leave<'a>(&'a AtomicUsize);
        impl Drop for Leave<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let _leave = Leave(&self.in_flight);
        if let Some(gate) = &self.gate {
            gate.acquire().await.unwrap().forget();
        }
        if let Some(msg) = self.fail_with.lock().unwrap().clone() {
            return Err(CoreError::Tunnel(msg));
        }
        let cancel = CancellationToken::new();
        self.tokens.lock().unwrap().push(cancel.clone());
        Ok(TunnelHandle {
            local_addr: self.local,
            cancel,
        })
    }
}

// ---- harness ------------------------------------------------------------------------------

/// A local port with nothing listening on it.
fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    l.local_addr().unwrap().port()
}

struct Harness {
    profiles: Arc<MemProfiles>,
    creds: Arc<FakeCreds>,
    tunnels: Arc<FakeTunnels>,
    mgr: Arc<ConnectionManager>,
}

fn harness_with(tunnels: Arc<FakeTunnels>) -> Harness {
    let profiles = Arc::new(MemProfiles::default());
    let creds = Arc::new(FakeCreds::default());
    let mgr = ConnectionManager::new(profiles.clone(), creds.clone(), tunnels.clone());
    Harness {
        profiles,
        creds,
        tunnels,
        mgr,
    }
}

fn harness() -> Harness {
    harness_with(FakeTunnels::new(closed_port()))
}

fn ssh_tunnel() -> TunnelConfig {
    TunnelConfig::Ssh {
        host: "bastion.example".into(),
        port: 22,
        username: "ops".into(),
        auth: SshAuth::Agent,
        known_host_fingerprint: None,
    }
}

/// A Postgres profile whose address refuses connections, with short timeouts.
fn dead_pg(name: &str) -> ConnectionProfile {
    let mut p = pg_profile(name);
    p.host = "127.0.0.1".into();
    p.port = closed_port();
    p.tls = TlsConfig {
        mode: TlsMode::Disable,
        ca_cert_path: None,
        use_rds_bundle: false,
    };
    p.pg.connect_timeout_secs = 2;
    p
}

fn dead_valkey(name: &str) -> ConnectionProfile {
    let mut p = valkey_profile(name, "127.0.0.1", closed_port());
    p.valkey.connect_timeout_secs = 2;
    p
}

fn status_of(h: &Harness, id: &str) -> Option<ConnectionStatus> {
    h.mgr.status().into_iter().find(|s| s.profile_id == id)
}

// ---- engine mismatch & lookup -------------------------------------------------------------

#[tokio::test]
async fn conn_engine_mismatch_is_invalid_and_never_dials() {
    let h = harness();
    let pg = h.profiles.upsert(dead_pg("pg")).unwrap();
    let vk = h.profiles.upsert(dead_valkey("vk")).unwrap();

    assert!(matches!(
        h.mgr.pg(&vk.id, None).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        h.mgr.valkey(&pg.id).await,
        Err(CoreError::Invalid(_))
    ));

    // Same through the agent's tool backend.
    let ws = tempdir().unwrap();
    let backend = AppToolBackend::new(
        h.mgr.clone(),
        Arc::new(WorkspaceFs::new(ws.path().join("w")).unwrap()),
    );
    assert!(matches!(
        backend
            .execute_sql(&vk.id, None, "select 1", true, 10)
            .await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        backend.schema_metadata(&vk.id, None, None, 10).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        backend.valkey_scan(&pg.id, 0, "*", 10).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        backend.valkey_get(&pg.id, 0, "k").await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        backend
            .valkey_is_write(&pg.id, &["GET".into(), "k".into()])
            .await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        backend
            .valkey_command(&pg.id, 0, vec!["PING".into()], false)
            .await,
        Err(CoreError::Invalid(_))
    ));

    assert!(h.tunnels.calls().is_empty());
    assert_eq!(h.creds.resolves.load(Ordering::SeqCst), 0);
    // A mismatch is a caller bug, not a connection failure: it must not mark the profile as errored.
    assert!(h.mgr.status().is_empty(), "{:?}", h.mgr.status());
}

#[tokio::test]
async fn conn_unknown_profile_is_not_found() {
    let h = harness();
    assert!(matches!(
        h.mgr.pg("ghost", None).await,
        Err(CoreError::NotFound(_))
    ));
    assert!(matches!(
        h.mgr.valkey("ghost").await,
        Err(CoreError::NotFound(_))
    ));
    assert!(matches!(
        h.mgr.connect("ghost").await,
        Err(CoreError::NotFound(_))
    ));
    // Disconnect is idempotent and fine for profiles that no longer exist.
    h.mgr.disconnect("ghost").await.unwrap();
    assert!(h.mgr.status().is_empty());
}

// ---- status transitions -------------------------------------------------------------------

#[tokio::test]
async fn conn_failed_connect_records_error_status_with_detail_then_disconnect_clears_it() {
    let h = harness();
    let p = h.profiles.upsert(dead_pg("pg")).unwrap();
    assert!(
        h.mgr.status().is_empty(),
        "nothing is tracked before first use"
    );

    let err = h
        .mgr
        .connect(&p.id)
        .await
        .expect_err("nothing is listening");
    let s = status_of(&h, &p.id).expect("failure is tracked");
    assert_eq!(s.state, ConnectionState::Error);
    assert_eq!(s.engine, EngineKind::Postgres);
    assert!(!s.tunneled);
    assert!(s.since.is_some());
    assert!(s.server_version.is_none());
    let detail = s.detail.expect("error detail");
    assert!(!detail.is_empty());
    assert_eq!(
        detail,
        err.to_string(),
        "status detail is the error the caller saw"
    );

    // A retry that fails again keeps exactly one record.
    let _ = h.mgr.pg(&p.id, None).await;
    assert_eq!(h.mgr.status().len(), 1);

    h.mgr.disconnect(&p.id).await.unwrap();
    assert!(h.mgr.status().is_empty());
}

#[tokio::test]
async fn conn_failed_valkey_connect_records_error_status() {
    let h = harness();
    let p = h.profiles.upsert(dead_valkey("vk")).unwrap();
    assert!(h.mgr.connect(&p.id).await.is_err());
    let s = status_of(&h, &p.id).unwrap();
    assert_eq!(
        (s.state, s.engine),
        (ConnectionState::Error, EngineKind::Valkey)
    );
    assert!(s.detail.is_some());
}

#[tokio::test]
async fn connecting_state_is_visible_while_the_tunnel_is_opening() {
    let (tunnels, gate) = FakeTunnels::gated(closed_port());
    let h = harness_with(tunnels);
    let mut p = dead_pg("pg");
    p.tunnel = Some(ssh_tunnel());
    let p = h.profiles.upsert(p).unwrap();

    let mgr = h.mgr.clone();
    let id = p.id.clone();
    let task = tokio::spawn(async move { mgr.connect(&id).await });

    tokio::time::timeout(Duration::from_secs(5), h.tunnels.entered.notified())
        .await
        .expect("tunnel open started");
    let s = status_of(&h, &p.id).expect("status while connecting");
    assert_eq!(s.state, ConnectionState::Connecting);
    assert!(s.tunneled);
    assert!(s.detail.is_none());

    gate.add_permits(1);
    let result = task.await.unwrap();
    assert!(result.is_err(), "the dead database refuses");
    assert_eq!(status_of(&h, &p.id).unwrap().state, ConnectionState::Error);
}

#[tokio::test]
async fn conn_dropping_a_pending_connect_does_not_leave_connecting_behind() {
    let (tunnels, _gate) = FakeTunnels::gated(closed_port()); // never released
    let h = harness_with(tunnels);
    let mut p = dead_pg("pg");
    p.tunnel = Some(ssh_tunnel());
    let p = h.profiles.upsert(p).unwrap();

    let mgr = h.mgr.clone();
    let id = p.id.clone();
    let task = tokio::spawn(async move { mgr.connect(&id).await });
    tokio::time::timeout(Duration::from_secs(5), h.tunnels.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        status_of(&h, &p.id).unwrap().state,
        ConnectionState::Connecting
    );

    task.abort();
    let _ = task.await;
    assert!(h.mgr.status().is_empty(), "{:?}", h.mgr.status());
    // The per-key lock was released with the future, so a new attempt is not wedged.
    let again = tokio::time::timeout(Duration::from_millis(200), h.mgr.pg(&p.id, None)).await;
    assert!(
        again.is_err(),
        "second attempt is running (blocked on the gate), not deadlocked on the lock"
    );
    assert_eq!(h.tunnels.calls().len(), 2);
}

#[tokio::test]
async fn conn_disconnect_during_connect_clears_the_connecting_marker() {
    let (tunnels, gate) = FakeTunnels::gated(closed_port());
    let h = harness_with(tunnels);
    let mut p = dead_pg("pg");
    p.tunnel = Some(ssh_tunnel());
    let p = h.profiles.upsert(p).unwrap();

    let mgr = h.mgr.clone();
    let id = p.id.clone();
    let task = tokio::spawn(async move { mgr.connect(&id).await });
    tokio::time::timeout(Duration::from_secs(5), h.tunnels.entered.notified())
        .await
        .unwrap();

    h.mgr.disconnect(&p.id).await.unwrap();
    assert!(h.mgr.status().is_empty());
    gate.add_permits(1);
    let _ = task.await.unwrap();
}

// ---- tunnels ------------------------------------------------------------------------------

#[tokio::test]
async fn conn_tunnel_is_opened_to_the_real_endpoint_and_closed_when_the_engine_fails() {
    let h = harness();
    let mut p = dead_pg("pg");
    p.host = "mydb.cluster-xyz.eu-west-1.rds.amazonaws.com".into();
    p.port = 5432;
    p.tunnel = Some(ssh_tunnel());
    let p = h.profiles.upsert(p).unwrap();

    let err = h
        .mgr
        .connect(&p.id)
        .await
        .expect_err("the fake tunnel points at a closed port");
    assert_ne!(err.code(), "tunnel", "tunnel itself opened fine: {err}");

    assert_eq!(
        h.tunnels.calls(),
        vec![OpenCall {
            profile_id: p.id.clone(),
            target_host: "mydb.cluster-xyz.eu-west-1.rds.amazonaws.com".into(),
            target_port: 5432
        }]
    );
    let tokens = h.tunnels.tokens();
    assert_eq!(tokens.len(), 1);
    assert!(
        tokens[0].is_cancelled(),
        "a tunnel whose engine failed must be torn down"
    );
    assert!(status_of(&h, &p.id).unwrap().tunneled);
}

#[tokio::test]
async fn conn_tunnel_failure_surfaces_as_a_tunnel_error_and_error_status() {
    let h = harness();
    *h.tunnels.fail_with.lock().unwrap() = Some("host key mismatch for bastion:22".into());
    let mut p = dead_valkey("vk");
    p.tunnel = Some(ssh_tunnel());
    let p = h.profiles.upsert(p).unwrap();

    let err = h.mgr.valkey(&p.id).await.err().unwrap();
    assert!(matches!(err, CoreError::Tunnel(_)), "{err:?}");
    let s = status_of(&h, &p.id).unwrap();
    assert_eq!(s.state, ConnectionState::Error);
    assert!(s.detail.unwrap().contains("host key mismatch"));
}

#[tokio::test]
async fn conn_untunnelled_profiles_never_touch_the_tunnel_provider() {
    let h = harness();
    let p = h.profiles.upsert(dead_pg("pg")).unwrap();
    let _ = h.mgr.connect(&p.id).await;
    assert!(h.tunnels.calls().is_empty());
}

#[tokio::test]
async fn conn_concurrent_callers_never_dial_the_same_key_in_parallel() {
    let (tunnels, gate) = FakeTunnels::gated(closed_port());
    let h = harness_with(tunnels);
    let mut p = dead_pg("pg");
    p.tunnel = Some(ssh_tunnel());
    let p = h.profiles.upsert(p).unwrap();

    let mut tasks = Vec::new();
    for _ in 0..4 {
        let mgr = h.mgr.clone();
        let id = p.id.clone();
        tasks.push(tokio::spawn(async move {
            mgr.pg(&id, Some("app")).await.map(|_| ())
        }));
    }
    // Let the first one in and the others pile up behind the per-key lock.
    tokio::time::timeout(Duration::from_secs(5), h.tunnels.entered.notified())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        h.tunnels.calls().len(),
        1,
        "followers wait for the leader instead of dialling"
    );
    gate.add_permits(16);
    for t in tasks {
        assert!(t.await.unwrap().is_err());
    }
    assert_eq!(h.tunnels.max_in_flight.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn conn_different_databases_connect_independently() {
    let (tunnels, gate) = FakeTunnels::gated(closed_port());
    let h = harness_with(tunnels);
    let mut p = dead_pg("pg");
    p.tunnel = Some(ssh_tunnel());
    let p = h.profiles.upsert(p).unwrap();

    let (m1, m2) = (h.mgr.clone(), h.mgr.clone());
    let (i1, i2) = (p.id.clone(), p.id.clone());
    let a = tokio::spawn(async move { m1.pg(&i1, Some("one")).await.map(|_| ()) });
    let b = tokio::spawn(async move { m2.pg(&i2, Some("two")).await.map(|_| ()) });
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        h.tunnels.calls().len(),
        2,
        "per-(profile, database) locks, not per-profile"
    );
    gate.add_permits(2);
    let _ = (a.await.unwrap(), b.await.unwrap());
}

// ---- test() -------------------------------------------------------------------------------

/// Review finding M7, the open point: a token that whoever reads it can replay is not sent
/// without TLS to another machine. The in-memory store here has no rules of its own, like a
/// `profiles.json` edited by hand.
#[tokio::test]
async fn conn_bearer_credentials_are_not_sent_without_tls_to_another_machine() {
    let h = harness();
    let aws = AwsTarget {
        identity: AwsIdentity::DefaultChain,
        region: "eu-west-1".into(),
    };

    let mut token = pg_profile("token");
    token.auth = AuthMethod::AccessToken {
        username: "me".into(),
        command: None,
    };
    token.tls.mode = TlsMode::Disable;
    token.pg.connect_timeout_secs = 2;
    let token = h.profiles.upsert(token).unwrap();

    let mut iam = token.clone();
    iam.id = String::new();
    iam.name = "iam".into();
    iam.auth = AuthMethod::RdsIam {
        username: "app".into(),
        aws: aws.clone(),
    };
    let iam = h.profiles.upsert(iam).unwrap();

    for p in [&token, &iam] {
        let e = h.mgr.pg(&p.id, None).await.err().expect("refused");
        assert!(
            matches!(&e, CoreError::Invalid(m) if m.contains("TLS")),
            "{}: {e:?}",
            p.name
        );
        assert!(
            matches!(h.mgr.test(p, None).await, Err(CoreError::Invalid(_))),
            "{}",
            p.name
        );
    }

    let mut cache = valkey_profile("cache", "cache.internal.example", 6379);
    cache.auth = AuthMethod::ElastiCacheIam {
        username: "app".into(),
        cache_name: "c".into(),
        serverless: false,
        aws,
    };
    cache.valkey.connect_timeout_secs = 2;
    let cache = h.profiles.upsert(cache).unwrap();
    let e = h.mgr.valkey(&cache.id).await.err().expect("refused");
    assert!(
        matches!(&e, CoreError::Invalid(m) if m.contains("TLS")),
        "{e:?}"
    );
    assert!(matches!(
        h.mgr.test(&cache, None).await,
        Err(CoreError::Invalid(_))
    ));

    assert_eq!(
        h.creds.resolves.load(Ordering::SeqCst),
        0,
        "no credential was fetched"
    );

    // Through the SSH tunnel the same profile is dialled (and fails here only because the
    // fake tunnel leads to a closed port).
    let mut tunnelled = token.clone();
    tunnelled.id = String::new();
    tunnelled.name = "tunnelled".into();
    tunnelled.tunnel = Some(ssh_tunnel());
    let tunnelled = h.profiles.upsert(tunnelled).unwrap();
    let e = h
        .mgr
        .pg(&tunnelled.id, None)
        .await
        .err()
        .expect("dead port");
    assert!(!matches!(e, CoreError::Invalid(_)), "{e:?}");
    assert_eq!(h.tunnels.calls().len(), 1);
}

#[tokio::test]
async fn conn_test_rejects_invalid_input_with_err() {
    let h = harness();
    let mut p = pg_profile("x");
    p.host = "  ".into();
    assert!(matches!(
        h.mgr.test(&p, None).await,
        Err(CoreError::Invalid(_))
    ));
    let mut p = pg_profile("x");
    p.port = 0;
    assert!(matches!(
        h.mgr.test(&p, None).await,
        Err(CoreError::Invalid(_))
    ));
}

#[tokio::test]
async fn conn_test_reports_connection_failures_as_ok_false_and_caches_nothing() {
    let h = harness();
    let p = dead_pg("unsaved"); // never added to the store: test() works on the in-flight form
    let r = h
        .mgr
        .test(&p, Some(SecretString::from("typed-password".to_string())))
        .await
        .unwrap();
    assert!(!r.ok);
    assert!(r.server_version.is_none());
    assert!(!r.message.is_empty());
    assert!(
        !r.message.contains("typed-password"),
        "no secrets in messages"
    );
    assert!(h.mgr.status().is_empty(), "test() is ephemeral");
    assert_eq!(
        h.creds.resolves.load(Ordering::SeqCst),
        0,
        "a password override replaces credential resolution"
    );

    let v = h.mgr.test(&dead_valkey("unsaved-vk"), None).await.unwrap();
    assert!(!v.ok);
    assert!(h.mgr.status().is_empty());
}

#[tokio::test]
async fn conn_test_measures_latency() {
    let h = harness();
    let r = h.mgr.test(&dead_pg("x"), None).await.unwrap();
    assert!(
        r.latency_ms < 10_000,
        "latency is reported in milliseconds: {}",
        r.latency_ms
    );
}

#[tokio::test]
async fn conn_test_with_failing_tunnel_is_ok_false_with_the_tunnel_message() {
    let h = harness();
    *h.tunnels.fail_with.lock().unwrap() = Some("connection refused by bastion".into());
    let mut p = dead_pg("x");
    p.tunnel = Some(ssh_tunnel());
    let r = h.mgr.test(&p, None).await.unwrap();
    assert!(!r.ok);
    assert!(
        r.message.contains("connection refused by bastion"),
        "{}",
        r.message
    );
}

#[tokio::test]
async fn conn_test_closes_its_tunnel() {
    let h = harness();
    let mut p = dead_pg("x");
    p.tunnel = Some(ssh_tunnel());
    let _ = h.mgr.test(&p, None).await.unwrap();
    let tokens = h.tunnels.tokens();
    assert_eq!(tokens.len(), 1);
    assert!(tokens[0].is_cancelled());
}

#[tokio::test]
async fn conn_test_invalid_tunnel_config_from_the_provider_is_an_err() {
    struct Picky;
    #[async_trait]
    impl TunnelProvider for Picky {
        async fn open(&self, _: &str, _: &TunnelConfig, _: &str, _: u16) -> Result<TunnelHandle> {
            Err(CoreError::Invalid("SSH tunnel username is empty".into()))
        }
    }
    let profiles = Arc::new(MemProfiles::default());
    let mgr = ConnectionManager::new(profiles, Arc::new(FakeCreds::default()), Arc::new(Picky));
    let mut p = dead_pg("x");
    p.tunnel = Some(ssh_tunnel());
    assert!(matches!(
        mgr.test(&p, None).await,
        Err(CoreError::Invalid(_))
    ));
}

// ---- reaper -------------------------------------------------------------------------------

#[tokio::test]
async fn conn_reaper_start_is_idempotent_and_a_pass_with_nothing_open_is_a_no_op() {
    let h = harness();
    h.mgr.start_idle_reaper();
    h.mgr.start_idle_reaper();
    assert_eq!(h.mgr.reap_idle().await, 0);
}

#[tokio::test]
async fn conn_profiles_accessor_returns_the_store() {
    let h = harness();
    let p = h.profiles.upsert(dead_pg("pg")).unwrap();
    assert_eq!(h.mgr.profiles().get(&p.id).unwrap().name, "pg");
}

// ---- AppToolBackend -----------------------------------------------------------------------

fn backend(h: &Harness) -> (tempfile::TempDir, Arc<AppToolBackend>) {
    let dir = tempdir().unwrap();
    let ws = Arc::new(WorkspaceFs::new(dir.path().join("ws")).unwrap());
    (dir, AppToolBackend::new(h.mgr.clone(), ws))
}

#[tokio::test]
async fn conn_backend_profile_lookup_delegates_to_the_store() {
    let h = harness();
    let a = h.profiles.upsert(dead_pg("a")).unwrap();
    let b = h.profiles.upsert(dead_valkey("b")).unwrap();
    let (_d, be) = backend(&h);
    assert_eq!(be.profile(&a.id).unwrap().name, "a");
    assert!(matches!(be.profile("ghost"), Err(CoreError::NotFound(_))));
    let names: Vec<_> = be
        .list_profiles()
        .unwrap()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert_eq!(names, ["a", "b"]);
    let _ = b;
}

#[tokio::test]
async fn conn_backend_workspace_calls_go_through_the_jail() {
    let h = harness();
    let (_d, be) = backend(&h);
    be.write_file("notes/todo.md", "- ship it").await.unwrap();
    assert_eq!(be.read_file("notes/todo.md").await.unwrap(), "- ship it");
    let files = be.list_files().await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "notes/todo.md");

    assert!(matches!(
        be.read_file("../../etc/passwd.txt").await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        be.write_file("/tmp/x.sql", "x").await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        be.write_file("run.sh", "x").await,
        Err(CoreError::Invalid(_))
    ));
}

#[tokio::test]
async fn conn_backend_sql_against_unreachable_database_is_a_connection_error_not_a_panic() {
    let h = harness();
    let p = h.profiles.upsert(dead_pg("pg")).unwrap();
    let (_d, be) = backend(&h);
    assert!(
        be.execute_sql(&p.id, None, "select 1", true, 100)
            .await
            .is_err()
    );
    assert!(
        be.schema_metadata(&p.id, Some("app"), None, 10)
            .await
            .is_err()
    );
    assert!(matches!(
        be.execute_sql("ghost", None, "select 1", true, 100).await,
        Err(CoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn conn_backend_valkey_calls_on_an_unreachable_server_are_errors() {
    let h = harness();
    let p = h.profiles.upsert(dead_valkey("vk")).unwrap();
    let (_d, be) = backend(&h);
    // The handle is needed before the (moot) limit is looked at, so an unreachable server is an error.
    assert!(be.valkey_scan(&p.id, 0, "*", 0).await.is_err());
}

#[tokio::test]
async fn conn_failure_status_is_tracked_per_profile() {
    // Two profiles, one failing: the failure of one must not show up on the other.
    let h = harness();
    let a = h.profiles.upsert(dead_pg("a")).unwrap();
    let b = h.profiles.upsert(dead_pg("b")).unwrap();
    let _ = h.mgr.connect(&a.id).await;
    assert!(status_of(&h, &b.id).is_none());
    let ids: HashMap<_, _> = h
        .mgr
        .status()
        .into_iter()
        .map(|s| (s.profile_id, s.state))
        .collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[&a.id], ConnectionState::Error);
}
