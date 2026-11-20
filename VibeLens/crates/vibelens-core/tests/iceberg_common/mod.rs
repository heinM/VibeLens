//! Shared helpers for the `iceberg_*` integration tests.
//!
//! The tests run against Iceberg tables written by pyiceberg (see `scripts/seed/iceberg-seed.py`).
//! Set `VG_TEST_ICEBERG_SQL_DB` (SQLite catalog file) and `VG_TEST_ICEBERG_WAREHOUSE` (warehouse
//! directory); without them every test skips (`crate::services::skip`, service `iceberg`). Tests that write regenerate a private copy
//! with the seed script, because the metadata files embed absolute paths and cannot be relocated.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vibelens_core::ErrorPayload;
use vibelens_core::aws::AwsService;
use vibelens_core::iceberg::{IcebergDeps, IcebergHandle};
use vibelens_core::model::*;
use vibelens_core::vault::MemoryVault;

pub struct Fixture {
    pub db: String,
    pub warehouse: String,
    /// Keeps a private copy alive.
    pub _tmp: Option<tempfile::TempDir>,
}

/// The shared read-only fixture, or `None` after reporting the skip: the test returns.
pub fn shared() -> Option<Fixture> {
    let db = std::env::var("VG_TEST_ICEBERG_SQL_DB").unwrap_or_default();
    let warehouse = std::env::var("VG_TEST_ICEBERG_WAREHOUSE").unwrap_or_default();
    if db.is_empty() || warehouse.is_empty() {
        crate::services::skip(
            "iceberg",
            "VG_TEST_ICEBERG_SQL_DB and VG_TEST_ICEBERG_WAREHOUSE are not both set",
        );
        return None;
    }
    Some(Fixture {
        db,
        warehouse,
        _tmp: None,
    })
}

/// A private, freshly seeded fixture that tests may write to. `None` after reporting the skip
/// when the env is unset or pyiceberg is not available: the test returns.
pub fn private_copy() -> Option<Fixture> {
    shared()?;
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/seed/iceberg-seed.py");
    let tmp = tempfile::tempdir().expect("tempdir");
    let db = tmp.path().join("catalog.db");
    let wh = tmp.path().join("warehouse");
    let out = std::process::Command::new("python3")
        .arg(&script)
        .arg("--sql")
        .arg(&db)
        .arg("--warehouse")
        .arg(&wh)
        .output();
    let out = match out {
        Ok(out) => out,
        Err(e) => {
            crate::services::skip("iceberg-seed", format!("python3 could not be run: {e}"));
            return None;
        }
    };
    if !out.status.success() {
        crate::services::skip(
            "iceberg-seed",
            format!(
                "the seed script could not write a private fixture: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
        );
        return None;
    }
    Some(Fixture {
        db: db.to_string_lossy().into_owned(),
        warehouse: wh.to_string_lossy().into_owned(),
        _tmp: Some(tmp),
    })
}

pub fn base_profile(id: &str, options: IcebergOptions) -> ConnectionProfile {
    ConnectionProfile {
        id: id.into(),
        name: format!("test {id}"),
        engine: EngineKind::Iceberg,
        host: String::new(),
        port: 0,
        database: None,
        auth: AuthMethod::None,
        tls: TlsConfig::default(),
        tunnel: None,
        environment: Environment::Dev,
        ai_data_access: AiDataAccess::SchemaOnly,
        color: None,
        pg: PgOptions::default(),
        valkey: ValkeyOptions::default(),
        created_at: None,
        updated_at: None,
        iceberg: Some(options),
    }
}

pub fn sql_profile(f: &Fixture) -> ConnectionProfile {
    base_profile(
        "ice-sql",
        IcebergOptions {
            catalog: IcebergCatalog::Sql {
                uri: format!("sqlite://{}", f.db),
                warehouse: format!("file://{}", f.warehouse),
                catalog_name: "vg_test".into(),
            },
            storage: IcebergStorage::Local,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: Some("shop".into()),
        },
    )
}

pub fn deps() -> IcebergDeps {
    IcebergDeps {
        aws: AwsService::new(),
        vault: Arc::new(MemoryVault::new()),
    }
}

pub async fn connect_sql(f: &Fixture) -> Arc<IcebergHandle> {
    IcebergHandle::connect(sql_profile(f), deps())
        .await
        .expect("connect")
}

/// Runs a script and returns every event (Started … Finished|Failed).
pub async fn events(h: &IcebergHandle, sql: &str, opts: ExecOptions) -> Vec<QueryEvent> {
    let (tx, mut rx) = mpsc::channel(1024);
    let cancel = CancellationToken::new();
    let run = h.execute_stream("q".into(), sql, opts, cancel, tx);
    let collect = async {
        let mut v = Vec::new();
        while let Some(e) = rx.recv().await {
            v.push(e);
        }
        v
    };
    let (r, v) = tokio::join!(run, collect);
    r.expect("execute_stream");
    v
}

pub fn failed(evs: &[QueryEvent]) -> Option<ErrorPayload> {
    evs.iter().find_map(|e| match e {
        QueryEvent::Failed { error, .. } => Some(error.clone()),
        _ => None,
    })
}

pub async fn collect(h: &IcebergHandle, sql: &str) -> QueryResult {
    h.execute_collect(sql, ExecOptions::default())
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

pub async fn scalar(h: &IcebergHandle, sql: &str) -> serde_json::Value {
    collect(h, sql)
        .await
        .rows
        .first()
        .and_then(|r| r.first().cloned())
        .expect("one cell")
}

pub fn props(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Reads `table` with pyiceberg (an independent implementation): `(row count, snapshot count)`.
pub fn pyiceberg_view(f: &Fixture, table: &str) -> Option<(u64, u64)> {
    let code = format!(
        "import sys\nfrom pyiceberg.catalog import load_catalog\nc=load_catalog('vg_test', type='sql', uri='sqlite:///{}', warehouse='file://{}')\nt=c.load_table('{}')\nprint(len(t.scan().to_arrow()), len(t.snapshots()))",
        f.db, f.warehouse, table
    );
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(code)
        .output()
        .ok()?;
    if !out.status.success() {
        eprintln!("pyiceberg: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.split_whitespace().map(|v| v.parse::<u64>().ok());
    Some((it.next()??, it.next()??))
}

pub mod mock_rest;
