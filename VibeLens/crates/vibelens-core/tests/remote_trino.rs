//! The Trino engine against an in-process mock speaking Trino's client REST protocol.
#![cfg(feature = "iceberg")]

mod remote_common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use remote_common::*;
use secrecy::SecretString;
use serde_json::{Value, json};
use vibelens_core::CoreError;
use vibelens_core::model::*;
use vibelens_core::remote::{HttpLimits, TrinoConfig, TrinoEngine};

const PASSWORD: &str = "Tr1no-S3cret-pw";

fn config(url: &str) -> TrinoConfig {
    TrinoConfig {
        url: url.to_string(),
        catalog: "lake".into(),
        schema: Some("shop".into()),
        user: "ana".into(),
        auth: HttpEngineAuth::Basic {
            username: "ana".into(),
        },
        ca_cert_path: None,
    }
}

fn engine(url: &str) -> TrinoEngine {
    TrinoEngine::new(config(url), Some(SecretString::from(PASSWORD.to_string())))
        .unwrap()
        .with_timing(timing())
}

fn basic_header() -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("ana:{PASSWORD}"))
    )
}

/// A mock whose handler sees its own base URL (for `nextUri`).
async fn trino(
    handler: impl Fn(&str, &Req) -> Resp + Send + Sync + 'static,
) -> (MockServer, Arc<OnceLock<String>>) {
    let base: Arc<OnceLock<String>> = Arc::default();
    let b = base.clone();
    let server = serve(Arc::new(move |req: &Req| {
        handler(b.get().map(String::as_str).unwrap_or(""), req)
    }))
    .await;
    base.set(server.url.clone()).unwrap();
    (server, base)
}

fn page(id: &str, next: Option<String>, state: &str) -> Value {
    let mut v = json!({"id": id, "infoUri": "http://elsewhere/ui", "stats": {"state": state}});
    if let Some(n) = next {
        v["nextUri"] = json!(n);
    }
    v
}

const COLS: &str = r#"[{"name":"id","type":"bigint"},{"name":"name","type":"varchar"},{"name":"at","type":"timestamp(3)"}]"#;

async fn stream(e: &TrinoEngine, sql: &str, o: ExecOptions) -> Vec<QueryEvent> {
    collect_events(None, |cancel, tx| {
        e.execute_stream("q1".into(), sql, o, cancel, tx)
    })
    .await
}

#[tokio::test]
async fn select_follows_next_uri_pages_rows_and_reports_stats() {
    let (server, _) = trino(|base, req| {
        let path = req.path.as_str();
        match (req.method.as_str(), path) {
            ("POST", "/v1/statement") => Resp::ok(&page(
                "20240131_000000_00001_abcde",
                Some(format!("{base}/v1/statement/queued/q/1")),
                "QUEUED",
            )),
            ("GET", "/v1/statement/queued/q/1") => {
                let mut p = page(
                    "20240131_000000_00001_abcde",
                    Some(format!("{base}/v1/statement/executing/q/2")),
                    "RUNNING",
                );
                p["columns"] = serde_json::from_str(COLS).unwrap();
                p["data"] = json!([
                    [1, "ann", "2024-01-31 12:34:56.789"],
                    [9007199254740993u64, null, null]
                ]);
                Resp::ok(&p)
            }
            ("GET", "/v1/statement/executing/q/2") => {
                let mut p = page(
                    "20240131_000000_00001_abcde",
                    Some(format!("{base}/v1/statement/executing/q/3")),
                    "RUNNING",
                );
                p["columns"] = serde_json::from_str(COLS).unwrap();
                p["data"] = json!([[3, "cy", null]]);
                Resp::ok(&p)
            }
            ("GET", "/v1/statement/executing/q/3") => {
                let mut p = page("20240131_000000_00001_abcde", None, "FINISHED");
                p["stats"]["processedBytes"] = json!(123456);
                Resp::ok(&p)
            }
            _ => Resp::text(404, "unexpected"),
        }
    })
    .await;
    let e = engine(&server.url);
    let events = stream(&e, "SELECT id, name, at FROM orders;", opts()).await;
    assert_eq!(
        kinds(&events),
        [
            "started", "columns", "rows", "rows", "stats", "done", "finished"
        ],
        "{events:#?}"
    );
    let QueryEvent::Columns { columns, .. } = &events[1] else {
        panic!()
    };
    assert_eq!(columns[2].type_name, "timestamp(3)");
    assert_eq!(
        rows(&events),
        vec![
            vec![json!("1"), json!("ann"), json!("2024-01-31T12:34:56.789")],
            vec![json!("9007199254740993"), Value::Null, Value::Null],
            vec![json!("3"), json!("cy"), Value::Null],
        ]
    );
    let QueryEvent::Stats {
        engine,
        bytes_scanned,
        cost_usd_estimate,
        engine_query_id,
        ..
    } = &events[4]
    else {
        panic!()
    };
    assert_eq!(engine, "trino");
    assert_eq!(*bytes_scanned, Some(123456));
    assert_eq!(*cost_usd_estimate, None);
    assert_eq!(
        engine_query_id.as_deref(),
        Some("20240131_000000_00001_abcde")
    );
    let QueryEvent::StatementDone {
        row_count,
        truncated,
        command_tag,
        ..
    } = &events[5]
    else {
        panic!()
    };
    assert_eq!((*row_count, *truncated), (3, false));
    assert_eq!(command_tag.as_deref(), Some("SELECT"));

    // Every request carries the profile's headers and the credential; the SQL is the body.
    let all = server.all();
    assert_eq!(all.len(), 4);
    assert_eq!(all[0].body, "SELECT id, name, at FROM orders");
    for r in &all {
        assert_eq!(r.headers["x-trino-user"], "ana");
        assert_eq!(r.headers["x-trino-catalog"], "lake");
        assert_eq!(r.headers["x-trino-schema"], "shop");
        assert_eq!(r.headers["x-trino-source"], "vibelens");
        assert_eq!(r.headers["authorization"], basic_header());
    }
}

