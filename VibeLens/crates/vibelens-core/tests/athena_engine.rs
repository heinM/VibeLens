//! The Athena engine against an in-process mock speaking Athena's JSON protocol: the real
//! `aws-sdk-athena` client, no AWS.
#![cfg(feature = "iceberg")]

mod athena_common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use athena_common::*;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vibelens_core::CoreError;
use vibelens_core::athena::cost_estimate_usd;
use vibelens_core::model::*;

fn opts() -> ExecOptions {
    ExecOptions {
        page_size: 2,
        ..ExecOptions::default()
    }
}

fn kinds(events: &[QueryEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            QueryEvent::Started { .. } => "started",
            QueryEvent::Columns { .. } => "columns",
            QueryEvent::Rows { .. } => "rows",
            QueryEvent::Stats { .. } => "stats",
            QueryEvent::StatementDone { .. } => "done",
            QueryEvent::Finished { .. } => "finished",
            QueryEvent::Failed { .. } => "failed",
        })
        .collect()
}

fn failed_code(events: &[QueryEvent]) -> (Option<usize>, String, String) {
    match events.last() {
        Some(QueryEvent::Failed {
            statement_index,
            error,
        }) => (*statement_index, error.code.clone(), error.message.clone()),
        other => panic!("expected Failed, got {other:?}"),
    }
}

const COLS: &[(&str, &str)] = &[("id", "bigint"), ("name", "varchar"), ("at", "timestamp")];

#[tokio::test]
async fn select_pages_skips_the_header_and_reports_stats() {
    let gets = Arc::new(AtomicUsize::new(0));
    let g = gets.clone();
    let server = serve(Arc::new(move |req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => match g.fetch_add(1, Ordering::SeqCst) {
            0 => execution("QUEUED", "DML", 0),
            1 => execution("RUNNING", "DML", 0),
            _ => execution("SUCCEEDED", "DML", 50 * 1024 * 1024),
        },
        "GetQueryResults" => match body(req)["NextToken"].as_str() {
            None => results_page(
                COLS,
                vec![
                    row(&[Some("id"), Some("name"), Some("at")]),
                    row(&[Some("1"), Some("ann"), Some("2024-01-31 12:34:56.789")]),
                    row(&[Some("2"), None, Some("2024-02-01 00:00:00.000")]),
                ],
                Some("p2"),
                Some(0),
            ),
            Some("p2") => results_page(
                COLS,
                vec![
                    row(&[Some("3"), Some("cy"), None]),
                    row(&[Some("9007199254740993"), Some(""), None]),
                ],
                Some("p3"),
                Some(0),
            ),
            Some("p3") => results_page(
                COLS,
                vec![row(&[Some("5"), Some("eve"), None])],
                None,
                Some(0),
            ),
            Some(other) => Resp::error(
                400,
                "InvalidRequestException",
                &format!("bad token {other}"),
            ),
        },
        other => Resp::error(
            400,
            "InvalidRequestException",
            &format!("unexpected {other}"),
        ),
    }))
    .await;
    let e = engine(&server);

    let events = stream(&e, "SELECT id, name, at FROM orders;", opts()).await;
    assert_eq!(
        kinds(&events),
        [
            "started", "columns", "rows", "rows", "rows", "stats", "done", "finished"
        ],
        "{events:#?}"
    );

    let QueryEvent::Started {
        query_id,
        statement_count,
    } = &events[0]
    else {
        panic!()
    };
    assert_eq!((query_id.as_str(), *statement_count), ("q1", 1));

    let QueryEvent::Columns { columns, .. } = &events[1] else {
        panic!()
    };
    let cols: Vec<_> = columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str()))
        .collect();
    assert_eq!(
        cols,
        [("id", "bigint"), ("name", "varchar"), ("at", "timestamp")]
    );

    // Header row dropped; pages of `page_size` = 2 rows; bigint stays a string, NULL is null,
    // the naive timestamp is ISO-8601.
    let pages: Vec<&Vec<Vec<Cell>>> = events
        .iter()
        .filter_map(|e| match e {
            QueryEvent::Rows { rows, .. } => Some(rows),
            _ => None,
        })
        .collect();
    assert_eq!(pages.iter().map(|p| p.len()).collect::<Vec<_>>(), [2, 2, 1]);
    assert_eq!(
        pages[0][0],
        vec![json!("1"), json!("ann"), json!("2024-01-31T12:34:56.789")]
    );
    assert_eq!(
        pages[0][1],
        vec![json!("2"), json!(null), json!("2024-02-01T00:00:00.000")]
    );
    assert_eq!(
        pages[1][1],
        vec![json!("9007199254740993"), json!(""), json!(null)]
    );
    assert_eq!(pages[2][0][1], json!("eve"));

    let QueryEvent::Stats {
        statement_index,
        engine,
        bytes_scanned,
        cost_usd_estimate,
        engine_query_id,
    } = &events[5]
    else {
        panic!()
    };
    assert_eq!(*statement_index, 0);
    assert_eq!(engine, "athena");
    assert_eq!(*bytes_scanned, Some(50 * 1024 * 1024));
    assert_eq!(
        *cost_usd_estimate,
        Some(cost_estimate_usd(50 * 1024 * 1024))
    );
    assert_eq!(engine_query_id.as_deref(), Some("qid-1"));

    let QueryEvent::StatementDone {
        command_tag,
        rows_affected,
        row_count,
        truncated,
        ..
    } = &events[6]
    else {
        panic!()
    };
    assert_eq!(command_tag.as_deref(), Some("SELECT"));
    assert_eq!(
        *rows_affected, None,
        "a SELECT's UpdateCount of 0 is not rows affected"
    );
    assert_eq!((*row_count, *truncated), (5, false));

    // What we sent.
    let starts = calls(&server, "StartQueryExecution");
    assert_eq!(starts.len(), 1);
    let s = &starts[0];
    assert_eq!(s["QueryString"], "SELECT id, name, at FROM orders");
    assert_eq!(s["WorkGroup"], "wg-test");
    assert_eq!(
        s["ResultConfiguration"]["OutputLocation"],
        "s3://results-bucket/athena/"
    );
    assert_eq!(s["QueryExecutionContext"]["Catalog"], "AwsDataCatalog");
    assert_eq!(s["QueryExecutionContext"]["Database"], "shop");
    assert!(
        s["ClientRequestToken"]
            .as_str()
            .is_some_and(|t| t.len() >= 32)
    );
    assert_eq!(gets.load(Ordering::SeqCst), 3, "QUEUED, RUNNING, SUCCEEDED");
    let results = calls(&server, "GetQueryResults");
    assert_eq!(results.len(), 3);
    assert!(
        results
            .iter()
            .all(|r| r["MaxResults"] == 1000 && r["QueryExecutionId"] == "qid-1")
    );
    assert_eq!(results[1]["NextToken"], "p2");
    assert_eq!(results[2]["NextToken"], "p3");

    // Requests are SigV4-signed with the static credentials and never carry the secret.
    let first = &server.all()[0];
    let auth = first.headers.get("authorization").expect("signed");
    assert!(auth.contains("Credential=AKIDTEST/") && auth.contains("/eu-west-1/athena/"));
    assert!(!server.all().iter().any(|r| r.body.contains("secret-test")));
}

