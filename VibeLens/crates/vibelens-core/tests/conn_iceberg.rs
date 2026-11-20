//! ConnectionManager + AppToolBackend for Apache Iceberg profiles.
//!
//! Two layers:
//! * Always-on tests that need no catalog: missing dependencies, engine mismatch, status
//!   transitions on a failing connect, "no tunnel for Iceberg", ephemeral `test()` results.
//! * Fixture tests against the pyiceberg-written SQLite catalog (`VG_TEST_ICEBERG_SQL_DB` and
//!   `VG_TEST_ICEBERG_WAREHOUSE`; they skip cleanly when unset): single-flight, the handle cache and
//!   its invalidation, disconnect, the idle reaper and the agent's tool backend.
#![cfg(feature = "iceberg")]

mod services;

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use secrecy::SecretString;
use tempfile::tempdir;
use vibelens_core::aws::AwsService;
use vibelens_core::connections::{AppToolBackend, ConnectionManager};
use vibelens_core::error::{CoreError, Result};
use vibelens_core::iceberg::IcebergDeps;
use vibelens_core::model::*;
use vibelens_core::ports::{
    CredentialSource, ProfileStore, ToolBackend, TunnelHandle, TunnelProvider,
};
use vibelens_core::vault::MemoryVault;
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

/// Iceberg connections never resolve database credentials; any call is a bug.
#[derive(Default)]
struct FakeCreds {
    resolves: AtomicUsize,
}

#[async_trait]
impl CredentialSource for FakeCreds {
    async fn resolve(&self, _profile: &ConnectionProfile) -> Result<ResolvedCredentials> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        Ok(ResolvedCredentials::anonymous())
    }
}

#[derive(Default)]
struct FakeTunnels {
    opened: AtomicUsize,
}

#[async_trait]
impl TunnelProvider for FakeTunnels {
    async fn open(
        &self,
        _profile_id: &str,
        _cfg: &TunnelConfig,
        _host: &str,
        _port: u16,
    ) -> Result<TunnelHandle> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        Err(CoreError::Tunnel(
            "an Iceberg connection must not open a tunnel".into(),
        ))
    }
}

// ---- harness ------------------------------------------------------------------------------

struct Harness {
    profiles: Arc<MemProfiles>,
    creds: Arc<FakeCreds>,
    tunnels: Arc<FakeTunnels>,
    mgr: Arc<ConnectionManager>,
}

fn harness() -> Harness {
    let profiles = Arc::new(MemProfiles::default());
    let creds = Arc::new(FakeCreds::default());
    let tunnels = Arc::new(FakeTunnels::default());
    let mgr = ConnectionManager::new(profiles.clone(), creds.clone(), tunnels.clone());
    Harness {
        profiles,
        creds,
        tunnels,
        mgr,
    }
}

fn harness_with_deps() -> Harness {
    let h = harness();
    h.mgr.set_iceberg_deps(IcebergDeps {
        aws: AwsService::new(),
        vault: Arc::new(MemoryVault::new()),
    });
    h
}

fn closed_port() -> u16 {
    let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    l.local_addr().unwrap().port()
}

fn base(name: &str, options: Option<IcebergOptions>) -> ConnectionProfile {
    ConnectionProfile {
        id: String::new(),
        name: name.into(),
        engine: EngineKind::Iceberg,
        host: String::new(),
        port: 0,
        database: None,
        auth: AuthMethod::None,
        tls: TlsConfig::default(),
        tunnel: None,
        environment: Environment::Dev,
        ai_data_access: AiDataAccess::Results,
        color: None,
        pg: PgOptions::default(),
        valkey: ValkeyOptions::default(),
        created_at: None,
        updated_at: None,
        iceberg: options,
    }
}

