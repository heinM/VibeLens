//! ConnectionManager + AppToolBackend for Amazon Redshift profiles, against the Data API mock.
//! Credentials come from a static profile in a throwaway AWS credentials file, through the same
//! `AwsService` the app uses.

mod redshift_common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use redshift_common::*;
use serde_json::json;
use vibelens_core::aws::{AwsService, AwsServiceConfig};
use vibelens_core::connections::{AppToolBackend, ConnectionManager};
use vibelens_core::error::{CoreError, Result};
use vibelens_core::iceberg::IcebergDeps;
use vibelens_core::model::*;
use vibelens_core::ports::{
    CredentialSource, ProfileStore, ToolBackend, TunnelHandle, TunnelProvider,
};
use vibelens_core::vault::MemoryVault;
use vibelens_core::workspace::WorkspaceFs;

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
    fn upsert(&self, p: ConnectionProfile) -> Result<ConnectionProfile> {
        let mut all = self.inner.lock().unwrap();
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

/// Redshift never resolves database credentials; a call would mean a password path was taken.
#[derive(Default)]
struct NoCreds {
    resolves: AtomicUsize,
}

#[async_trait]
impl CredentialSource for NoCreds {
    async fn resolve(&self, _profile: &ConnectionProfile) -> Result<ResolvedCredentials> {
        self.resolves.fetch_add(1, Ordering::SeqCst);
        Err(CoreError::Auth("no credentials in this test".into()))
    }
}

#[derive(Default)]
struct NoTunnels {
    opened: AtomicUsize,
}

#[async_trait]
impl TunnelProvider for NoTunnels {
    async fn open(
        &self,
        _profile_id: &str,
        _cfg: &TunnelConfig,
        _host: &str,
        _port: u16,
    ) -> Result<TunnelHandle> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        Err(CoreError::Tunnel(
            "a Redshift connection must not open a tunnel".into(),
        ))
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    profiles: Arc<MemProfiles>,
    creds: Arc<NoCreds>,
    tunnels: Arc<NoTunnels>,
    mgr: Arc<ConnectionManager>,
    server: MockServer,
}

async fn harness(handler: Handler) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let credentials = dir.path().join("credentials");
    std::fs::write(&config, "[profile rs]\nregion = eu-west-1\n").unwrap();
    std::fs::write(
        &credentials,
        format!("[rs]\naws_access_key_id = {ACCESS_KEY}\naws_secret_access_key = {SECRET_KEY}\n"),
    )
    .unwrap();
    let aws = AwsService::with_config(AwsServiceConfig::with_paths(
        &config,
        &credentials,
        dir.path().join("sso"),
    ));
    let profiles = Arc::new(MemProfiles::default());
    let creds = Arc::new(NoCreds::default());
    let tunnels = Arc::new(NoTunnels::default());
    let mgr = ConnectionManager::new(profiles.clone(), creds.clone(), tunnels.clone());
    mgr.set_iceberg_deps(IcebergDeps {
        aws,
        vault: Arc::new(MemoryVault::new()),
    });
    let server = serve(handler).await;
    mgr.set_redshift_endpoint_url(server.url.clone());
    Harness {
        _dir: dir,
        profiles,
        creds,
        tunnels,
        mgr,
        server,
    }
}

fn profile(id: &str, options: RedshiftOptions) -> ConnectionProfile {
    ConnectionProfile {
        id: id.into(),
        name: format!("warehouse {id}"),
        engine: EngineKind::Redshift,
        host: String::new(),
        port: 0,
        database: None,
        auth: AuthMethod::None,
        tls: TlsConfig::default(),
        // A stale tunnel from an earlier configuration is ignored, never opened.
        tunnel: Some(TunnelConfig::Ssh {
            host: "bastion.example".into(),
            port: 22,
            username: "ops".into(),
            auth: SshAuth::Agent,
            known_host_fingerprint: None,
        }),
        environment: Environment::Dev,
        ai_data_access: AiDataAccess::Results,
        color: None,
        pg: PgOptions::default(),
        valkey: ValkeyOptions::default(),
        created_at: None,
        updated_at: None,
        iceberg: None,
        redshift: Some(RedshiftOptions {
            aws: AwsTarget {
                identity: AwsIdentity::Profile {
                    profile_name: "rs".into(),
                },
                region: "eu-west-1".into(),
            },
            ..options
        }),
    }
}