#[tokio::test]
async fn session_headers_from_the_server_are_not_carried_forward() {
    let (server, _) = trino(|base, req| match req.path.as_str() {
        "/v1/statement" => {
            let mut r = Resp::ok(&page(
                "q",
                Some(format!("{base}/v1/statement/x/1")),
                "QUEUED",
            ));
            r.headers
                .push(("x-trino-set-catalog".into(), "evil".into()));
            r.headers.push(("x-trino-set-session".into(), "a=b".into()));
            r
        }
        _ => Resp::ok(&page("q", None, "FINISHED")),
    })
    .await;
    let e = engine(&server.url);
    stream(&e, "SELECT 1", opts()).await;
    stream(&e, "SELECT 2", opts()).await;
    for r in server.all() {
        assert_eq!(r.headers["x-trino-catalog"], "lake");
        assert!(!r.headers.contains_key("x-trino-session"));
    }
}

#[tokio::test]
async fn a_write_reports_rows_affected_without_a_result_set() {
    let (server, _) = trino(|base, req| match req.path.as_str() {
        "/v1/statement" => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "QUEUED",
        )),
        _ => {
            let mut p = page("q", None, "FINISHED");
            p["updateType"] = json!("INSERT");
            p["updateCount"] = json!(3);
            p["columns"] = json!([{"name": "rows", "type": "bigint"}]);
            p["data"] = json!([[3]]);
            Resp::ok(&p)
        }
    })
    .await;
    let r = engine(&server.url)
        .execute_collect("INSERT INTO t SELECT * FROM s", ExecOptions::default())
        .await
        .unwrap();
    assert!(r.columns.is_empty() && r.rows.is_empty());
    assert_eq!(r.rows_affected, Some(3));
    assert_eq!(r.command_tag.as_deref(), Some("INSERT"));
}

#[tokio::test]
async fn a_query_error_fails_the_statement_with_trinos_message_and_no_secret() {
    let (server, _) = trino(|base, req| match req.path.as_str() {
        "/v1/statement" => Resp::ok(&page("q", Some(format!("{base}/v1/statement/x/1")), "QUEUED")),
        _ => {
            let mut p = page("q", None, "FAILED");
            p["error"] = json!({
                "message": format!("line 1:8: Column 'nope' cannot be resolved (auth was {PASSWORD})"),
                "errorName": "COLUMN_NOT_FOUND",
                "errorType": "USER_ERROR",
                "failureInfo": {"stack": ["io.trino.Something.java:12"]}
            });
            Resp::ok(&p)
        }
    })
    .await;
    let events = stream(&engine(&server.url), "SELECT nope", opts()).await;
    let (idx, code, msg) = failure(&events);
    assert_eq!((idx, code.as_str()), (Some(0), "database"));
    assert!(
        msg.contains("COLUMN_NOT_FOUND") && msg.contains("cannot be resolved"),
        "{msg}"
    );
    assert!(
        !msg.contains(PASSWORD) && !msg.contains("Something.java"),
        "{msg}"
    );
}

