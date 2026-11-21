//! The Redshift engine against an in-process mock of the Redshift Data API: the real
//! `aws-sdk-redshiftdata` client, no AWS. Each rule test says what would go wrong without it.

mod redshift_common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use redshift_common::*;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vibelens_core::CoreError;
use vibelens_core::model::*;
use vibelens_core::redshift::{RedshiftEngine, RedshiftTiming};

fn opts() -> ExecOptions {
    ExecOptions {
        page_size: 2,
        ..ExecOptions::default()
    }
}

fn ro() -> ExecOptions {
    ExecOptions {
        read_only: true,
        ..ExecOptions::default()
    }
}

fn refuse_everything() -> Handler {
    Arc::new(|_req: &Req| Resp::error(500, "InternalServerException", "must not be called"))
}

const COLS: &[(&str, &str)] = &[
    ("id", "int8"),
    ("qty", "int4"),
    ("price", "numeric"),
    ("name", "varchar"),
    ("at", "timestamp"),
    ("ok", "bool"),
    ("ratio", "float8"),
    ("doc", "super"),
];

#[tokio::test]
async fn select_streams_pages_with_redshift_types_and_stats() {
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    let server = serve(Arc::new(move |req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => match p.fetch_add(1, Ordering::SeqCst) {
            0 => described("stmt-1", "SUBMITTED", false, -1),
            1 => described("stmt-1", "STARTED", false, -1),
            _ => described("stmt-1", "FINISHED", true, 5),
        },
        "GetStatementResult" => match body(req)["NextToken"].as_str() {
            None => result_page(
                COLS,
                vec![
                    vec![
                        long(9007199254740993),
                        long(3),
                        text("12.50"),
                        text("ann"),
                        text("2024-01-31 12:34:56.789"),
                        boolean(true),
                        double(0.5),
                        text(r#"{"a":[1,2]}"#),
                    ],
                    vec![
                        long(2),
                        null(),
                        null(),
                        text(""),
                        null(),
                        boolean(false),
                        null(),
                        null(),
                    ],
                    vec![
                        long(3),
                        long(1),
                        text("0.10"),
                        text("cy"),
                        null(),
                        null(),
                        null(),
                        null(),
                    ],
                ],
                Some("p2"),
            ),
            Some("p2") => result_page(
                COLS,
                vec![
                    vec![
                        long(4),
                        long(1),
                        text("1"),
                        text("dee"),
                        null(),
                        null(),
                        null(),
                        null(),
                    ],
                    vec![
                        long(5),
                        long(1),
                        text("2"),
                        text("eve"),
                        null(),
                        null(),
                        null(),
                        null(),
                    ],
                ],
                None,
            ),
            Some(other) => Resp::error(400, "ValidationException", &format!("bad token {other}")),
        },
        other => Resp::error(400, "ValidationException", &format!("unexpected {other}")),
    }))
    .await;
    let e = engine(&server);

    let events = stream(&e, "SELECT * FROM sales;", opts()).await;
    assert_eq!(
        kinds(&events),
        [
            "started", "columns", "rows", "rows", "rows", "stats", "done", "finished"
        ],
        "{events:#?}"
    );
    let QueryEvent::Columns { columns, .. } = &events[1] else {
        panic!()
    };
    assert_eq!(
        columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["id", "qty", "price", "name", "at", "ok", "ratio", "doc"]
    );
    let pages = rows_of(&events);
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2, 1]);
    // int8 and numeric are strings, int4 a number, the naive timestamp ISO 8601, SUPER embedded.
    assert_eq!(
        pages[0][0],
        vec![
            json!("9007199254740993"),
            json!(3),
            json!("12.50"),
            json!("ann"),
            json!("2024-01-31T12:34:56.789"),
            json!(true),
            json!(0.5),
            json!({"a": [1, 2]}),
        ]
    );
    assert_eq!(pages[0][1][1], json!(null));
    assert_eq!(pages[0][1][3], json!(""), "an empty string is not NULL");

    let QueryEvent::Stats {
        engine,
        bytes_scanned,
        cost_usd_estimate,
        engine_query_id,
        ..
    } = &events[5]
    else {
        panic!()
    };
    assert_eq!(engine, "redshift");
    assert_eq!((*bytes_scanned, *cost_usd_estimate), (None, None));
    assert_eq!(engine_query_id.as_deref(), Some("4242"));
    let QueryEvent::StatementDone {
        command_tag,
        row_count,
        truncated,
        rows_affected,
        ..
    } = &events[6]
    else {
        panic!()
    };
    assert_eq!(command_tag.as_deref(), Some("SELECT"));
    assert_eq!((*row_count, *truncated, *rows_affected), (5, false, None));

    // What was sent: the statement, the target, an idempotency token, and no secret.
    let exec = server.calls("ExecuteStatement");
    assert_eq!(exec.len(), 1);
    assert_eq!(exec[0]["Sql"], "SELECT * FROM sales");
    assert_eq!(exec[0]["Database"], "dev");
    assert_eq!(exec[0]["ClusterIdentifier"], "analytics-1");
    assert_eq!(exec[0]["StatementName"], "vibelens");
    assert!(
        exec[0]["ClientToken"]
            .as_str()
            .is_some_and(|t| t.len() >= 32)
    );
    assert_eq!(
        polls.load(Ordering::SeqCst),
        3,
        "SUBMITTED, STARTED, FINISHED"
    );
    let pages_req = server.calls("GetStatementResult");
    assert_eq!(pages_req.len(), 2);
    assert!(pages_req.iter().all(|r| r["Id"] == "stmt-1"));
    let first = &server.all()[0];
    let auth = first.headers.get("authorization").expect("signed");
    assert!(
        auth.contains(&format!("Credential={ACCESS_KEY}/"))
            && auth.contains("/eu-west-1/redshift-data/"),
        "{auth}"
    );
    assert!(!server.all().iter().any(|r| r.body.contains(SECRET_KEY)));
}

