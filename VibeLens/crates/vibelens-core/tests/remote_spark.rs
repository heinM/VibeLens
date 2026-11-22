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
    /// Name and proxy user of the last session created, as `GET /sessions/{id}` reports them.
    created: Mutex<(String, Value)>,
    /// `GET /sessions/{id}` reports another name (Livy restarted, the id is someone else's).
    foreign: std::sync::atomic::AtomicBool,
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
            created: Mutex::new((String::new(), Value::Null)),
            foreign: std::sync::atomic::AtomicBool::new(false),
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
                let body = req.json();
                *self.created.lock().unwrap() = (
                    body["name"].as_str().unwrap_or("").to_string(),
                    body["proxyUser"].clone(),
                );
                self.foreign.store(false, Ordering::SeqCst);
                Resp::json(201, &json!({"id": id, "state": "starting", "kind": "sql"}))
            }
            ("GET", ["sessions", _]) => {
                let n = self.session_polls.fetch_add(1, Ordering::SeqCst);
                let state = if n % (self.starting_polls + 1) < self.starting_polls {
                    "starting"
                } else {
                    "idle"
                };
                let (name, proxy) = self.created.lock().unwrap().clone();
                let name = if self.foreign.load(Ordering::SeqCst) {
                    "someone-elses-session".to_string()
                } else {
                    name
                };
                Resp::ok(&json!({"id": 3, "state": state, "name": name, "proxyUser": proxy}))
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

// ---- Audit 2026-11-21 ---------------------------------------------------------------------

/// Items 1 to 3: statements where Spark's lexer and parser read the text differently from the
/// check. Without the refusals each runs something other than what was checked.
#[tokio::test]
async fn lexer_tricks_and_substitution_are_refused_before_any_request() {
    let (server, _) = livy(Livy::new(|_| empty_ok())).await;
    let e = engine(&server.url);
    let ro = ExecOptions {
        read_only: true,
        ..opts()
    };
    for sql in [
        "-- x \\\nSELECT '\nINSERT INTO lake.shop.orders SELECT * FROM lake.shop.orders -- '",
        "SELECT 1 /* /* */ ' */ + reflect(\"a\",\"b\") + /* ' */ 2",
        "SELECT * FROM (MAP a USING \"cat /var/run/secrets/kubernetes.io/serviceaccount/token\" FROM (SELECT 1 a) t) x",
        "SELECT '${env:AWS_SECRET_ACCESS_KEY}', '${spark.hadoop.fs.s3a.secret.key}'",
        "SELECT 1 -- ${system:line.separator} , reflect('a', 'b')",
    ] {
        let events = stream(&e, sql, ro.clone()).await;
        assert_eq!(failure(&events).1, "policy_denied", "{sql}");
    }
    // `${` is refused in every mode, and so is a session change hidden behind a comment the
    // check cannot read.
    for sql in [
        "INSERT INTO t SELECT '${env:AWS_SECRET_ACCESS_KEY}'",
        "/* /* */ */ SET spark.sql.ansi.enabled=false",
        "-- x \\\nUSE lake.other",
    ] {
        let events = stream(&e, sql, opts()).await;
        let (_, code, msg) = failure(&events);
        assert_eq!(code, "policy_denied", "{sql}: {msg}");
    }
    assert_eq!(server.count(), 0);
}

/// Item 8: after a Livy restart the stored id can be someone else's session. Without the name
/// check our statements would run there and `close()` would delete it.
#[tokio::test]
async fn a_session_id_that_is_no_longer_ours_is_neither_used_nor_deleted() {
    let (server, mock) = livy(Livy::new(|_| table(&[("n", "integer")], json!([[1]])))).await;
    let e = engine(&server.url);
    e.execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap();
    mock.foreign.store(true, Ordering::SeqCst);
    e.execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap();
    assert_eq!(server.calls("POST", "/sessions").len(), 2);
    assert_eq!(e.session_id(), Some(4));
    // Statements went to the new session only, after the switch.
    let to_3 = server.calls("POST", "/sessions/3/statements").len();
    let to_4 = server.calls("POST", "/sessions/4/statements").len();
    assert_eq!((to_3, to_4), (2, 2), "USE + SELECT on each");

    mock.foreign.store(true, Ordering::SeqCst);
    e.close().await;
    assert_eq!(server.calls("DELETE", "/sessions/3").len(), 0);
    assert_eq!(server.calls("DELETE", "/sessions/4").len(), 0);
}

/// Item 6: a future dropped while the session starts (agent Stop) must not leak the session.
#[tokio::test]
async fn a_dropped_run_deletes_the_session_it_was_starting() {
    let mut l = Livy::new(|_| empty_ok());
    l.starting_polls = usize::MAX - 1;
    let (server, _) = livy(l).await;
    let e = engine(&server.url);
    let run = e.execute_collect("SELECT 1", ExecOptions::default());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), run)
            .await
            .is_err()
    );
    for _ in 0..200 {
        if !server.calls("DELETE", "/sessions/3").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(server.calls("DELETE", "/sessions/3").len(), 1);
    assert_eq!(e.session_id(), None);
}