/// Without the redaction, a server that echoes the request (or an error that prints it) would
/// put the password, or the Basic header that encodes it, into the UI and the agent's context.
#[tokio::test]
async fn a_rejected_credential_is_reported_without_the_secret() {
    let (server, _) = trino(|_, req| {
        Resp::text(
            401,
            &format!(
                "bad credentials: {} / {}",
                req.headers
                    .get("authorization")
                    .cloned()
                    .unwrap_or_default(),
                PASSWORD
            ),
        )
    })
    .await;
    let err = engine(&server.url)
        .execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(matches!(err, CoreError::Auth(_)), "{err:?}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("ana:{PASSWORD}"));
    assert!(!msg.contains(PASSWORD) && !msg.contains(&encoded), "{msg}");
}

#[tokio::test]
async fn next_uri_gets_are_retried_on_503_but_the_post_is_not() {
    let gets = Arc::new(AtomicUsize::new(0));
    let g = gets.clone();
    let (server, _) = trino(move |base, req| match req.path.as_str() {
        "/v1/statement" => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "QUEUED",
        )),
        _ => match g.fetch_add(1, Ordering::SeqCst) {
            0 => Resp::text(503, "busy"),
            1 => Resp::text(502, "busy"),
            _ => Resp::ok(&page("q", None, "FINISHED")),
        },
    })
    .await;
    let events = stream(&engine(&server.url), "SELECT 1", opts()).await;
    assert_eq!(kinds(&events).last(), Some(&"finished"), "{events:#?}");
    assert_eq!(server.calls("GET", "/v1/statement/x/1").len(), 3);

    let (server, _) = trino(|_, _| Resp::text(503, "busy")).await;
    let events = stream(&engine(&server.url), "SELECT 1", opts()).await;
    assert_eq!(failure(&events).1, "other");
    assert_eq!(server.count(), 1, "POST /v1/statement is not idempotent");
}

/// Without the endpoint check, a coordinator (or anyone who can answer for it) could point
/// `nextUri` at another host, and the Basic credential would be sent there.
#[tokio::test]
async fn a_next_uri_on_another_host_port_or_scheme_is_never_requested() {
    type Make = Box<dyn Fn(&str, u16) -> String + Send + Sync>;
    let elsewhere = serve(Arc::new(|_: &Req| Resp::ok(&json!({})))).await;
    let cases: Vec<(&str, Make)> = vec![
        (
            "port",
            Box::new(|_: &str, port: u16| format!("http://127.0.0.1:{port}/v1/statement/x/1")),
        ),
        (
            "host",
            Box::new(|base: &str, _| base.replace("127.0.0.1", "localhost") + "/v1/statement/x/1"),
        ),
        (
            "scheme",
            Box::new(|base: &str, _| base.replace("http://", "https://") + "/v1/statement/x/1"),
        ),
    ];
    for (case, make) in cases {
        let port = elsewhere.port;
        let (server, _) = trino(move |base, req| match req.method.as_str() {
            "POST" => Resp::ok(&page("q", Some(make(base, port)), "QUEUED")),
            _ => Resp::ok(&page("q", None, "FINISHED")),
        })
        .await;
        let events = stream(&engine(&server.url), "SELECT 1", opts()).await;
        let (_, _, msg) = failure(&events);
        assert!(
            msg.contains("not the configured endpoint")
                && msg.contains("http-server.process-forwarded=true"),
            "{case}: {msg}"
        );
        assert_eq!(server.count(), 1, "{case}: only the POST");
    }
    assert_eq!(elsewhere.count(), 0);
}