#[tokio::test]
async fn writes_report_rows_affected_and_read_no_result() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "FINISHED", false, 3),
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let e = engine(&server);
    let events = stream(&e, "UPDATE sales SET qty = 0 WHERE id < 4", opts()).await;
    assert_eq!(kinds(&events), ["started", "stats", "done", "finished"]);
    let QueryEvent::StatementDone {
        command_tag,
        rows_affected,
        ..
    } = &events[2]
    else {
        panic!()
    };
    assert_eq!(command_tag.as_deref(), Some("UPDATE"));
    assert_eq!(*rows_affected, Some(3));
    assert_eq!(server.count("GetStatementResult"), 0);

    let r = e
        .execute_collect(
            "CREATE TABLE t (a int)",
            None,
            ExecOptions::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.command_tag.as_deref(), Some("CREATE TABLE"));
}

#[tokio::test]
async fn failed_and_aborted_statements() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => {
            if body(req)["Sql"].as_str().unwrap_or("").contains("nope") {
                executed("stmt-bad")
            } else if body(req)["Sql"].as_str().unwrap_or("").contains("gone") {
                executed("stmt-gone")
            } else {
                executed("stmt-ok")
            }
        }
        "DescribeStatement" => match body(req)["Id"].as_str() {
            Some("stmt-bad") => {
                failed("stmt-bad", "ERROR: column \"nope\" does not exist in sales")
            }
            Some("stmt-gone") => described("stmt-gone", "ABORTED", false, -1),
            _ => described("stmt-ok", "FINISHED", true, 1),
        },
        "GetStatementResult" => result_page(&[("n", "int4")], vec![vec![long(7)]], None),
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let e = engine(&server);

    let events = stream(&e, "SELECT 7; SELECT nope FROM sales; SELECT 8", opts()).await;
    assert_eq!(
        kinds(&events),
        ["started", "columns", "rows", "stats", "done", "failed"]
    );
    let (idx, code, msg) = failure(&events);
    assert_eq!((idx, code.as_str()), (Some(1), "database"));
    assert!(msg.contains("column \"nope\" does not exist"), "{msg}");
    assert_eq!(server.count("ExecuteStatement"), 2, "stops at the failure");
    assert_eq!(server.count("CancelStatement"), 0);

    let events = stream(&e, "SELECT gone", opts()).await;
    assert_eq!(failure(&events).1, "cancelled");

    let err = e
        .execute_collect(
            "SELECT nope",
            None,
            ExecOptions::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Database(m) if m.contains("does not exist")));
}

#[tokio::test]
async fn cancelling_reaches_cancel_statement() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "STARTED", false, -1),
        "CancelStatement" => Resp::ok(&json!({"Status": true})),
        _ => Resp::error(400, "ValidationException", "unexpected"),
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
                None,
                ExecOptions::default(),
                cancel,
                tx,
            )
            .await
        })
    };
    for _ in 0..200 {
        if server.count("DescribeStatement") >= 2 {
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
    assert_eq!(failure(&events).1, "cancelled");
    // Without this the statement keeps running on the cluster after the user pressed Stop.
    let cancels = server.calls("CancelStatement");
    assert_eq!(cancels.len(), 1);
    assert_eq!(cancels[0]["Id"], "stmt-1");
}

#[tokio::test]
async fn a_timeout_cancels_the_statement_on_the_server() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "PICKED", false, -1),
        "CancelStatement" => Resp::ok(&json!({"Status": true})),
        _ => Resp::error(400, "ValidationException", "unexpected"),
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
    assert_eq!(failure(&events).1, "timeout");
    assert_eq!(server.calls("CancelStatement")[0]["Id"], "stmt-1");
}