#[tokio::test]
async fn show_statements_without_a_header_row_lose_nothing() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("SUCCEEDED", "UTILITY", 0),
        "GetQueryResults" => results_page(
            &[("tab_name", "varchar")],
            vec![row(&[Some("customers")]), row(&[Some("orders")])],
            None,
            None,
        ),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let r = engine(&server)
        .execute_collect("SHOW TABLES", ExecOptions::default())
        .await
        .unwrap();
    assert_eq!(
        r.rows,
        vec![vec![json!("customers")], vec![json!("orders")]]
    );
    assert_eq!(r.command_tag.as_deref(), Some("SHOW"));
}

#[tokio::test]
async fn max_rows_truncates_and_says_so() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("SUCCEEDED", "DML", 1),
        "GetQueryResults" => match body(req)["NextToken"].as_str() {
            None => results_page(
                &[("n", "integer")],
                vec![
                    row(&[Some("n")]),
                    row(&[Some("1")]),
                    row(&[Some("2")]),
                    row(&[Some("3")]),
                ],
                Some("more"),
                None,
            ),
            Some(_) => results_page(&[("n", "integer")], vec![row(&[Some("4")])], None, None),
        },
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let e = engine(&server);
    let r = e
        .execute_collect(
            "SELECT n FROM t",
            ExecOptions {
                max_rows: 3,
                ..ExecOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)], vec![json!(2)], vec![json!(3)]]);
    assert_eq!((r.row_count, r.truncated), (3, true));
    assert_eq!(
        calls(&server, "GetQueryResults").len(),
        1,
        "stops paging at the cap"
    );

    // Exactly at the limit, with nothing more to read: not truncated.
    let r = e
        .execute_collect(
            "SELECT n FROM t",
            ExecOptions {
                max_rows: 4,
                ..ExecOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((r.row_count, r.truncated), (4, false));
}

#[tokio::test]
async fn a_failed_query_surfaces_the_reason_and_athena_error() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => failed_execution(
            "SYNTAX_ERROR: line 1:8: Column 'nope' cannot be resolved",
            "line 1:8: Column 'nope' cannot be resolved",
        ),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let e = engine(&server);

    let events = stream(&e, "SELECT 1; SELECT nope FROM t", opts()).await;
    // Statement 0 fails first: the engine submits one statement at a time and stops.
    assert_eq!(kinds(&events), ["started", "failed"]);
    let (idx, code, msg) = failed_code(&events);
    assert_eq!(idx, Some(0));
    assert_eq!(code, "database");
    assert!(msg.contains("Column 'nope' cannot be resolved"), "{msg}");
    assert!(msg.contains("SYNTAX_ERROR"), "{msg}");
    assert_eq!(calls(&server, "StartQueryExecution").len(), 1);

    let err = e
        .execute_collect("SELECT nope", ExecOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Database(m) if m.contains("cannot be resolved")));
}