fn warehouse() -> Handler {
    Arc::new(|req: &Req| match op(req).as_str() {
        "ListDatabases" => Resp::ok(&json!({"Databases": ["dev"]})),
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "FINISHED", true, 1),
        "GetStatementResult" => result_page(&[("n", "int4")], vec![vec![long(1)]], None),
        "ListSchemas" => Resp::ok(&json!({"Schemas": ["public"]})),
        "ListTables" => {
            Resp::ok(&json!({"Tables": [{"schema": "public", "name": "sales", "type": "TABLE"}]}))
        }
        "DescribeTable" => Resp::ok(&json!({"ColumnList": [
            {"name": "id", "typeName": "int8", "nullable": 0, "schemaName": "public", "tableName": "sales", "length": 0, "precision": 19, "scale": 0}
        ]})),
        other => Resp::error(400, "ValidationException", &format!("unexpected {other}")),
    })
}

#[tokio::test]
async fn connect_reports_the_target_and_never_tunnels() {
    let h = harness(warehouse()).await;
    h.profiles
        .upsert(profile("r1", provisioned(RedshiftAuth::IamIdentity)))
        .unwrap();
    let st = h.mgr.connect("r1").await.unwrap();
    assert_eq!(st.engine, EngineKind::Redshift);
    assert_eq!(st.state, ConnectionState::Connected);
    assert_eq!(
        st.server_version.as_deref(),
        Some("Redshift · analytics-1 · dev · eu-west-1 · as the IAM identity")
    );
    assert!(!st.tunneled);
    assert_eq!(h.tunnels.opened.load(Ordering::SeqCst), 0);
    assert_eq!(
        h.creds.resolves.load(Ordering::SeqCst),
        0,
        "no password path"
    );
    // The connect check is one small ListDatabases, signed with the profile's identity.
    let pings = h.server.calls("ListDatabases");
    assert_eq!(pings.len(), 1);
    assert_eq!(pings[0]["MaxResults"], 1);
    assert_eq!(pings[0]["ClusterIdentifier"], "analytics-1");
    let auth = h.server.all()[0].headers["authorization"].clone();
    assert!(auth.contains(ACCESS_KEY), "{auth}");

    // Reused while the profile is unchanged; replaced when its Redshift options change.
    let a = h.mgr.redshift("r1").await.unwrap();
    let b = h.mgr.redshift("r1").await.unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    let mut p = h.profiles.get("r1").unwrap();
    p.redshift.as_mut().unwrap().database = "sales".into();
    h.profiles.upsert(p).unwrap();
    let c = h.mgr.redshift("r1").await.unwrap();
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(
        h.mgr.status()[0].server_version.as_deref(),
        Some("Redshift · analytics-1 · sales · eu-west-1 · as the IAM identity")
    );

    h.mgr.disconnect("r1").await.unwrap();
    assert!(h.mgr.status().is_empty());
}

#[tokio::test]
async fn a_failed_connect_is_an_error_status_without_credentials() {
    let h = harness(Arc::new(|_req: &Req| {
        Resp::error(400, "ValidationException", "Cluster not found: analytics-1")
    }))
    .await;
    h.profiles
        .upsert(profile("r1", provisioned(RedshiftAuth::IamIdentity)))
        .unwrap();
    let err = h.mgr.connect("r1").await.unwrap_err();
    assert!(matches!(err, CoreError::Aws(_)), "{err:?}");
    let st = &h.mgr.status()[0];
    assert_eq!(st.state, ConnectionState::Error);
    let detail = st.detail.clone().unwrap();
    assert!(detail.contains("Cluster not found"), "{detail}");
    assert!(!detail.contains(SECRET_KEY) && !detail.contains(ACCESS_KEY));
}