#[tokio::test]
async fn throttling_is_retried_with_the_same_client_token() {
    let starts = Arc::new(AtomicUsize::new(0));
    let s = starts.clone();
    let polls = Arc::new(Mutex::new(0));
    let p = polls.clone();
    let server = serve(Arc::new(move |req: &Req| match op(req).as_str() {
        "ExecuteStatement" => {
            if s.fetch_add(1, Ordering::SeqCst) < 2 {
                Resp::error(400, "ThrottlingException", "Rate exceeded")
            } else {
                executed("stmt-1")
            }
        }
        "DescribeStatement" => {
            let mut n = p.lock().unwrap();
            *n += 1;
            if *n == 1 {
                Resp::error(400, "ThrottlingException", "slow down")
            } else {
                described("stmt-1", "FINISHED", true, 1)
            }
        }
        "GetStatementResult" => result_page(&[("n", "int4")], vec![vec![long(1)]], None),
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let r = engine(&server)
        .execute_collect(
            "SELECT 1 AS n",
            None,
            ExecOptions::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)]]);
    // Without the token a throttled-looking failure that did reach Redshift would run the
    // statement twice.
    let exec = server.calls("ExecuteStatement");
    assert_eq!(exec.len(), 3);
    assert_eq!(exec[0]["ClientToken"], exec[2]["ClientToken"]);
    assert_eq!(*polls.lock().unwrap(), 2, "a throttled poll is retried too");
}

#[tokio::test]
async fn throttling_gives_up_after_three_retries_and_errors_carry_no_credentials() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => Resp::error(400, "ThrottlingException", "Rate exceeded"),
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let err = engine(&server)
        .execute_collect(
            "SELECT 1",
            None,
            ExecOptions::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    match &err {
        CoreError::Aws(m) => {
            assert!(
                m.contains("ExecuteStatement")
                    && m.contains("ThrottlingException")
                    && m.contains("Rate exceeded"),
                "{m}"
            );
        }
        other => panic!("{other:?}"),
    }
    let text = format!("{err} {err:?}");
    assert!(
        !text.contains(SECRET_KEY) && !text.contains(ACCESS_KEY),
        "{text}"
    );
    assert_eq!(server.count("ExecuteStatement"), 4, "1 attempt + 3 retries");
}

#[tokio::test]
async fn service_errors_are_not_retried_bounded_and_free_of_secrets() {
    let huge = format!("{} {}", "x".repeat(20_000), "tail");
    let server = serve(Arc::new(move |_req: &Req| {
        Resp::error(400, "ValidationException", &huge)
    }))
    .await;
    let opts = provisioned(RedshiftAuth::Secret {
        secret_arn: "arn:aws:secretsmanager:eu-west-1:123456789012:secret:rs-app-AbCdEf".into(),
    });
    let events = stream(
        &engine_for(&server, opts),
        "SELECT 1",
        ExecOptions::default(),
    )
    .await;
    let (idx, code, msg) = failure(&events);
    assert_eq!((idx, code.as_str()), (Some(0), "aws"));
    assert!(msg.contains("ValidationException"), "{msg}");
    assert!(msg.len() < 5 * 1024, "error text is bounded: {}", msg.len());
    assert!(!msg.contains(SECRET_KEY) && !msg.contains(ACCESS_KEY));
    assert_eq!(server.all().len(), 1);
}

