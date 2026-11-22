//! Integration tests for the MySQL engine against a real server. Skipped unless `VG_TEST_MYSQL`
//! is set, e.g.
//!
//! ```text
//! VG_TEST_MYSQL="host=127.0.0.1 port=53306 user=root password=vibelens dbname=shop" \
//!   cargo test -p vibelens-core --test mysql_engine
//! ```
//!
//! The user must be able to create databases and users (root of a throwaway container). The
//! verified-TLS tests also need `VG_TEST_MYSQL_CA`: a CA file that signed the server's
//! certificate for `127.0.0.1` (see `docs/TEST_PLAN.md`, MY-T1).

mod services;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vibelens_core::CoreError;
use vibelens_core::model::*;
use vibelens_core::mysql::{MysqlConnectParams, MysqlHandle};
use vibelens_core::ports::CredentialFn;

// ---- harness -------------------------------------------------------------------------------

#[derive(Clone)]
struct Dsn {
    host: String,
    port: u16,
    user: String,
    password: String,
    dbname: String,
}

fn dsn() -> Option<Dsn> {
    let raw = std::env::var("VG_TEST_MYSQL").ok()?;
    let mut d = Dsn {
        host: "127.0.0.1".into(),
        port: 3306,
        user: "root".into(),
        password: String::new(),
        dbname: "mysql".into(),
    };
    for kv in raw.split_whitespace() {
        let (k, v) = kv.split_once('=')?;
        match k {
            "host" => d.host = v.into(),
            "port" => d.port = v.parse().ok()?,
            "user" => d.user = v.into(),
            "password" => d.password = v.into(),
            "dbname" => d.dbname = v.into(),
            _ => {}
        }
    }
    Some(d)
}

macro_rules! require_mysql {
    () => {
        match dsn() {
            Some(d) => d,
            None => {
                crate::services::skip("mysql", "VG_TEST_MYSQL is not set");
                return;
            }
        }
    };
}

fn creds(user: &str, password: &str) -> CredentialFn {
    let (user, password) = (user.to_string(), password.to_string());
    Arc::new(move || {
        let c = ResolvedCredentials {
            username: Some(user.clone()),
            password: Some(SecretString::from(password.clone())),
            ..ResolvedCredentials::anonymous()
        };
        async move { Ok(c) }.boxed()
    })
}

fn params(d: &Dsn, database: &str, tls: TlsConfig) -> MysqlConnectParams {
    MysqlConnectParams {
        host: d.host.clone(),
        port: d.port,
        tls_server_name: d.host.clone(),
        database: database.to_string(),
        tls,
        options: MysqlOptions {
            max_pool_size: 2,
            ..Default::default()
        },
        cleartext_plugin: false,
    }
}

fn require_tls() -> TlsConfig {
    TlsConfig {
        mode: TlsMode::Require,
        ..Default::default()
    }
}

async fn handle_as(d: &Dsn, database: &str, user: &str, pw: &str) -> Arc<MysqlHandle> {
    MysqlHandle::connect(params(d, database, require_tls()), creds(user, pw))
        .await
        .expect("connect")
}

async fn handle(d: &Dsn) -> Arc<MysqlHandle> {
    handle_as(d, &d.dbname, &d.user, &d.password).await
}

fn opts() -> ExecOptions {
    ExecOptions::default()
}

fn ro() -> ExecOptions {
    ExecOptions {
        read_only: true,
        ..Default::default()
    }
}

async fn collect(h: &MysqlHandle, sql: &str, o: ExecOptions) -> vibelens_core::Result<QueryResult> {
    h.execute_collect(sql, o, CancellationToken::new()).await
}

async fn scalar(h: &MysqlHandle, sql: &str) -> Value {
    collect(h, sql, opts()).await.expect(sql).rows[0][0].clone()
}

/// An integer cell, whether it arrived as a number (INT) or a string (BIGINT, and MySQL's
/// comparison and COUNT results are BIGINT).
fn int(v: &Value) -> i64 {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or_else(|| panic!("not an integer: {v}"))
}

/// A database of its own for one test, dropped by [`Scratch::drop_now`].
struct Scratch {
    admin: Arc<MysqlHandle>,
    name: String,
}