#[tokio::test]
async fn a_failure_in_a_later_statement_reports_its_index() {
    let starts = Arc::new(AtomicUsize::new(0));
    let s = starts.clone();
    let server = serve(Arc::new(move |req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started(&format!("qid-{}", s.fetch_add(1, Ordering::SeqCst) + 1)),
        "GetQueryExecution" => {
            if body(req)["QueryExecutionId"] == "qid-1" {
                execution("SUCCEEDED", "DML", 1)
            } else {
                failed_execution("TABLE_NOT_FOUND: no such table", "no such table")
            }
        }
        "GetQueryResults" => results_page(
            &[("n", "integer")],
            vec![row(&[Some("n")]), row(&[Some("7")])],
            None,
            None,
        ),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let events = stream(&engine(&server), "SELECT 7; SELECT * FROM missing", opts()).await;
    assert_eq!(
        kinds(&events),
        ["started", "columns", "rows", "stats", "done", "failed"],
        "{events:#?}"
    );
    assert_eq!(failed_code(&events).0, Some(1));
    // Each statement is its own query.
    let starts = calls(&server, "StartQueryExecution");
    assert_eq!(starts[0]["QueryString"], "SELECT 7");
    assert_eq!(starts[1]["QueryString"], "SELECT * FROM missing");
    assert_ne!(
        starts[0]["ClientRequestToken"],
        starts[1]["ClientRequestToken"]
    );
}

#[tokio::test]
async fn cancelling_stops_the_athena_query() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("RUNNING", "DML", 0),
        "StopQueryExecution" => Resp::ok("{}"),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let e = Arc::new(engine(&server));
    let (tx, mut rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    let task = {
        let (e, cancel) = (e.clone(), cancel.clone());
        tokio::spawn(async move {
            e.execute_stream(
                "q1".into(),
                "SELECT * FROM big",
                ExecOptions::default(),
                cancel,
                tx,
            )
            .await
        })
    };
    // Wait until the query is running, then cancel.
    for _ in 0..200 {
        if server.to_target("AmazonAthena.GetQueryExecution") >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    cancel.cancel();
    task.await.unwrap().unwrap();
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    assert_eq!(
        kinds(&events),
        ["started", "failed"],
        "exactly one terminal event"
    );
    assert_eq!(failed_code(&events).1, "cancelled");
    let stops = calls(&server, "StopQueryExecution");
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0]["QueryExecutionId"], "qid-1");
}

#[tokio::test]
async fn a_statement_timeout_stops_the_query() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("QUEUED", "DML", 0),
        "StopQueryExecution" => Resp::ok("{}"),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let events = stream(
        &engine(&server),
        "SELECT 1",
        ExecOptions {
            timeout_ms: Some(80),
            ..ExecOptions::default()
        },
    )
    .await;
    assert_eq!(failed_code(&events).1, "timeout");
    assert_eq!(calls(&server, "StopQueryExecution").len(), 1);
}

#[tokio::test]
async fn an_externally_cancelled_query_fails_as_cancelled_without_a_stop() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("CANCELLED", "DML", 0),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let events = stream(&engine(&server), "SELECT 1", opts()).await;
    assert_eq!(failed_code(&events).1, "cancelled");
}

