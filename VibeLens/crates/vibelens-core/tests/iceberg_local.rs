//! Local (DataFusion) engine + metadata against pyiceberg-written tables in a SQL (SQLite) catalog.
//! Gated on `VG_TEST_ICEBERG_SQL_DB` and `VG_TEST_ICEBERG_WAREHOUSE`.
#![cfg(feature = "iceberg")]

mod iceberg_common;

use iceberg_common::*;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vibelens_core::iceberg::IcebergHandle;
use vibelens_core::model::*;

macro_rules! fixture {
    () => {
        match shared() {
            Some(f) => f,
            None => {
                eprintln!("skipping: VG_TEST_ICEBERG_SQL_DB / VG_TEST_ICEBERG_WAREHOUSE not set");
                return;
            }
        }
    };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connects_and_lists_namespaces_and_tables() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    assert_eq!(h.describe_target(), "SQL · catalog.db · Local engine");
    let ns = h.list_namespaces(None).await.unwrap();
    let shop = ns
        .iter()
        .find(|n| n.levels == ["shop"])
        .expect("shop namespace");
    assert_eq!(
        shop.properties.get("owner").map(String::as_str),
        Some("vibelens-demo")
    );
    let tables = h.list_tables(vec!["shop".into()]).await.unwrap();
    let names: Vec<_> = tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["customers", "events", "orders"]);
    assert!(
        tables
            .iter()
            .all(|t| t.kind == IcebergObjectKind::Table && t.namespace == ["shop"])
    );
    assert!(h.idle_secs() < 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn describes_orders() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let t = h
        .describe_table(vec!["shop".into()], "orders")
        .await
        .unwrap();
    assert_eq!(t.name, "orders");
    assert_eq!(
        t.snapshots.len(),
        4,
        "{:#?}",
        t.snapshots
            .iter()
            .map(|s| (&s.snapshot_id, &s.operation))
            .collect::<Vec<_>>()
    );
    // newest first, ids are strings
    assert!(
        t.snapshots
            .windows(2)
            .all(|w| w[0].timestamp_ms >= w[1].timestamp_ms)
    );
    assert!(
        t.snapshots
            .iter()
            .all(|s| s.snapshot_id.parse::<i64>().is_ok())
    );
    assert_eq!(
        t.current_snapshot_id.as_deref(),
        Some(t.snapshots[0].snapshot_id.as_str())
    );
    assert!(
        t.snapshots
            .iter()
            .any(|s| s.operation.as_deref() == Some("overwrite")
                || s.operation.as_deref() == Some("delete"))
    );
    assert!(t.snapshots.iter().all(
        |s| s.summary.contains_key("total-records") || s.operation.as_deref() == Some("delete")
    ));
    // partition spec
    assert_eq!(t.partition_spec.len(), 1);
    assert_eq!(t.partition_spec[0].transform, "day");
    assert_eq!(t.partition_spec[0].name, "placed_day");
    assert_eq!(t.partition_spec[0].source_id, 5);
    // schema
    let total = t.fields.iter().find(|f| f.name == "total").unwrap();
    assert_eq!(total.type_name, "decimal(12, 2)");
    assert_eq!(total.id, 4);
    assert!(!total.required);
    let id = t.fields.iter().find(|f| f.name == "id").unwrap();
    assert_eq!((id.type_name.as_str(), id.required), ("long", true));
    assert_eq!(
        t.fields
            .iter()
            .find(|f| f.name == "placed_at")
            .unwrap()
            .type_name,
        "timestamptz"
    );
    assert_eq!(t.current_schema_id, 0);
    assert!(t.schema_count >= 1);
    assert_eq!(t.format_version, 2);
    assert!(t.table_uuid.is_some() && t.metadata_location.is_some() && t.last_updated_ms.is_some());
    assert!(
        t.refs.iter().any(|r| r.name == "main"
            && r.kind == "branch"
            && Some(&r.snapshot_id) == t.current_snapshot_id.as_ref()),
        "{:?}",
        t.refs
    );
    assert_eq!(
        t.properties.get("write.format.default").map(String::as_str),
        Some("parquet")
    );
    assert!(t.location.contains("orders"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn describes_nested_events() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let t = h
        .describe_table(vec!["shop".into()], "events")
        .await
        .unwrap();
    let ctx = t.fields.iter().find(|f| f.name == "ctx").unwrap();
    assert_eq!(ctx.type_name, "struct<device: string, app_version: int>");
    assert_eq!(
        ctx.children
            .iter()
            .map(|c| (c.id, c.name.as_str()))
            .collect::<Vec<_>>(),
        [(8, "device"), (9, "app_version")]
    );
    let tags = t.fields.iter().find(|f| f.name == "tags").unwrap();
    assert_eq!(tags.type_name, "list<string>");
    assert_eq!(
        (tags.children[0].id, tags.children[0].name.as_str()),
        (10, "element")
    );
    let attrs = t.fields.iter().find(|f| f.name == "attrs").unwrap();
    assert_eq!(attrs.type_name, "map<string, string>");
    assert_eq!(
        attrs
            .children
            .iter()
            .map(|c| (c.id, c.name.as_str()))
            .collect::<Vec<_>>(),
        [(11, "key"), (12, "value")]
    );
    let transforms: Vec<_> = t
        .partition_spec
        .iter()
        .map(|p| p.transform.as_str())
        .collect();
    assert_eq!(transforms, ["bucket[8]", "identity"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn schema_metadata_maps_every_table() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let m = h.schema_metadata(None, 100).await.unwrap();
    assert!(!m.truncated);
    let names: Vec<_> = m
        .tables
        .iter()
        .map(|t| format!("{}.{}", t.schema, t.name))
        .collect();
    assert_eq!(
        names,
        [
            "history.branched",
            "history.rolled_back",
            "shop.customers",
            "shop.events",
            "shop.orders"
        ]
    );
    let orders = m.tables.iter().find(|t| t.name == "orders").unwrap();
    assert_eq!(orders.kind, ObjectKind::PartitionedTable);
    assert_eq!(orders.estimated_rows, Some(3000));
    let total = orders.columns.iter().find(|c| c.name == "total").unwrap();
    assert_eq!(
        (total.data_type.as_str(), total.nullable, total.ordinal),
        ("decimal(12, 2)", true, 4)
    );
    let customers = m.tables.iter().find(|t| t.name == "customers").unwrap();
    assert!(
        customers
            .columns
            .iter()
            .find(|c| c.name == "id")
            .unwrap()
            .is_primary_key
    );
    let capped = h.schema_metadata(None, 2).await.unwrap();
    assert!(capped.truncated);
    assert_eq!(capped.tables.len(), 2);
    let only = h
        .schema_metadata(Some(vec!["shop".into()]), 100)
        .await
        .unwrap();
    assert_eq!(only.tables.len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn counts_and_time_travel() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.orders").await,
        json!("3000")
    );
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM orders").await,
        json!("3000"),
        "default namespace"
    );
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM iceberg.shop.customers").await,
        json!("500")
    );
    let t = h
        .describe_table(vec!["shop".into()], "orders")
        .await
        .unwrap();
    let first = &t.snapshots.last().unwrap().snapshot_id;
    assert_eq!(
        t.snapshots.last().unwrap().operation.as_deref(),
        Some("append")
    );
    assert_eq!(
        scalar(&h, &format!("SELECT count(*) FROM shop.\"orders@{first}\"")).await,
        json!("2000")
    );
    // by branch name and by timestamp
    let main = scalar(&h, "SELECT count(*) FROM shop.\"orders@main\"").await;
    assert_eq!(main, json!("3000"));
    let ts = chrono::DateTime::from_timestamp_millis(t.snapshots.last().unwrap().timestamp_ms)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    assert_eq!(
        scalar(&h, &format!("SELECT count(*) FROM shop.\"orders@{ts}\"")).await,
        json!("2000")
    );
    // every snapshot is addressable and the counts differ
    let mut counts = Vec::new();
    for s in &t.snapshots {
        let v = scalar(
            &h,
            &format!("SELECT count(*) FROM shop.\"orders@{}\"", s.snapshot_id),
        )
        .await;
        counts.push(v.as_str().unwrap().parse::<u64>().unwrap());
    }
    assert!(
        counts.contains(&3500) && counts.contains(&2000) && counts.contains(&3000),
        "{counts:?}"
    );
    // unknown snapshot is a clear error
    let e = h
        .execute_collect("SELECT * FROM shop.\"orders@42\"", ExecOptions::default())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("42"), "{e}");
    // metadata tables
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.\"orders$snapshots\"").await,
        json!("4")
    );
}

