//! Writes through the local engine and snapshot maintenance. Each test seeds its own private
//! fixture with pyiceberg (the shared one is never modified).
#![cfg(feature = "iceberg")]

mod services;

mod iceberg_common;

use iceberg_common::*;
use serde_json::json;
use vibelens_core::model::*;

macro_rules! fixture {
    () => {
        match private_copy() {
            Some(f) => f,
            None => {
                // The fixture helper has reported the skip.
                return;
            }
        }
    };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn insert_appends_and_creates_a_snapshot() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let before = h
        .describe_table(vec!["shop".into()], "customers")
        .await
        .unwrap();
    assert_eq!(before.snapshots.len(), 1);

    let evs = events(&h, "INSERT INTO shop.customers VALUES (501, 'new1@example.com', 'ZA', false), (502, 'new2@example.com', 'NL', true)", ExecOptions::default()).await;
    assert!(failed(&evs).is_none(), "{evs:?}");
    let done = evs
        .iter()
        .find_map(|e| {
            if let QueryEvent::StatementDone {
                rows_affected,
                command_tag,
                ..
            } = e
            {
                Some((*rows_affected, command_tag.clone()))
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(done, (Some(2), Some("INSERT 0 2".into())));

    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.customers").await,
        json!("502")
    );
    let after = h
        .describe_table(vec!["shop".into()], "customers")
        .await
        .unwrap();
    assert_eq!(after.snapshots.len(), 2);
    assert_ne!(after.current_snapshot_id, before.current_snapshot_id);
    assert_eq!(after.snapshots[0].operation.as_deref(), Some("append"));
    assert_eq!(
        after.snapshots[0]
            .summary
            .get("added-records")
            .map(String::as_str),
        Some("2")
    );
    // An independent implementation (pyiceberg) agrees with what we committed.
    assert_eq!(pyiceberg_view(&f, "shop.customers"), Some((502, 2)));
    // The previous snapshot is still readable (time travel after a write).
    let old = &before.snapshots[0].snapshot_id;
    assert_eq!(
        scalar(
            &h,
            &format!("SELECT count(*) FROM shop.\"customers@{old}\"")
        )
        .await,
        json!("500")
    );
    // Appended rows are the real thing.
    let r = collect(
        &h,
        "SELECT id, email, vip FROM shop.customers WHERE id > 500 ORDER BY id",
    )
    .await;
    assert_eq!(
        r.rows,
        vec![
            vec![json!("501"), json!("new1@example.com"), json!(false)],
            vec![json!("502"), json!("new2@example.com"), json!(true)]
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn insert_select_into_a_partitioned_table() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let before = scalar(&h, "SELECT count(*) FROM shop.orders").await;
    assert_eq!(before, json!("3000"));
    let evs = events(&h, "INSERT INTO shop.orders SELECT id + 100000, customer_id, status, total, placed_at FROM shop.orders WHERE id <= 10", ExecOptions::default()).await;
    assert!(failed(&evs).is_none(), "{evs:?}");
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.orders").await,
        json!("3010")
    );
    let t = h
        .describe_table(vec!["shop".into()], "orders")
        .await
        .unwrap();
    assert_eq!(t.snapshots.len(), 5);
    assert_eq!(pyiceberg_view(&f, "shop.orders").map(|v| v.0), Some(3010));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_table_and_ctas() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let r = h
        .execute_collect(
            "CREATE TABLE shop.vip_customers AS SELECT id, email FROM shop.customers WHERE vip",
            ExecOptions::default(),
        )
        .await;
    match r {
        Ok(_) => {
            let n = scalar(&h, "SELECT count(*) FROM shop.vip_customers").await;
            assert_eq!(n, json!("29"), "customers with id % 17 == 0 among 1..=500");
            let t = h
                .describe_table(vec!["shop".into()], "vip_customers")
                .await
                .unwrap();
            assert_eq!(
                t.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
                ["id", "email"]
            );
            assert_eq!(t.fields[0].type_name, "long");
        }
        Err(e) => panic!("CTAS should work: {e}"),
    }
    h.execute_collect(
        "CREATE TABLE shop.scratch (a BIGINT, b VARCHAR)",
        ExecOptions::default(),
    )
    .await
    .expect("plain create table");
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.scratch").await,
        json!("0")
    );
    h.execute_collect(
        "INSERT INTO shop.scratch VALUES (1, 'x')",
        ExecOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.scratch").await,
        json!("1")
    );
    let names: Vec<_> = h
        .list_tables(vec!["shop".into()])
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(
        names.contains(&"scratch".to_string()) && names.contains(&"vip_customers".to_string()),
        "{names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expire_snapshots_keeps_the_newest_and_refs() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let before = h
        .describe_table(vec!["shop".into()], "orders")
        .await
        .unwrap();
    assert_eq!(before.snapshots.len(), 4);
    let future_cutoff = chrono::Utc::now().timestamp_millis() + 60_000;
    let r = h
        .expire_snapshots(vec!["shop".into()], "orders", Some(future_cutoff), None)
        .await
        .unwrap();
    assert_eq!(
        r,
        ExpireSnapshotsResult {
            expired: 3,
            remaining: 1
        }
    );
    let after = h
        .describe_table(vec!["shop".into()], "orders")
        .await
        .unwrap();
    assert_eq!(after.snapshots.len(), 1);
    assert_eq!(
        after.snapshots[0].snapshot_id, before.snapshots[0].snapshot_id,
        "the current snapshot survives"
    );
    assert_eq!(after.current_snapshot_id, before.current_snapshot_id);
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.orders").await,
        json!("3000"),
        "data is untouched"
    );
    assert_eq!(
        pyiceberg_view(&f, "shop.orders"),
        Some((3000, 1)),
        "pyiceberg reads the expired table"
    );
    // Idempotent.
    let again = h
        .expire_snapshots(vec!["shop".into()], "orders", Some(future_cutoff), None)
        .await
        .unwrap();
    assert_eq!(
        again,
        ExpireSnapshotsResult {
            expired: 0,
            remaining: 1
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn expire_snapshots_retain_last_and_default_age() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    // Default cutoff is now - 5 days: everything in the fixture is newer, so nothing expires.
    let r = h
        .expire_snapshots(vec!["shop".into()], "orders", None, None)
        .await
        .unwrap();
    assert_eq!(
        r,
        ExpireSnapshotsResult {
            expired: 0,
            remaining: 4
        }
    );
    // retain_last keeps the newest N of the main lineage even when older than the cutoff.
    let future_cutoff = chrono::Utc::now().timestamp_millis() + 60_000;
    let r = h
        .expire_snapshots(vec!["shop".into()], "orders", Some(future_cutoff), Some(2))
        .await
        .unwrap();
    assert_eq!(r.remaining, 2);
    assert_eq!(r.expired, 2);
    let e = h
        .expire_snapshots(vec!["shop".into()], "orders", None, Some(0))
        .await
        .unwrap_err();
    assert_eq!(e.code(), "invalid");
}
