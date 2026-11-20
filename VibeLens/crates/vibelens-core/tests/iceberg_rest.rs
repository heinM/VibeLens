//! REST catalog against an in-process mock server: auth flows, headers, table loading from the
//! pyiceberg fixture, and the "profile storage settings beat catalog-vended ones" rule.
#![cfg(feature = "iceberg")]

mod services;

mod iceberg_common;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use iceberg_common::mock_rest::MockRest;
use iceberg_common::*;
use secrecy::SecretString;
use serde_json::json;
use vibelens_core::aws::AwsService;
use vibelens_core::iceberg::{IcebergDeps, IcebergHandle};
use vibelens_core::model::*;
use vibelens_core::ports::SecretVault;
use vibelens_core::vault::MemoryVault;

macro_rules! fixture {
    () => {
        match shared() {
            Some(f) => f,
            None => {
                // The fixture helper has reported the skip.
                return;
            }
        }
    };
}

fn rest_profile(
    uri: &str,
    auth: RestAuth,
    storage: IcebergStorage,
    headers: BTreeMap<String, String>,
) -> ConnectionProfile {
    base_profile(
        "ice-rest",
        IcebergOptions {
            catalog: IcebergCatalog::Rest {
                uri: uri.into(),
                warehouse: Some("wh".into()),
                auth,
                headers,
            },
            storage,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: Some("shop".into()),
        },
    )
}