/// Without this, a redirect would carry the Authorization header and the SQL to its target.
#[tokio::test]
async fn a_redirect_is_not_followed_and_its_target_is_shown_without_credentials() {
    let elsewhere = serve(Arc::new(|_: &Req| Resp::ok(&json!({})))).await;
    let target = format!(
        "http://user:pw@127.0.0.1:{}/v1/statement?token=abc#f",
        elsewhere.port
    );
    let (server, _) = trino(move |_, _| Resp::redirect(&target)).await;
    let events = stream(&engine(&server.url), "SELECT 1", opts()).await;
    let (_, _, msg) = failure(&events);
    assert!(
        msg.contains("redirect") && msg.contains(&format!("127.0.0.1:{}", elsewhere.port)),
        "{msg}"
    );
    assert!(
        !msg.contains("token=abc") && !msg.contains("pw@") && !msg.contains("#f"),
        "{msg}"
    );
    assert_eq!(elsewhere.count(), 0);
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn a_credential_is_refused_over_cleartext_to_another_machine() {
    for auth in [
        HttpEngineAuth::Bearer,
        HttpEngineAuth::Basic {
            username: "ana".into(),
        },
    ] {
        let cfg = TrinoConfig {
            auth,
            ..config("http://10.255.255.1:8080")
        };
        let r = TrinoEngine::new(cfg, Some(SecretString::from(PASSWORD.to_string())));
        match r {
            Err(CoreError::Invalid(m)) => assert!(m.contains("unencrypted"), "{m}"),
            Err(e) => panic!("{e:?}"),
            Ok(_) => panic!("accepted a credential over cleartext"),
        }
    }
    // No credential: allowed, nothing secret travels.
    let cfg = TrinoConfig {
        auth: HttpEngineAuth::None,
        ..config("http://10.255.255.1:8080")
    };
    assert!(TrinoEngine::new(cfg, None).is_ok());
}

/// Read-only is enforced before submission: Trino has no read-only transaction.
#[tokio::test]
async fn read_only_and_session_refusals_happen_before_any_request() {
    let (server, _) = trino(|_, _| Resp::ok(&page("q", None, "FINISHED"))).await;
    let e = engine(&server.url);
    let ro = ExecOptions {
        read_only: true,
        ..opts()
    };
    for sql in [
        "DELETE FROM t",
        "SELECT 1; DROP TABLE t",
        "INSERT INTO t VALUES (1)",
        "CALL system.rollback_to_snapshot('a', 't', 1)",
    ] {
        let events = stream(&e, sql, ro.clone()).await;
        assert_eq!(failure(&events).1, "policy_denied", "{sql}");
    }
    for sql in ["USE lake.other", "SET SESSION a = 'b'", "START TRANSACTION"] {
        let events = stream(&e, sql, opts()).await;
        let (_, code, msg) = failure(&events);
        assert_eq!(code, "policy_denied", "{sql}");
        assert!(msg.contains("catalog.schema.table"), "{msg}");
    }
    assert_eq!(server.count(), 0);
    let events = stream(&e, "SHOW TABLES", ro).await;
    assert_eq!(kinds(&events).last(), Some(&"finished"));
}

/// Without the DELETE, a cancelled query would keep running (and using the cluster).
#[tokio::test]
async fn cancel_deletes_the_query_on_the_coordinator() {
    let (server, _) = trino(|base, req| match (req.method.as_str(), req.path.as_str()) {
        ("POST", _) => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "QUEUED",
        )),
        ("DELETE", _) => Resp::text(204, ""),
        _ => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "RUNNING",
        ))
        .after(Duration::from_millis(20)),
    })
    .await;
    let e = engine(&server.url);
    let events = collect_events(Some(Duration::from_millis(150)), |cancel, tx| {
        e.execute_stream("q1".into(), "SELECT * FROM big", opts(), cancel, tx)
    })
    .await;
    assert_eq!(failure(&events).1, "cancelled");
    let dels = server.calls("DELETE", "/v1/statement/x/1");
    assert_eq!(dels.len(), 1);
    assert_eq!(dels[0].headers["authorization"], basic_header());
}

#[tokio::test]
async fn a_timeout_fails_the_statement_and_deletes_the_query() {
    let (server, _) = trino(|base, req| match req.method.as_str() {
        "POST" => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "QUEUED",
        )),
        "DELETE" => Resp::text(204, ""),
        _ => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "RUNNING",
        ))
        .after(Duration::from_millis(30)),
    })
    .await;
    let o = ExecOptions {
        timeout_ms: Some(150),
        ..opts()
    };
    let events = stream(&engine(&server.url), "SELECT * FROM big", o).await;
    let (_, code, msg) = failure(&events);
    assert_eq!(code, "timeout", "{msg}");
    assert_eq!(server.calls("DELETE", "/v1/statement/x/1").len(), 1);
}

#[tokio::test]
async fn max_rows_truncates_and_stops_the_query() {
    let (server, _) = trino(|base, req| match req.method.as_str() {
        "POST" => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "QUEUED",
        )),
        "DELETE" => Resp::text(204, ""),
        _ => {
            let mut p = page("q", Some(format!("{base}/v1/statement/x/1")), "RUNNING");
            p["columns"] = json!([{"name": "n", "type": "integer"}]);
            p["data"] = json!([[1], [2], [3]]);
            Resp::ok(&p)
        }
    })
    .await;
    let o = ExecOptions {
        max_rows: 4,
        ..opts()
    };
    let r = engine(&server.url)
        .execute_collect("SELECT n FROM t", o)
        .await
        .unwrap();
    assert_eq!((r.row_count, r.truncated), (4, true));
    assert_eq!(server.calls("DELETE", "/v1/statement/x/1").len(), 1);
}