#[tokio::test]
async fn max_rows_truncates_and_stops_paging() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "FINISHED", true, 5),
        "GetStatementResult" => match body(req)["NextToken"].as_str() {
            None => result_page(
                &[("n", "int4")],
                vec![vec![long(1)], vec![long(2)], vec![long(3)]],
                Some("more"),
            ),
            Some(_) => result_page(&[("n", "int4")], vec![vec![long(4)]], None),
        },
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let e = engine(&server);
    let cap = |n| ExecOptions {
        max_rows: n,
        ..ExecOptions::default()
    };
    let r = e
        .execute_collect("SELECT n FROM t", None, cap(2), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)], vec![json!(2)]]);
    assert_eq!((r.row_count, r.truncated), (2, true));
    assert_eq!(
        server.count("GetStatementResult"),
        1,
        "stops paging at the cap"
    );

    // At the end of a page with more pages to come: truncated, and the next page is not read.
    let before = server.count("GetStatementResult");
    let r = e
        .execute_collect("SELECT n FROM t", None, cap(3), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!((r.row_count, r.truncated), (3, true));
    assert_eq!(server.count("GetStatementResult") - before, 1);

    // Exactly everything: not truncated.
    let r = e
        .execute_collect("SELECT n FROM t", None, cap(4), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!((r.row_count, r.truncated), (4, false));
}

#[tokio::test]
async fn oversized_cells_become_markers_and_big_pages_are_split() {
    let big = "y".repeat(MAX_CELL_BYTES + 10);
    // Under the cell cap, so kept whole; five of them weigh more than one page may.
    let medium = "z".repeat(MAX_CELL_BYTES - 1000);
    let mut records = vec![vec![text(&big)]];
    records.extend((0..5).map(|_| vec![text(&medium)]));
    records.push(vec![text("small")]);
    let server = serve(one_result(&[("v", "varchar")], records)).await;
    let events = stream(
        &engine(&server),
        "SELECT v FROM docs",
        ExecOptions {
            page_size: 100,
            ..ExecOptions::default()
        },
    )
    .await;
    let pages = rows_of(&events);
    let first = &pages[0][0][0];
    // Without the cap a single value could be any size the server sends, copied several times.
    assert!(is_truncated_cell(first), "{}", &first.to_string()[..80]);
    assert_eq!(first["$truncated"]["bytes"], MAX_CELL_BYTES + 10);
    // The page budget cut the page although `page_size` was not reached.
    assert!(pages.len() >= 2, "{} pages", pages.len());
    assert_eq!(pages.iter().map(Vec::len).sum::<usize>(), 7);
    assert_eq!(
        pages[1][0][0].as_str().map(str::len),
        Some(MAX_CELL_BYTES - 1000)
    );
}

#[tokio::test]
async fn read_only_refuses_before_any_request() {
    let server = serve(refuse_everything()).await;
    let e = engine(&server);
    for sql in [
        "UPDATE sales SET qty = 0",
        "DELETE FROM sales",
        "INSERT INTO sales VALUES (1)",
        "CREATE TABLE x AS SELECT 1",
        "SELECT * INTO new_sales FROM sales",
        "DROP TABLE sales",
        "TRUNCATE sales",
        "UNLOAD ('select 1') TO 's3://b/p' IAM_ROLE default",
        "COPY sales FROM 's3://b/p' IAM_ROLE default",
        "CALL refresh_all()",
        "GRANT SELECT ON sales TO bob",
        // A read first must not smuggle a write after it: the whole script is refused.
        "SELECT 1; DELETE FROM sales",
        "SELECT 'unterminated",
        "not sql at all",
    ] {
        let events = stream(&e, sql, ro()).await;
        assert_eq!(kinds(&events), ["started", "failed"], "{sql}");
        let (idx, code, msg) = failure(&events);
        assert_eq!((idx, code.as_str()), (None, "policy_denied"), "{sql}");
        assert!(msg.contains("read-only run"), "{msg}");
        let err = e
            .execute_collect(sql, None, ro(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::PolicyDenied(_)), "{sql}");
    }
    // Without the check these would have been submitted: the Data API has no read-only session.
    assert!(server.all().is_empty(), "{:?}", server.all());
}

#[tokio::test]
async fn read_only_still_runs_queries_and_show_forms() {
    let server = serve(one_result(&[("n", "int4")], vec![vec![long(1)]])).await;
    let e = engine(&server);
    for sql in [
        "SELECT 1",
        "WITH a AS (SELECT 1 AS n) SELECT n FROM a",
        "EXPLAIN SELECT * FROM sales",
        "SHOW TABLES FROM SCHEMA dev.public",
        "SHOW search_path",
    ] {
        e.execute_collect(sql, None, ro(), CancellationToken::new())
            .await
            .unwrap_or_else(|err| panic!("{sql}: {err}"));
    }
}

#[tokio::test]
async fn transaction_control_is_refused_in_every_mode_before_any_request() {
    let server = serve(refuse_everything()).await;
    let e = engine(&server);
    for sql in [
        "BEGIN; UPDATE sales SET qty = 0; COMMIT",
        "START TRANSACTION",
        "UPDATE sales SET qty = 0; ROLLBACK",
        "END",
        "ABORT",
        "COMMIT",
    ] {
        for read_only in [false, true] {
            let events = stream(
                &e,
                sql,
                ExecOptions {
                    read_only,
                    ..ExecOptions::default()
                },
            )
            .await;
            let (idx, code, msg) = failure(&events);
            assert_eq!((idx, code.as_str()), (None, "policy_denied"), "{sql}");
            // Without the refusal the UPDATE would commit before the ROLLBACK, which then does
            // nothing: the script would look atomic and not be.
            assert!(msg.contains("commits each statement on its own"), "{msg}");
        }
    }
    assert!(server.all().is_empty(), "{:?}", server.all());
}

#[tokio::test]
async fn target_and_auth_parameters_per_mode() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "FINISHED", false, 0),
        "ListDatabases" => Resp::ok(&json!({"Databases": ["dev"]})),
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let arn = "arn:aws:secretsmanager:eu-west-1:123456789012:secret:rs-app-AbCdEf";
    let cases: Vec<(RedshiftOptions, Value)> = vec![
        (
            provisioned(RedshiftAuth::IamIdentity),
            json!({"ClusterIdentifier": "analytics-1"}),
        ),
        (
            serverless(RedshiftAuth::IamIdentity),
            json!({"WorkgroupName": "default-wg"}),
        ),
        (
            provisioned(RedshiftAuth::DbUser {
                db_user: "app_reader".into(),
            }),
            json!({"ClusterIdentifier": "analytics-1", "DbUser": "app_reader"}),
        ),
        (
            provisioned(RedshiftAuth::Secret {
                secret_arn: arn.into(),
            }),
            json!({"ClusterIdentifier": "analytics-1", "SecretArn": arn}),
        ),
        (
            serverless(RedshiftAuth::Secret {
                secret_arn: arn.into(),
            }),
            json!({"WorkgroupName": "default-wg", "SecretArn": arn}),
        ),
    ];
    for (opts, want) in cases {
        let before = server.all().len();
        let e = engine_for(&server, opts.clone());
        e.execute_collect(
            "CREATE TABLE t (a int)",
            None,
            ExecOptions::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        e.list_databases().await.unwrap();
        let sent: Vec<Value> = server.all()[before..]
            .iter()
            .filter(|r| matches!(op(r).as_str(), "ExecuteStatement" | "ListDatabases"))
            .map(body)
            .collect();
        assert_eq!(sent.len(), 2);
        for b in sent {
            assert_eq!(b["Database"], "dev", "{opts:?}");
            for key in ["ClusterIdentifier", "WorkgroupName", "DbUser", "SecretArn"] {
                // IAM identity sends neither DbUser nor SecretArn, and only one of the targets.
                assert_eq!(b.get(key), want.get(key), "{key} for {opts:?}: {b}");
            }
        }
    }
}