#[tokio::test]
async fn read_only_rejects_writes_before_calling_athena() {
    let server = serve(Arc::new(|_req: &Req| {
        Resp::error(500, "InternalFailure", "must not be called")
    }))
    .await;
    let e = engine(&server);
    for sql in [
        "UPDATE shop.orders SET status = 'x' WHERE id = 1",
        "DELETE FROM shop.orders",
        "INSERT INTO shop.orders VALUES (1)",
        "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
        "CREATE TABLE x AS SELECT 1",
        "DROP TABLE shop.orders",
        "OPTIMIZE shop.orders REWRITE DATA USING BIN_PACK",
        "VACUUM shop.orders",
        // A read first must not smuggle a write after it: the whole script is refused.
        "SELECT 1; DELETE FROM shop.orders",
        "SELECT 1; SELECT 'unterminated",
        "UNLOAD (SELECT 1) TO 's3://b/p/'",
        "EXPLAIN ANALYZE INSERT INTO t SELECT 1",
    ] {
        let events = stream(
            &e,
            sql,
            ExecOptions {
                read_only: true,
                ..ExecOptions::default()
            },
        )
        .await;
        assert_eq!(kinds(&events), ["started", "failed"], "{sql}");
        let (idx, code, msg) = failed_code(&events);
        assert_eq!((idx, code.as_str()), (None, "policy_denied"), "{sql}");
        assert!(msg.contains("read-only run"), "{msg}");

        let err = e
            .execute_collect(
                sql,
                ExecOptions {
                    read_only: true,
                    ..ExecOptions::default()
                },
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, CoreError::PolicyDenied(m) if m.starts_with("read-only run")),
            "{sql}"
        );
    }
    assert!(
        server.all().is_empty(),
        "no request may reach Athena: {:?}",
        server.all()
    );
}

#[tokio::test]
async fn read_only_still_runs_queries_including_trino_only_syntax() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("SUCCEEDED", "DML", 1),
        "GetQueryResults" => results_page(
            &[("n", "integer")],
            vec![row(&[Some("n")]), row(&[Some("1")])],
            None,
            None,
        ),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let e = engine(&server);
    let ro = || ExecOptions {
        read_only: true,
        ..ExecOptions::default()
    };
    for sql in [
        "SELECT 1",
        "WITH a AS (SELECT 1 AS n) SELECT n FROM a",
        "SHOW TABLES",
        "DESCRIBE shop.orders",
        // Not valid PostgreSQL, so the classifier cannot parse it; the text fallback vouches.
        "SELECT * FROM shop.orders FOR TIMESTAMP AS OF TIMESTAMP '2024-01-01 00:00:00 UTC'",
    ] {
        e.execute_collect(sql, ro())
            .await
            .unwrap_or_else(|err| panic!("{sql}: {err}"));
    }
}

#[tokio::test]
async fn dml_reports_rows_affected_from_update_count() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("SUCCEEDED", "DML", 0),
        "GetQueryResults" => results_page(
            &[("rows", "bigint")],
            vec![row(&[Some("rows")]), row(&[Some("3")])],
            None,
            Some(3),
        ),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let e = engine(&server);
    for (sql, tag) in [
        ("UPDATE shop.orders SET status = 'x' WHERE id < 4", "UPDATE"),
        ("DELETE FROM shop.orders WHERE id < 4", "DELETE"),
        ("INSERT INTO shop.orders SELECT * FROM staging", "INSERT"),
        (
            "MERGE INTO shop.orders t USING s ON t.id = s.id WHEN MATCHED THEN DELETE",
            "MERGE",
        ),
    ] {
        let events = stream(&e, sql, opts()).await;
        assert_eq!(
            kinds(&events),
            ["started", "stats", "done", "finished"],
            "{sql}: {events:#?}"
        );
        let QueryEvent::StatementDone {
            command_tag,
            rows_affected,
            row_count,
            ..
        } = &events[2]
        else {
            panic!()
        };
        assert_eq!(command_tag.as_deref(), Some(tag));
        assert_eq!(*rows_affected, Some(3), "{sql}");
        assert_eq!(*row_count, 0);
    }
    let r = e
        .execute_collect("DELETE FROM shop.orders", opts())
        .await
        .unwrap();
    assert_eq!(r.rows_affected, Some(3));
    assert!(r.columns.is_empty() && r.rows.is_empty());
}

