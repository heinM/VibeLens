//! SQL catalog on Postgres: password injection from the vault and `$n` bind style.
//! Gated on `VG_TEST_PG="host=… port=… user=… password=… dbname=…"`; uses a throwaway database.
#![cfg(feature = "iceberg")]

mod services;

mod iceberg_common;

use std::sync::Arc;

use iceberg_common::*;
use secrecy::SecretString;
use vibelens_core::aws::AwsService;
use vibelens_core::iceberg::{IcebergDeps, IcebergHandle};
use vibelens_core::model::*;
use vibelens_core::ports::SecretVault;
use vibelens_core::vault::MemoryVault;

fn kv(raw: &str, key: &str) -> Option<String> {
    raw.split_whitespace()
        .find_map(|p| p.strip_prefix(&format!("{key}=")).map(str::to_string))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn postgres_catalog_gets_its_password_from_the_vault() {
    let Some(raw) = std::env::var("VG_TEST_PG").ok().filter(|s| !s.is_empty()) else {
        services::skip("pg", "VG_TEST_PG is not set");
        return;
    };
    let (host, port, user, pw) = (
        kv(&raw, "host").unwrap_or("127.0.0.1".into()),
        kv(&raw, "port").unwrap_or("5432".into()),
        kv(&raw, "user").unwrap(),
        kv(&raw, "password").unwrap(),
    );
    let dbname = kv(&raw, "dbname").unwrap_or("postgres".into());
    let admin = format!("host={host} port={port} user={user} password={pw} dbname={dbname}");
    let (client, conn) = match tokio_postgres::connect(&admin, tokio_postgres::NoTls).await {
        Ok(x) => x,
        Err(e) => {
            services::skip(
                "pg",
                format!(
                    "Postgres could not be reached without TLS, which this test needs for its own setup ({e})"
                ),
            );
            return;
        }
    };
    tokio::spawn(conn);
    let scratch = format!("vg_iceberg_{}", std::process::id());
    client
        .batch_execute(&format!("DROP DATABASE IF EXISTS {scratch}"))
        .await
        .unwrap();
    client
        .batch_execute(&format!("CREATE DATABASE {scratch}"))
        .await
        .unwrap();

    let vault = Arc::new(MemoryVault::new());
    vault
        .set(
            &vault_keys::iceberg_secret("ice-pg"),
            &SecretString::from(pw.clone()),
        )
        .await
        .unwrap();
    let profile = base_profile(
        "ice-pg",
        IcebergOptions {
            catalog: IcebergCatalog::Sql {
                uri: format!("postgres://{user}@{host}:{port}/{scratch}"),
                warehouse: "file:///tmp/vg-iceberg-pg-warehouse".into(),
                catalog_name: "pgcat".into(),
            },
            storage: IcebergStorage::Local,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: None,
        },
    );
    let h = IcebergHandle::connect(
        profile.clone(),
        IcebergDeps {
            aws: AwsService::new(),
            vault,
        },
    )
    .await;
    let outcome = async {
        let h = h.as_ref().map_err(|e| e.to_string())?;
        assert_eq!(
            h.describe_target(),
            format!("SQL · {host}:{port}/{scratch} · Local engine")
        );
        let ns = h.list_namespaces(None).await.map_err(|e| e.to_string())?;
        assert!(ns.is_empty(), "fresh catalog");
        Ok::<(), String>(())
    }
    .await;

    // Without the password in the vault the server rejects us, and the message does not echo a URI with credentials.
    let no_pw = IcebergHandle::connect(
        profile,
        IcebergDeps {
            aws: AwsService::new(),
            vault: Arc::new(MemoryVault::new()),
        },
    )
    .await;

    drop(h);
    client
        .batch_execute(&format!("DROP DATABASE IF EXISTS {scratch} WITH (FORCE)"))
        .await
        .ok();
    outcome.unwrap();
    // pg_hba may be `trust` for local connections; only assert the leak rule.
    if let Err(e) = no_pw {
        assert!(!e.to_string().contains(&pw), "{e}");
    }
}