#[tokio::test]
async fn invalid_identifiers_are_refused_before_anything_is_signed() {
    let bad: Vec<RedshiftOptions> = vec![
        RedshiftOptions {
            target: RedshiftTarget::Provisioned {
                cluster_identifier: "analytics--1".into(),
            },
            ..provisioned(RedshiftAuth::IamIdentity)
        },
        RedshiftOptions {
            target: RedshiftTarget::Serverless {
                workgroup_name: "wg/../x".into(),
            },
            ..provisioned(RedshiftAuth::IamIdentity)
        },
        RedshiftOptions {
            database: "dev; drop".into(),
            ..provisioned(RedshiftAuth::IamIdentity)
        },
        RedshiftOptions {
            aws: AwsTarget {
                identity: AwsIdentity::DefaultChain,
                region: "evil.example.com/x".into(),
            },
            ..provisioned(RedshiftAuth::IamIdentity)
        },
        provisioned(RedshiftAuth::DbUser {
            db_user: "IAM:alice".into(),
        }),
        serverless(RedshiftAuth::DbUser {
            db_user: "alice".into(),
        }),
        provisioned(RedshiftAuth::Secret {
            secret_arn: "arn:aws:s3:::bucket/secret".into(),
        }),
    ];
    for o in bad {
        let err = RedshiftEngine::new(o.clone(), creds()).err();
        assert!(
            matches!(err, Some(CoreError::Invalid(_))),
            "{o:?} was accepted"
        );
    }

    // A database named per call is held to the same rule, before any request.
    let server = serve(refuse_everything()).await;
    let e = engine(&server);
    let (tx, mut rx) = mpsc::channel(8);
    e.execute_stream(
        "q".into(),
        "SELECT 1",
        Some("dev\" ; x"),
        ExecOptions::default(),
        CancellationToken::new(),
        tx,
    )
    .await
    .unwrap();
    let mut events = Vec::new();
    while let Some(ev) = rx.recv().await {
        events.push(ev);
    }
    assert_eq!(failure(&events).1, "invalid");
    assert!(e.list_schemas(Some("a b")).await.is_err());
    assert!(e.list_tables(None, "").await.is_err());
    assert!(server.all().is_empty());
}