#[tokio::test]
async fn oversized_bodies_fail_and_oversized_cells_are_cut() {
    let (server, _) = trino(|_, _| Resp::ok(&json!({"pad": "x".repeat(4096)}))).await;
    let e = TrinoEngine::with_limits(
        config(&server.url),
        Some(SecretString::from(PASSWORD.to_string())),
        HttpLimits {
            max_body_bytes: 1024,
            ..HttpLimits::default()
        },
    )
    .unwrap();
    let err = e
        .execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("larger than"), "{err}");

    let big = "y".repeat(MAX_CELL_BYTES + 10);
    let (server, _) = trino(move |base, req| match req.method.as_str() {
        "POST" => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "QUEUED",
        )),
        _ => {
            let mut p = page("q", None, "FINISHED");
            p["columns"] = json!([{"name": "s", "type": "varchar"}]);
            p["data"] = json!([[big.clone()]]);
            Resp::ok(&p)
        }
    })
    .await;
    let r = engine(&server.url)
        .execute_collect("SELECT s FROM t", ExecOptions::default())
        .await
        .unwrap();
    assert!(
        is_truncated_cell(&r.rows[0][0]),
        "{:?}",
        &r.rows[0][0].to_string()[..80]
    );
    assert_eq!(r.rows[0][0]["$truncated"]["bytes"], MAX_CELL_BYTES + 10);
}

#[tokio::test]
async fn a_script_runs_in_order_and_stops_at_the_first_failure() {
    let (server, _) = trino(|base, req| match req.method.as_str() {
        "POST" if req.body.contains("bad") => {
            let mut p = page("q", None, "FAILED");
            p["error"] = json!({"message": "boom", "errorName": "GENERIC"});
            Resp::ok(&p)
        }
        "POST" => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "QUEUED",
        )),
        _ => Resp::ok(&page("q", None, "FINISHED")),
    })
    .await;
    let events = stream(
        &engine(&server.url),
        "SELECT 1; SELECT bad; SELECT 3",
        opts(),
    )
    .await;
    assert_eq!(failure(&events).0, Some(1));
    let posts: Vec<String> = server
        .calls("POST", "/v1/statement")
        .into_iter()
        .map(|r| r.body)
        .collect();
    assert_eq!(posts, ["SELECT 1", "SELECT bad"]);
}

// ---- Audit 2026-11-21 ---------------------------------------------------------------------

/// Item 7: behind a shared gateway the coordinator lives under a path. Without the path check a
/// hostile coordinator could send the credential to another service on the same host.
#[tokio::test]
async fn a_next_uri_outside_the_statement_protocol_is_never_requested() {
    for (case, next) in [
        ("other service", "/other-service/admin/x"),
        ("sibling of the prefix", "/trino-admin/v1/statement/x/1"),
        ("query API", "/trino/v1/query/q"),
        ("user info", "USERINFO"),
    ] {
        let next = next.to_string();
        let (server, _) = trino(move |base, req| match req.method.as_str() {
            "POST" if req.path == "/trino/v1/statement" => {
                let uri = if next == "USERINFO" {
                    base.replace("http://", "http://u:p@") + "/trino/v1/statement/x/1"
                } else {
                    format!("{base}{next}")
                };
                Resp::ok(&page("q", Some(uri), "QUEUED"))
            }
            _ => Resp::ok(&page("q", None, "FINISHED")),
        })
        .await;
        let e = engine(&format!("{}/trino/", server.url));
        let events = stream(&e, "SELECT 1", opts()).await;
        let (_, _, msg) = failure(&events);
        assert!(msg.contains("nextUri"), "{case}: {msg}");
        assert_eq!(server.count(), 1, "{case}: only the POST");
    }
    // The protocol's own URLs under the prefix are followed.
    let (server, _) = trino(|base, req| match req.method.as_str() {
        "POST" => Resp::ok(&page(
            "q",
            Some(format!("{base}/trino/v1/statement/executing/q/s/1")),
            "QUEUED",
        )),
        _ => Resp::ok(&page("q", None, "FINISHED")),
    })
    .await;
    let events = stream(
        &engine(&format!("{}/trino", server.url)),
        "SELECT 1",
        opts(),
    )
    .await;
    assert_eq!(kinds(&events).last(), Some(&"finished"), "{events:#?}");
}