impl Scratch {
    async fn new(d: &Dsn) -> (Scratch, Arc<MysqlHandle>) {
        let admin = handle(d).await;
        let name = format!("vg_my_{}", uuid::Uuid::new_v4().simple());
        collect(&admin, &format!("CREATE DATABASE `{name}`"), opts())
            .await
            .expect("create database");
        let h = handle_as(d, &name, &d.user, &d.password).await;
        (Scratch { admin, name }, h)
    }
    async fn drop_now(self) {
        let _ = collect(
            &self.admin,
            &format!("DROP DATABASE `{}`", self.name),
            opts(),
        )
        .await;
    }
}

/// Statements on other sessions whose text starts with `prefix` (the engine's own sessions,
/// stopped or not).
async fn running(admin: &MysqlHandle, marker: &str) -> i64 {
    let v = scalar(
        admin,
        &format!(
            "SELECT COUNT(*) FROM information_schema.PROCESSLIST \
             WHERE INFO LIKE '%{marker}%' AND INFO NOT LIKE '%PROCESSLIST%'"
        ),
    )
    .await;
    int(&v)
}

async fn wait_gone(admin: &MysqlHandle, marker: &str) -> bool {
    for _ in 0..50 {
        if running(admin, marker).await == 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

// ---- connecting, versions, catalog ---------------------------------------------------------

#[tokio::test]
async fn connects_reports_version_and_lists_the_catalog() {
    let d = require_mysql!();
    let h = handle(&d).await;
    let v = h.server_version().expect("version");
    assert!(v.starts_with('8') || v.starts_with('9'), "{v}");
    let (scratch, s) = Scratch::new(&d).await;
    collect(
        &s,
        "CREATE TABLE parent (id INT PRIMARY KEY, name VARCHAR(20) NOT NULL DEFAULT 'x' COMMENT 'the name');\
         CREATE TABLE child (id INT PRIMARY KEY, parent_id INT, note TEXT, \
           UNIQUE KEY uq_note (note(10)), CONSTRAINT fk_parent FOREIGN KEY (parent_id) REFERENCES parent (id));\
         CREATE VIEW v AS SELECT id FROM parent;\
         CREATE FUNCTION f() RETURNS INT DETERMINISTIC RETURN 1;\
         CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
        opts(),
    )
    .await
    .expect("schema");
    assert!(h.list_databases().await.unwrap().contains(&scratch.name));
    let objs = s.list_objects(&scratch.name).await.unwrap();
    let kinds: Vec<(String, ObjectKind)> = objs.iter().map(|o| (o.name.clone(), o.kind)).collect();
    for want in [
        ("child", ObjectKind::Table),
        ("parent", ObjectKind::Table),
        ("v", ObjectKind::View),
        ("f", ObjectKind::Function),
        ("p", ObjectKind::Procedure),
    ] {
        assert!(
            kinds.contains(&(want.0.into(), want.1)),
            "{want:?} in {kinds:?}"
        );
    }
    let t = s.describe_table(&scratch.name, "child").await.unwrap();
    assert_eq!(t.schema, scratch.name);
    assert!(t.columns.iter().any(|c| c.name == "id" && c.is_primary_key));
    assert_eq!(t.foreign_keys.len(), 1);
    assert_eq!(t.foreign_keys[0].referenced_table, "parent");
    assert!(
        t.indexes
            .iter()
            .any(|i| i.is_primary && i.definition.starts_with("PRIMARY KEY"))
    );
    assert!(t.indexes.iter().any(|i| i.name == "uq_note" && i.is_unique));
    let p = s.describe_table(&scratch.name, "parent").await.unwrap();
    let name = p.columns.iter().find(|c| c.name == "name").unwrap();
    assert_eq!(name.default.as_deref(), Some("x"));
    assert_eq!(name.comment.as_deref(), Some("the name"));
    assert!(matches!(
        s.describe_table(&scratch.name, "nope").await,
        Err(CoreError::NotFound(_))
    ));
    let meta = s.schema_metadata(None, 1).await.unwrap();
    assert_eq!(meta.tables.len(), 1);
    assert!(meta.truncated);
    // A hostile name is data, not SQL.
    assert!(s.list_objects("x' OR '1'='1").await.unwrap().is_empty());
    scratch.drop_now().await;
}

// ---- type mapping --------------------------------------------------------------------------

#[tokio::test]
async fn odd_values_map_without_loss_or_panic() {
    let d = require_mysql!();
    let (scratch, s) = Scratch::new(&d).await;
    collect(
        &s,
        "SET SESSION sql_mode = '';\
         CREATE TABLE t (u BIGINT UNSIGNED, i BIGINT, d DECIMAL(65,30), b BIT(10), j JSON, \
           z DATE, zt DATETIME, g GEOMETRY, bin VARBINARY(10), bl BLOB, y YEAR, f DOUBLE, e ENUM('a','b'), tx TEXT);\
         INSERT INTO t VALUES (18446744073709551615, -9223372036854775808, \
           12345678901234567890123456789012345.123456789012345678901234567890, b'1000000001', \
           '{\"a\": [1, 2.5, null], \"s\": \"ü\"}', '0000-00-00', '0000-00-00 00:00:00', \
           ST_GeomFromText('POINT(1 2)'), X'00FF', X'DEADBEEF', 2026, 1e308, 'b', 'naïve')",
        opts(),
    )
    .await
    .expect("setup");
    let r = collect(&s, "SELECT * FROM t", opts()).await.unwrap();
    let row = &r.rows[0];
    let col = |n: &str| r.columns.iter().position(|c| c.name == n).unwrap();
    assert_eq!(row[col("u")], json!("18446744073709551615"));
    assert_eq!(row[col("i")], json!("-9223372036854775808"));
    assert_eq!(
        row[col("d")],
        json!("12345678901234567890123456789012345.123456789012345678901234567890")
    );
    assert_eq!(row[col("b")], json!("1000000001"));
    assert_eq!(row[col("j")], json!({"a": [1, 2.5, null], "s": "ü"}));
    assert_eq!(row[col("z")], json!("0000-00-00"));
    assert_eq!(row[col("zt")], json!("0000-00-00 00:00:00"));
    assert!(row[col("g")]["$bytes"].is_string(), "{}", row[col("g")]);
    assert_eq!(row[col("bin")], json!({"$bytes": "AP8=", "len": 2}));
    assert_eq!(row[col("bl")], json!({"$bytes": "3q2+7w==", "len": 4}));
    assert_eq!(row[col("y")], json!(2026));
    assert_eq!(row[col("f")], json!(1e308));
    assert_eq!(row[col("e")], json!("b"));
    assert_eq!(row[col("tx")], json!("naïve"));
    assert_eq!(r.columns[col("u")].type_name, "bigint unsigned");
    assert_eq!(r.columns[col("bl")].type_name, "blob");
    assert_eq!(r.columns[col("j")].type_name, "json");

    // Text that is not UTF-8 (results not converted, a latin1 value): bytes, not mojibake.
    let r = collect(
        &s,
        "SET character_set_results = NULL; SELECT CONVERT(X'E9' USING latin1) AS l",
        opts(),
    )
    .await
    .unwrap();
    assert_eq!(r.rows[0][0], json!({"$bytes": "6Q==", "len": 1}));

    // A value over 1 MiB comes back as a marker, not in full.
    let r = collect(&s, "SELECT REPEAT('x', 1100000) AS big", opts())
        .await
        .unwrap();
    assert!(is_truncated_cell(&r.rows[0][0]));
    assert_eq!(r.rows[0][0]["$truncated"]["bytes"], json!(1_100_000));
    scratch.drop_now().await;
}

// ---- multiple statements -------------------------------------------------------------------

#[tokio::test]
async fn a_script_runs_statement_by_statement_on_one_session() {
    let d = require_mysql!();
    let h = handle(&d).await;
    let (tx, mut rx) = mpsc::channel(256);
    h.execute_stream(
        "q".into(),
        "SET @a = 41; SELECT @a + 1 AS v; -- trailing comment;\nSELECT ';' AS s",
        opts(),
        CancellationToken::new(),
        tx,
    )
    .await
    .unwrap();
    let mut events = Vec::new();
    while let Some(e) = rx.recv().await {
        events.push(e);
    }
    assert!(matches!(
        events[0],
        QueryEvent::Started {
            statement_count: 3,
            ..
        }
    ));
    assert!(matches!(events.last(), Some(QueryEvent::Finished { .. })));
    let rows: Vec<&Vec<Vec<Cell>>> = events
        .iter()
        .filter_map(|e| match e {
            QueryEvent::Rows { rows, .. } => Some(rows),
            _ => None,
        })
        .collect();
    assert_eq!(int(&rows[0][0][0]), 42);
    assert_eq!(rows[1][0][0], json!(";"));
}

#[tokio::test]
async fn delimiter_is_refused_before_anything_runs() {
    let d = require_mysql!();
    let h = handle(&d).await;
    let e = collect(&h, "DELIMITER //\nSELECT 1 //", opts())
        .await
        .unwrap_err();
    assert!(e.to_string().contains("mysql command-line client"), "{e}");
}

#[tokio::test]
async fn routine_bodies_with_semicolons_are_created_whole() {
    let d = require_mysql!();
    let (scratch, s) = Scratch::new(&d).await;
    collect(
        &s,
        "CREATE PROCEDURE twice(IN x INT)\nBEGIN\n  DECLARE y INT DEFAULT 0;\n  IF x > 0 THEN SET y = x * 2; END IF;\n  SELECT y AS doubled;\nEND;\nSELECT 1",
        opts(),
    )
    .await
    .expect("create procedure");
    let r = collect(&s, "CALL twice(21)", opts()).await.unwrap();
    assert_eq!(r.rows[0][0], json!(42));
    scratch.drop_now().await;
}

// ---- read-only runs ------------------------------------------------------------------------

#[tokio::test]
async fn read_only_runs_refuse_before_sending_and_inside_the_transaction() {
    let d = require_mysql!();
    let (scratch, s) = Scratch::new(&d).await;
    collect(
        &s,
        "CREATE TABLE t (a INT); INSERT INTO t VALUES (1)",
        opts(),
    )
    .await
    .unwrap();
    // Reads run, inside a read-only transaction.
    let r = collect(&s, "SELECT a FROM t", ro()).await.unwrap();
    assert_eq!(r.rows, vec![vec![json!(1)]]);
    let r = collect(
        &s,
        "SELECT @@transaction_read_only AS ro, @@in_transaction AS tx",
        ro(),
    )
    .await;
    // `@@in_transaction` is not a variable on every version; the status check is what counts.
    if let Ok(r) = r {
        assert_eq!(int(&r.rows[0][0]), 1);
    }
    let before = Instant::now();
    for sql in [
        "INSERT INTO t VALUES (2)",
        "SELECT a FROM t INTO OUTFILE '/tmp/vg_outfile'",
        "SELECT SLEEP(5)",
        "SELECT GET_LOCK('vg', 1)",
        "SELECT @x := 1",
        "SET GLOBAL max_connections = 5",
        "DO SLEEP(5)",
        "SELECT 1 /*!50000 , SLEEP(5) */",
        "SELECT a FROM t FOR UPDATE",
        "SELECT LOAD_FILE('/etc/hosts')",
        "SELECT 'a\\'; DELETE FROM t; -- '",
    ] {
        let e = collect(&s, sql, ro()).await.unwrap_err();
        assert!(
            matches!(&e, CoreError::PolicyDenied(m) if m.starts_with("not run in read-only mode")),
            "{sql}: {e:?}"
        );
    }
    assert!(before.elapsed() < Duration::from_secs(3), "nothing slept");
    assert_eq!(int(&scalar(&s, "SELECT COUNT(*) FROM t").await), 1);
    assert_eq!(
        int(&scalar(&s, "SELECT IS_FREE_LOCK('vg')").await),
        1,
        "GET_LOCK never ran"
    );
    scratch.drop_now().await;
}

// ---- server refusals that the client capabilities cause ------------------------------------
// (Two statements in one COM_QUERY: `mysql::exec` unit tests, which can bypass the splitter.)

#[tokio::test]
async fn load_data_local_is_refused_even_when_the_server_allows_it() {
    let d = require_mysql!();
    let (scratch, s) = Scratch::new(&d).await;
    collect(&s, "CREATE TABLE t (line TEXT)", opts())
        .await
        .unwrap();
    let path = std::env::temp_dir().join(format!("vg_infile_{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&path, "secret line\n").unwrap();
    // The server may have local_infile = ON; the client never advertised CLIENT_LOCAL_FILES.
    let e = collect(
        &s,
        &format!("LOAD DATA LOCAL INFILE '{}' INTO TABLE t", path.display()),
        opts(),
    )
    .await
    .unwrap_err();
    let m = e.to_string();
    assert!(
        m.contains("1148")
            || m.contains("3948")
            || m.contains("2068")
            || m.contains("LOCAL INFILE"),
        "{m}"
    );
    assert_eq!(int(&scalar(&s, "SELECT COUNT(*) FROM t").await), 0);
    let _ = std::fs::remove_file(&path);
    scratch.drop_now().await;
}

// ---- cancel, timeout, dropped runs ---------------------------------------------------------

#[tokio::test]
async fn cancel_stops_the_statement_on_the_server() {
    let d = require_mysql!();
    let admin = handle(&d).await;
    let h = handle(&d).await;
    let marker = format!("vg_cancel_{}", uuid::Uuid::new_v4().simple());
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let sql = format!("SELECT SLEEP(60) AS `{marker}`");
    let run = tokio::spawn({
        let h = h.clone();
        async move { h.execute_collect(&sql, opts(), c2).await }
    });
    for _ in 0..50 {
        if running(&admin, &marker).await > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(running(&admin, &marker).await, 1, "precondition: it runs");
    let t0 = Instant::now();
    cancel.cancel();
    let r = run.await.unwrap();
    assert!(matches!(r, Err(CoreError::Cancelled)), "{r:?}");
    assert!(t0.elapsed() < Duration::from_secs(10));
    assert!(
        wait_gone(&admin, &marker).await,
        "still running on the server"
    );
}

#[tokio::test]
async fn a_timeout_stops_any_statement_on_the_server() {
    let d = require_mysql!();
    let admin = handle(&d).await;
    let h = handle(&d).await;
    for (verb, sql) in [
        ("select", "SELECT SLEEP(60) AS `{m}`"),
        // `max_execution_time` does not cover DO; the client-side limit and KILL QUERY do.
        ("do", "DO SLEEP(60) /* {m} */"),
    ] {
        let marker = format!("vg_timeout_{verb}_{}", uuid::Uuid::new_v4().simple());
        let sql = sql.replace("{m}", &marker);
        let t0 = Instant::now();
        let r = collect(
            &h,
            &sql,
            ExecOptions {
                timeout_ms: Some(700),
                ..Default::default()
            },
        )
        .await;
        assert!(t0.elapsed() < Duration::from_secs(15), "{verb}");
        match r {
            Err(CoreError::Timeout(_)) => {}
            // MySQL's max_execution_time answers a SELECT with a row holding 1 instead of an
            // error on some versions; either way it is stopped.
            Ok(q) if verb == "select" => assert_eq!(int(&q.rows[0][0]), 1),
            other => panic!("{verb}: {other:?}"),
        }
        assert!(wait_gone(&admin, &marker).await, "{verb}: still running");
    }
}

#[tokio::test]
async fn a_dropped_run_kills_its_statement() {
    let d = require_mysql!();
    let admin = handle(&d).await;
    let h = handle(&d).await;
    let marker = format!("vg_drop_{}", uuid::Uuid::new_v4().simple());
    let sql = format!("SELECT SLEEP(60) AS `{marker}`");
    let run = tokio::spawn({
        let h = h.clone();
        async move {
            h.execute_collect(&sql, opts(), CancellationToken::new())
                .await
        }
    });
    for _ in 0..50 {
        if running(&admin, &marker).await > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(running(&admin, &marker).await, 1);
    // An agent tool timeout or an aborted task drops the future mid-statement.
    run.abort();
    assert!(
        wait_gone(&admin, &marker).await,
        "still running after the run was dropped"
    );
}

// ---- bounds --------------------------------------------------------------------------------

#[tokio::test]
async fn rows_are_capped_while_streaming() {
    let d = require_mysql!();
    let h = handle(&d).await;
    let big = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1000) \
               SELECT i FROM n";
    let capped = ExecOptions {
        max_rows: 7,
        page_size: 3,
        ..Default::default()
    };
    let t0 = Instant::now();
    let r = collect(&h, big, capped.clone()).await.unwrap();
    assert_eq!(r.rows.len(), 7);
    assert!(r.truncated);
    assert!(t0.elapsed() < Duration::from_secs(20));
    // Read-only: the cut statement is the last one run.
    let e = collect(
        &h,
        &format!("{big}; SELECT 1"),
        ExecOptions {
            read_only: true,
            ..capped
        },
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("statement 2 was not run"), "{e}");
    // Pages are cut by size as well as by count.
    let (tx, mut rx) = mpsc::channel(1024);
    h.execute_stream(
        "q".into(),
        "SELECT REPEAT('x', 900000) FROM (SELECT 1 UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6) a",
        ExecOptions {
            page_size: 500,
            ..Default::default()
        },
        CancellationToken::new(),
        tx,
    )
    .await
    .unwrap();
    let mut pages = 0;
    while let Some(e) = rx.recv().await {
        if let QueryEvent::Rows { rows, .. } = e {
            pages += 1;
            assert!(rows.len() < 6);
        }
    }
    assert!(pages >= 2, "{pages}");
}

// ---- pool hygiene --------------------------------------------------------------------------

#[tokio::test]
async fn a_returned_connection_is_reset() {
    let d = require_mysql!();
    let (scratch, _) = Scratch::new(&d).await;
    let mut p = params(&d, &d.dbname, require_tls());
    p.options.max_pool_size = 1;
    let h = MysqlHandle::connect(p, creds(&d.user, &d.password))
        .await
        .unwrap();
    let id = scalar(&h, "SELECT CONNECTION_ID()").await;
    collect(
        &h,
        &format!(
            "SET @leak = 'x'; SET SESSION sql_mode = 'ANSI_QUOTES'; USE `{}`; \
             CREATE TEMPORARY TABLE tmp_leak (a INT); SELECT GET_LOCK('vg_leak', 1)",
            scratch.name
        ),
        opts(),
    )
    .await
    .unwrap();
    let r = collect(
        &h,
        "SELECT CONNECTION_ID(), @leak, DATABASE(), @@session.sql_mode LIKE '%ANSI_QUOTES%', IS_FREE_LOCK('vg_leak')",
        opts(),
    )
    .await
    .unwrap();
    assert_eq!(r.rows[0][0], id, "the same session was reused");
    assert_eq!(r.rows[0][1], Value::Null, "user variable reset");
    assert_eq!(r.rows[0][2], json!(d.dbname), "database restored");
    assert_eq!(int(&r.rows[0][3]), 0, "sql_mode reset");
    assert_eq!(int(&r.rows[0][4]), 1, "lock released");
    assert!(collect(&h, "SELECT * FROM tmp_leak", opts()).await.is_err());
    // A script that leaves a transaction open is rolled back and says so.
    collect(
        &h,
        &format!("CREATE TABLE `{}`.t (a INT)", scratch.name),
        opts(),
    )
    .await
    .unwrap();
    let e = collect(
        &h,
        &format!(
            "START TRANSACTION; INSERT INTO `{}`.t VALUES (1)",
            scratch.name
        ),
        opts(),
    )
    .await
    .unwrap_err();
    assert!(e.to_string().contains("rolled back"), "{e}");
    assert_eq!(
        int(&scalar(&h, &format!("SELECT COUNT(*) FROM `{}`.t", scratch.name)).await),
        0
    );
    scratch.drop_now().await;
}

// ---- authentication and TLS ----------------------------------------------------------------

/// `caching_sha2_password` full authentication sends the password. Without TLS the patched
/// driver refuses instead of asking the server for an RSA key (which an attacker could supply).
#[tokio::test]
async fn full_authentication_never_happens_without_tls() {
    let d = require_mysql!();
    let admin = handle(&d).await;
    let user = format!("vg_{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    collect(
        &admin,
        &format!(
            "CREATE USER '{user}'@'%' IDENTIFIED WITH caching_sha2_password BY 'pw-{user}'; \
             GRANT SELECT ON `{}`.* TO '{user}'@'%'; FLUSH PRIVILEGES",
            d.dbname
        ),
        opts(),
    )
    .await
    .unwrap();
    let plain = TlsConfig {
        mode: TlsMode::Disable,
        ..Default::default()
    };
    let e = MysqlHandle::connect(
        params(&d, &d.dbname, plain.clone()),
        creds(&user, &format!("pw-{user}")),
    )
    .await
    .err()
    .expect("refused");
    assert!(
        matches!(&e, CoreError::Auth(m) if m.contains("full authentication")),
        "{e:?}"
    );
    // Over TLS it works, and afterwards the server's cache lets a plain login use the
    // challenge-response (no password on the wire).
    let h = MysqlHandle::connect(
        params(&d, &d.dbname, require_tls()),
        creds(&user, &format!("pw-{user}")),
    )
    .await
    .expect("TLS login");
    assert_eq!(
        int(&scalar(&h, "SELECT CURRENT_USER() LIKE 'vg_%'").await),
        1
    );
    MysqlHandle::connect(
        params(&d, &d.dbname, plain),
        creds(&user, &format!("pw-{user}")),
    )
    .await
    .expect("fast authentication without TLS");
    // A wrong password is an authentication error that does not echo it.
    let e = MysqlHandle::connect(
        params(&d, &d.dbname, require_tls()),
        creds(&user, "wrong-secret"),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(&e, CoreError::Auth(_)), "{e:?}");
    assert!(!e.to_string().contains("wrong-secret"));
    let _ = collect(&admin, &format!("DROP USER '{user}'@'%'"), opts()).await;
}

#[tokio::test]
async fn verify_full_checks_the_chain_and_the_host_name() {
    let d = require_mysql!();
    let Ok(ca) = std::env::var("VG_TEST_MYSQL_CA") else {
        crate::services::skip("mysql-tls", "VG_TEST_MYSQL_CA is not set");
        return;
    };
    let verify = |ca: Option<String>| TlsConfig {
        mode: TlsMode::VerifyFull,
        ca_cert_path: ca,
        use_rds_bundle: false,
    };
    // Signed by the CA, valid for 127.0.0.1: connects.
    let h = MysqlHandle::connect(
        params(&d, &d.dbname, verify(Some(ca.clone()))),
        creds(&d.user, &d.password),
    )
    .await
    .expect("verified connection");
    assert_eq!(
        int(&scalar(&h, "SELECT VARIABLE_VALUE <> '' FROM performance_schema.session_status WHERE VARIABLE_NAME = 'Ssl_cipher'").await),
        1
    );
    // Without the CA (OS roots only): refused before authentication.
    let e = MysqlHandle::connect(
        params(&d, &d.dbname, verify(None)),
        creds(&d.user, &d.password),
    )
    .await
    .err()
    .expect("unknown issuer");
    assert!(e.to_string().to_lowercase().contains("certificate"), "{e}");
    // Right CA, wrong name (what a tunnel to the wrong host looks like): refused.
    let mut p = params(&d, &d.dbname, verify(Some(ca)));
    p.tls_server_name = "db.elsewhere.example".into();
    let e = MysqlHandle::connect(p, creds(&d.user, &d.password))
        .await
        .err()
        .expect("name mismatch");
    assert!(e.to_string().to_lowercase().contains("certificate"), "{e}");
}

// ---- through the connection manager and the agent's tool backend ---------------------------

mod manager {
    use super::*;
    use async_trait::async_trait;
    use vibelens_core::connections::{AppToolBackend, ConnectionManager};
    use vibelens_core::ports::{
        CredentialSource, ProfileStore, ToolBackend, TunnelHandle, TunnelProvider,
    };
    use vibelens_core::profiles::JsonProfileStore;
    use vibelens_core::workspace::WorkspaceFs;

    struct StaticCreds(String, String);

    #[async_trait]
    impl CredentialSource for StaticCreds {
        async fn resolve(
            &self,
            _p: &ConnectionProfile,
        ) -> vibelens_core::Result<ResolvedCredentials> {
            Ok(ResolvedCredentials {
                username: Some(self.0.clone()),
                password: Some(SecretString::from(self.1.clone())),
                ..ResolvedCredentials::anonymous()
            })
        }
    }

    struct NoTunnels;

    #[async_trait]
    impl TunnelProvider for NoTunnels {
        async fn open(
            &self,
            _: &str,
            _: &TunnelConfig,
            _: &str,
            _: u16,
        ) -> vibelens_core::Result<TunnelHandle> {
            Err(CoreError::Tunnel("no tunnels in this test".into()))
        }
    }

    #[tokio::test]
    async fn the_agent_backend_runs_pinned_reads_and_stops_on_cancel() {
        let d = require_mysql!();
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JsonProfileStore::new(dir.path().to_path_buf()).unwrap());
        let mgr = ConnectionManager::new(
            store.clone(),
            Arc::new(StaticCreds(d.user.clone(), d.password.clone())),
            Arc::new(NoTunnels),
        );
        let ws = Arc::new(WorkspaceFs::new(dir.path().join("ws")).unwrap());
        let backend = AppToolBackend::new(mgr.clone(), ws);
        let p = store
            .upsert(ConnectionProfile {
                id: String::new(),
                name: "live-mysql".into(),
                engine: EngineKind::Mysql,
                host: d.host.clone(),
                port: d.port,
                database: Some(d.dbname.clone()),
                auth: AuthMethod::Password {
                    username: d.user.clone(),
                },
                tls: require_tls(),
                tunnel: None,
                environment: Environment::Dev,
                ai_data_access: AiDataAccess::Results,
                color: None,
                pg: Default::default(),
                valkey: Default::default(),
                created_at: None,
                updated_at: None,
                iceberg: None,
                redshift: None,
                mysql: Default::default(),
            })
            .unwrap();
        let st = mgr.connect(&p.id).await.unwrap();
        assert_eq!(st.engine, EngineKind::Mysql);
        assert_eq!(st.state, ConnectionState::Connected);
        assert!(st.server_version.unwrap().starts_with("MySQL "));

        let r = backend
            .execute_sql(
                &p.id,
                None,
                "SELECT DATABASE()",
                true,
                50,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(r.rows[0][0], json!(d.dbname), "the profile's database");
        let r = backend
            .execute_sql(
                &p.id,
                Some("mysql"),
                "SELECT DATABASE()",
                true,
                50,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(
            r.rows[0][0],
            json!("mysql"),
            "the requested database, own pool"
        );
        let e = backend
            .execute_sql(
                &p.id,
                None,
                "CREATE TABLE vg_nope (a INT)",
                true,
                50,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(e, CoreError::PolicyDenied(_)), "{e:?}");
        assert!(
            backend
                .list_databases(&p.id)
                .await
                .unwrap()
                .contains(&d.dbname)
        );
        let meta = backend
            .schema_metadata(&p.id, None, None, 50)
            .await
            .unwrap();
        assert_eq!(meta.database, d.dbname);

        // Stop: the agent's token fires while a statement runs on the server.
        let marker = format!("vg_agent_stop_{}", uuid::Uuid::new_v4().simple());
        let cancel = CancellationToken::new();
        // A read the check accepts (SLEEP would not pass), slow enough to still be running.
        let sql = format!(
            "SELECT COUNT(*) AS `{marker}` FROM information_schema.COLUMNS a, \
             information_schema.COLUMNS b, information_schema.COLUMNS c"
        );
        let run = tokio::spawn({
            let (backend, id, cancel) = (backend.clone(), p.id.clone(), cancel.clone());
            async move { backend.execute_sql(&id, None, &sql, true, 50, cancel).await }
        });
        let admin = handle(&d).await;
        for _ in 0..50 {
            if running(&admin, &marker).await > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(running(&admin, &marker).await, 1, "precondition: it runs");
        cancel.cancel();
        let r = run.await.unwrap();
        assert!(matches!(r, Err(CoreError::Cancelled)), "{r:?}");
        assert!(
            wait_gone(&admin, &marker).await,
            "the agent's statement kept running"
        );

        mgr.disconnect(&p.id).await.unwrap();
        assert!(mgr.status().iter().all(|s| s.profile_id != p.id));
    }

    /// A profile that would send a password to another machine without verified TLS never
    /// reaches the network.
    #[tokio::test]
    async fn an_unsafe_profile_is_refused_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JsonProfileStore::new(dir.path().to_path_buf()).unwrap());
        let mgr = ConnectionManager::new(
            store.clone(),
            Arc::new(StaticCreds("u".into(), "p".into())),
            Arc::new(NoTunnels),
        );
        let p = ConnectionProfile {
            id: "remote".into(),
            name: "remote".into(),
            engine: EngineKind::Mysql,
            // TEST-NET-1: nothing answers there, and nothing may be tried.
            host: "192.0.2.10".into(),
            port: 3306,
            database: Some("shop".into()),
            auth: AuthMethod::Password {
                username: "u".into(),
            },
            tls: require_tls(),
            tunnel: None,
            environment: Environment::Dev,
            ai_data_access: AiDataAccess::Results,
            color: None,
            pg: Default::default(),
            valkey: Default::default(),
            created_at: None,
            updated_at: None,
            iceberg: None,
            redshift: None,
            mysql: Default::default(),
        };
        // Refused on save …
        let e = store.upsert(p.clone()).unwrap_err();
        assert!(
            matches!(&e, CoreError::Invalid(m) if m.contains("Verify full")),
            "{e:?}"
        );
        // … and on use: a test call (direct IPC) with a typed password never dials.
        let t0 = Instant::now();
        let e = mgr
            .test(&p, Some(SecretString::from("typed")))
            .await
            .unwrap_err();
        assert!(
            matches!(&e, CoreError::Invalid(m) if m.contains("Verify full")),
            "{e:?}"
        );
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "it tried the network"
        );
    }
}