#[tokio::test]
async fn database_override_is_sent() {
    let server = serve(one_result(&[("n", "int4")], vec![vec![long(1)]])).await;
    engine(&server)
        .execute_collect(
            "SELECT 1",
            Some(" sales "),
            ExecOptions::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(server.calls("ExecuteStatement")[0]["Database"], "sales");
}

#[tokio::test]
async fn empty_scripts_finish_without_calling_redshift() {
    let server = serve(refuse_everything()).await;
    let e = engine(&server);
    let events = stream(&e, "  -- nothing\n ;; ", opts()).await;
    assert_eq!(kinds(&events), ["started", "finished"]);
    assert_eq!(
        e.execute_collect("", None, opts(), CancellationToken::new())
            .await
            .unwrap(),
        QueryResult::default()
    );
    assert!(server.all().is_empty());
}

// ---- audit fixes (2026-11) -------------------------------------------------------------------

/// HIGH 1: Redshift reads `\'` inside a literal as an escaped quote, the classifier does not.
/// Without the check the first payload is classified as a SELECT of two literals while Redshift
/// runs `SELECT … INTO public.pwned FROM secrets`, and the second hides `pg_terminate_backend`.
#[tokio::test]
async fn backslash_literals_are_refused_before_any_request_in_every_mode() {
    let server = serve(refuse_everything()).await;
    let e = engine(&server);
    for sql in [
        r"SELECT '\' , ' AS x INTO public.pwned FROM secrets -- '",
        r"SELECT '\' , ' , pg_terminate_backend(123) -- '",
    ] {
        for read_only in [false, true] {
            let events = stream(
                &e,
                sql,
                ExecOptions {
                    read_only,
                    ..ExecOptions::default()
                },
            )
            .await;
            let (idx, code, msg) = failure(&events);
            assert_eq!((idx, code.as_str()), (None, "policy_denied"), "{sql}");
            assert!(
                msg.contains("backslash") && msg.contains("CHR(92)"),
                "{msg}"
            );
        }
    }
    assert!(server.all().is_empty(), "{:?}", server.all());
}

/// Re-audit: a non-ASCII dollar-quote tag made the lexers disagree about every string boundary
/// after it. Both payloads are refused in every mode before any request.
#[tokio::test]
async fn non_ascii_dollar_quote_tags_are_refused_before_any_request() {
    let server = serve(refuse_everything()).await;
    let e = engine(&server);
    for sql in [
        r"SELECT $é$ ' $é$ , '\' , ' AS x INTO public.pwned FROM secrets -- '",
        "SELECT $é$ ' $é$ , exfil_lambda(ssn) , $é$ ' $é$ FROM customers",
    ] {
        for read_only in [false, true] {
            let events = stream(
                &e,
                sql,
                ExecOptions {
                    read_only,
                    ..ExecOptions::default()
                },
            )
            .await;
            let (idx, code, _) = failure(&events);
            assert_eq!((idx, code.as_str()), (None, "policy_denied"), "{sql}");
        }
    }
    assert!(server.all().is_empty(), "{:?}", server.all());
}

/// MEDIUM 2: on Redshift nothing but this check stops a Lambda UDF or a SageMaker function in a
/// read-only run from sending row data out.
#[tokio::test]
async fn read_only_refuses_unknown_functions_before_any_request() {
    let server = serve(refuse_everything()).await;
    let e = engine(&server);
    for sql in [
        "SELECT f_notify(card_number) FROM payments",
        "SELECT myschema.f(x) FROM t",
        "SELECT change_user_priority('bob', 'highest')",
    ] {
        let events = stream(&e, sql, ro()).await;
        let (_, code, msg) = failure(&events);
        assert_eq!(code, "policy_denied", "{sql}");
        let name = sql.split('(').next().unwrap().rsplit(' ').next().unwrap();
        assert!(msg.contains(name), "{msg}");
    }
    assert!(server.all().is_empty());
}

/// MEDIUM 3: a statement run without a timeout still has one (the default ceiling), and passing
/// it cancels the statement on the server.
#[tokio::test]
async fn a_run_without_a_timeout_gets_the_default_ceiling() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "STARTED", false, -1),
        "CancelStatement" => Resp::ok(&json!({"Status": true})),
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let e = RedshiftEngine::new(provisioned(RedshiftAuth::IamIdentity), creds())
        .unwrap()
        .with_endpoint_url(&server.url)
        .with_timing(RedshiftTiming {
            default_timeout: Duration::from_millis(80),
            ..fast()
        });
    let events = stream(&e, "SELECT 1", ExecOptions::default()).await;
    assert_eq!(failure(&events).1, "timeout");
    assert_eq!(server.calls("CancelStatement")[0]["Id"], "stmt-1");
}

/// MEDIUM 3: when the result receiver goes away (the window closed) while the statement runs,
/// polling stops and the statement is cancelled on the server.
#[tokio::test]
async fn a_closed_result_channel_stops_polling_and_cancels() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "STARTED", false, -1),
        "CancelStatement" => Resp::ok(&json!({"Status": true})),
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let e = Arc::new(engine(&server));
    let (tx, mut rx) = mpsc::channel(4);
    let task = {
        let e = e.clone();
        tokio::spawn(async move {
            e.execute_stream(
                "q".into(),
                "SELECT * FROM big",
                None,
                ExecOptions::default(),
                CancellationToken::new(),
                tx,
            )
            .await
        })
    };
    assert!(matches!(rx.recv().await, Some(QueryEvent::Started { .. })));
    for _ in 0..200 {
        if server.count("DescribeStatement") >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    drop(rx);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the run noticed the closed channel")
        .unwrap()
        .unwrap();
    assert_eq!(server.calls("CancelStatement")[0]["Id"], "stmt-1");
}