/// Item 6: `close()` while a session is still starting: the new session is deleted, not kept.
#[tokio::test]
async fn closing_while_a_session_starts_deletes_it() {
    let mut l = Livy::new(|_| empty_ok());
    l.starting_polls = 6;
    let (server, _) = livy(l).await;
    let e = Arc::new(engine(&server.url));
    let task = {
        let e = e.clone();
        tokio::spawn(async move { e.execute_collect("SELECT 1", ExecOptions::default()).await })
    };
    for _ in 0..200 {
        if !server.calls("POST", "/sessions").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    e.close().await;
    let err = task.await.unwrap().unwrap_err();
    assert!(matches!(err, CoreError::NotConnected(_)), "{err:?}");
    assert_eq!(server.calls("DELETE", "/sessions/3").len(), 1);
}

/// Item 6: the bounded run cancels the Livy statement when its token fires, and waiting for
/// another run to finish can be cancelled as well.
#[tokio::test]
async fn collect_runs_honour_their_token() {
    let mut l = Livy::new(|_| empty_ok());
    l.running_polls = usize::MAX;
    let (server, _) = livy(l).await;
    let e = Arc::new(engine(&server.url));
    let first = tokio_util::sync::CancellationToken::new();
    let task = {
        let (e, first) = (e.clone(), first.clone());
        tokio::spawn(async move {
            e.execute_collect_cancellable("SELECT * FROM big", ExecOptions::default(), first)
                .await
        })
    };
    for _ in 0..200 {
        if !server.calls("POST", "/sessions/3/statements").is_empty()
            && server.calls("POST", "/sessions/3/statements").len() >= 2
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // A second run waits for the first; its own token gets it out.
    let second = tokio_util::sync::CancellationToken::new();
    let s = second.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        s.cancel();
    });
    let err = tokio::time::timeout(
        Duration::from_secs(2),
        e.execute_collect_cancellable("SELECT 2", ExecOptions::default(), second),
    )
    .await
    .expect("the wait was cancelled")
    .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");

    first.cancel();
    let err = task.await.unwrap().unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");
    assert_eq!(
        server
            .calls("POST", "/sessions/3/statements/1/cancel")
            .len(),
        1
    );
}

/// Item 9: column counts, row shapes and status answers are bounded.
#[tokio::test]
async fn columns_rows_and_status_answers_are_bounded() {
    let many: Vec<(String, String)> = (0..2001)
        .map(|i| (format!("c{i}"), "integer".to_string()))
        .collect();
    let fields: Vec<(&str, &str)> = many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let wide = table(&fields, json!([]));
    let (server, _) = livy(Livy::new(move |_| wide.clone())).await;
    let err = engine(&server.url)
        .execute_collect("SELECT * FROM t", ExecOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("columns"), "{err}");

    let (server, _) = livy(Livy::new(|_| {
        table(&[("a", "integer"), ("b", "integer")], json!([[1]]))
    }))
    .await;
    let err = engine(&server.url)
        .execute_collect("SELECT * FROM t", ExecOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("1 values for 2 columns"), "{err}");

    // A session status answer carries no rows: 8 MiB is plenty.
    let big = "x".repeat(9 * 1024 * 1024);
    let s = serve(Arc::new(move |req: &Req| match req.method.as_str() {
        "POST" => Resp::json(201, &json!({"id": 3})),
        "GET" => Resp::ok(&json!({"id": 3, "state": "starting", "pad": big})),
        _ => Resp::ok(&json!({})),
    }))
    .await;
    let err = engine(&s.url)
        .execute_collect("SELECT 1", ExecOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("larger than 8 MiB"), "{err}");
}

// ---- Re-audit 2026-11-21 ------------------------------------------------------------------

/// Item 1: one Livy session serves approved writes and unapproved reads. Without the refusal,
/// an approved temporary function bound to GenericUDFReflect would let every later read-only
/// `SELECT f('java.lang.Runtime', …)` through, and a temporary view would shadow a table.
#[tokio::test]
async fn session_scoped_state_is_refused_in_every_mode() {
    let (server, _) = livy(Livy::new(|_| empty_ok())).await;
    let e = engine(&server.url);
    for sql in [
        "CREATE TEMPORARY FUNCTION f AS 'org.apache.hadoop.hive.ql.udf.generic.GenericUDFReflect'",
        "CREATE OR REPLACE TEMPORARY VIEW orders AS SELECT 1 AS id",
        "CREATE GLOBAL TEMPORARY VIEW v AS SELECT 1",
        "DROP TEMPORARY FUNCTION f",
        "DECLARE VARIABLE v INT",
        "SET VAR v = 1",
        "EXECUTE IMMEDIATE 'USE other.ns'",
        "EXECUTE IMMEDIATE 'SET spark.sql.ansi.enabled=false'",
        "ADD JAR s3://b/x.jar",
        "CACHE TABLE t",
        "UNCACHE TABLE t",
        "CLEAR CACHE",
    ] {
        let events = stream(&e, sql, opts()).await;
        let (_, code, msg) = failure(&events);
        assert_eq!(code, "policy_denied", "{sql}: {msg}");
        assert!(msg.contains("shared session"), "{sql}: {msg}");
    }
    // Item 3: unquoted file relations in read-only runs.
    let ro = ExecOptions {
        read_only: true,
        ..opts()
    };
    for sql in [
        "SELECT * FROM text.passwd",
        "SELECT * FROM csv.__spark_conf__",
        "SELECT * FROM `text`.somefile",
    ] {
        let events = stream(&e, sql, ro.clone()).await;
        assert_eq!(failure(&events).1, "policy_denied", "{sql}");
    }
    assert_eq!(server.count(), 0);
}

