//! Cluster-mode behaviour against a real cluster.
//!
//! Skipped unless `VG_TEST_VALKEY_CLUSTER=host:port` points at any node of a (plaintext)
//! cluster, e.g. three local masters:
//!
//! ```text
//! for p in 7101 7102 7103; do redis-server --port $p --cluster-enabled yes ... & done
//! redis-cli --cluster create 127.0.0.1:7101 127.0.0.1:7102 127.0.0.1:7103 --cluster-yes
//! VG_TEST_VALKEY_CLUSTER=127.0.0.1:7101 cargo test -p vibelens-core --test valkey_cluster
//! ```
//!
//! The test writes keys prefixed `vgtest:` and deletes them at the end.

use std::collections::BTreeSet;
use std::sync::Arc;

use futures_util::FutureExt;
use serde_json::json;
use vibelens_core::CoreError;
use vibelens_core::model::*;
use vibelens_core::ports::CredentialFn;
use vibelens_core::valkey::{ValkeyConnectParams, ValkeyHandle};

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

async fn connect() -> Option<Arc<ValkeyHandle>> {
    let target = std::env::var("VG_TEST_VALKEY_CLUSTER")
        .ok()
        .filter(|s| !s.is_empty())?;
    let (host, port) = target
        .rsplit_once(':')
        .expect("VG_TEST_VALKEY_CLUSTER=host:port");
    let params = ValkeyConnectParams {
        host: host.into(),
        port: port.parse().expect("port"),
        tls_server_name: host.into(),
        tls: TlsConfig {
            mode: TlsMode::Disable,
            ca_cert_path: None,
            use_rds_bundle: false,
        },
        options: ValkeyOptions {
            cluster: true,
            connect_timeout_secs: 10,
            ..Default::default()
        },
    };
    let creds: CredentialFn = Arc::new(|| async { Ok(ResolvedCredentials::anonymous()) }.boxed());
    Some(
        ValkeyHandle::connect(params, creds)
            .await
            .expect("cluster connect"),
    )
}

async fn cleanup(h: &ValkeyHandle, keys: &[String]) {
    for k in keys {
        let _ = h.command(0, argv(&["DEL", k]), true).await;
    }
}