#[tokio::test]
async fn test_connection_checks_the_whole_path_and_refuses_invalid_drafts_offline() {
    let h = harness(warehouse()).await;
    let ok = h
        .mgr
        .test(
            &profile("draft", serverless(RedshiftAuth::IamIdentity)),
            None,
        )
        .await
        .unwrap();
    assert!(ok.ok, "{}", ok.message);
    assert_eq!(
        ok.server_version.as_deref(),
        Some("Redshift · default-wg · dev · eu-west-1 · as the IAM identity")
    );
    assert_eq!(
        h.server.calls("ListDatabases")[0]["WorkgroupName"],
        "default-wg"
    );

    // dbUser on Serverless, a bad cluster id, a non-ARN secret: invalid input, no request.
    let before = h.server.all().len();
    for o in [
        serverless(RedshiftAuth::DbUser {
            db_user: "alice".into(),
        }),
        RedshiftOptions {
            target: RedshiftTarget::Provisioned {
                cluster_identifier: "-bad".into(),
            },
            ..provisioned(RedshiftAuth::IamIdentity)
        },
        provisioned(RedshiftAuth::Secret {
            secret_arn: "my-secret".into(),
        }),
    ] {
        let err = h.mgr.test(&profile("draft", o), None).await.unwrap_err();
        assert!(matches!(err, CoreError::Invalid(_)), "{err:?}");
    }
    let mut none = profile("draft", provisioned(RedshiftAuth::IamIdentity));
    none.redshift = None;
    assert!(matches!(
        h.mgr.test(&none, None).await,
        Err(CoreError::Invalid(_))
    ));
    assert_eq!(h.server.all().len(), before);
}

#[tokio::test]
async fn other_engines_accessors_refuse_a_redshift_profile() {
    let h = harness(warehouse()).await;
    h.profiles
        .upsert(profile("r1", provisioned(RedshiftAuth::IamIdentity)))
        .unwrap();
    let Err(err) = h.mgr.pg("r1", None).await else {
        panic!("a Redshift profile opened a Postgres pool")
    };
    assert!(err.to_string().contains("a Redshift connection"), "{err}");
    assert!(h.server.all().is_empty());
}

#[tokio::test]
async fn agent_backend_runs_redshift_sql_and_reads_its_catalog() {
    let h = harness(warehouse()).await;
    h.profiles
        .upsert(profile("r1", provisioned(RedshiftAuth::IamIdentity)))
        .unwrap();
    let ws = tempfile::tempdir().unwrap();
    let backend = AppToolBackend::new(
        h.mgr.clone(),
        Arc::new(WorkspaceFs::new(ws.path().join("w")).unwrap()),
    );

    let r = backend
        .execute_sql(
            "r1",
            Some("sales"),
            "SELECT 1 AS n",
            true,
            10,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)]]);
    assert_eq!(h.server.calls("ExecuteStatement")[0]["Database"], "sales");

    // A read-only agent call never submits a write.
    let before = h.server.count("ExecuteStatement");
    let err = backend
        .execute_sql(
            "r1",
            None,
            "DELETE FROM sales",
            true,
            10,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::PolicyDenied(_)), "{err:?}");
    assert_eq!(h.server.count("ExecuteStatement"), before);

    let meta = backend
        .schema_metadata("r1", None, Some("public"), 50)
        .await
        .unwrap();
    assert_eq!(meta.database, "dev");
    assert_eq!(meta.tables.len(), 1);
    assert_eq!(meta.tables[0].columns[0].data_type, "int8");
}