/// A REST catalog whose address refuses connections.
fn dead_rest(name: &str) -> ConnectionProfile {
    base(
        name,
        Some(IcebergOptions {
            catalog: IcebergCatalog::Rest {
                uri: format!("http://127.0.0.1:{}", closed_port()),
                warehouse: None,
                auth: RestAuth::None,
                headers: Default::default(),
            },
            storage: IcebergStorage::Auto,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: None,
        }),
    )
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

fn pg_profile(name: &str) -> ConnectionProfile {
    ConnectionProfile {
        engine: EngineKind::Postgres,
        host: "127.0.0.1".into(),
        port: 5432,
        auth: AuthMethod::Password {
            username: "u".into(),
        },
        ..base(name, None)
    }
}

fn status_of(h: &Harness, id: &str) -> Option<ConnectionStatus> {
    h.mgr.status().into_iter().find(|s| s.profile_id == id)
}

fn backend(h: &Harness) -> (Arc<AppToolBackend>, tempfile::TempDir) {
    let ws = tempdir().unwrap();
    let b = AppToolBackend::new(
        h.mgr.clone(),
        Arc::new(WorkspaceFs::new(ws.path().join("w")).unwrap()),
    );
    (b, ws)
}

// ---- always-on: configuration errors ---------------------------------------------------------

#[tokio::test]
async fn ice_missing_deps_is_invalid_and_is_not_a_connection_failure() {
    let h = harness();
    let p = h.profiles.upsert(dead_rest("lake")).unwrap();

    let err = h.mgr.iceberg(&p.id).await.err().expect("must fail");
    assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
    assert!(
        err.to_string().contains("set_iceberg_deps"),
        "the message tells the developer what is missing: {err}"
    );
    assert!(matches!(
        h.mgr.connect(&p.id).await,
        Err(CoreError::Invalid(_))
    ));
    // A wiring bug is not a connection failure: no Error status is recorded.
    assert!(h.mgr.status().is_empty(), "{:?}", h.mgr.status());

    // The ephemeral test reports it the same way, and the agent backend surfaces it too.
    assert!(matches!(
        h.mgr.test(&p, None).await,
        Err(CoreError::Invalid(_))
    ));
    let (b, _ws) = backend(&h);
    let err = b
        .execute_sql(&p.id, None, "select 1", true, 10)
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::Invalid(_)) && err.to_string().contains("set_iceberg_deps"),
        "dispatch must reach the Iceberg path, not the Postgres one: {err}"
    );
    assert!(matches!(
        b.schema_metadata(&p.id, None, Some("shop"), 10).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        b.describe_iceberg_table(&p.id, vec!["shop".into()], "orders")
            .await,
        Err(CoreError::Invalid(_))
    ));
    assert_eq!(h.tunnels.opened.load(Ordering::SeqCst), 0);
    assert_eq!(h.creds.resolves.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ice_second_set_iceberg_deps_is_ignored() {
    let h = harness_with_deps();
    // Must not panic or replace anything.
    h.mgr.set_iceberg_deps(IcebergDeps {
        aws: AwsService::new(),
        vault: Arc::new(MemoryVault::new()),
    });
    let p = h.profiles.upsert(dead_rest("lake")).unwrap();
    // Deps are present, so the failure is a connection failure, not the wiring error.
    let err = h.mgr.iceberg(&p.id).await.err().expect("dead catalog");
    assert!(!err.to_string().contains("set_iceberg_deps"), "{err}");
}

#[tokio::test]
async fn ice_engine_mismatch_and_missing_options_are_invalid_and_never_dial() {
    let h = harness_with_deps();
    let ice = h.profiles.upsert(dead_rest("lake")).unwrap();
    let pg = h.profiles.upsert(pg_profile("pg")).unwrap();
    let no_opts = h.profiles.upsert(base("empty", None)).unwrap();

    assert!(matches!(
        h.mgr.iceberg(&pg.id).await,
        Err(CoreError::Invalid(m)) if m.contains("not Iceberg")
    ));
    assert!(matches!(
        h.mgr.pg(&ice.id, None).await,
        Err(CoreError::Invalid(m)) if m.contains("not Postgres")
    ));
    assert!(matches!(
        h.mgr.valkey(&ice.id).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        h.mgr.iceberg(&no_opts.id).await,
        Err(CoreError::Invalid(m)) if m.contains("no Iceberg catalog options")
    ));
    assert!(matches!(
        h.mgr.iceberg("ghost").await,
        Err(CoreError::NotFound(_))
    ));

    // Valkey tools on an Iceberg profile are a mismatch too.
    let (b, _ws) = backend(&h);
    assert!(matches!(
        b.valkey_scan(&ice.id, 0, "*", 5).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        b.valkey_is_write(&ice.id, &["GET".into(), "k".into()])
            .await,
        Err(CoreError::Invalid(_))
    ));

    assert!(h.mgr.status().is_empty(), "{:?}", h.mgr.status());
    assert_eq!(h.tunnels.opened.load(Ordering::SeqCst), 0);
}

// ---- always-on: status transitions on failure --------------------------------------------------

/// Review finding M7: a profile edited by hand (the in-memory store has no rules) that would
/// send a secret in the clear, or that keeps one outside the keychain, is refused when used.
#[tokio::test]
async fn ice_profiles_with_a_secret_in_the_wrong_place_are_refused_on_use() {
    let h = harness();
    let vault = Arc::new(MemoryVault::new());
    h.mgr.set_iceberg_deps(IcebergDeps {
        aws: AwsService::new(),
        vault: vault.clone(),
    });
    let options = |catalog| {
        Some(IcebergOptions {
            catalog,
            storage: IcebergStorage::Local,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: None,
        })
    };
    let rest = |uri: String, auth, header: Option<(&str, &str)>| IcebergCatalog::Rest {
        uri,
        warehouse: None,
        auth,
        headers: header
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    };
    let here = format!("http://127.0.0.1:{}", closed_port());
    for p in [
        base(
            "token to a cleartext catalog",
            options(rest(
                "http://catalog.internal.example:8181".into(),
                RestAuth::Bearer,
                None,
            )),
        ),
        base(
            "credential in a header",
            options(rest(
                here.clone(),
                RestAuth::None,
                Some(("Authorization", "Basic c2VjcmV0")),
            )),
        ),
        base(
            "password in the catalog URI",
            options(IcebergCatalog::Sql {
                uri: format!("postgres://iceberg:hunter2@127.0.0.1:{}/c", closed_port()),
                warehouse: "file:///tmp/vg-none".into(),
                catalog_name: "c".into(),
            }),
        ),
    ] {
        let saved = h.profiles.upsert(p).unwrap();
        // A token is in the keychain, so only the rule stands between it and the network.
        use vibelens_core::ports::SecretVault;
        vault
            .set(
                &vault_keys::iceberg_secret(&saved.id),
                &SecretString::from("s3cr3t-token".to_string()),
            )
            .await
            .unwrap();
        let e = h.mgr.iceberg(&saved.id).await.err().expect("refused");
        assert!(
            matches!(&e, CoreError::Invalid(m) if !m.contains("hunter2") && !m.contains("c2VjcmV0") && !m.contains("s3cr3t")),
            "{}: {e:?}",
            saved.name
        );
        assert!(
            matches!(h.mgr.test(&saved, None).await, Err(CoreError::Invalid(_))),
            "{}",
            saved.name
        );
    }
}

#[tokio::test]
async fn ice_failed_connect_records_error_status_with_detail_then_disconnect_clears_it() {
    let h = harness_with_deps();
    // A stale tunnel setting must be ignored: Iceberg never tunnels.
    let mut dead = dead_rest("lake");
    dead.tunnel = Some(ssh_tunnel());
    let p = h.profiles.upsert(dead).unwrap();
    assert!(
        h.mgr.status().is_empty(),
        "nothing is tracked before first use"
    );

    let err = h.mgr.connect(&p.id).await.unwrap_err();
    assert!(!matches!(err, CoreError::Invalid(_)), "{err:?}");
    let st = status_of(&h, &p.id).expect("a failed connect leaves a status");
    assert_eq!(st.state, ConnectionState::Error);
    assert_eq!(st.engine, EngineKind::Iceberg);
    assert!(!st.tunneled, "Iceberg connections are never tunnelled");
    assert!(st.since.is_some() && st.server_version.is_none());
    let detail = st.detail.expect("error detail");
    assert!(!detail.is_empty());
    assert_eq!(
        h.tunnels.opened.load(Ordering::SeqCst),
        0,
        "no tunnel for Iceberg"
    );
    assert_eq!(h.creds.resolves.load(Ordering::SeqCst), 0);

    // Retrying keeps failing the same way and keeps one status row.
    assert!(h.mgr.iceberg(&p.id).await.is_err());
    assert_eq!(h.mgr.status().len(), 1);

    h.mgr.disconnect(&p.id).await.unwrap();
    assert!(h.mgr.status().is_empty());
    h.mgr.disconnect(&p.id).await.unwrap(); // idempotent
}

#[tokio::test]
async fn ice_concurrent_failed_connects_all_fail_and_leave_a_single_error() {
    let h = harness_with_deps();
    let p = h.profiles.upsert(dead_rest("lake")).unwrap();
    let results = futures_util::future::join_all((0..4).map(|_| h.mgr.iceberg(&p.id))).await;
    assert!(results.iter().all(Result::is_err));
    let all = h.mgr.status();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].state, ConnectionState::Error);
}