async fn deps_with_secret(secret: Option<&str>) -> IcebergDeps {
    let vault = Arc::new(MemoryVault::new());
    if let Some(s) = secret {
        vault
            .set(
                &vault_keys::iceberg_secret("ice-rest"),
                &SecretString::from(s.to_string()),
            )
            .await
            .unwrap();
    }
    IcebergDeps {
        aws: AwsService::new(),
        vault,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn oauth2_client_credentials_then_bearer_on_every_call_and_select_over_files() {
    let f = fixture!();
    let mock = MockRest::start(&f.warehouse, true, &[]).await;
    let auth = RestAuth::OAuth2 {
        client_id: "cid".into(),
        scope: Some("PRINCIPAL_ROLE:ALL".into()),
        server_uri: None,
    };
    let h = IcebergHandle::connect(
        rest_profile(&mock.uri, auth, IcebergStorage::Local, BTreeMap::new()),
        deps_with_secret(Some("s3cr3t")).await,
    )
    .await
    .expect("connect");
    assert_eq!(
        h.describe_target(),
        format!(
            "REST · {} · Local engine",
            mock.uri.trim_start_matches("http://")
        )
    );

    let ns = h.list_namespaces(None).await.unwrap();
    assert_eq!(
        ns.iter().map(|n| n.levels.clone()).collect::<Vec<_>>(),
        vec![vec!["shop".to_string()]]
    );
    assert_eq!(
        ns[0].properties.get("owner").map(String::as_str),
        Some("mock")
    );
    let tables = h.list_tables(vec!["shop".into()]).await.unwrap();
    assert_eq!(
        tables.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["customers", "orders"]
    );
    let t = h
        .describe_table(vec!["shop".into()], "orders")
        .await
        .unwrap();
    assert_eq!(t.snapshots.len(), 4);
    assert_eq!(t.partition_spec[0].transform, "day");

    // SELECT reads the Parquet files straight from the local warehouse.
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM orders").await,
        json!("3000")
    );
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.customers").await,
        json!("500")
    );

    // The OAuth2 exchange: client-credentials grant with our id, secret and scope.
    let tokens = mock.token_requests();
    assert_eq!(tokens.len(), 1, "one exchange, token cached afterwards");
    let form: BTreeMap<String, String> = url::form_urlencoded::parse(tokens[0].body.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("client_credentials")
    );
    assert_eq!(form.get("client_id").map(String::as_str), Some("cid"));
    assert_eq!(
        form.get("client_secret").map(String::as_str),
        Some("s3cr3t")
    );
    assert_eq!(
        form.get("scope").map(String::as_str),
        Some("PRINCIPAL_ROLE:ALL")
    );
    // Every catalog request carries the issued token.
    let others: Vec<_> = mock
        .requests()
        .into_iter()
        .filter(|r| !r.path.starts_with("/v1/oauth/tokens"))
        .collect();
    assert!(others.len() >= 5);
    for r in &others {
        assert_eq!(
            r.headers.get("authorization").map(String::as_str),
            Some("Bearer tok-1"),
            "{} {}",
            r.method,
            r.path
        );
    }
    // The secret never shows in diagnostics.
    let dbg = format!(
        "{:?}",
        h.debug_file_io_props(vec!["shop".into()], "orders")
            .await
            .unwrap()
    );
    assert!(!dbg.contains("s3cr3t"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_token_is_renewed_once_and_the_call_retried() {
    let f = fixture!();
    let mock = MockRest::start(&f.warehouse, true, &[]).await;
    let auth = RestAuth::OAuth2 {
        client_id: "cid".into(),
        scope: None,
        server_uri: None,
    };
    let h = IcebergHandle::connect(
        rest_profile(&mock.uri, auth, IcebergStorage::Local, BTreeMap::new()),
        deps_with_secret(Some("s3cr3t")).await,
    )
    .await
    .unwrap();
    assert_eq!(mock.token_requests().len(), 1);
    mock.revoke_tokens();
    tokio::time::sleep(Duration::from_millis(3300)).await; // the catalog won't re-authenticate in a tight loop
    let tables = h
        .list_tables(vec!["shop".into()])
        .await
        .expect("retried with a fresh token");
    assert_eq!(tables.len(), 2);
    assert_eq!(mock.token_requests().len(), 2);
    let last = mock
        .requests()
        .into_iter()
        .rev()
        .find(|r| r.path.ends_with("/tables"))
        .unwrap();
    assert_eq!(
        last.headers.get("authorization").map(String::as_str),
        Some("Bearer tok-2")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_token_and_custom_headers() {
    let f = fixture!();
    let mock = MockRest::start(&f.warehouse, true, &[]).await;
    mock.accept_token("static-pat");
    let headers: BTreeMap<String, String> = props(&[("X-Tenant", "acme")]);
    let h = IcebergHandle::connect(
        rest_profile(&mock.uri, RestAuth::Bearer, IcebergStorage::Local, headers),
        deps_with_secret(Some("static-pat")).await,
    )
    .await
    .unwrap();
    h.list_tables(vec!["shop".into()]).await.unwrap();
    assert!(
        mock.token_requests().is_empty(),
        "a static token needs no exchange"
    );
    for r in mock.requests() {
        assert_eq!(
            r.headers.get("authorization").map(String::as_str),
            Some("Bearer static-pat")
        );
        assert_eq!(
            r.headers.get("x-tenant").map(String::as_str),
            Some("acme"),
            "{}",
            r.path
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn anonymous_catalogs_send_no_authorization() {
    let f = fixture!();
    let mock = MockRest::start(&f.warehouse, false, &[]).await;
    let h = IcebergHandle::connect(
        rest_profile(
            &mock.uri,
            RestAuth::None,
            IcebergStorage::Local,
            BTreeMap::new(),
        ),
        deps_with_secret(None).await,
    )
    .await
    .unwrap();
    h.list_namespaces(None).await.unwrap();
    assert!(
        mock.requests()
            .iter()
            .all(|r| !r.headers.contains_key("authorization"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn missing_secret_is_an_auth_error() {
    let f = fixture!();
    let mock = MockRest::start(&f.warehouse, true, &[]).await;
    let e = IcebergHandle::connect(
        rest_profile(
            &mock.uri,
            RestAuth::Bearer,
            IcebergStorage::Local,
            BTreeMap::new(),
        ),
        deps_with_secret(None).await,
    )
    .await
    .err()
    .expect("must fail");
    assert_eq!(e.code(), "auth");
    assert_eq!(e.to_string(), "authentication failed: no token saved");
    assert!(mock.requests().is_empty(), "no request without credentials");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rejected_credentials_are_an_auth_error() {
    let f = fixture!();
    let mock = MockRest::start(&f.warehouse, true, &[]).await; // accepts only tokens it issues
    let e = IcebergHandle::connect(
        rest_profile(
            &mock.uri,
            RestAuth::Bearer,
            IcebergStorage::Local,
            BTreeMap::new(),
        ),
        deps_with_secret(Some("wrong")).await,
    )
    .await
    .err()
    .expect("must fail");
    assert_eq!(e.code(), "auth", "{e}");
    assert!(!e.to_string().contains("wrong"), "{e}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn profile_storage_settings_override_what_the_catalog_vends() {
    let f = fixture!();
    // The server vends a MinIO endpoint that only resolves inside Docker.
    let vended = [
        ("s3.endpoint", "http://minio:9000"),
        ("s3.path-style-access", "false"),
        ("s3.region", "vended-region"),
    ];
    let mock = MockRest::start(&f.warehouse, false, &vended).await;

    // 1. Auto: whatever the server vends applies.
    let h = IcebergHandle::connect(
        rest_profile(
            &mock.uri,
            RestAuth::None,
            IcebergStorage::Auto,
            BTreeMap::new(),
        ),
        deps_with_secret(None).await,
    )
    .await
    .unwrap();
    let p = h
        .debug_file_io_props(vec!["shop".into()], "orders")
        .await
        .unwrap();
    assert_eq!(
        p.get("s3.endpoint").map(String::as_str),
        Some("http://minio:9000")
    );
    assert_eq!(
        p.get("s3.region").map(String::as_str),
        Some("vended-region")
    );

    // 2. Explicit S3 settings win over the vended ones, key by key; untouched keys still come from the server.
    let storage = IcebergStorage::S3 {
        aws: None,
        endpoint: Some("http://localhost:9000".into()),
        region: None,
        path_style: true,
    };
    let h = IcebergHandle::connect(
        rest_profile(&mock.uri, RestAuth::None, storage, BTreeMap::new()),
        deps_with_secret(None).await,
    )
    .await
    .unwrap();
    let p = h
        .debug_file_io_props(vec!["shop".into()], "orders")
        .await
        .unwrap();
    assert_eq!(
        p.get("s3.endpoint").map(String::as_str),
        Some("http://localhost:9000"),
        "{p:?}"
    );
    assert_eq!(
        p.get("s3.path-style-access").map(String::as_str),
        Some("true")
    );
    assert_eq!(
        p.get("s3.region").map(String::as_str),
        Some("vended-region")
    );

    // 3. Static keys from the vault, with region and endpoint from the profile.
    let vault = Arc::new(MemoryVault::new());
    vault
        .set(
            &vault_keys::s3_access_key_id("ice-rest"),
            &SecretString::from("minio".to_string()),
        )
        .await
        .unwrap();
    vault
        .set(
            &vault_keys::s3_secret_access_key("ice-rest"),
            &SecretString::from("minio123".to_string()),
        )
        .await
        .unwrap();
    let storage = IcebergStorage::S3Static {
        endpoint: Some("http://127.0.0.1:9000".into()),
        region: Some("us-east-1".into()),
        path_style: true,
    };
    let deps = IcebergDeps {
        aws: AwsService::new(),
        vault,
    };
    let h = IcebergHandle::connect(
        rest_profile(&mock.uri, RestAuth::None, storage, BTreeMap::new()),
        deps,
    )
    .await
    .unwrap();
    let p = h
        .debug_file_io_props(vec!["shop".into()], "orders")
        .await
        .unwrap();
    assert_eq!(
        p.get("s3.endpoint").map(String::as_str),
        Some("http://127.0.0.1:9000")
    );
    assert_eq!(p.get("s3.region").map(String::as_str), Some("us-east-1"));
    assert_eq!(
        p.get("s3.access-key-id").map(String::as_str),
        Some("***"),
        "masked in diagnostics"
    );
    assert!(p.contains_key("s3.secret-access-key"));

    // 4. S3Static without saved keys is an auth error, not a silent anonymous connection.
    let storage = IcebergStorage::S3Static {
        endpoint: None,
        region: None,
        path_style: false,
    };
    let e = IcebergHandle::connect(
        rest_profile(&mock.uri, RestAuth::None, storage, BTreeMap::new()),
        deps_with_secret(None).await,
    )
    .await
    .err()
    .unwrap();
    assert_eq!(e.code(), "auth");
}