#[tokio::test]
async fn ddl_reads_no_results_and_costs_nothing() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("SUCCEEDED", "DDL", 0),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let events = stream(
        &engine(&server),
        "CREATE TABLE shop.t (id bigint) LOCATION 's3://b/t/'",
        opts(),
    )
    .await;
    assert_eq!(kinds(&events), ["started", "stats", "done", "finished"]);
    let QueryEvent::Stats {
        cost_usd_estimate, ..
    } = &events[1]
    else {
        panic!()
    };
    assert_eq!(*cost_usd_estimate, Some(0.0));
    let QueryEvent::StatementDone { command_tag, .. } = &events[2] else {
        panic!()
    };
    assert_eq!(command_tag.as_deref(), Some("CREATE TABLE"));
    assert_eq!(server.to_target("AmazonAthena.GetQueryResults"), 0);
}

#[tokio::test]
async fn athena_types_map_to_cells() {
    let all: &[(&str, &str)] = &[
        ("c_tiny", "tinyint"),
        ("c_small", "smallint"),
        ("c_int", "integer"),
        ("c_big", "bigint"),
        ("c_real", "real"),
        ("c_dbl", "double"),
        ("c_dec", "decimal"),
        ("c_bool", "boolean"),
        ("c_date", "date"),
        ("c_ts", "timestamp"),
        ("c_tstz", "timestamp with time zone"),
        ("c_json", "json"),
        ("c_arr", "array"),
        ("c_row", "row"),
        ("c_map", "map"),
        ("c_str", "varchar"),
        ("c_bin", "varbinary"),
        ("c_null", "bigint"),
    ];
    let header: Vec<Option<&str>> = all.iter().map(|(n, _)| Some(*n)).collect();
    let data: Vec<Option<&str>> = vec![
        Some("7"),
        Some("-300"),
        Some("42"),
        Some("9007199254740993"),
        Some("1.5"),
        Some("2.5E10"),
        Some("12345.67"),
        Some("true"),
        Some("2024-02-29"),
        Some("2024-01-31 12:34:56.789"),
        Some("2024-01-31 12:34:56.789 +02:00"),
        Some(r#"{"a":[1,2]}"#),
        Some("[a, b]"),
        Some("{x=1, y=z}"),
        Some("{k=v}"),
        Some("héllo"),
        Some("68 69"),
        None,
    ];
    let (h, d) = (row(&header), row(&data));
    let server = serve(Arc::new(move |req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("SUCCEEDED", "DML", 1),
        "GetQueryResults" => {
            let mut resp = results_page(all, vec![h.clone(), d.clone()], None, None);
            // Athena sends `decimal` bare, with the parameters in Precision/Scale.
            let mut v: serde_json::Value = serde_json::from_str(&resp.body).unwrap();
            v["ResultSet"]["ResultSetMetadata"]["ColumnInfo"][6]["Precision"] = json!(12);
            v["ResultSet"]["ResultSetMetadata"]["ColumnInfo"][6]["Scale"] = json!(2);
            resp.body = v.to_string();
            resp
        }
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let r = engine(&server)
        .execute_collect("SELECT * FROM t", ExecOptions::default())
        .await
        .unwrap();
    assert_eq!(r.columns[6].type_name, "decimal(12,2)");
    assert_eq!(
        r.rows[0],
        vec![
            json!(7),
            json!(-300),
            json!(42),
            json!("9007199254740993"),
            json!(1.5),
            json!(2.5e10),
            json!("12345.67"),
            json!(true),
            json!("2024-02-29"),
            json!("2024-01-31T12:34:56.789"),
            json!("2024-01-31T10:34:56.789Z"),
            json!({"a": [1, 2]}),
            json!("[a, b]"),
            json!("{x=1, y=z}"),
            json!("{k=v}"),
            json!("héllo"),
            json!({"$bytes": "aGk=", "len": 2}),
            json!(null),
        ]
    );
}

#[tokio::test]
async fn throttling_is_retried_with_the_same_idempotency_token() {
    let starts = Arc::new(AtomicUsize::new(0));
    let s = starts.clone();
    let server = serve(Arc::new(move |req: &Req| match op(req).as_str() {
        "StartQueryExecution" => {
            if s.fetch_add(1, Ordering::SeqCst) < 2 {
                Resp::error(400, "TooManyRequestsException", "Rate exceeded")
            } else {
                started("qid-1")
            }
        }
        "GetQueryExecution" => execution("SUCCEEDED", "DML", 1),
        "GetQueryResults" => results_page(
            &[("n", "integer")],
            vec![row(&[Some("n")]), row(&[Some("1")])],
            None,
            None,
        ),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let r = engine(&server)
        .execute_collect("SELECT 1 AS n", ExecOptions::default())
        .await
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)]]);
    let starts = calls(&server, "StartQueryExecution");
    assert_eq!(starts.len(), 3);
    assert_eq!(
        starts[0]["ClientRequestToken"],
        starts[2]["ClientRequestToken"]
    );
}

#[tokio::test]
async fn throttling_gives_up_after_three_retries_with_the_service_code() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => Resp::error(400, "TooManyRequestsException", "Rate exceeded"),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let err = engine(&server)
        .execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap_err();
    match err {
        CoreError::Aws(m) => {
            assert!(
                m.contains("StartQueryExecution")
                    && m.contains("TooManyRequestsException")
                    && m.contains("Rate exceeded"),
                "{m}"
            );
            assert!(!m.contains("secret-test") && !m.contains("AKIDTEST"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        calls(&server, "StartQueryExecution").len(),
        4,
        "1 attempt + 3 retries"
    );
}

#[tokio::test]
async fn non_throttling_service_errors_are_not_retried_and_keep_code_and_message() {
    let server = serve(Arc::new(|_req: &Req| {
        Resp::error(
            400,
            "InvalidRequestException",
            "WorkGroup wg-test is not found.",
        )
    }))
    .await;
    let events = stream(&engine(&server), "SELECT 1", opts()).await;
    let (idx, code, msg) = failed_code(&events);
    assert_eq!((idx, code.as_str()), (Some(0), "aws"));
    assert!(
        msg.contains("InvalidRequestException") && msg.contains("WorkGroup wg-test is not found."),
        "{msg}"
    );
    assert_eq!(server.all().len(), 1);
}

#[tokio::test]
async fn throttled_polls_are_retried_too() {
    let polls = Arc::new(Mutex::new(0));
    let p = polls.clone();
    let server = serve(Arc::new(move |req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => {
            let mut n = p.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Resp::error(400, "TooManyRequestsException", "slow down")
            } else {
                execution("SUCCEEDED", "DDL", 0)
            }
        }
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let r = engine(&server)
        .execute_collect("CREATE TABLE t (a int)", ExecOptions::default())
        .await
        .unwrap();
    assert_eq!(r.command_tag.as_deref(), Some("CREATE TABLE"));
    assert_eq!(*polls.lock().unwrap(), 2);
}

#[tokio::test]
async fn empty_scripts_finish_immediately_without_calling_athena() {
    let server = serve(Arc::new(|_req: &Req| {
        Resp::error(500, "InternalFailure", "must not be called")
    }))
    .await;
    let e = engine(&server);
    let events = stream(&e, "  -- nothing\n ;; ", opts()).await;
    assert_eq!(kinds(&events), ["started", "finished"]);
    assert_eq!(
        e.execute_collect("", opts()).await.unwrap(),
        QueryResult::default()
    );
    assert!(server.all().is_empty());
}

#[tokio::test]
async fn omitted_settings_are_left_out_of_the_request() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "StartQueryExecution" => started("qid-1"),
        "GetQueryExecution" => execution("SUCCEEDED", "DDL", 0),
        _ => Resp::error(400, "InvalidRequestException", "unexpected"),
    }))
    .await;
    let creds = aws_credential_types::Credentials::new("AKIDTEST", "secret-test", None, None, "t");
    let cfg = vibelens_core::athena::AthenaConfig {
        region: "eu-west-1".into(),
        workgroup: "primary".into(),
        output_location: None,
        catalog: "s3tablescatalog/my-bucket".into(),
        database: None,
    };
    let e = vibelens_core::athena::AthenaEngine::new(
        cfg,
        aws_credential_types::provider::SharedCredentialsProvider::new(creds),
    )
    .with_endpoint_url(&server.url);
    e.execute_collect("CREATE DATABASE d", ExecOptions::default())
        .await
        .unwrap();
    let s = &calls(&server, "StartQueryExecution")[0];
    assert!(s.get("ResultConfiguration").is_none());
    assert_eq!(
        s["QueryExecutionContext"]["Catalog"],
        "s3tablescatalog/my-bucket"
    );
    assert!(s["QueryExecutionContext"].get("Database").is_none());
    assert_eq!(s["WorkGroup"], "primary");
}