#[tokio::test]
async fn ice_test_reports_unreachable_catalogs_as_results_and_ignores_host_and_port() {
    let h = harness_with_deps();
    // host = "" and port = 0 are normal for Iceberg and must not trip the generic validation.
    let p = dead_rest("lake");
    assert!(p.host.is_empty() && p.port == 0);
    let res = h.mgr.test(&p, None).await.expect("a result, not an error");
    assert!(!res.ok);
    assert!(!res.message.is_empty());
    assert!(res.server_version.is_none());
    // The wizard's unsaved secret does not change that.
    let res = h
        .mgr
        .test(&p, Some(SecretString::from("tok".to_string())))
        .await
        .unwrap();
    assert!(!res.ok);
    assert!(!res.message.contains("tok") || res.message.len() < 400);
    // Nothing is cached or reported by an ephemeral test.
    assert!(h.mgr.status().is_empty());

    let mut missing = dead_rest("none");
    missing.iceberg = None;
    assert!(matches!(
        h.mgr.test(&missing, None).await,
        Err(CoreError::Invalid(m)) if m.contains("Iceberg")
    ));
}

#[tokio::test]
async fn ice_reaper_ignores_failed_profiles_and_never_panics_without_handles() {
    let h = harness_with_deps();
    let p = h.profiles.upsert(dead_rest("lake")).unwrap();
    let _ = h.mgr.iceberg(&p.id).await;
    assert_eq!(h.mgr.reap_idle().await, 0);
    assert_eq!(
        h.mgr.status().len(),
        1,
        "the Error row stays until disconnect"
    );
}