/// MEDIUM 3: the catalog call is clamped to the default table cap whatever IPC asks for.
#[tokio::test]
async fn schema_metadata_clamps_the_table_cap() {
    let tables: Vec<serde_json::Value> = (0..2500)
        .map(|i| json!({"schema": "public", "name": format!("t{i}"), "type": "TABLE"}))
        .collect();
    let server = serve(Arc::new(move |req: &Req| match op(req).as_str() {
        "ListTables" => Resp::ok(&json!({"Tables": tables})),
        "DescribeTable" => Resp::ok(&json!({"ColumnList": []})),
        other => Resp::error(400, "ValidationException", &format!("unexpected {other}")),
    }))
    .await;
    let meta = engine(&server)
        .schema_metadata(None, Some("public"), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        meta.tables.len(),
        vibelens_core::redshift::DEFAULT_MAX_TABLES
    );
    assert!(meta.truncated);
}

/// MEDIUM 3: the whole catalog call has a deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn schema_metadata_has_an_overall_deadline() {
    let server = serve(Arc::new(|_req: &Req| {
        std::thread::sleep(Duration::from_millis(300));
        Resp::ok(&json!({"Schemas": ["public"]}))
    }))
    .await;
    let e = RedshiftEngine::new(provisioned(RedshiftAuth::IamIdentity), creds())
        .unwrap()
        .with_endpoint_url(&server.url)
        .with_timing(RedshiftTiming {
            metadata_timeout: Duration::from_millis(100),
            ..fast()
        });
    let err = e.schema_metadata(None, None, 10).await.unwrap_err();
    assert!(matches!(err, CoreError::Timeout(_)), "{err:?}");
}