/// Item 2: the `USE` built from the profile goes through the same `${` check as everything
/// else. Without it a hand-edited namespace `${env:AWS_SECRET_ACCESS_KEY}` is substituted by
/// Spark and comes back in the "could not switch" error.
#[tokio::test]
async fn the_use_statement_built_from_the_profile_is_checked_too() {
    let (server, _) = livy(Livy::new(|_| empty_ok())).await;
    let e = engine_with(SparkConfig {
        namespace: Some("${env:AWS_SECRET_ACCESS_KEY}".into()),
        ..config(&server.url)
    });
    let events = stream(&e, "SELECT 1", opts()).await;
    let (_, code, msg) = failure(&events);
    assert_eq!(code, "policy_denied", "{msg}");
    assert!(server.calls("POST", "/sessions/3/statements").is_empty());
    // The session it had started is not left behind.
    assert_eq!(server.calls("DELETE", "/sessions/3").len(), 1);
}

/// Item 8: Stop while `POST /sessions` is in flight. Without the submit task the session
/// created when the answer finally came would be orphaned.
#[tokio::test]
async fn a_session_whose_creation_was_cancelled_is_deleted_when_the_answer_arrives() {
    let s = serve(Arc::new(|req: &Req| {
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/sessions") => Resp::json(201, &json!({"id": 7, "state": "starting"}))
                .after(Duration::from_millis(300)),
            _ => Resp::ok(&json!({"msg": "ok"})),
        }
    }))
    .await;
    let e = engine(&s.url);
    let cancel = tokio_util::sync::CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        c.cancel();
    });
    let err = e
        .execute_collect_cancellable("SELECT 1", ExecOptions::default(), cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");
    for _ in 0..200 {
        if !s.calls("DELETE", "/sessions/7").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(s.calls("DELETE", "/sessions/7").len(), 1);
}

/// Item 8: Stop while `POST …/statements` is in flight: the statement created when the answer
/// arrives is cancelled.
#[tokio::test]
async fn a_statement_whose_submission_was_cancelled_is_cancelled_when_the_answer_arrives() {
    let name = Arc::new(Mutex::new(String::new()));
    let n2 = name.clone();
    let s = serve(Arc::new(move |req: &Req| {
        let code = req.json()["code"].as_str().unwrap_or("").to_string();
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/sessions") => {
                *n2.lock().unwrap() = req.json()["name"].as_str().unwrap_or("").to_string();
                Resp::json(201, &json!({"id": 7, "state": "starting"}))
            }
            ("GET", "/sessions/7") => Resp::ok(&json!({
                "id": 7, "state": "idle", "name": n2.lock().unwrap().clone(), "proxyUser": "ana"
            })),
            ("POST", "/sessions/7/statements") if code.starts_with("USE ") => {
                Resp::json(201, &json!({"id": 0, "state": "waiting"}))
            }
            ("GET", "/sessions/7/statements/0") => {
                Resp::ok(&json!({"id": 0, "state": "available", "output": empty_ok()}))
            }
            ("POST", "/sessions/7/statements") => {
                Resp::json(201, &json!({"id": 1, "state": "waiting"}))
                    .after(Duration::from_millis(300))
            }
            _ => Resp::ok(&json!({"msg": "ok"})),
        }
    }))
    .await;
    let e = engine(&s.url);
    let cancel = tokio_util::sync::CancellationToken::new();
    let c = cancel.clone();
    let probe = s.requests.clone();
    tokio::spawn(async move {
        // Cancel once the user's statement has been sent.
        for _ in 0..400 {
            let sent = probe
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r.method == "POST" && r.path == "/sessions/7/statements")
                .count();
            if sent >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        c.cancel();
    });
    let err = e
        .execute_collect_cancellable("SELECT * FROM big", ExecOptions::default(), cancel)
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Cancelled), "{err:?}");
    for _ in 0..200 {
        if !s
            .calls("POST", "/sessions/7/statements/1/cancel")
            .is_empty()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(s.calls("POST", "/sessions/7/statements/1/cancel").len(), 1);
}