// ---- fixture-backed ------------------------------------------------------------------------------

struct Fixture {
    db: String,
    warehouse: String,
}

/// The pyiceberg fixture, or `None` after reporting the skip: the test returns.
fn fixture() -> Option<Fixture> {
    let db = std::env::var("VG_TEST_ICEBERG_SQL_DB").unwrap_or_default();
    let warehouse = std::env::var("VG_TEST_ICEBERG_WAREHOUSE").unwrap_or_default();
    if db.is_empty() || warehouse.is_empty() {
        services::skip(
            "iceberg",
            "VG_TEST_ICEBERG_SQL_DB and VG_TEST_ICEBERG_WAREHOUSE are not both set",
        );
        return None;
    }
    Some(Fixture { db, warehouse })
}

fn fixture_profile(f: &Fixture, name: &str) -> ConnectionProfile {
    base(
        name,
        Some(IcebergOptions {
            catalog: IcebergCatalog::Sql {
                uri: format!("sqlite://{}", f.db),
                warehouse: format!("file://{}", f.warehouse),
                catalog_name: "vg_test".into(),
            },
            storage: IcebergStorage::Local,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: Some("shop".into()),
        }),
    )
}

#[tokio::test]
async fn ice_connect_reports_connected_with_the_target_description() {
    let Some(f) = fixture() else { return };
    let h = harness_with_deps();
    let p = h.profiles.upsert(fixture_profile(&f, "lake")).unwrap();

    let st = h.mgr.connect(&p.id).await.expect("connect");
    assert_eq!(st.state, ConnectionState::Connected);
    assert_eq!(st.engine, EngineKind::Iceberg);
    assert!(!st.tunneled && st.detail.is_none() && st.since.is_some());
    let target = st
        .server_version
        .expect("describe_target is the server_version");
    assert!(!target.is_empty());

    let direct = h.mgr.iceberg(&p.id).await.unwrap();
    assert_eq!(direct.describe_target(), target);
    assert_eq!(
        status_of(&h, &p.id).unwrap().state,
        ConnectionState::Connected
    );

    h.mgr.disconnect(&p.id).await.unwrap();
    assert!(h.mgr.status().is_empty());
    // The profile can connect again after a disconnect.
    assert_eq!(
        h.mgr.connect(&p.id).await.unwrap().state,
        ConnectionState::Connected
    );
    assert_eq!(h.tunnels.opened.load(Ordering::SeqCst), 0);
    assert_eq!(h.creds.resolves.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn ice_concurrent_callers_share_one_handle() {
    let Some(f) = fixture() else { return };
    let h = harness_with_deps();
    let p = h.profiles.upsert(fixture_profile(&f, "lake")).unwrap();
    let handles: Vec<_> = futures_util::future::join_all((0..8).map(|_| h.mgr.iceberg(&p.id)))
        .await
        .into_iter()
        .map(|r| r.expect("connect"))
        .collect();
    for other in &handles[1..] {
        assert!(
            Arc::ptr_eq(&handles[0], other),
            "single-flight: one handle for N callers"
        );
    }
    assert_eq!(h.mgr.status().len(), 1);
}

#[tokio::test]
async fn ice_cache_survives_cosmetic_edits_but_not_connection_edits() {
    let Some(f) = fixture() else { return };
    let h = harness_with_deps();
    let p = h.profiles.upsert(fixture_profile(&f, "lake")).unwrap();
    let first = h.mgr.iceberg(&p.id).await.unwrap();

    // Cosmetic: name, colour, environment, AI access, idle timeout.
    let mut cosmetic = p.clone();
    cosmetic.name = "renamed".into();
    cosmetic.color = Some("#123456".into());
    cosmetic.environment = Environment::Prod;
    cosmetic.ai_data_access = AiDataAccess::SchemaOnly;
    cosmetic.pg.idle_disconnect_secs = 17;
    h.profiles.upsert(cosmetic.clone()).unwrap();
    assert!(Arc::ptr_eq(&first, &h.mgr.iceberg(&p.id).await.unwrap()));

    // The default namespace changes how unqualified names resolve: new handle.
    let mut ns = cosmetic.clone();
    ns.iceberg.as_mut().unwrap().default_namespace = None;
    h.profiles.upsert(ns.clone()).unwrap();
    let second = h.mgr.iceberg(&p.id).await.unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(Arc::ptr_eq(&second, &h.mgr.iceberg(&p.id).await.unwrap()));

    // Storage edits too (the local catalog and warehouse still answer; only the FileIO differs).
    let mut storage = ns.clone();
    storage.iceberg.as_mut().unwrap().storage = IcebergStorage::Auto;
    h.profiles.upsert(storage).unwrap();
    let third = h.mgr.iceberg(&p.id).await.unwrap();
    assert!(!Arc::ptr_eq(&second, &third));

    // A catalog edit that breaks the connection surfaces as an error on next use, not a stale hit.
    let mut broken = ns;
    broken.iceberg.as_mut().unwrap().catalog = IcebergCatalog::Rest {
        uri: format!("http://127.0.0.1:{}", closed_port()),
        warehouse: None,
        auth: RestAuth::None,
        headers: Default::default(),
    };
    h.profiles.upsert(broken).unwrap();
    assert!(h.mgr.iceberg(&p.id).await.is_err());
    assert_eq!(status_of(&h, &p.id).unwrap().state, ConnectionState::Error);
}

#[tokio::test]
async fn ice_changing_the_query_engine_replaces_the_handle() {
    let Some(f) = fixture() else { return };
    let h = harness_with_deps();
    let p = h.profiles.upsert(fixture_profile(&f, "lake")).unwrap();
    let first = h.mgr.iceberg(&p.id).await.unwrap();
    let mut athena = p.clone();
    athena.iceberg.as_mut().unwrap().query_engine = IcebergQueryEngine::Athena {
        workgroup: "primary".into(),
        output_location: None,
        aws: None,
    };
    h.profiles.upsert(athena).unwrap();
    // Athena is only valid for Glue / S3 Tables: this is rejected, and the stale handle is gone.
    assert!(h.mgr.iceberg(&p.id).await.is_err());
    h.profiles.upsert(p.clone()).unwrap();
    let again = h.mgr.iceberg(&p.id).await.unwrap();
    assert!(
        !Arc::ptr_eq(&first, &again),
        "the stale handle was discarded"
    );
}

#[tokio::test]
async fn ice_test_connects_ephemerally_and_caches_nothing() {
    let Some(f) = fixture() else { return };
    let h = harness_with_deps();
    // A profile that was never saved (empty id) can be tested.
    let res = h
        .mgr
        .test(&fixture_profile(&f, "wizard"), None)
        .await
        .unwrap();
    assert!(res.ok, "{}", res.message);
    let target = res.server_version.expect("target description");
    assert!(res.message.contains(&target), "{}", res.message);
    assert!(h.mgr.status().is_empty());

    let mut bad = fixture_profile(&f, "wizard");
    bad.iceberg.as_mut().unwrap().catalog = IcebergCatalog::Sql {
        uri: "sqlite:///nonexistent-dir-for-vibelens/catalog.db".into(),
        warehouse: "file:///nonexistent".into(),
        catalog_name: "x".into(),
    };
    let res = h.mgr.test(&bad, None).await.unwrap();
    assert!(!res.ok && !res.message.is_empty());
}

#[tokio::test]
async fn ice_idle_reaper_closes_idle_handles_and_respects_never() {
    let Some(f) = fixture() else { return };
    let h = harness_with_deps();

    // idle_disconnect_secs = 0 means never.
    let mut keep = fixture_profile(&f, "keep");
    keep.pg.idle_disconnect_secs = 0;
    let keep = h.profiles.upsert(keep).unwrap();
    // 1 second: reaped once idle for longer than that.
    let mut quick = fixture_profile(&f, "quick");
    quick.pg.idle_disconnect_secs = 1;
    let quick = h.profiles.upsert(quick).unwrap();
    // Long timeout: still fresh.
    let mut fresh = fixture_profile(&f, "fresh");
    fresh.pg.idle_disconnect_secs = 3600;
    let fresh = h.profiles.upsert(fresh).unwrap();
    // Deleted from the store while connected: reaped regardless of idle time.
    let gone = h.profiles.upsert(fixture_profile(&f, "gone")).unwrap();

    for p in [&keep, &quick, &fresh, &gone] {
        h.mgr.connect(&p.id).await.unwrap();
    }
    assert_eq!(h.mgr.status().len(), 4);
    h.profiles.delete(&gone.id).unwrap();
    assert_eq!(
        h.mgr.reap_idle().await,
        1,
        "only the deleted profile right away"
    );

    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(
        h.mgr.reap_idle().await,
        1,
        "the 1-second profile is now idle"
    );
    let left: Vec<_> = h.mgr.status().into_iter().map(|s| s.profile_id).collect();
    assert_eq!(left.len(), 2);
    assert!(left.contains(&keep.id) && left.contains(&fresh.id));

    // A reaped profile reconnects on next use.
    assert_eq!(
        h.mgr.connect(&quick.id).await.unwrap().state,
        ConnectionState::Connected
    );
    // Use resets idleness.
    let handle = h.mgr.iceberg(&quick.id).await.unwrap();
    let _ = handle.list_namespaces(None).await.unwrap();
    assert_eq!(h.mgr.reap_idle().await, 0);
}

#[tokio::test]
async fn ice_tool_backend_runs_sql_and_reads_the_schema() {
    let Some(f) = fixture() else { return };
    let h = harness_with_deps();
    let p = h.profiles.upsert(fixture_profile(&f, "lake")).unwrap();
    let (b, _ws) = backend(&h);

    // `database` is meaningless for Iceberg and must be ignored.
    let res = b
        .execute_sql(
            &p.id,
            Some("whatever"),
            "select count(*) as n from shop.customers",
            true,
            100,
        )
        .await
        .expect("select");
    assert_eq!(res.rows, vec![vec![serde_json::json!("500")]]);
    let res = b
        .execute_sql(
            &p.id,
            None,
            "select id from customers order by id limit 3",
            true,
            100,
        )
        .await
        .expect("default namespace resolves unqualified names");
    assert_eq!(res.row_count, 3);

    // max_rows is honoured.
    let res = b
        .execute_sql(&p.id, None, "select id from shop.customers", true, 7)
        .await
        .unwrap();
    assert_eq!(res.rows.len(), 7);
    assert!(res.truncated);

    // Read-only means queries only: writes are refused before anything runs.
    let err = b
        .execute_sql(
            &p.id,
            None,
            "insert into shop.customers select * from shop.customers",
            true,
            10,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, CoreError::PolicyDenied(_) | CoreError::Invalid(_)),
        "{err:?}"
    );
    let still = b
        .execute_sql(&p.id, None, "select count(*) from shop.customers", true, 10)
        .await
        .unwrap();
    assert_eq!(still.rows, vec![vec![serde_json::json!("500")]]);
    // The local engine cannot do row-level DML at all, even when the agent is allowed to write.
    let err = b
        .execute_sql(
            &p.id,
            None,
            "delete from shop.customers where id = 1",
            false,
            10,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");

    // Whole catalog, one namespace, a nested spelling and an empty filter.
    let all = b
        .schema_metadata(&p.id, Some("ignored"), None, 100)
        .await
        .unwrap();
    let names: Vec<String> = all
        .tables
        .iter()
        .map(|t| format!("{}.{}", t.schema, t.name))
        .collect();
    for want in ["shop.customers", "shop.orders", "shop.events"] {
        assert!(names.contains(&want.to_string()), "{names:?}");
    }
    let shop = b
        .schema_metadata(&p.id, None, Some("shop"), 100)
        .await
        .unwrap();
    assert_eq!(shop.tables.len(), 3);
    assert!(shop.tables.iter().all(|t| t.schema == "shop"));
    assert!(shop.tables.iter().all(|t| !t.columns.is_empty()));
    let blank = b
        .schema_metadata(&p.id, None, Some("  "), 100)
        .await
        .unwrap();
    assert_eq!(blank.tables.len(), all.tables.len());
    let capped = b
        .schema_metadata(&p.id, None, Some("shop"), 1)
        .await
        .unwrap();
    assert_eq!(capped.tables.len(), 1);
    assert!(capped.truncated);
    let none = b.schema_metadata(&p.id, None, Some("no_such_ns"), 10).await;
    assert!(none.is_err() || none.unwrap().tables.is_empty());
}

#[tokio::test]
async fn ice_tool_backend_connects_lazily_and_registers_status() {
    let Some(f) = fixture() else { return };
    let h = harness_with_deps();
    let p = h.profiles.upsert(fixture_profile(&f, "lake")).unwrap();
    let (b, _ws) = backend(&h);
    assert!(h.mgr.status().is_empty());
    b.schema_metadata(&p.id, None, Some("shop"), 10)
        .await
        .unwrap();
    let st = status_of(&h, &p.id).expect("lazy connect registers the profile");
    assert_eq!(st.state, ConnectionState::Connected);
    assert_eq!(st.engine, EngineKind::Iceberg);
}

#[tokio::test]
async fn ice_tool_backend_describes_a_table_with_partitions_and_snapshots() {
    let Some(f) = fixture() else { return };
    let h = harness_with_deps();
    let p = h.profiles.upsert(fixture_profile(&f, "lake")).unwrap();
    let (b, _ws) = backend(&h);
    let info = b
        .describe_iceberg_table(&p.id, vec!["shop".into()], "orders")
        .await
        .expect("describe");
    assert_eq!(info.name, "orders");
    assert!(
        !info.partition_spec.is_empty(),
        "orders is partitioned by day(placed_at)"
    );
    assert!(info.snapshots.len() >= 4);
    assert!(matches!(
        b.describe_iceberg_table(&p.id, vec!["shop".into()], "no_such_table")
            .await,
        Err(CoreError::NotFound(_) | CoreError::Invalid(_))
    ));
    // Postgres profiles are a mismatch, not a silent fallback.
    let pg = h.profiles.upsert(pg_profile("pg")).unwrap();
    assert!(matches!(
        b.describe_iceberg_table(&pg.id, vec!["shop".into()], "orders")
            .await,
        Err(CoreError::Invalid(_))
    ));
}