/// LOW 4: a requested database must be ASCII, so a look-alike of the default cannot be named.
#[tokio::test]
async fn a_look_alike_database_name_is_refused() {
    let server = serve(refuse_everything()).await;
    let e = engine(&server);
    let err = e
        .execute_collect(
            "SELECT 1",
            Some("d\u{0435}v"),
            ExecOptions::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ASCII"), "{err}");
    assert!(e.list_schemas(Some("d\u{0435}v")).await.is_err());
    assert!(server.all().is_empty());
}

// ---- catalog browsing ------------------------------------------------------------------------

#[tokio::test]
async fn listings_page_and_filter_to_the_exact_schema() {
    let server = serve(Arc::new(|req: &Req| {
        let b = body(req);
        match op(req).as_str() {
            "ListDatabases" => match b["NextToken"].as_str() {
                None => Resp::ok(&json!({"Databases": ["dev", "sales"], "NextToken": "d2"})),
                Some(_) => Resp::ok(&json!({"Databases": ["staging"]})),
            },
            "ListSchemas" => Resp::ok(&json!({"Schemas": ["public", "pg_catalog", "my_s"]})),
            "ListTables" => Resp::ok(&json!({"Tables": [
                {"schema": "my_s", "name": "orders", "type": "TABLE"},
                {"schema": "my_s", "name": "v_orders", "type": "VIEW"},
                // `my_s` is a LIKE pattern: `myXs` matches it too and must be dropped.
                {"schema": "myXs", "name": "other", "type": "TABLE"},
            ]})),
            "DescribeTable" => match b["NextToken"].as_str() {
                None => Resp::ok(&json!({"ColumnList": [
                    {"name": "id", "typeName": "int8", "nullable": 0, "schemaName": "my_s", "tableName": "orders", "length": 0, "precision": 19, "scale": 0},
                    {"name": "total", "typeName": "numeric", "nullable": 1, "schemaName": "my_s", "tableName": "orders", "precision": 12, "scale": 2, "length": 0, "columnDefault": "0"},
                ], "NextToken": "c2"})),
                Some(_) => Resp::ok(&json!({"ColumnList": [
                    {"name": "note", "typeName": "varchar", "nullable": 1, "schemaName": "my_s", "tableName": "orders", "length": 256, "precision": 0, "scale": 0},
                    {"name": "id", "typeName": "int8", "nullable": 1, "schemaName": "my_s", "tableName": "v_orders", "length": 0, "precision": 19, "scale": 0},
                ]})),
            },
            other => Resp::error(400, "ValidationException", &format!("unexpected {other}")),
        }
    }))
    .await;
    let e = engine_for(&server, serverless(RedshiftAuth::IamIdentity));

    assert_eq!(
        e.list_databases().await.unwrap(),
        ["dev", "sales", "staging"]
    );
    assert_eq!(server.calls("ListDatabases")[1]["NextToken"], "d2");
    assert_eq!(
        e.list_schemas(Some("sales")).await.unwrap(),
        ["public", "pg_catalog", "my_s"]
    );
    assert_eq!(server.calls("ListSchemas")[0]["Database"], "sales");

    let tables = e.list_tables(None, "my_s").await.unwrap();
    assert_eq!(
        tables,
        vec![
            RedshiftTableRef {
                schema: "my_s".into(),
                name: "orders".into(),
                kind: "TABLE".into()
            },
            RedshiftTableRef {
                schema: "my_s".into(),
                name: "v_orders".into(),
                kind: "VIEW".into()
            },
        ]
    );
    assert_eq!(server.calls("ListTables")[0]["SchemaPattern"], "my_s");
    assert_eq!(server.calls("ListTables")[0]["WorkgroupName"], "default-wg");

    let cols = e.describe_table(None, "my_s", "orders").await.unwrap();
    assert_eq!(
        cols,
        vec![
            RedshiftColumn {
                name: "id".into(),
                type_name: "int8".into(),
                nullable: false,
                length: None,
                precision: Some(19),
                scale: Some(0),
                default_value: None,
            },
            RedshiftColumn {
                name: "total".into(),
                type_name: "numeric(12,2)".into(),
                nullable: true,
                length: None,
                precision: Some(12),
                scale: Some(2),
                default_value: Some("0".into()),
            },
            RedshiftColumn {
                name: "note".into(),
                type_name: "varchar".into(),
                nullable: true,
                length: Some(256),
                precision: None,
                scale: None,
                default_value: None,
            },
        ]
    );
    let d = &server.calls("DescribeTable")[0];
    assert_eq!(
        (&d["Schema"], &d["Table"]),
        (&json!("my_s"), &json!("orders"))
    );

    let meta = e.schema_metadata(None, Some("my_s"), 10).await.unwrap();
    assert_eq!(meta.database, "dev");
    assert!(!meta.truncated);
    assert_eq!(meta.tables.len(), 2);
    assert_eq!(meta.tables[0].name, "orders");
    assert_eq!(meta.tables[0].columns.len(), 3);
    assert_eq!(meta.tables[1].kind, ObjectKind::View);
    assert_eq!(meta.tables[1].columns[0].name, "id");
    // One DescribeTable per schema (no table named), not one per table.
    let schema_wide: Vec<Value> = server
        .calls("DescribeTable")
        .into_iter()
        .filter(|b| b.get("Table").is_none())
        .collect();
    assert_eq!(schema_wide.len(), 2, "two pages of one schema-wide call");

    // A cap below the table count says so.
    let meta = e.schema_metadata(None, Some("my_s"), 1).await.unwrap();
    assert_eq!(meta.tables.len(), 1);
    assert!(meta.truncated);

    // Whole database: system schemas are skipped.
    let before = server.calls("ListTables").len();
    e.schema_metadata(None, None, 100).await.unwrap();
    let listed: Vec<Value> = server.calls("ListTables")[before..].to_vec();
    assert_eq!(
        listed
            .iter()
            .map(|b| b["SchemaPattern"].clone())
            .collect::<Vec<_>>(),
        [json!("public"), json!("my_s")]
    );
}

#[tokio::test]
async fn describing_a_missing_table_is_not_found() {
    let server = serve(Arc::new(|_req: &Req| Resp::ok(&json!({"ColumnList": []})))).await;
    let err = engine(&server)
        .describe_table(None, "public", "nope")
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::NotFound(_)), "{err:?}");
}

/// Audit 2026-11-21, item 6: an agent Stop or a tool timeout drops the future that runs the
/// statement. Without the run task and its cancel-on-drop guard the statement would keep
/// running in the warehouse.
#[tokio::test]
async fn dropping_the_collect_future_still_cancels_the_statement() {
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "STARTED", false, -1),
        "CancelStatement" => Resp::ok(&json!({"Status": true})),
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let e = engine(&server);
    let run = e.execute_collect(
        "SELECT * FROM big",
        None,
        ExecOptions::default(),
        CancellationToken::new(),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), run)
            .await
            .is_err()
    );
    for _ in 0..200 {
        if server.count("CancelStatement") > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(server.calls("CancelStatement")[0]["Id"], "stmt-1");

    // And with the token: the call returns only after CancelStatement was sent.
    let server = serve(Arc::new(|req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-2"),
        "DescribeStatement" => described("stmt-2", "STARTED", false, -1),
        "CancelStatement" => Resp::ok(&json!({"Status": true})),
        _ => Resp::error(400, "ValidationException", "unexpected"),
    }))
    .await;
    let e = engine(&server);
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.cancel();
    });
    let err = e
        .execute_collect("SELECT * FROM big", None, ExecOptions::default(), cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");
    assert_eq!(server.count("CancelStatement"), 1);
}
