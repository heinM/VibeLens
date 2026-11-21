//! An Iceberg connection whose SQL runs on Trino or Spark: the handle reads the engine secret
//! from the vault, sends SQL to the engine (with any catalog kind; an empty SQLite catalog
//! here), and deletes the Livy session when it closes.
#![cfg(feature = "iceberg")]

mod remote_common;

use std::sync::{Arc, OnceLock};

use remote_common::*;
use secrecy::SecretString;
use serde_json::{Value, json};
use vibelens_core::aws::AwsService;
use vibelens_core::iceberg::{IcebergDeps, IcebergHandle};
use vibelens_core::model::*;
use vibelens_core::ports::SecretVault;
use vibelens_core::vault::MemoryVault;

/// A profile built from JSON, so it keeps compiling when `ConnectionProfile` gains defaulted
/// fields.
fn profile(id: &str, dir: &std::path::Path, engine: Value) -> ConnectionProfile {
    serde_json::from_value(json!({
        "id": id,
        "name": format!("test {id}"),
        "engine": "iceberg",
        "host": "",
        "port": 0,
        "database": null,
        "auth": {"kind": "none"},
        "tunnel": null,
        "color": null,
        "createdAt": null,
        "updatedAt": null,
        "environment": "dev",
        "iceberg": {
            "catalog": {
                "kind": "sql",
                "uri": format!("sqlite://{}?mode=rwc", dir.join("catalog.db").display()),
                "warehouse": format!("file://{}", dir.join("wh").display()),
                "catalogName": "t",
            },
            "storage": {"kind": "local"},
            "queryEngine": engine,
            "defaultNamespace": "shop",
        },
    }))
    .unwrap()
}

async fn deps_with(key: &str, secret: &str) -> IcebergDeps {
    let vault = Arc::new(MemoryVault::new());
    vault
        .set(key, &SecretString::from(secret.to_string()))
        .await
        .unwrap();
    IcebergDeps {
        aws: AwsService::new(),
        vault,
    }
}

#[tokio::test]
async fn trino_runs_the_sql_with_the_secret_from_the_vault() {
    let base: Arc<OnceLock<String>> = Arc::default();
    let b = base.clone();
    let server = serve(Arc::new(move |req: &Req| match req.method.as_str() {
        "POST" => Resp::ok(&json!({"id": "q", "nextUri": format!("{}/v1/statement/x/1", b.get().unwrap()), "stats": {"state": "QUEUED"}})),
        _ => Resp::ok(&json!({"id": "q", "stats": {"state": "FINISHED"},
            "columns": [{"name": "n", "type": "integer"}], "data": [[1]]})),
    }))
    .await;
    base.set(server.url.clone()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let p = profile(
        "p-trino",
        dir.path(),
        json!({"kind": "trino", "url": server.url, "catalog": "lake", "user": "ana",
               "auth": {"kind": "bearer"}, "caCertPath": null}),
    );
    let deps = deps_with(&vault_keys::engine_secret("p-trino"), "jwt-abc").await;
    let h = IcebergHandle::connect(p, deps).await.unwrap();
    assert!(
        h.describe_target().contains("Trino (127.0.0.1:"),
        "{}",
        h.describe_target()
    );
    let r = h
        .execute_collect("SELECT n FROM t", ExecOptions::default())
        .await
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)]]);
    h.probe_engine().await.unwrap();
    let posts = server.calls("POST", "/v1/statement");
    assert_eq!(posts[0].headers["authorization"], "Bearer jwt-abc");
    assert_eq!(posts[0].headers["x-trino-schema"], "shop");
    assert_eq!(posts[1].body, "SELECT 1");
}

#[tokio::test]
async fn a_missing_engine_secret_fails_the_connect() {
    let dir = tempfile::tempdir().unwrap();
    let p = profile(
        "p-nosecret",
        dir.path(),
        json!({"kind": "trino", "url": "https://trino.example", "catalog": "lake", "user": "ana",
               "auth": {"kind": "basic", "username": "ana"}}),
    );
    let deps = deps_with("unrelated", "x").await;
    let err = IcebergHandle::connect(p, deps).await.err().unwrap();
    assert_eq!(err.code(), "auth", "{err}");
}

/// The profile file can be edited by hand: the rules a save applies run again on connect.
#[tokio::test]
async fn a_hand_edited_profile_with_a_cleartext_credential_does_not_connect() {
    let dir = tempfile::tempdir().unwrap();
    let p = profile(
        "p-clear",
        dir.path(),
        json!({"kind": "spark", "url": "http://livy.internal:8998", "catalog": "lake",
               "auth": {"kind": "basic", "username": "ana"}}),
    );
    let deps = deps_with(&vault_keys::engine_secret("p-clear"), "pw").await;
    let err = IcebergHandle::connect(p, deps).await.err().unwrap();
    assert_eq!(err.code(), "invalid", "{err}");
    assert!(err.to_string().contains("unencrypted"), "{err}");
}

#[tokio::test]
async fn closing_a_spark_connection_deletes_its_livy_session() {
    let server = serve(Arc::new(|req: &Req| {
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/sessions") => Resp::json(201, &json!({"id": 9, "state": "starting"})),
            ("GET", "/sessions/9") => Resp::ok(&json!({"id": 9, "state": "idle"})),
            ("POST", "/sessions/9/statements") => Resp::json(201, &json!({"id": 0})),
            ("GET", "/sessions/9/statements/0") => Resp::ok(&json!({"id": 0, "state": "available",
                "output": {"status": "ok", "data": {"application/json": {"schema": {"fields": []}, "data": []}}}})),
            ("DELETE", "/sessions/9") => Resp::ok(&json!({"msg": "deleted"})),
            _ => Resp::text(404, "unexpected"),
        }
    }))
    .await;
    let dir = tempfile::tempdir().unwrap();
    let p = profile(
        "p-spark",
        dir.path(),
        json!({"kind": "spark", "url": server.url, "catalog": "lake", "auth": {"kind": "none"},
               "sessionConf": {"spark.sql.shuffle.partitions": "4"}}),
    );
    let deps = deps_with("unrelated", "x").await;
    let h = IcebergHandle::connect(p, deps).await.unwrap();
    h.execute_collect("CREATE TABLE x (a int)", ExecOptions::default())
        .await
        .unwrap();
    h.close().await;
    assert_eq!(server.calls("DELETE", "/sessions/9").len(), 1);
}