/// Item 10: every early exit deletes the query, not only cancel, timeout and truncation.
/// Without it an oversized or malformed answer, or a coordinator that keeps failing, would
/// leave the query running on the cluster.
#[tokio::test]
async fn every_early_exit_deletes_the_query() {
    for case in ["oversized", "bad json", "503 after retries"] {
        let (server, _) = trino(move |base, req| match req.method.as_str() {
            "POST" => Resp::ok(&page(
                "q",
                Some(format!("{base}/v1/statement/x/1")),
                "QUEUED",
            )),
            "DELETE" => Resp::text(204, ""),
            _ => match case {
                "oversized" => Resp::ok(&json!({"pad": "x".repeat(8000)})),
                "bad json" => Resp::text(200, "this is not json"),
                _ => Resp::text(503, "busy"),
            },
        })
        .await;
        let e = TrinoEngine::with_limits(
            config(&server.url),
            Some(SecretString::from(PASSWORD.to_string())),
            HttpLimits {
                max_body_bytes: 4096,
                ..HttpLimits::default()
            },
        )
        .unwrap()
        .with_timing(timing());
        let events = stream(&e, "SELECT 1", opts()).await;
        failure(&events);
        assert_eq!(
            server.calls("DELETE", "/v1/statement/x/1").len(),
            1,
            "{case}"
        );
    }
}

/// Item 9: a declared column count is bounded and a row must match the columns.
#[tokio::test]
async fn columns_are_capped_and_rows_must_match_them() {
    for case in ["too many columns", "short row", "long row"] {
        let (server, _) = trino(move |base, req| match req.method.as_str() {
            "POST" => Resp::ok(&page(
                "q",
                Some(format!("{base}/v1/statement/x/1")),
                "QUEUED",
            )),
            "DELETE" => Resp::text(204, ""),
            _ => {
                let mut p = page("q", Some(format!("{base}/v1/statement/x/2")), "RUNNING");
                let n = if case == "too many columns" { 2001 } else { 2 };
                p["columns"] = Value::Array(
                    (0..n)
                        .map(|i| json!({"name": format!("c{i}"), "type": "integer"}))
                        .collect(),
                );
                p["data"] = match case {
                    "short row" => json!([[1]]),
                    "long row" => json!([[1, 2, 3]]),
                    _ => json!([]),
                };
                Resp::ok(&p)
            }
        })
        .await;
        let events = stream(&engine(&server.url), "SELECT * FROM t", opts()).await;
        let (_, _, msg) = failure(&events);
        assert!(
            msg.contains("column") || msg.contains("values"),
            "{case}: {msg}"
        );
        let deletes = server.all().iter().filter(|r| r.method == "DELETE").count();
        assert_eq!(deletes, 1, "{case}");
    }
}

/// Item 6: the bounded run the agent uses deletes the query when its token fires, before it
/// returns.
#[tokio::test]
async fn cancelling_a_collect_run_deletes_the_query() {
    let (server, _) = trino(|base, req| match req.method.as_str() {
        "POST" => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "QUEUED",
        )),
        "DELETE" => Resp::text(204, ""),
        _ => Resp::ok(&page(
            "q",
            Some(format!("{base}/v1/statement/x/1")),
            "RUNNING",
        ))
        .after(Duration::from_millis(20)),
    })
    .await;
    let e = engine(&server.url);
    let cancel = tokio_util::sync::CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c.cancel();
    });
    let err = e
        .execute_collect_cancellable("SELECT * FROM big", ExecOptions::default(), cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");
    assert_eq!(server.calls("DELETE", "/v1/statement/x/1").len(), 1);
}

/// Item 10: a table function in a read-only run is refused before any request.
#[tokio::test]
async fn table_functions_are_refused_before_any_request() {
    let (server, _) = trino(|_, _| Resp::ok(&page("q", None, "FINISHED"))).await;
    let ro = ExecOptions {
        read_only: true,
        ..opts()
    };
    let events = stream(
        &engine(&server.url),
        "SELECT * FROM TABLE(pg.system.query(query => 'DELETE FROM accounts RETURNING 1'))",
        ro,
    )
    .await;
    assert_eq!(failure(&events).1, "policy_denied");
    assert_eq!(server.count(), 0);
}
