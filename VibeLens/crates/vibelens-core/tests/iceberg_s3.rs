//! Iceberg tables on an S3-compatible store (MinIO, Ceph, R2, a local `moto_server`), read by
//! the local engine with static keys and **no region**: the setup the dev stack uses.
//! Regression: reads failed with "region is missing" because nothing supplied `s3.region`.
//!
//! Gated on `VG_TEST_S3_ENDPOINT` (e.g. `http://127.0.0.1:9555` for `moto_server -p 9555`, or
//! `http://localhost:9000` for the dev MinIO) with access key `admin` / secret `password`, plus a
//! `python3` with pyiceberg to write the tables. Skips cleanly otherwise.
#![cfg(feature = "iceberg")]

mod services;

mod iceberg_common;

use std::path::PathBuf;
use std::sync::Arc;

use iceberg_common::*;
use secrecy::SecretString;
use vibelens_core::iceberg::{IcebergDeps, IcebergHandle};
use vibelens_core::model::*;
use vibelens_core::ports::SecretVault;
use vibelens_core::vault::MemoryVault;

/// Seeds the demo tables into a fresh SQLite catalog whose warehouse is `s3://<bucket>/`.
fn seed_on_s3(endpoint: &str, bucket: &str, db: &std::path::Path) -> bool {
    let script =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/seed/iceberg-seed.py");
    let code = format!(
        r#"
import importlib.util, sys
spec = importlib.util.spec_from_file_location("seed", {script:?})
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
m.ensure_bucket({endpoint:?}, bucket={bucket:?}, timeout_s=20)
from pyiceberg.catalog import load_catalog
m.catalog = lambda args: load_catalog("vg_test", **{{
    "type": "sql", "uri": "sqlite:///" + {db:?}, "warehouse": "s3://{bucket}/",
    "s3.endpoint": {endpoint:?}, "s3.access-key-id": "admin", "s3.secret-access-key": "password",
    "s3.region": "us-east-1", "s3.path-style-access": "true",
}})
sys.argv = ["seed", "--sql", {db:?}, "--warehouse", {wh:?}]
m.main()
"#,
        script = script.to_string_lossy(),
        db = db.to_string_lossy(),
        wh = db
            .with_file_name("unused-local-warehouse")
            .to_string_lossy(),
    );
    match std::process::Command::new("python3")
        .arg("-c")
        .arg(&code)
        .output()
    {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            services::skip(
                "iceberg-seed",
                format!(
                    "the seed script could not write tables to S3: {}",
                    String::from_utf8_lossy(&o.stderr)
                ),
            );
            false
        }
        Err(e) => {
            services::skip("iceberg-seed", format!("python3 could not be run: {e}"));
            false
        }
    }
}

async fn s3_static_profile(
    endpoint: &str,
    bucket: &str,
    db: &std::path::Path,
    region: Option<&str>,
) -> (ConnectionProfile, IcebergDeps) {
    let profile = base_profile(
        "ice-s3",
        IcebergOptions {
            catalog: IcebergCatalog::Sql {
                uri: format!("sqlite://{}", db.display()),
                warehouse: format!("s3://{bucket}/"),
                catalog_name: "vg_test".into(),
            },
            storage: IcebergStorage::S3Static {
                endpoint: Some(endpoint.into()),
                region: region.map(str::to_string),
                path_style: true,
            },
            query_engine: IcebergQueryEngine::Local,
            default_namespace: Some("shop".into()),
        },
    );
    let vault = Arc::new(MemoryVault::new());
    vault
        .set(
            &vault_keys::s3_access_key_id("ice-s3"),
            &SecretString::from("admin"),
        )
        .await
        .unwrap();
    vault
        .set(
            &vault_keys::s3_secret_access_key("ice-s3"),
            &SecretString::from("password"),
        )
        .await
        .unwrap();
    (
        profile,
        IcebergDeps {
            aws: vibelens_core::aws::AwsService::new(),
            vault,
        },
    )
}

#[tokio::test]
async fn reads_from_s3_compatible_store_without_a_region() {
    let Some(endpoint) = std::env::var("VG_TEST_S3_ENDPOINT")
        .ok()
        .filter(|e| !e.is_empty())
    else {
        services::skip("s3", "VG_TEST_S3_ENDPOINT is not set");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("catalog.db");
    let bucket = format!("vg-{}", uuid::Uuid::new_v4().simple());
    if !seed_on_s3(&endpoint, &bucket, &db) {
        return;
    }

    for region in [None, Some(""), Some("us-east-1")] {
        let (profile, deps) = s3_static_profile(&endpoint, &bucket, &db, region).await;
        let h = IcebergHandle::connect(profile, deps)
            .await
            .expect("connect");
        let n = scalar(&h, "SELECT count(*) FROM shop.orders").await;
        assert_eq!(
            n.as_str().unwrap_or_default().trim_matches('"'),
            "3000",
            "region {region:?}: {n}"
        );
        let nested = collect(&h, "SELECT * FROM shop.events LIMIT 5").await;
        assert_eq!(nested.rows.len(), 5, "region {region:?}");
        h.close().await;
    }
}