/// The snapshots of a `history` fixture table, oldest first. These tables were added to the
/// seed script later, so an older fixture does not have them.
async fn history_snapshots(h: &IcebergHandle, table: &str) -> Vec<IcebergSnapshot> {
    let t = h
        .describe_table(vec!["history".into()], table)
        .await
        .unwrap_or_else(|e| {
            panic!("history.{table} is missing: seed the fixture again with scripts/seed/iceberg-seed.py ({e})")
        });
    let mut snapshots = t.snapshots;
    snapshots.sort_by_key(|s| s.timestamp_ms);
    assert_eq!(snapshots.len(), 3, "{table}");
    assert!(
        snapshots[0].timestamp_ms < snapshots[1].timestamp_ms
            && snapshots[1].timestamp_ms + 1 < snapshots[2].timestamp_ms,
        "{table}: the fixture's commits are some milliseconds apart"
    );
    snapshots
}

/// `SELECT count(*)` of a `history` table as it was at `ms`.
async fn count_at(h: &IcebergHandle, table: &str, ms: i64) -> Value {
    let at = chrono::DateTime::from_timestamp_millis(ms)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    scalar(h, &format!("SELECT count(*) FROM history.\"{table}@{at}\"")).await
}

/// `history.rolled_back`: 100 rows, then 150, then rolled back to the first snapshot, then 10
/// rows on top of that. In the time between the rollback and the last commit the table held 100
/// rows, although the newest snapshot from before that time is the one with 150 (finding M8).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timestamp_after_a_rollback_reads_what_the_table_was_rolled_back_to() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let s = history_snapshots(&h, "rolled_back").await;
    assert_eq!(
        s[2].parent_snapshot_id.as_deref(),
        Some(s[0].snapshot_id.as_str()),
        "the last commit sits on the first snapshot, not on the second"
    );
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM history.rolled_back").await,
        json!("110")
    );
    assert_eq!(
        count_at(&h, "rolled_back", s[2].timestamp_ms - 1).await,
        json!("100"),
        "one millisecond before the last commit the table was rolled back to its first snapshot"
    );
    // Before the rollback the second snapshot was current, although it is not an ancestor of
    // what is current now.
    assert_eq!(
        count_at(&h, "rolled_back", s[1].timestamp_ms).await,
        json!("150")
    );
    assert_eq!(
        count_at(&h, "rolled_back", s[0].timestamp_ms).await,
        json!("100")
    );
    assert_eq!(
        count_at(&h, "rolled_back", s[2].timestamp_ms).await,
        json!("110")
    );
    // The snapshot itself is still there to be read by id.
    assert_eq!(
        scalar(
            &h,
            &format!(
                "SELECT count(*) FROM history.\"rolled_back@{}\"",
                s[1].snapshot_id
            )
        )
        .await,
        json!("150")
    );
    // Before the table had any snapshot: an error that says so.
    let before = chrono::DateTime::from_timestamp_millis(s[0].timestamp_ms - 1)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let e = h
        .execute_collect(
            &format!("SELECT count(*) FROM history.\"rolled_back@{before}\""),
            ExecOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no snapshot at or before"), "{e}");
}