#[tokio::test]
async fn scans_every_primary_exactly_once() {
    let Some(h) = connect().await else {
        eprintln!("VG_TEST_VALKEY_CLUSTER not set; skipping");
        return;
    };

    let info = h.server_info().await.unwrap();
    assert_eq!(info.mode.as_deref(), Some("cluster"));

    let dbs = h.list_dbs().await.unwrap();
    assert_eq!(dbs.len(), 1);
    assert_eq!(dbs[0].index, 0);

    // Only db 0 exists
    assert!(matches!(
        h.scan(3, "*", "0", 10).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        h.get_value(1, "x", 10).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        h.command(2, argv(&["PING"]), true).await,
        Err(CoreError::Invalid(_))
    ));

    // Keys spread over all slots, hence over every primary.
    let keys: Vec<String> = (0..300).map(|i| format!("vgtest:{i:04}")).collect();
    for (i, k) in keys.iter().enumerate() {
        match i % 3 {
            0 => drop(h.command(0, argv(&["SET", k, "v"]), true).await.unwrap()),
            1 => drop(
                h.command(0, argv(&["HSET", k, "f", "v"]), true)
                    .await
                    .unwrap(),
            ),
            _ => drop(h.command(0, argv(&["RPUSH", k, "a"]), true).await.unwrap()),
        }
    }

    let mut seen = Vec::new();
    let mut cursor = "0".to_string();
    let mut nodes_visited = BTreeSet::new();
    let mut pages = 0;
    loop {
        let page = h.scan(0, "vgtest:*", &cursor, 25).await.unwrap();
        pages += 1;
        if !page.done {
            let (node, _) = page
                .cursor
                .split_once(':')
                .expect("cluster cursor is node:cursor");
            nodes_visited.insert(node.parse::<usize>().expect("node index"));
        }
        for k in page.keys {
            let i: usize = k.key["vgtest:".len()..].parse().unwrap();
            let want = [KeyType::String, KeyType::Hash, KeyType::List][i % 3];
            assert_eq!(k.key_type, Some(want), "{}", k.key);
            seen.push(k.key);
        }
        cursor = page.cursor;
        if page.done {
            break;
        }
        assert!(pages < 10_000);
    }
    let unique: BTreeSet<_> = seen.iter().cloned().collect();
    assert_eq!(seen.len(), unique.len(), "duplicates across nodes/pages");
    assert_eq!(unique.len(), 300);
    assert!(
        nodes_visited.len() >= 2,
        "scan must walk several primaries, visited {nodes_visited:?}"
    );

    // Key-routed reads and writes work through the cluster client.
    let v = h.get_value(0, "vgtest:0001", 10).await.unwrap();
    assert_eq!(v.key_type, KeyType::Hash);
    assert_eq!(v.value, json!({"f": "v"}));
    assert!(matches!(
        h.command(0, argv(&["SET", "vgtest:0000", "changed"]), false)
            .await
            .unwrap(),
        CommandOutcome::RequiresConfirmation { .. }
    ));
    match h
        .command(0, argv(&["GET", "vgtest:0000"]), false)
        .await
        .unwrap()
    {
        CommandOutcome::Ok { result, .. } => assert_eq!(result, json!("v")),
        other => panic!("{other:?}"),
    }
    assert!(
        !h.is_write_command(&argv(&["CONFIG", "GET", "maxmemory"]))
            .await
            .unwrap()
    );
    assert!(
        h.is_write_command(&argv(&["CONFIG", "SET", "maxmemory", "0"]))
            .await
            .unwrap()
    );

    // A cursor from a different topology is rejected, not silently restarted.
    assert!(matches!(
        h.scan(0, "*", "99:0", 10).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(
        matches!(h.scan(0, "*", "12", 10).await, Err(CoreError::Invalid(_))),
        "standalone cursor in cluster mode"
    );

    cleanup(&h, &keys).await;
}

/// TLS cluster: `VG_TEST_VALKEY_CLUSTER_TLS=host:port`, `VG_TEST_VALKEY_CLUSTER_CA=/path/ca.pem` and
/// `VG_TEST_VALKEY_CLUSTER_SNI=name` (the name on the certificate, which must also carry an IP
/// SAN for the addresses the cluster advertises). The seed is dialled at `host` but verified as
/// `SNI`.
#[tokio::test]
async fn tls_cluster_verifies_the_certificate_name_not_the_dial_host() {
    let (Some(target), Some(ca), Some(sni)) = (
        std::env::var("VG_TEST_VALKEY_CLUSTER_TLS")
            .ok()
            .filter(|s| !s.is_empty()),
        std::env::var("VG_TEST_VALKEY_CLUSTER_CA")
            .ok()
            .filter(|s| !s.is_empty()),
        std::env::var("VG_TEST_VALKEY_CLUSTER_SNI")
            .ok()
            .filter(|s| !s.is_empty()),
    ) else {
        eprintln!("VG_TEST_VALKEY_CLUSTER_TLS/CA/SNI not set; skipping");
        return;
    };
    let (host, port) = target.rsplit_once(':').expect("host:port");
    let mk = |mode: TlsMode, name: &str, ca: Option<&str>| ValkeyConnectParams {
        host: host.into(),
        port: port.parse().expect("port"),
        tls_server_name: name.into(),
        tls: TlsConfig {
            mode,
            ca_cert_path: ca.map(str::to_owned),
            use_rds_bundle: false,
        },
        options: ValkeyOptions {
            cluster: true,
            connect_timeout_secs: 5,
            ..Default::default()
        },
    };
    let creds: CredentialFn = Arc::new(|| async { Ok(ResolvedCredentials::anonymous()) }.boxed());

    let h = ValkeyHandle::connect(mk(TlsMode::VerifyFull, &sni, Some(&ca)), creds.clone())
        .await
        .expect("verify-full with the right name");
    h.command(0, argv(&["SET", "vgtest:tls", "1"]), true)
        .await
        .unwrap();
    assert_eq!(
        h.get_value(0, "vgtest:tls", 1).await.unwrap().value,
        json!("1")
    );
    let page = h.scan(0, "vgtest:tls", "0", 1000).await;
    assert!(page.is_ok(), "{page:?}");
    cleanup(&h, &["vgtest:tls".to_string()]).await;

    // A name that is not on the certificate must be rejected.
    let bad = ValkeyHandle::connect(
        mk(TlsMode::VerifyFull, "wrong.example", Some(&ca)),
        creds.clone(),
    )
    .await;
    assert!(bad.is_err(), "hostname mismatch must fail");
    // Untrusted CA must be rejected.
    let untrusted = ValkeyHandle::connect(mk(TlsMode::VerifyFull, &sni, None), creds.clone()).await;
    assert!(untrusted.is_err(), "untrusted certificate must fail");
    // `Require` needs redis built with `tls-rustls-insecure`; today it is a clear, typed error.
    match ValkeyHandle::connect(mk(TlsMode::Require, &sni, None), creds).await {
        Err(CoreError::Invalid(m)) => assert!(m.contains("require"), "{m}"),
        Ok(_) => {} // fine once the feature is enabled
        Err(other) => panic!("{other:?}"),
    }
}