/// `history.branched`: 100 rows on main, 50 more on the branch `audit` only, then 10 more on
/// main. The branch's snapshot was never the table's current one, so no time selects it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timestamp_never_reads_a_snapshot_of_another_branch() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let s = history_snapshots(&h, "branched").await;
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM history.branched").await,
        json!("110")
    );
    assert_eq!(
        count_at(&h, "branched", s[2].timestamp_ms - 1).await,
        json!("100"),
        "after the commit on the branch and before the next one on main, main still had 100 rows"
    );
    assert_eq!(
        count_at(&h, "branched", s[1].timestamp_ms).await,
        json!("100"),
        "the instant of the branch's commit"
    );
    assert_eq!(
        count_at(&h, "branched", s[2].timestamp_ms).await,
        json!("110")
    );
    // The branch is read by its name.
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM history.\"branched@audit\"").await,
        json!("150")
    );
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM history.\"branched@main\"").await,
        json!("110")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nested_values_arrive_as_json() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let r = collect(
        &h,
        "SELECT event_id, ctx, tags, attrs FROM shop.events ORDER BY event_id LIMIT 1",
    )
    .await;
    assert_eq!(
        r.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["event_id", "ctx", "tags", "attrs"]
    );
    assert_eq!(r.columns[0].type_name, "bigint");
    assert!(
        r.columns[1].type_name.starts_with("struct<"),
        "{}",
        r.columns[1].type_name
    );
    let row = &r.rows[0];
    assert_eq!(row[0], json!("1"));
    let ctx = row[1].as_object().expect("struct as object");
    assert!(ctx["device"].is_string());
    assert!(ctx["app_version"].is_number(), "int32 stays a number");
    assert!(row[2].is_array());
    let attrs = row[3].as_array().expect("map as array");
    assert_eq!(attrs[0], json!({"key": "ref", "value": "r0"}));
    assert_eq!(attrs[1], json!({"key": "utm", "value": "mail"}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn decimals_and_int64_are_strings_and_timestamps_rfc3339() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let r = collect(
        &h,
        "SELECT id, customer_id, total, placed_at FROM shop.orders ORDER BY id LIMIT 2",
    )
    .await;
    for c in &r.rows[0][..3] {
        assert!(c.is_string(), "{c}");
    }
    assert_eq!(r.rows[0][0], json!("1"));
    assert!(
        r.rows[0][2].as_str().unwrap().contains('.'),
        "decimal keeps its scale: {}",
        r.rows[0][2]
    );
    assert_eq!(r.rows[0][3], json!("2026-10-01T00:37:00Z"));
    assert_eq!(r.rows[1][3], json!("2026-10-01T01:14:00Z"));
    assert_eq!(r.columns[2].type_name, "decimal(12, 2)");
    assert_eq!(r.columns[3].type_name, "timestamp with time zone");
    assert_eq!(r.columns[0].type_oid, 0);
    // aggregates over decimals stay exact strings
    let sum = scalar(&h, "SELECT sum(total) FROM shop.orders").await;
    assert!(sum.is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paging_and_truncation() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let evs = events(
        &h,
        "SELECT id FROM shop.customers",
        ExecOptions {
            page_size: 100,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(
        evs.first(),
        Some(QueryEvent::Started {
            statement_count: 1,
            ..
        })
    ));
    assert!(matches!(evs.last(), Some(QueryEvent::Finished { .. })));
    let pages: Vec<usize> = evs
        .iter()
        .filter_map(|e| {
            if let QueryEvent::Rows { rows, .. } = e {
                Some(rows.len())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(pages, [100, 100, 100, 100, 100]);
    let done = evs
        .iter()
        .find_map(|e| {
            if let QueryEvent::StatementDone {
                row_count,
                truncated,
                command_tag,
                ..
            } = e
            {
                Some((*row_count, *truncated, command_tag.clone()))
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(done, (500, false, Some("SELECT 500".into())));

    let evs = events(
        &h,
        "SELECT id FROM shop.customers",
        ExecOptions {
            page_size: 50,
            max_rows: 120,
            ..Default::default()
        },
    )
    .await;
    let total: usize = evs
        .iter()
        .filter_map(|e| {
            if let QueryEvent::Rows { rows, .. } = e {
                Some(rows.len())
            } else {
                None
            }
        })
        .sum();
    assert_eq!(total, 120);
    assert!(evs.iter().any(|e| matches!(
        e,
        QueryEvent::StatementDone {
            truncated: true,
            row_count: 120,
            ..
        }
    )));

    // Exactly at the cap is not truncated.
    let evs = events(
        &h,
        "SELECT id FROM shop.customers",
        ExecOptions {
            max_rows: 500,
            ..Default::default()
        },
    )
    .await;
    assert!(evs.iter().any(|e| matches!(
        e,
        QueryEvent::StatementDone {
            truncated: false,
            row_count: 500,
            ..
        }
    )));

    // collect agrees
    let r = h
        .execute_collect(
            "SELECT id FROM shop.customers",
            ExecOptions {
                max_rows: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((r.rows.len(), r.truncated, r.row_count), (10, true, 10));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multiple_statements_report_per_statement() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let evs = events(
        &h,
        "SELECT 1 AS a; SELECT count(*) AS n FROM shop.customers; -- trailing",
        ExecOptions::default(),
    )
    .await;
    assert!(matches!(
        evs.first(),
        Some(QueryEvent::Started {
            statement_count: 2,
            ..
        })
    ));
    let cols: Vec<_> = evs
        .iter()
        .filter(|e| matches!(e, QueryEvent::Columns { .. }))
        .collect();
    assert_eq!(cols.len(), 2);
    assert!(matches!(evs.last(), Some(QueryEvent::Finished { .. })));
    // The failing statement is identified and stops the script.
    let evs = events(
        &h,
        "SELECT 1; SELECT * FROM shop.nope; SELECT 3",
        ExecOptions::default(),
    )
    .await;
    match evs.last() {
        Some(QueryEvent::Failed {
            statement_index: Some(1),
            error,
        }) => assert_eq!(error.code, "invalid", "{error:?}"),
        other => panic!("unexpected end: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn read_only_rejects_everything_but_queries() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let ro = ExecOptions {
        read_only: true,
        ..Default::default()
    };
    for sql in [
        "INSERT INTO shop.customers VALUES (9999, 'x@y.z', 'ZA', false)",
        "UPDATE shop.customers SET vip = true",
        "DELETE FROM shop.customers",
        "CREATE TABLE shop.zzz (a INT)",
        "SELECT 1; INSERT INTO shop.customers VALUES (9999, 'x@y.z', 'ZA', false)",
        "COPY shop.customers TO '/tmp/x.csv'",
    ] {
        let evs = events(&h, sql, ro.clone()).await;
        let err = failed(&evs).unwrap_or_else(|| panic!("{sql} should fail"));
        assert_eq!(err.code, "policy_denied", "{sql}: {err:?}");
    }
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.customers").await,
        json!("500")
    );
    let err = h
        .execute_collect("DELETE FROM shop.customers", ro.clone())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "policy_denied");
    // queries still work
    let r = h
        .execute_collect("SELECT count(*) FROM shop.customers", ro)
        .await
        .unwrap();
    assert_eq!(r.rows[0][0], json!("500"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn row_level_dml_gets_a_friendly_error() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    for sql in [
        "UPDATE shop.customers SET vip = true WHERE id = 1",
        "DELETE FROM shop.customers WHERE id = 1",
        "MERGE INTO shop.customers t USING shop.customers s ON t.id = s.id WHEN MATCHED THEN DELETE",
    ] {
        let e = h
            .execute_collect(sql, ExecOptions::default())
            .await
            .unwrap_err();
        assert_eq!(e.code(), "invalid", "{sql}");
        let m = e.to_string();
        assert!(
            m.contains("UPDATE/DELETE/MERGE") && m.contains("Athena"),
            "{m}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_stops_a_long_query() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let (tx, mut rx) = mpsc::channel(64);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        c2.cancel();
    });
    let t0 = Instant::now();
    let sql =
        "SELECT count(*) FROM shop.orders a CROSS JOIN shop.orders b CROSS JOIN shop.orders c";
    let run = h.execute_stream("q".into(), sql, ExecOptions::default(), cancel, tx);
    let collect = async {
        let mut v = Vec::new();
        while let Some(e) = rx.recv().await {
            v.push(e);
        }
        v
    };
    let (r, evs) = tokio::join!(run, collect);
    r.unwrap();
    assert!(
        t0.elapsed() < Duration::from_secs(20),
        "took {:?}",
        t0.elapsed()
    );
    let err = failed(&evs).expect("failed event");
    assert_eq!(err.code, "cancelled");
    // The handle is still usable.
    assert_eq!(
        scalar(&h, "SELECT count(*) FROM shop.customers").await,
        json!("500")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn statement_timeout_is_reported() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let sql =
        "SELECT count(*) FROM shop.orders a CROSS JOIN shop.orders b CROSS JOIN shop.orders c";
    let e = h
        .execute_collect(
            sql,
            ExecOptions {
                timeout_ms: Some(200),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.code(), "timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unknown_table_and_namespace_errors() {
    let f = fixture!();
    let h = connect_sql(&f).await;
    let e = h
        .describe_table(vec!["shop".into()], "nope")
        .await
        .unwrap_err();
    assert_eq!(e.code(), "not_found", "{e}");
    let e = h
        .describe_table(vec!["nowhere".into()], "t")
        .await
        .unwrap_err();
    assert!(matches!(e.code(), "not_found" | "database"), "{e}");
    let _ = Value::Null;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connect_failures_are_readable_and_leak_nothing() {
    let _f = fixture!();
    // Missing SQLite file.
    let mut p = sql_profile(&Fixture {
        db: "/nonexistent/dir/catalog.db".into(),
        warehouse: "/tmp".into(),
        _tmp: None,
    });
    p.id = "ice-missing".into();
    let e = vibelens_core::iceberg::IcebergHandle::connect(p, deps())
        .await
        .err()
        .expect("must fail");
    assert!(matches!(e.code(), "database"), "{e}");
    // Closed port.
    let p = base_profile(
        "ice-closed",
        IcebergOptions {
            catalog: IcebergCatalog::Rest {
                uri: "http://127.0.0.1:9".into(),
                warehouse: None,
                auth: RestAuth::None,
                headers: Default::default(),
            },
            storage: IcebergStorage::Local,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: None,
        },
    );
    let e = vibelens_core::iceberg::IcebergHandle::connect(p, deps())
        .await
        .err()
        .expect("must fail");
    assert!(!e.to_string().is_empty());
    eprintln!("closed port error: {e}");
    // Athena with a non-AWS catalog is rejected up front.
    let p = base_profile(
        "ice-bad-athena",
        IcebergOptions {
            catalog: IcebergCatalog::Rest {
                uri: "http://127.0.0.1:9".into(),
                warehouse: None,
                auth: RestAuth::None,
                headers: Default::default(),
            },
            storage: IcebergStorage::Auto,
            query_engine: IcebergQueryEngine::Athena {
                workgroup: "primary".into(),
                output_location: None,
                aws: None,
            },
            default_namespace: None,
        },
    );
    let e = vibelens_core::iceberg::IcebergHandle::connect(p, deps())
        .await
        .err()
        .unwrap();
    assert_eq!(e.code(), "invalid");
    // HMS is compiled out by default.
    let p = base_profile(
        "ice-hms",
        IcebergOptions {
            catalog: IcebergCatalog::Hms {
                address: "127.0.0.1:9083".into(),
                warehouse: "/tmp".into(),
            },
            storage: IcebergStorage::Local,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: None,
        },
    );
    let e = vibelens_core::iceberg::IcebergHandle::connect(p, deps())
        .await
        .err()
        .unwrap();
    #[cfg(not(feature = "iceberg-hms"))]
    assert!(
        e.to_string()
            .contains("no Hive Metastore support (feature iceberg-hms)"),
        "{e}"
    );
    #[cfg(feature = "iceberg-hms")]
    let _ = e;
}
