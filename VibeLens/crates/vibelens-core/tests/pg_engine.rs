//! Integration tests for the Postgres engine. Skipped unless `VG_TEST_PG` is set, e.g.
//!
//! ```text
//! VG_TEST_PG="host=127.0.0.1 port=5432 user=postgres password=vibelens dbname=vibelens_test" \
//!   cargo test -p vibelens-core --test pg_engine
//! ```

mod services;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vibelens_core::model::*;
use vibelens_core::pg::{PgConnectParams, PgHandle};
use vibelens_core::ports::CredentialFn;
use vibelens_core::{CoreError, ErrorPayload};

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
    let raw = std::env::var("VG_TEST_PG").ok()?;
    let mut d = Dsn {
        host: "127.0.0.1".into(),
        port: 5432,
        user: "postgres".into(),
        password: String::new(),
        dbname: "postgres".into(),
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

macro_rules! require_pg {
    () => {
        match dsn() {
            Some(d) => d,
            None => {
                crate::services::skip("pg", "VG_TEST_PG is not set");
                return;
            }
        }
    };
}

fn creds_for(d: &Dsn, password: &str, counter: Arc<AtomicUsize>) -> CredentialFn {
    let user = d.user.clone();
    let password = password.to_string();
    Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        let c = ResolvedCredentials {
            username: Some(user.clone()),
            password: Some(SecretString::from(password.clone())),
            ..ResolvedCredentials::anonymous()
        };
        async move { Ok(c) }.boxed()
    })
}

fn params(d: &Dsn, mode: TlsMode) -> PgConnectParams {
    PgConnectParams {
        host: d.host.clone(),
        port: d.port,
        tls_server_name: d.host.clone(),
        database: d.dbname.clone(),
        tls: TlsConfig {
            mode,
            ..Default::default()
        },
        options: PgOptions {
            max_pool_size: 4,
            ..Default::default()
        },
    }
}

async fn connect_with(d: &Dsn, p: PgConnectParams) -> Arc<PgHandle> {
    PgHandle::connect(p, creds_for(d, &d.password, Arc::new(AtomicUsize::new(0))))
        .await
        .expect("connect")
}

async fn connect(d: &Dsn) -> Arc<PgHandle> {
    connect_with(d, params(d, TlsMode::Require)).await
}

/// Single-connection pool: proves that clean-up after failures leaves the connection reusable.
async fn connect_single(d: &Dsn) -> Arc<PgHandle> {
    let mut p = params(d, TlsMode::Require);
    p.options.max_pool_size = 1;
    connect_with(d, p).await
}

fn is_terminal(e: &QueryEvent) -> bool {
    matches!(e, QueryEvent::Finished { .. } | QueryEvent::Failed { .. })
}

/// Runs a script and returns every event, asserting the protocol invariants
/// (`Started` first, exactly one terminal event, and it is last).
async fn run(h: &PgHandle, sql: &str, opts: ExecOptions) -> Vec<QueryEvent> {
    let (tx, mut rx) = mpsc::channel(8);
    let exec = h.execute_stream("q".into(), sql, opts, CancellationToken::new(), tx);
    let collect = async {
        let mut v = Vec::new();
        while let Some(e) = rx.recv().await {
            v.push(e);
        }
        v
    };
    let (r, evs) = tokio::join!(exec, collect);
    r.expect("execute_stream returned Err");
    check_invariants(&evs);
    evs
}

fn check_invariants(evs: &[QueryEvent]) {
    assert!(
        matches!(evs.first(), Some(QueryEvent::Started { .. })),
        "first event must be Started: {evs:?}"
    );
    assert_eq!(
        evs.iter().filter(|e| is_terminal(e)).count(),
        1,
        "exactly one terminal event: {evs:?}"
    );
    assert!(
        is_terminal(evs.last().expect("events")),
        "terminal event must be last: {evs:?}"
    );
}

fn failed(evs: &[QueryEvent]) -> (Option<usize>, ErrorPayload) {
    match evs.last() {
        Some(QueryEvent::Failed {
            statement_index,
            error,
        }) => (*statement_index, error.clone()),
        other => panic!("expected Failed, got {other:?}\nall: {evs:?}"),
    }
}

fn assert_finished(evs: &[QueryEvent]) {
    assert!(
        matches!(evs.last(), Some(QueryEvent::Finished { .. })),
        "expected Finished: {evs:?}"
    );
}

fn row_pages(evs: &[QueryEvent]) -> Vec<usize> {
    evs.iter()
        .filter_map(|e| {
            if let QueryEvent::Rows { rows, .. } = e {
                Some(rows.len())
            } else {
                None
            }
        })
        .collect()
}

fn dones(evs: &[QueryEvent]) -> Vec<&QueryEvent> {
    evs.iter()
        .filter(|e| matches!(e, QueryEvent::StatementDone { .. }))
        .collect()
}

async fn scalar(h: &PgHandle, expr: &str) -> Value {
    let r = h
        .execute_collect(&format!("SELECT {expr}"), ExecOptions::default())
        .await
        .unwrap_or_else(|e| panic!("SELECT {expr}: {e}"));
    assert_eq!(r.rows.len(), 1, "SELECT {expr}");
    r.rows[0][0].clone()
}

/// Raw driver connection for fixtures, independent of the code under test.
async fn raw(d: &Dsn) -> tokio_postgres::Client {
    let mut cfg = tokio_postgres::Config::new();
    cfg.host(&d.host)
        .port(d.port)
        .user(&d.user)
        .password(&d.password)
        .dbname(&d.dbname);
    let (client, conn) = cfg
        .connect(tokio_postgres::NoTls)
        .await
        .expect("raw connect");
    tokio::spawn(async move {
        let _ = conn.await;
    });
    client
}

/// A uniquely named schema, dropped (with CASCADE) when the guard goes out of scope.
struct Fixture {
    dsn: Dsn,
    schema: String,
}

impl Fixture {
    async fn new(d: &Dsn, setup: &str) -> Fixture {
        let schema = format!("vg_t_{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
        let c = raw(d).await;
        c.batch_execute(&format!("CREATE SCHEMA {schema}"))
            .await
            .expect("create schema");
        c.batch_execute(&setup.replace("$S", &schema))
            .await
            .unwrap_or_else(|e| panic!("fixture setup failed: {e:?}"));
        Fixture {
            dsn: d.clone(),
            schema,
        }
    }
    fn q(&self, sql: &str) -> String {
        sql.replace("$S", &self.schema)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let d = self.dsn.clone();
        let schema = self.schema.clone();
        let _ = std::thread::spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                rt.block_on(async {
                    let c = raw(&d).await;
                    let _ = c
                        .batch_execute(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
                        .await;
                });
            }
        })
        .join();
    }
}

// ---- connection ----------------------------------------------------------------------------

#[tokio::test]
async fn connects_with_tls_disabled_and_required() {
    let d = require_pg!();
    for mode in [TlsMode::Disable, TlsMode::Require] {
        let h = connect_with(&d, params(&d, mode)).await;
        assert_eq!(h.database(), d.dbname);
        let v = h.server_version().await.expect("server_version");
        assert!(v.starts_with(|c: char| c.is_ascii_digit()), "{v}");
        let ssl = scalar(&h, "ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()").await;
        assert_eq!(
            ssl,
            json!(mode == TlsMode::Require),
            "TLS state for {mode:?}"
        );
        h.close().await;
    }
}

#[tokio::test]
async fn tls_server_name_is_independent_of_dialed_address() {
    let d = require_pg!();
    // Tunnel shape: dial the local address, but the "real" server name differs.
    let mut p = params(&d, TlsMode::Require);
    p.tls_server_name = "mydb.cluster-abc.eu-west-1.rds.amazonaws.com".into();
    let h = connect_with(&d, p).await;
    assert_eq!(scalar(&h, "1::int4").await, json!(1));
}

#[tokio::test]
async fn verify_full_rejects_the_self_signed_server_cert() {
    let d = require_pg!();
    let res = PgHandle::connect(
        params(&d, TlsMode::VerifyFull),
        creds_for(&d, &d.password, Arc::new(AtomicUsize::new(0))),
    )
    .await;
    assert!(res.is_err(), "self-signed certificate must not verify");
}

#[tokio::test]
async fn wrong_password_is_an_auth_error_and_does_not_leak_it() {
    let d = require_pg!();
    let bad = "definitely-not-the-password-9f3a";
    let err = match PgHandle::connect(
        params(&d, TlsMode::Require),
        creds_for(&d, bad, Arc::new(AtomicUsize::new(0))),
    )
    .await
    {
        Ok(_) => panic!("must not connect with a wrong password"),
        Err(e) => e,
    };
    // Depending on pg_hba the server may use trust auth for this host; only assert when it rejects.
    assert!(matches!(err, CoreError::Auth(_)), "{err:?}");
    assert!(!err.to_string().contains(bad));
}

#[tokio::test]
async fn credentials_are_minted_per_physical_connection() {
    let d = require_pg!();
    let counter = Arc::new(AtomicUsize::new(0));
    let mut p = params(&d, TlsMode::Require);
    p.options.max_pool_size = 3;
    let h = PgHandle::connect(p, creds_for(&d, &d.password, counter.clone()))
        .await
        .expect("connect");
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    // Three concurrent sleeps need three physical connections.
    let sleeps = (0..3).map(|_| h.execute_collect("SELECT pg_sleep(0.4)", ExecOptions::default()));
    for r in futures_util::future::join_all(sleeps).await {
        r.expect("sleep");
    }
    assert_eq!(counter.load(Ordering::SeqCst), 3);
    // Reusing pooled connections does not call the credential function again.
    scalar(&h, "1").await;
    assert_eq!(counter.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn idle_secs_and_close() {
    let d = require_pg!();
    let h = connect(&d).await;
    assert_eq!(h.idle_secs(), 0);
    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert!(h.idle_secs() >= 2, "idle_secs = {}", h.idle_secs());
    scalar(&h, "1").await;
    assert_eq!(h.idle_secs(), 0, "use resets the idle clock");
    h.close().await;
    let err = h
        .execute_collect("SELECT 1", ExecOptions::default())
        .await
        .expect_err("closed");
    assert!(matches!(err, CoreError::NotConnected(_)), "{err:?}");
}

// ---- catalog -------------------------------------------------------------------------------

const CATALOG_SETUP: &str = r#"
CREATE TABLE $S.parent (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    note text DEFAULT 'none',
    created timestamptz NOT NULL DEFAULT now(),
    total numeric(12,2)
);
COMMENT ON TABLE $S.parent IS 'the parents';
COMMENT ON COLUMN $S.parent.name IS 'display name';
CREATE TABLE $S.parent2 (x int, y int, PRIMARY KEY (x, y));
CREATE TABLE $S.child (
    a int,
    b int,
    parent_id bigint REFERENCES $S.parent (id),
    cy int,
    cx int,
    PRIMARY KEY (a, b),
    CONSTRAINT child_composite_fk FOREIGN KEY (cy, cx) REFERENCES $S.parent2 (y, x)
);
CREATE INDEX child_parent_idx ON $S.child (parent_id);
CREATE UNIQUE INDEX child_b_uq ON $S.child (b, a);
CREATE VIEW $S.v AS SELECT id, name FROM $S.parent;
COMMENT ON VIEW $S.v IS 'a view';
CREATE MATERIALIZED VIEW $S.mv AS SELECT 1 AS one;
CREATE SEQUENCE $S.seq;
CREATE FUNCTION $S.add(a int, b text) RETURNS int LANGUAGE sql AS 'SELECT a';
COMMENT ON FUNCTION $S.add(int, text) IS 'adds';
CREATE PROCEDURE $S.proc(x int) LANGUAGE sql AS 'SELECT 1';
INSERT INTO $S.parent (name) SELECT 'p' || g FROM generate_series(1, 50) g;
ANALYZE $S.parent;
"#;

#[tokio::test]
async fn catalog_databases_and_schemas() {
    let d = require_pg!();
    let fx = Fixture::new(&d, CATALOG_SETUP).await;
    let h = connect(&d).await;

    let dbs = h.list_databases().await.expect("list_databases");
    let me = dbs
        .iter()
        .find(|x| x.name == d.dbname)
        .expect("current database listed");
    assert!(!me.is_template && me.allow_connections);
    assert!(me.size_bytes.is_some_and(|s| s > 0));
    let t0 = dbs
        .iter()
        .find(|x| x.name == "template0")
        .expect("template0");
    assert!(t0.is_template && !t0.allow_connections);

    let schemas = h.list_schemas().await.expect("list_schemas");
    let find = |n: &str| schemas.iter().find(|s| s.name == n);
    assert!(find("pg_catalog").expect("pg_catalog").is_system);
    assert!(
        find("information_schema")
            .expect("information_schema")
            .is_system
    );
    assert!(
        schemas
            .iter()
            .filter(|s| s.name.starts_with("pg_toast"))
            .all(|s| s.is_system)
    );
    let mine = find(&fx.schema).expect("fixture schema");
    assert!(!mine.is_system);
    assert!(mine.owner.is_some());
}

#[tokio::test]
async fn catalog_list_objects() {
    let d = require_pg!();
    let fx = Fixture::new(&d, CATALOG_SETUP).await;
    let h = connect(&d).await;
    let objs = h.list_objects(&fx.schema).await.expect("list_objects");
    let get = |n: &str| {
        objs.iter()
            .find(|o| o.name == n)
            .unwrap_or_else(|| panic!("{n} missing from {objs:?}"))
    };

    assert_eq!(get("parent").kind, ObjectKind::Table);
    assert_eq!(get("parent").comment.as_deref(), Some("the parents"));
    assert_eq!(
        get("parent").estimated_rows,
        Some(50),
        "analysed table has an estimate"
    );
    assert_eq!(
        get("child").estimated_rows,
        None,
        "never analysed -> None, not -1"
    );
    assert_eq!(get("v").kind, ObjectKind::View);
    assert_eq!(get("v").comment.as_deref(), Some("a view"));
    assert_eq!(get("v").estimated_rows, None);
    assert_eq!(get("mv").kind, ObjectKind::MaterializedView);
    assert_eq!(get("seq").kind, ObjectKind::Sequence);
    let f = get("add");
    assert_eq!(f.kind, ObjectKind::Function);
    assert_eq!(f.signature.as_deref(), Some("(a integer, b text)"));
    assert_eq!(f.comment.as_deref(), Some("adds"));
    let p = get("proc");
    assert_eq!(p.kind, ObjectKind::Procedure);
    assert!(
        matches!(
            p.signature.as_deref(),
            Some("(x integer)" | "(IN x integer)")
        ),
        "{:?}",
        p.signature
    );
    // Indexes are not objects of their own.
    assert!(objs.iter().all(|o| o.name != "child_parent_idx"));
    assert!(objs.iter().all(|o| o.schema == fx.schema));
}

const PARTITION_SETUP: &str = r#"
CREATE TABLE $S.events (id bigint, created date NOT NULL, region text) PARTITION BY RANGE (created);
CREATE TABLE $S.events_2024_10 PARTITION OF $S.events FOR VALUES FROM ('2024-10-01') TO ('2024-11-01');
CREATE TABLE $S.events_2024_02 PARTITION OF $S.events FOR VALUES FROM ('2024-02-01') TO ('2024-03-01');
CREATE TABLE $S.events_default PARTITION OF $S.events DEFAULT;
CREATE TABLE $S.events_2025 PARTITION OF $S.events FOR VALUES FROM ('2025-01-01') TO ('2026-01-01')
    PARTITION BY LIST (region);
CREATE TABLE $S.events_2025_eu PARTITION OF $S.events_2025 FOR VALUES IN ('eu');
CREATE TABLE $S.plain (a int);
INSERT INTO $S.events SELECT g, '2024-02-10', 'x' FROM generate_series(1, 100) g;
ANALYZE $S.events;
"#;

#[tokio::test]
async fn catalog_partitions_nest_under_their_parent() {
    let d = require_pg!();
    let fx = Fixture::new(&d, PARTITION_SETUP).await;
    // A partition may live in another schema than its parent.
    let other = Fixture::new(&d, "").await;
    raw(&d)
        .await
        .batch_execute(&fx.q(&format!(
            "CREATE TABLE {}.events_2023 PARTITION OF $S.events FOR VALUES FROM ('2023-01-01') TO ('2024-01-01')",
            other.schema
        )))
        .await
        .expect("cross-schema partition");
    let h = connect(&d).await;

    // list_objects still returns every partition, flagged with its parent.
    let objs = h.list_objects(&fx.schema).await.expect("list_objects");
    let get = |n: &str| {
        objs.iter()
            .find(|o| o.name == n)
            .unwrap_or_else(|| panic!("{n} in {objs:?}"))
    };
    let events = get("events");
    assert_eq!(events.kind, ObjectKind::PartitionedTable);
    assert!(!events.is_partition && events.parent_name.is_none() && events.parent_schema.is_none());
    assert!(!get("plain").is_partition);
    for n in [
        "events_2024_10",
        "events_2024_02",
        "events_default",
        "events_2025",
    ] {
        let o = get(n);
        assert!(o.is_partition, "{n}");
        assert_eq!(o.parent_schema.as_deref(), Some(fx.schema.as_str()), "{n}");
        assert_eq!(o.parent_name.as_deref(), Some("events"), "{n}");
    }
    // Sub-partitions are partitions of their own parent, not of the root.
    assert_eq!(
        get("events_2025_eu").parent_name.as_deref(),
        Some("events_2025")
    );
    let others = h
        .list_objects(&other.schema)
        .await
        .expect("list_objects other");
    let o = others
        .iter()
        .find(|o| o.name == "events_2023")
        .expect("cross-schema partition");
    assert_eq!(o.parent_schema.as_deref(), Some(fx.schema.as_str()));
    assert_eq!(o.parent_name.as_deref(), Some("events"));

    // The agent's catalog dump is unchanged: the root only.
    let meta = h
        .schema_metadata(Some(&fx.schema), 100)
        .await
        .expect("meta");
    let mut names: Vec<_> = meta.tables.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["events", "plain"]);

    let l = h
        .list_partitions(&fx.schema, "events")
        .await
        .expect("list_partitions");
    assert_eq!(l.partition_key.as_deref(), Some("RANGE (created)"));
    assert!(!l.truncated);
    assert!(
        l.partman.is_none(),
        "pg_partman is not installed: no info, no error"
    );
    let got: Vec<(&str, &str)> = l
        .partitions
        .iter()
        .map(|p| (p.schema.as_str(), p.name.as_str()))
        .collect();
    assert_eq!(
        got,
        [
            (other.schema.as_str(), "events_2023"),
            (fx.schema.as_str(), "events_2024_02"),
            (fx.schema.as_str(), "events_2024_10"),
            (fx.schema.as_str(), "events_2025"),
            (fx.schema.as_str(), "events_default"),
        ],
        "by bound, default last, any schema"
    );
    let p = |n: &str| l.partitions.iter().find(|p| p.name == n).unwrap();
    assert_eq!(
        p("events_2024_02").bound,
        "FOR VALUES FROM ('2024-02-01') TO ('2024-03-01')"
    );
    assert_eq!(p("events_2024_02").estimated_rows, Some(100));
    assert!(p("events_2024_02").size_bytes.is_some_and(|s| s > 0));
    assert!(!p("events_2024_02").is_default && !p("events_2024_02").is_partitioned);
    assert_eq!(p("events_default").bound, "DEFAULT");
    assert!(p("events_default").is_default);
    assert!(p("events_2025").is_partitioned && !p("events_2025").is_default);
    assert!(p("events_2025").size_bytes.is_some(), "sum over the leaves");

    // A partitioned partition expands again.
    let sub = h
        .list_partitions(&fx.schema, "events_2025")
        .await
        .expect("sub");
    assert_eq!(sub.partition_key.as_deref(), Some("LIST (region)"));
    assert_eq!(sub.partitions.len(), 1);
    assert_eq!(sub.partitions[0].name, "events_2025_eu");
    assert_eq!(sub.partitions[0].bound, "FOR VALUES IN ('eu')");

    // A table without partitions, and one that does not exist, are not partitioned tables.
    let leaf = h
        .list_partitions(&fx.schema, "plain")
        .await
        .expect_err("plain table");
    assert!(matches!(leaf, CoreError::NotFound(_)), "{leaf:?}");
    assert!(h.list_partitions(&fx.schema, "nope").await.is_err());
    // Names are bound parameters, never spliced into SQL.
    assert!(
        h.list_partitions("x' OR '1'='1", "events\"; DROP TABLE plain; --")
            .await
            .is_err()
    );
    assert!(
        !h.list_objects(&fx.schema)
            .await
            .unwrap()
            .iter()
            .all(|o| o.name != "plain")
    );
}

#[tokio::test]
async fn catalog_partition_list_is_ordered_numerically_and_capped() {
    let d = require_pg!();
    let fx = Fixture::new(
        &d,
        r#"
CREATE TABLE $S.many (k int NOT NULL) PARTITION BY LIST (k);
DO $$ BEGIN
  FOR i IN 1..1001 LOOP
    EXECUTE format('CREATE TABLE $S.many_%s PARTITION OF $S.many FOR VALUES IN (%s)', i, i);
  END LOOP;
END $$;
"#,
    )
    .await;
    let h = connect(&d).await;
    let l = h
        .list_partitions(&fx.schema, "many")
        .await
        .expect("list_partitions");
    assert!(l.truncated, "1001 partitions exceed the cap");
    assert_eq!(l.partitions.len(), 1000);
    let bounds: Vec<&str> = l.partitions.iter().map(|p| p.bound.as_str()).collect();
    assert_eq!(
        &bounds[..3],
        [
            "FOR VALUES IN (1)",
            "FOR VALUES IN (2)",
            "FOR VALUES IN (3)"
        ]
    );
    assert_eq!(bounds[9], "FOR VALUES IN (10)", "10 after 9, not after 1");
    assert_eq!(bounds[999], "FOR VALUES IN (1000)");
}

#[tokio::test]
async fn catalog_describe_table() {
    let d = require_pg!();
    let fx = Fixture::new(&d, CATALOG_SETUP).await;
    let h = connect(&d).await;

    let t = h
        .describe_table(&fx.schema, "parent")
        .await
        .expect("describe parent");
    assert_eq!(
        (t.kind, t.comment.as_deref(), t.estimated_rows),
        (ObjectKind::Table, Some("the parents"), Some(50))
    );
    let col = |n: &str| {
        t.columns
            .iter()
            .find(|c| c.name == n)
            .unwrap_or_else(|| panic!("column {n}"))
    };
    assert_eq!(
        t.columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "name", "note", "created", "total"]
    );
    assert_eq!(
        t.columns.iter().map(|c| c.ordinal).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(col("id").data_type, "bigint");
    assert!(col("id").is_primary_key && !col("id").nullable);
    assert_eq!(
        col("id").default.as_deref(),
        Some("GENERATED ALWAYS AS IDENTITY")
    );
    assert_eq!(col("name").data_type, "text");
    assert!(!col("name").nullable && !col("name").is_primary_key);
    assert_eq!(col("name").comment.as_deref(), Some("display name"));
    assert_eq!(col("note").default.as_deref(), Some("'none'::text"));
    assert!(col("note").nullable);
    assert_eq!(col("created").data_type, "timestamp with time zone");
    assert_eq!(col("created").default.as_deref(), Some("now()"));
    assert_eq!(col("total").data_type, "numeric(12,2)");
    assert_eq!(t.indexes.len(), 1);
    assert!(t.indexes[0].is_primary && t.indexes[0].is_unique);
    assert!(
        t.indexes[0]
            .definition
            .starts_with("CREATE UNIQUE INDEX parent_pkey ON"),
        "{}",
        t.indexes[0].definition
    );
    assert!(t.foreign_keys.is_empty());

    let c = h
        .describe_table(&fx.schema, "child")
        .await
        .expect("describe child");
    let pk_cols: Vec<_> = c
        .columns
        .iter()
        .filter(|x| x.is_primary_key)
        .map(|x| x.name.as_str())
        .collect();
    assert_eq!(pk_cols, ["a", "b"]);
    let idx: Vec<_> = c
        .indexes
        .iter()
        .map(|i| (i.name.as_str(), i.is_primary, i.is_unique))
        .collect();
    assert_eq!(
        idx,
        [
            ("child_pkey", true, true),
            ("child_b_uq", false, true),
            ("child_parent_idx", false, false)
        ]
    );
    assert_eq!(c.foreign_keys.len(), 2);
    let composite = c
        .foreign_keys
        .iter()
        .find(|f| f.name == "child_composite_fk")
        .expect("composite fk");
    // Declared order (cy, cx) -> (y, x), which differs from attnum order.
    assert_eq!(composite.columns, ["cy", "cx"]);
    assert_eq!(composite.referenced_columns, ["y", "x"]);
    assert_eq!(
        (
            composite.referenced_schema.as_str(),
            composite.referenced_table.as_str()
        ),
        (fx.schema.as_str(), "parent2")
    );
    let simple = c
        .foreign_keys
        .iter()
        .find(|f| f.referenced_table == "parent")
        .expect("simple fk");
    assert_eq!(
        (simple.columns.clone(), simple.referenced_columns.clone()),
        (vec!["parent_id".to_string()], vec!["id".to_string()])
    );

    let v = h
        .describe_table(&fx.schema, "v")
        .await
        .expect("describe view");
    assert_eq!(v.kind, ObjectKind::View);
    assert_eq!(v.columns.len(), 2);

    let err = h
        .describe_table(&fx.schema, "nope")
        .await
        .expect_err("missing");
    assert!(matches!(err, CoreError::NotFound(_)), "{err:?}");
}

#[tokio::test]
async fn catalog_schema_metadata_is_capped_and_set_based() {
    let d = require_pg!();
    let fx = Fixture::new(&d, CATALOG_SETUP).await;
    let h = connect(&d).await;

    let m = h
        .schema_metadata(Some(&fx.schema), 100)
        .await
        .expect("schema_metadata");
    assert_eq!(m.database, d.dbname);
    assert!(m.server_version.is_some());
    assert!(!m.truncated);
    let names: Vec<_> = m.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        ["child", "mv", "parent", "parent2", "v"],
        "tables/views only, ordered, no sequences or functions"
    );
    let child = m.tables.iter().find(|t| t.name == "child").expect("child");
    assert_eq!(child.columns.len(), 5);
    assert_eq!(child.indexes.len(), 3);
    assert_eq!(child.foreign_keys.len(), 2);
    let parent = m
        .tables
        .iter()
        .find(|t| t.name == "parent")
        .expect("parent");
    assert_eq!(parent.estimated_rows, Some(50));
    assert_eq!(parent.comment.as_deref(), Some("the parents"));

    let capped = h
        .schema_metadata(Some(&fx.schema), 2)
        .await
        .expect("capped");
    assert_eq!(capped.tables.len(), 2);
    assert!(capped.truncated);
    let exact = h.schema_metadata(Some(&fx.schema), 5).await.expect("exact");
    assert_eq!(exact.tables.len(), 5);
    assert!(
        !exact.truncated,
        "exactly max_tables relations is not truncation"
    );
    let none = h.schema_metadata(Some(&fx.schema), 0).await.expect("zero");
    assert!(none.tables.is_empty() && none.truncated);

    // No filter: user schemas only.
    // The scan covers every user schema in the shared test database, so a table another test
    // drops mid-scan (`pg_get_expr` / `pg_get_indexdef` on a vanished relation) fails it; that
    // is a property of the concurrent test run, not of this code. Retry a few times.
    let mut attempt = 0;
    let all = loop {
        match h.schema_metadata(None, 100_000).await {
            Ok(m) => break m,
            Err(CoreError::Database(m)) if m.contains("could not open relation") && attempt < 5 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            Err(e) => panic!("unfiltered: {e:?}"),
        }
    };
    assert!(all.tables.iter().any(|t| t.schema == fx.schema));
    assert!(
        all.tables
            .iter()
            .all(|t| t.schema != "pg_catalog" && t.schema != "information_schema")
    );
}

// ---- type encoding -------------------------------------------------------------------------

#[tokio::test]
async fn encodes_scalar_types() {
    let d = require_pg!();
    let h = connect(&d).await;
    let cases: &[(&str, Value)] = &[
        ("true", json!(true)),
        ("false", json!(false)),
        ("NULL::int4", Value::Null),
        ("32767::int2", json!(32767)),
        ("(-2147483648)::int4", json!(-2147483648i64)),
        ("4294967295::oid", json!(4294967295u32)),
        ("1.5::float4", json!(1.5)),
        ("0.1::float4", json!(0.1)),
        ("1.25::float8", json!(1.25)),
        ("'NaN'::float8", json!("NaN")),
        ("'Infinity'::float8", json!("Infinity")),
        ("'-Infinity'::float4", json!("-Infinity")),
        // int8 is a string, including values a JS double cannot hold.
        ("9007199254740993::int8", json!("9007199254740993")),
        ("9223372036854775807::int8", json!("9223372036854775807")),
        (
            "(-9223372036854775808)::int8",
            json!("-9223372036854775808"),
        ),
        ("42::int8", json!("42")),
        // numeric
        ("0::numeric", json!("0")),
        ("0.00::numeric", json!("0.00")),
        ("123.4500::numeric", json!("123.4500")),
        ("(-0.000001)::numeric", json!("-0.000001")),
        (
            "12345678901234567890.123456789::numeric",
            json!("12345678901234567890.123456789"),
        ),
        ("100000000::numeric", json!("100000000")),
        (
            "1000000000000000000000000000000::numeric",
            json!("1000000000000000000000000000000"),
        ),
        ("3.14159::numeric(10,2)", json!("3.14")),
        ("'NaN'::numeric", json!("NaN")),
        // money
        ("1234.56::numeric::money", json!("1234.56")),
        ("(-0.05)::numeric::money", json!("-0.05")),
        // text-likes
        ("'héllo wörld'::text", json!("héllo wörld")),
        ("'ab'::varchar(5)", json!("ab")),
        ("'ab'::bpchar(4)", json!("ab  ")),
        ("'nm'::name", json!("nm")),
        ("'x'::\"char\"", json!("x")),
        ("'literal'", json!("literal")),
        ("'<a/>'::xml", json!("<a/>")),
        (
            "'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid",
            json!("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"),
        ),
        // json
        (
            r#"'{"a":[1,2,{"b":null}],"s":"x"}'::json"#,
            json!({"a": [1, 2, {"b": null}], "s": "x"}),
        ),
        (
            r#"'{"a":[1,2,{"b":null}],"s":"x"}'::jsonb"#,
            json!({"a": [1, 2, {"b": null}], "s": "x"}),
        ),
        ("'[]'::jsonb", json!([])),
        ("'null'::jsonb", Value::Null),
        // temporal
        (
            "'2024-03-15 10:20:30.123456'::timestamp",
            json!("2024-03-15T10:20:30.123456"),
        ),
        (
            "'2024-03-15 10:20:30'::timestamp",
            json!("2024-03-15T10:20:30"),
        ),
        (
            "'2024-03-15 10:20:30+02'::timestamptz",
            json!("2024-03-15T08:20:30Z"),
        ),
        (
            "'2024-03-15 10:20:30.5+00'::timestamptz",
            json!("2024-03-15T10:20:30.500Z"),
        ),
        ("'1999-12-31'::date", json!("1999-12-31")),
        ("'2024-03-15'::date", json!("2024-03-15")),
        ("'infinity'::date", json!("infinity")),
        ("'-infinity'::timestamp", json!("-infinity")),
        ("'infinity'::timestamptz", json!("infinity")),
        ("'10:20:30.5'::time", json!("10:20:30.5")),
        ("'24:00:00'::time", json!("24:00:00")),
        ("'10:20:30+02'::timetz", json!("10:20:30+02:00")),
        ("'10:20:30-05:30'::timetz", json!("10:20:30-05:30")),
        (
            "'1 year 2 mons 3 days 04:05:06'::interval",
            json!("1 year 2 mons 3 days 04:05:06"),
        ),
        // binary
        (
            "'\\xdeadbeef'::bytea",
            json!({"$bytes": "3q2+7w==", "len": 4}),
        ),
        ("''::bytea", json!({"$bytes": "", "len": 0})),
        // network
        ("'192.168.1.5/24'::inet", json!("192.168.1.5/24")),
        ("'10.0.0.1'::inet", json!("10.0.0.1")),
        ("'10.0.0.0/8'::cidr", json!("10.0.0.0/8")),
        ("'::1'::inet", json!("::1")),
        ("'2001:db8::/32'::cidr", json!("2001:db8::/32")),
        ("'08:00:2b:01:02:03'::macaddr", json!("08:00:2b:01:02:03")),
        // misc
        ("B'10100'::varbit", json!("10100")),
        ("'(1,2)'::point", json!("(1,2)")),
        ("int4range(1, 10)", json!("[1,10)")),
        ("'empty'::int4range", json!("empty")),
        ("'16/B374D848'::pg_lsn", json!("16/B374D848")),
    ];
    for (expr, want) in cases {
        let got = scalar(&h, expr).await;
        assert_eq!(&got, want, "SELECT {expr}");
    }
}

#[tokio::test]
async fn encodes_arrays() {
    let d = require_pg!();
    let h = connect(&d).await;
    let cases: &[(&str, Value)] = &[
        ("ARRAY[1,2,NULL]::int4[]", json!([1, 2, null])),
        (
            "ARRAY[9007199254740993, 1]::int8[]",
            json!(["9007199254740993", "1"]),
        ),
        (
            "ARRAY['a','b,c','d\"e']::text[]",
            json!(["a", "b,c", "d\"e"]),
        ),
        ("'{}'::int4[]", json!([])),
        ("ARRAY[[1,2],[3,4]]", json!([[1, 2], [3, 4]])),
        ("ARRAY[1.5,2.50]::numeric[]", json!(["1.5", "2.50"])),
        (
            "ARRAY['2024-01-01'::date, NULL]",
            json!(["2024-01-01", null]),
        ),
        ("ARRAY[true,false]", json!([true, false])),
        ("ARRAY[NULL::text]", json!([null])),
        ("ARRAY[1.5, 2.5]::float8[]", json!([1.5, 2.5])),
        (
            "ARRAY['a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::uuid]",
            json!(["a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"]),
        ),
        (
            "ARRAY['\\x01'::bytea]",
            json!([{"$bytes": "AQ==", "len": 1}]),
        ),
        ("ARRAY['{\"k\":1}'::jsonb]", json!([{"k": 1}])),
        (
            "ARRAY['2024-03-15 10:00:00+00'::timestamptz]",
            json!(["2024-03-15T10:00:00Z"]),
        ),
        ("ARRAY['10.0.0.1'::inet]", json!(["10.0.0.1"])),
    ];
    for (expr, want) in cases {
        let got = scalar(&h, expr).await;
        assert_eq!(&got, want, "SELECT {expr}");
    }
}

#[tokio::test]
async fn encodes_enums_domains_and_unsupported_types_without_panicking() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TYPE $S.mood AS ENUM ('sad','ok'); CREATE DOMAIN $S.posint AS int CHECK (VALUE > 0); CREATE DOMAIN $S.label AS text;").await;
    let h = connect(&d).await;
    assert_eq!(scalar(&h, &fx.q("'ok'::$S.mood")).await, json!("ok"));
    assert_eq!(scalar(&h, &fx.q("5::$S.posint")).await, json!(5));
    assert_eq!(scalar(&h, &fx.q("'hi'::$S.label")).await, json!("hi"));
    assert_eq!(
        scalar(&h, &fx.q("ARRAY['sad','ok']::$S.mood[]")).await,
        json!(["sad", "ok"])
    );
    // Not decoded: a readable placeholder, never an error.
    assert_eq!(scalar(&h, "'a:1'::tsvector").await, json!("<tsvector>"));
    assert_eq!(scalar(&h, "ROW(1,2)").await, json!("<record>"));
    // ... and the connection is still fine afterwards.
    assert_eq!(scalar(&h, "1::int4").await, json!(1));
}

#[tokio::test]
async fn numeric_and_interval_match_the_servers_own_text_output() {
    let d = require_pg!();
    let h = connect(&d).await;
    let nums = [
        "0",
        "0.0",
        "-0.0",
        "1",
        "-1",
        "0.1",
        "0.0001",
        "0.00001",
        "0.000012345",
        "1.5",
        "99999999",
        "100000000",
        "100000000.5",
        "10000",
        "9999.9999",
        "10000.0001",
        "-12345.6789",
        "12345678901234567890.123456789",
        "1e-20",
        "123456789012345678901234567890123456789",
        "0.1000",
        "1000000.000000",
        "'NaN'",
    ];
    let intervals = [
        "0",
        "'1 day'",
        "'-1 day'",
        "'1 year'",
        "'-1 year 2 mons'",
        "'1 year 2 mons 3 days 04:05:06.789'",
        "'100 hours'",
        "'-100 hours'",
        "'1 day -01:00:00'",
        "'-1 day 01:00:00'",
        "'2 mons -3 days'",
        "'0.000001 seconds'",
        "'25 months'",
        "'-25 months'",
        "'1 mon 1 sec'",
    ];
    for n in nums {
        let r = h
            .execute_collect(
                &format!("SELECT ({n})::numeric AS v, ({n})::numeric::text AS t"),
                ExecOptions::default(),
            )
            .await
            .expect("numeric");
        assert_eq!(r.rows[0][0], r.rows[0][1], "numeric {n}");
    }
    for i in intervals {
        let sql = if i == "0" {
            "SELECT '0'::interval AS v, '0'::interval::text AS t".to_string()
        } else {
            format!("SELECT ({i})::interval AS v, ({i})::interval::text AS t")
        };
        let r = h
            .execute_collect(&sql, ExecOptions::default())
            .await
            .expect("interval");
        assert_eq!(r.rows[0][0], r.rows[0][1], "interval {i}");
    }
}

#[tokio::test]
async fn columns_report_type_names_and_oids() {
    let d = require_pg!();
    let h = connect(&d).await;
    let r = h
        .execute_collect(
            "SELECT 1::int4 AS a, 'x'::text AS b, 1::numeric AS c, ARRAY[1]::int8[] AS d",
            ExecOptions::default(),
        )
        .await
        .expect("query");
    let cols: Vec<_> = r
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.type_name.as_str(), c.type_oid))
        .collect();
    assert_eq!(
        cols,
        [
            ("a", "int4", 23),
            ("b", "text", 25),
            ("c", "numeric", 1700),
            ("d", "_int8", 1016)
        ]
    );
}

// ---- execution -----------------------------------------------------------------------------

#[tokio::test]
async fn streams_rows_in_pages() {
    let d = require_pg!();
    let h = connect(&d).await;
    let evs = run(
        &h,
        "SELECT g AS n, 'row' || g AS label FROM generate_series(1, 5) g",
        ExecOptions {
            page_size: 2,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(
        &evs[0],
        QueryEvent::Started {
            statement_count: 1,
            ..
        }
    ));
    match &evs[1] {
        QueryEvent::Columns {
            statement_index: 0,
            columns,
        } => assert_eq!(
            columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["n", "label"]
        ),
        other => panic!("expected Columns, got {other:?}"),
    }
    assert_eq!(row_pages(&evs), [2, 2, 1]);
    match &evs[2] {
        QueryEvent::Rows { rows, .. } => assert_eq!(rows[0], vec![json!(1), json!("row1")]),
        other => panic!("expected Rows, got {other:?}"),
    }
    match dones(&evs)[0] {
        QueryEvent::StatementDone {
            statement_index: 0,
            command_tag,
            rows_affected: None,
            row_count: 5,
            truncated: false,
            ..
        } => assert_eq!(command_tag.as_deref(), Some("SELECT")),
        other => panic!("{other:?}"),
    }
    assert_finished(&evs);
}

#[tokio::test]
async fn page_size_zero_is_treated_as_one_and_empty_results_have_columns_only() {
    let d = require_pg!();
    let h = connect(&d).await;
    let evs = run(
        &h,
        "SELECT g FROM generate_series(1, 3) g",
        ExecOptions {
            page_size: 0,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(row_pages(&evs), [1, 1, 1]);
    let evs = run(&h, "SELECT 1 AS a WHERE false", ExecOptions::default()).await;
    assert!(evs.iter().any(|e| matches!(e, QueryEvent::Columns { .. })));
    assert!(row_pages(&evs).is_empty());
    assert_finished(&evs);
}

// ---- oversized values (review finding H1) ------------------------------------------------------

/// One row with three values far over the limit and one small one, through both result paths.
#[tokio::test]
async fn oversized_values_arrive_cut_and_marked_not_whole() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    const LIMIT: usize = vibelens_core::model::MAX_CELL_BYTES;
    let sql = "SELECT repeat('x', 3000000) AS t, \
               decode(repeat('ab', 1500000), 'hex') AS b, \
               ('{\"k\":\"' || repeat('y', 2000000) || '\"}')::jsonb AS j, \
               repeat('z', 10) AS small";

    let streamed: Vec<Vec<Value>> = run(&h, sql, ExecOptions::default())
        .await
        .into_iter()
        .filter_map(|e| match e {
            QueryEvent::Rows { rows, .. } => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    let collected = h
        .execute_collect(sql, ExecOptions::default())
        .await
        .unwrap()
        .rows;

    for (path, rows) in [("stream", streamed), ("collect", collected)] {
        assert_eq!(rows.len(), 1, "{path}");
        let row = &rows[0];
        let shape = |c: &Value| {
            let t = &c["$truncated"];
            (
                t["kind"].as_str().map(str::to_owned),
                t["bytes"].as_u64(),
                t["preview"].as_str().map(str::len),
            )
        };
        assert_eq!(
            shape(&row[0]),
            (Some("text".into()), Some(3_000_000), Some(LIMIT)),
            "{path}: text"
        );
        // base64 of LIMIT bytes
        assert_eq!(
            shape(&row[1]),
            (
                Some("bytes".into()),
                Some(1_500_000),
                Some(LIMIT.div_ceil(3) * 4)
            ),
            "{path}: bytea"
        );
        assert_eq!(
            shape(&row[2]),
            // jsonb prints `{"k": "yyy…"}`, with a space after the colon: 9 bytes around the 2 MB.
            (Some("json".into()), Some(2_000_009), Some(LIMIT)),
            "{path}: jsonb"
        );
        assert_eq!(row[3], json!("zzzzzzzzzz"), "{path}: small value");
        let sent: usize = row.iter().map(|c| c.to_string().len()).sum();
        assert!(
            sent < 4 * LIMIT,
            "{path}: {sent} bytes of JSON for one row of 6.5 MB"
        );
    }
}

/// Twenty rows of 600 KB each are 12 MB: they must not travel as one page just because twenty
/// is fewer than `page_size`.
#[tokio::test]
async fn a_page_of_large_cells_is_sent_before_it_reaches_page_size() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let evs = run(
        &h,
        "SELECT repeat('x', 600000) FROM generate_series(1, 20)",
        ExecOptions {
            page_size: 500,
            ..Default::default()
        },
    )
    .await;
    let pages = row_pages(&evs);
    assert_eq!(pages.iter().sum::<usize>(), 20, "{pages:?}");
    let budget = vibelens_core::model::PAGE_BYTE_BUDGET;
    for e in &evs {
        if let QueryEvent::Rows { rows, .. } = e {
            let bytes: usize = rows.iter().flatten().map(|c| c.to_string().len()).sum();
            assert!(
                bytes <= budget + 700_000,
                "a page of {} rows weighs {bytes} bytes: {pages:?}",
                rows.len()
            );
        }
    }
    // Small rows still fill whole pages.
    let evs = run(
        &h,
        "SELECT g FROM generate_series(1, 1200) g",
        ExecOptions {
            page_size: 500,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(row_pages(&evs), [500, 500, 200]);
}

#[tokio::test]
async fn max_rows_truncates_without_draining_the_stream() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let t0 = Instant::now();
    let evs = run(
        &h,
        "SELECT g FROM (SELECT generate_series(1, 50000000) AS g) s",
        ExecOptions {
            max_rows: 5,
            page_size: 2,
            ..Default::default()
        },
    )
    .await;
    assert!(
        t0.elapsed() < Duration::from_secs(10),
        "took {:?}: the stream must not be drained",
        t0.elapsed()
    );
    assert_eq!(row_pages(&evs), [2, 2, 1]);
    match dones(&evs)[0] {
        QueryEvent::StatementDone {
            row_count: 5,
            truncated: true,
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert_finished(&evs);
    // The single pooled connection is still usable after abandoning a half-read stream.
    assert_eq!(scalar(&h, "7::int4").await, json!(7));

    // Exactly max_rows rows is not truncation.
    let evs = run(
        &h,
        "SELECT g FROM generate_series(1, 5) g",
        ExecOptions {
            max_rows: 5,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(
        dones(&evs)[0],
        QueryEvent::StatementDone {
            row_count: 5,
            truncated: false,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------------------------
// Read-only runs and the row cap (review finding H2)
// ---------------------------------------------------------------------------------------------

/// A result the server streams row by row and would take minutes to send in full.
const HUGE_RESULT: &str = "SELECT g FROM (SELECT generate_series(1, 2000000000) AS g) s";

/// How long a truncated run may take. Far more than it needs, far less than the whole result.
const TRUNCATED_RUN_BUDGET: Duration = Duration::from_secs(10);

/// Waits until no backend is running a statement whose text contains `marker` any more.
async fn assert_server_stopped(d: &Dsn, marker: &str) {
    let c = raw(d).await;
    let t0 = Instant::now();
    loop {
        let running: i64 = c
            .query_one(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE pid <> pg_backend_pid() AND state <> 'idle' AND position($1 in query) > 0",
                &[&marker],
            )
            .await
            .expect("pg_stat_activity")
            .get(0);
        if running == 0 {
            return;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "{running} backend(s) still producing the abandoned result after {:?}",
            t0.elapsed()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The read-only twin of `max_rows_truncates_without_draining_the_stream`. A read-only run
/// checks its transaction after every statement and ends it with `ROLLBACK`, both on the same
/// connection: they must not wait for the rest of a result nobody will read.
#[tokio::test]
async fn read_only_max_rows_truncates_without_draining_the_stream() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    // Its own upper bound, so `pg_stat_activity` can tell this statement from other tests'.
    const MARKER: &str = "generate_series(1, 2000000123)";
    let sql = format!("SELECT g FROM (SELECT {MARKER} AS g) s");
    let t0 = Instant::now();
    let evs = tokio::time::timeout(
        TRUNCATED_RUN_BUDGET * 2,
        run(
            &h,
            &sql,
            ExecOptions {
                max_rows: 5,
                page_size: 2,
                read_only: true,
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap_or_else(|_| {
        panic!(
            "still running after {:?}: the read-only run waited for the whole result",
            t0.elapsed()
        )
    });
    assert!(
        t0.elapsed() < TRUNCATED_RUN_BUDGET,
        "took {:?}: the stream must not be drained",
        t0.elapsed()
    );
    assert_eq!(row_pages(&evs), [2, 2, 1]);
    match dones(&evs)[0] {
        QueryEvent::StatementDone {
            row_count: 5,
            truncated: true,
            ..
        } => {}
        other => panic!("{other:?}"),
    }
    assert_finished(&evs);
    // The server was told to stop, not left to produce two thousand million rows for nobody.
    assert_server_stopped(&d, MARKER).await;
    // The pool (of one) still serves queries, read-only and not.
    assert_eq!(scalar(&h, "7::int4").await, json!(7));
    let r = h
        .execute_collect(
            "SELECT current_setting('transaction_read_only')",
            ExecOptions {
                read_only: true,
                ..Default::default()
            },
        )
        .await
        .expect("read-only run after a truncated one");
    assert_eq!(r.rows, vec![vec![json!("on")]]);

    // Exactly max_rows rows is not truncation, and keeps the connection.
    let pid = scalar(&h, "pg_backend_pid()").await;
    let evs = run(
        &h,
        "SELECT g FROM generate_series(1, 5) g",
        ExecOptions {
            max_rows: 5,
            read_only: true,
            ..Default::default()
        },
    )
    .await;
    assert!(matches!(
        dones(&evs)[0],
        QueryEvent::StatementDone {
            row_count: 5,
            truncated: false,
            ..
        }
    ));
    assert_finished(&evs);
    assert_eq!(
        scalar(&h, "pg_backend_pid()").await,
        pid,
        "an untruncated read-only run returns its connection to the pool"
    );
}

/// The AI agent's path: materialised, read-only, 200 rows, 30 s statement timeout. It must get
/// its 200 rows at once, not a statement-timeout error half a minute later.
#[tokio::test]
async fn read_only_execute_collect_truncates_without_draining_the_stream() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let t0 = Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(45),
        h.execute_collect(
            HUGE_RESULT,
            ExecOptions {
                max_rows: 200,
                read_only: true,
                timeout_ms: Some(30_000),
                ..Default::default()
            },
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("still running after {:?}", t0.elapsed()));
    let took = t0.elapsed();
    let r = r.unwrap_or_else(|e| panic!("after {took:?}: {e}"));
    assert!(
        took < TRUNCATED_RUN_BUDGET,
        "took {took:?}: the stream must not be drained"
    );
    assert_eq!((r.rows.len(), r.row_count, r.truncated), (200, 200, true));
    assert_eq!(r.rows[0], vec![json!(1)]);
    assert_eq!(r.rows[199], vec![json!(200)]);
    assert_eq!(scalar(&h, "7::int4").await, json!(7));
}

/// A statement that is cut off stops a read-only script: the statements after it would have to
/// wait for the rest of its result, and running them on another connection would lose the
/// `SET`s, temporary tables and cursors the script made. The run says which were not run.
#[tokio::test]
async fn read_only_script_stops_after_a_truncated_statement_that_is_not_the_last() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let script = format!("SELECT 1 AS one; {HUGE_RESULT}; SELECT 42 AS answer; SELECT 43");
    let opts = || ExecOptions {
        max_rows: 5,
        read_only: true,
        ..Default::default()
    };
    let t0 = Instant::now();
    let evs = tokio::time::timeout(TRUNCATED_RUN_BUDGET * 2, run(&h, &script, opts()))
        .await
        .unwrap_or_else(|_| panic!("still running after {:?}", t0.elapsed()));
    assert!(
        t0.elapsed() < TRUNCATED_RUN_BUDGET,
        "took {:?}",
        t0.elapsed()
    );

    let summary: Vec<_> = dones(&evs)
        .into_iter()
        .map(|e| match e {
            QueryEvent::StatementDone {
                statement_index,
                row_count,
                truncated,
                ..
            } => (*statement_index, *row_count, *truncated),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        summary,
        [(0, 1, false), (1, 5, true)],
        "the rows that were read are still delivered: {evs:?}"
    );
    let (idx, err) = failed(&evs);
    assert_eq!(
        idx,
        Some(2),
        "reported on the first statement that did not run"
    );
    assert_eq!(err.code, "invalid");
    assert!(
        err.message
            .contains("statement 2 returned more than 5 rows")
            && err.message.contains("statements 3 to 4 were not run"),
        "{}",
        err.message
    );
    assert!(
        !evs.iter().any(|e| matches!(
            e,
            QueryEvent::Columns { statement_index, .. } if *statement_index >= 2
        )),
        "nothing after the truncated statement ran: {evs:?}"
    );

    // The materialised form reports the same thing as an error.
    let e = h
        .execute_collect(&format!("{HUGE_RESULT}; SELECT 42"), opts())
        .await
        .expect_err("the last statement did not run");
    assert!(e.to_string().contains("statement 2 was not run"), "{e}");
    assert_eq!(scalar(&h, "7::int4").await, json!(7));
}

#[tokio::test]
async fn execute_collect_honours_max_rows() {
    let d = require_pg!();
    let h = connect(&d).await;
    let r = h
        .execute_collect(
            "SELECT g FROM generate_series(1, 1000) g",
            ExecOptions {
                max_rows: 10,
                ..Default::default()
            },
        )
        .await
        .expect("collect");
    assert_eq!((r.rows.len(), r.row_count, r.truncated), (10, 10, true));
    assert_eq!(r.columns.len(), 1);
    assert_eq!(r.command_tag.as_deref(), Some("SELECT"));
}

#[tokio::test]
async fn multi_statement_scripts_report_each_statement() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "").await;
    let h = connect(&d).await;
    let script = fx.q("CREATE TABLE $S.t (a int); INSERT INTO $S.t VALUES (1),(2),(3); SELECT a FROM $S.t ORDER BY a; UPDATE $S.t SET a = a + 1 WHERE a > 1; DELETE FROM $S.t WHERE a = 1; INSERT INTO $S.t VALUES (9) RETURNING a;");
    let evs = run(&h, &script, ExecOptions::default()).await;
    assert!(
        matches!(
            &evs[0],
            QueryEvent::Started {
                statement_count: 6,
                ..
            }
        ),
        "{:?}",
        evs[0]
    );
    let summary: Vec<_> = dones(&evs)
        .into_iter()
        .map(|e| match e {
            QueryEvent::StatementDone {
                statement_index,
                command_tag,
                rows_affected,
                row_count,
                ..
            } => (
                *statement_index,
                command_tag.clone().unwrap_or_default(),
                *rows_affected,
                *row_count,
            ),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(
        summary,
        [
            (0, "CREATE TABLE".to_string(), None, 0),
            (1, "INSERT".to_string(), Some(3), 0),
            (2, "SELECT".to_string(), None, 3),
            (3, "UPDATE".to_string(), Some(2), 0),
            (4, "DELETE".to_string(), Some(1), 0),
            (5, "INSERT".to_string(), Some(1), 1), // RETURNING
        ]
    );
    assert_finished(&evs);

    // execute_collect reports the last row-returning statement.
    let r = h
        .execute_collect(
            &fx.q("SELECT 1 AS first; SELECT a FROM $S.t ORDER BY a"),
            ExecOptions::default(),
        )
        .await
        .expect("collect");
    assert_eq!(r.columns[0].name, "a");
    assert_eq!(r.rows, vec![vec![json!(3)], vec![json!(4)], vec![json!(9)]]);
    // ... or the last statement when none returns rows.
    let r = h
        .execute_collect(
            &fx.q("DELETE FROM $S.t WHERE a = 9"),
            ExecOptions::default(),
        )
        .await
        .expect("collect dml");
    assert_eq!(
        (r.command_tag.as_deref(), r.rows_affected, r.columns.len()),
        (Some("DELETE"), Some(1), 0)
    );
}

#[tokio::test]
async fn an_error_stops_the_script_and_names_the_statement() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let evs = run(
        &h,
        "SELECT 1; SELECT * FROM definitely_not_a_table; SELECT 3",
        ExecOptions::default(),
    )
    .await;
    let (idx, err) = failed(&evs);
    assert_eq!(idx, Some(1));
    assert_eq!(err.code, "database");
    assert!(
        err.message.contains("definitely_not_a_table"),
        "{}",
        err.message
    );
    assert_eq!(dones(&evs).len(), 1, "statement 3 must not run");
    // The pooled connection was left clean.
    assert_eq!(scalar(&h, "1::int4").await, json!(1));
}

#[tokio::test]
async fn syntax_errors_fail_with_a_position() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let evs = run(&h, "SELEC 1", ExecOptions::default()).await;
    let (idx, err) = failed(&evs);
    assert_eq!(idx, Some(0));
    assert_eq!(err.code, "database");
    assert!(err.message.contains("syntax error"), "{}", err.message);
    assert!(err.message.contains("position 1"), "{}", err.message);
    assert_eq!(scalar(&h, "1::int4").await, json!(1));

    let e = h
        .execute_collect("SELEC 1", ExecOptions::default())
        .await
        .expect_err("collect error");
    assert!(matches!(e, CoreError::Database(_)), "{e:?}");
}

#[tokio::test]
async fn statement_timeout_applies_and_is_reset_afterwards() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let t0 = Instant::now();
    let evs = run(
        &h,
        "SELECT pg_sleep(10)",
        ExecOptions {
            timeout_ms: Some(300),
            ..Default::default()
        },
    )
    .await;
    let (_, err) = failed(&evs);
    assert_eq!(err.code, "timeout", "{err:?}");
    assert!(t0.elapsed() < Duration::from_secs(5));
    // Same physical connection, no timeout requested: must not inherit the previous 300ms.
    let r = h
        .execute_collect(
            "SELECT pg_sleep(0.6), current_setting('statement_timeout')",
            ExecOptions::default(),
        )
        .await
        .expect("no leaked timeout");
    assert_eq!(r.rows[0][1], json!("0"));
    // Read-only mode applies it too.
    let evs = run(
        &h,
        "SELECT pg_sleep(10)",
        ExecOptions {
            timeout_ms: Some(300),
            read_only: true,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(failed(&evs).1.code, "timeout");
    // PgOptions.statement_timeout_ms is the default.
    let mut p = params(&d, TlsMode::Require);
    p.options.statement_timeout_ms = Some(250);
    let h2 = connect_with(&d, p).await;
    let evs = run(&h2, "SELECT pg_sleep(10)", ExecOptions::default()).await;
    assert_eq!(failed(&evs).1.code, "timeout");
}

#[tokio::test]
async fn scripts_with_their_own_transactions() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TABLE $S.t (a int)").await;
    let h = connect_single(&d).await;
    let count = |h: Arc<PgHandle>, sql: String| async move {
        h.execute_collect(&sql, ExecOptions::default())
            .await
            .expect("count")
            .rows[0][0]
            .clone()
    };

    // BEGIN ... COMMIT persists.
    assert_finished(
        &run(
            &h,
            &fx.q("BEGIN; INSERT INTO $S.t VALUES (1); COMMIT;"),
            ExecOptions::default(),
        )
        .await,
    );
    assert_eq!(
        count(h.clone(), fx.q("SELECT count(*) FROM $S.t")).await,
        json!("1")
    );

    // A failure inside the user's transaction rolls it back and leaves the connection usable.
    let evs = run(
        &h,
        &fx.q("BEGIN; INSERT INTO $S.t VALUES (2); SELECT 1/0; COMMIT;"),
        ExecOptions::default(),
    )
    .await;
    assert_eq!(failed(&evs).0, Some(2));
    assert_eq!(
        count(h.clone(), fx.q("SELECT count(*) FROM $S.t")).await,
        json!("1"),
        "insert 2 was rolled back"
    );

    // A transaction left open is rolled back and reported, not leaked into the pool.
    let evs = run(
        &h,
        &fx.q("BEGIN; INSERT INTO $S.t VALUES (3)"),
        ExecOptions::default(),
    )
    .await;
    let (idx, err) = failed(&evs);
    assert_eq!((idx, err.code.as_str()), (None, "invalid"));
    assert!(err.message.contains("rolled back"), "{}", err.message);
    assert_eq!(
        count(h.clone(), fx.q("SELECT count(*) FROM $S.t")).await,
        json!("1")
    );
    // Explicit ROLLBACK is fine too.
    assert_finished(
        &run(
            &h,
            &fx.q("BEGIN; INSERT INTO $S.t VALUES (4); ROLLBACK"),
            ExecOptions::default(),
        )
        .await,
    );
    assert_eq!(
        count(h.clone(), fx.q("SELECT count(*) FROM $S.t")).await,
        json!("1")
    );
}

// ---- read-only -----------------------------------------------------------------------------

#[tokio::test]
async fn read_only_blocks_writes_and_cannot_be_escaped() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TABLE $S.t (a int); INSERT INTO $S.t VALUES (1)").await;
    let h = connect_single(&d).await; // one connection: clean-up after each failure is exercised
    let ro = ExecOptions {
        read_only: true,
        ..Default::default()
    };
    let n = |h: Arc<PgHandle>, sql: String| async move {
        h.execute_collect(&sql, ExecOptions::default())
            .await
            .expect("count")
            .rows[0][0]
            .clone()
    };

    // Reads work.
    let evs = run(&h, &fx.q("SELECT a FROM $S.t"), ro.clone()).await;
    assert_eq!(row_pages(&evs), [1]);
    assert_finished(&evs);

    // Writes are rejected by the server.
    for sql in [
        "INSERT INTO $S.t VALUES (2)",
        "UPDATE $S.t SET a = 5",
        "DELETE FROM $S.t",
        "CREATE TABLE $S.x (b int)",
        "TRUNCATE $S.t",
    ] {
        let evs = run(&h, &fx.q(sql), ro.clone()).await;
        let (idx, err) = failed(&evs);
        assert_eq!(idx, Some(0), "{sql}");
        assert!(
            err.message.contains("read-only transaction"),
            "{sql}: {}",
            err.message
        );
    }
    assert_eq!(
        n(h.clone(), fx.q("SELECT count(*) FROM $S.t")).await,
        json!("1")
    );

    // Ways out of the read-only transaction are refused or rejected by the server.
    for sql in [
        "COMMIT; INSERT INTO $S.t VALUES (2)",
        "ROLLBACK; INSERT INTO $S.t VALUES (2)",
        "END; INSERT INTO $S.t VALUES (2)",
        "SET TRANSACTION READ WRITE; INSERT INTO $S.t VALUES (2)",
        "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE; INSERT INTO $S.t VALUES (2)",
        "BEGIN READ WRITE; INSERT INTO $S.t VALUES (2)",
        "SELECT set_config('transaction_read_only', 'off', true); INSERT INTO $S.t VALUES (2)",
        "SELECT 1; COMMIT; INSERT INTO $S.t VALUES (2)",
    ] {
        let evs = run(&h, &fx.q(sql), ro.clone()).await;
        let (_, err) = failed(&evs);
        assert!(
            ["policy_denied", "database"].contains(&err.code.as_str()),
            "{sql}: {err:?}"
        );
        assert_eq!(
            n(h.clone(), fx.q("SELECT count(*) FROM $S.t")).await,
            json!("1"),
            "escaped via: {sql}"
        );
    }

    // After all that, the connection is back to normal read-write behaviour.
    assert_finished(
        &run(
            &h,
            &fx.q("INSERT INTO $S.t VALUES (2)"),
            ExecOptions::default(),
        )
        .await,
    );
    assert_eq!(
        n(h.clone(), fx.q("SELECT count(*) FROM $S.t")).await,
        json!("2")
    );
    // ... and read-only mode leaves nothing open.
    assert_eq!(
        scalar(&h, "current_setting('transaction_read_only')").await,
        json!("off")
    );
}

#[tokio::test]
async fn read_only_collect_blocks_insert() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TABLE $S.t (a int)").await;
    let h = connect(&d).await;
    let err = h
        .execute_collect(
            &fx.q("INSERT INTO $S.t VALUES (1)"),
            ExecOptions {
                read_only: true,
                ..Default::default()
            },
        )
        .await
        .expect_err("blocked");
    assert!(
        matches!(&err, CoreError::Database(m) if m.contains("read-only")),
        "{err:?}"
    );
    let r = h
        .execute_collect(&fx.q("SELECT count(*) FROM $S.t"), ExecOptions::default())
        .await
        .expect("count");
    assert_eq!(r.rows[0][0], json!("0"));
}

// ---- cancellation --------------------------------------------------------------------------

#[tokio::test]
async fn cancel_interrupts_a_running_query_quickly() {
    let d = require_pg!();
    let h = connect(&d).await;
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel(8);
    let task = {
        let h = h.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            h.execute_stream(
                "q".into(),
                "SELECT pg_sleep(30)",
                ExecOptions::default(),
                cancel,
                tx,
            )
            .await
        })
    };
    // Wait until the query is really running server-side.
    assert!(matches!(rx.recv().await, Some(QueryEvent::Started { .. })));
    tokio::time::sleep(Duration::from_millis(400)).await;

    let t0 = Instant::now();
    cancel.cancel();
    let mut evs = vec![QueryEvent::Started {
        query_id: "q".into(),
        statement_count: 1,
    }];
    while let Some(e) = rx.recv().await {
        evs.push(e);
    }
    let elapsed = t0.elapsed();
    task.await.expect("join").expect("execute_stream");
    assert!(elapsed < Duration::from_secs(2), "cancel took {elapsed:?}");
    check_invariants(&evs);
    let (idx, err) = failed(&evs);
    assert_eq!((idx, err.code.as_str()), (Some(0), "cancelled"));

    // The server really stopped the query (not just our side).
    tokio::time::sleep(Duration::from_millis(500)).await;
    let leftovers = scalar(&h, "count(*)::int4 FROM pg_stat_activity WHERE query LIKE 'SELECT pg_sleep(30)%' AND pid <> pg_backend_pid() AND state = 'active'").await;
    assert_eq!(leftovers, json!(0));
    // And the handle recovers on a fresh connection.
    assert_eq!(scalar(&h, "1::int4").await, json!(1));
}

#[tokio::test]
async fn cancel_in_read_only_mode_and_before_start() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let cancel = CancellationToken::new();
    let (tx, mut rx) = mpsc::channel(8);
    let task = {
        let h = h.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            h.execute_stream(
                "q".into(),
                "SELECT pg_sleep(30)",
                ExecOptions {
                    read_only: true,
                    ..Default::default()
                },
                cancel,
                tx,
            )
            .await
        })
    };
    rx.recv().await.expect("started");
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t0 = Instant::now();
    cancel.cancel();
    let mut last = None;
    while let Some(e) = rx.recv().await {
        last = Some(e);
    }
    task.await.expect("join").expect("ok");
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert!(matches!(last, Some(QueryEvent::Failed { error, .. }) if error.code == "cancelled"));
    assert_eq!(
        scalar(&h, "current_setting('transaction_read_only')").await,
        json!("off")
    );

    // Already-cancelled token: Started, then Failed(cancelled), no hang.
    let token = CancellationToken::new();
    token.cancel();
    let (tx, mut rx) = mpsc::channel(8);
    h.execute_stream(
        "q2".into(),
        "SELECT pg_sleep(30)",
        ExecOptions::default(),
        token,
        tx,
    )
    .await
    .expect("ok");
    let mut evs = Vec::new();
    while let Some(e) = rx.recv().await {
        evs.push(e);
    }
    check_invariants(&evs);
    assert_eq!(failed(&evs).1.code, "cancelled");
}

#[tokio::test]
async fn cancel_while_rows_are_streaming_with_backpressure() {
    let d = require_pg!();
    let h = connect(&d).await;
    let cancel = CancellationToken::new();
    // Tiny channel and nobody reading yet: the engine must block on send, and still be cancellable.
    let (tx, mut rx) = mpsc::channel(1);
    let task = {
        let h = h.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            h.execute_stream(
                "q".into(),
                "SELECT g FROM (SELECT generate_series(1, 100000000) AS g) s",
                ExecOptions {
                    page_size: 10,
                    ..Default::default()
                },
                cancel,
                tx,
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    cancel.cancel();
    let t0 = Instant::now();
    let mut last = None;
    while let Some(e) = rx.recv().await {
        last = Some(e);
    }
    task.await.expect("join").expect("ok");
    assert!(t0.elapsed() < Duration::from_secs(3));
    assert!(
        matches!(last, Some(QueryEvent::Failed { ref error, .. }) if error.code == "cancelled"),
        "{last:?}"
    );
}

#[tokio::test]
async fn dropping_the_receiver_stops_the_query_and_returns_ok() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let (tx, rx) = mpsc::channel(1);
    let task = {
        let h = h.clone();
        tokio::spawn(async move {
            h.execute_stream(
                "q".into(),
                "SELECT g FROM (SELECT generate_series(1, 100000000) AS g) s",
                ExecOptions {
                    page_size: 10,
                    ..Default::default()
                },
                CancellationToken::new(),
                tx,
            )
            .await
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(rx);
    let res = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("must stop")
        .expect("join");
    assert!(res.is_ok());
    // Even the abandoned connection does not wedge the pool.
    assert_eq!(scalar(&h, "1::int4").await, json!(1));
}

#[tokio::test]
async fn execute_stream_errs_only_when_started_cannot_be_sent() {
    let d = require_pg!();
    let h = connect(&d).await;
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    let res = h
        .execute_stream(
            "q".into(),
            "SELECT 1",
            ExecOptions::default(),
            CancellationToken::new(),
            tx,
        )
        .await;
    assert!(res.is_err());
}

// ---- misc ----------------------------------------------------------------------------------

#[tokio::test]
async fn empty_and_comment_only_scripts_finish_without_statements() {
    let d = require_pg!();
    let h = connect(&d).await;
    for sql in ["", "  ;  ; ", "-- nothing here\n"] {
        let evs = run(&h, sql, ExecOptions::default()).await;
        assert!(
            matches!(
                &evs[0],
                QueryEvent::Started {
                    statement_count: 0,
                    ..
                }
            ),
            "{sql:?}: {evs:?}"
        );
        assert_finished(&evs);
    }
    let r = h
        .execute_collect("", ExecOptions::default())
        .await
        .expect("empty collect");
    assert!(r.rows.is_empty());
}

#[tokio::test]
async fn concurrent_queries_share_the_pool() {
    let d = require_pg!();
    let h = connect(&d).await;
    let jobs = (0..12).map(|i| {
        let h = h.clone();
        async move {
            h.execute_collect(
                &format!("SELECT {i}::int4, pg_sleep(0.05)"),
                ExecOptions::default(),
            )
            .await
            .map(|r| r.rows[0][0].clone())
        }
    });
    let out = futures_util::future::join_all(jobs).await;
    for (i, r) in out.into_iter().enumerate() {
        assert_eq!(r.expect("query"), json!(i));
    }
}

// ---- simple-protocol fallback ----------------------------------------------------------------

async fn run_unsplit(h: &PgHandle, text: &str, opts: ExecOptions) -> Vec<QueryEvent> {
    let (tx, mut rx) = mpsc::channel(8);
    let exec = h.execute_statements(
        "q".into(),
        vec![text.to_string()],
        opts,
        CancellationToken::new(),
        tx,
    );
    let collect = async {
        let mut v = Vec::new();
        while let Some(e) = rx.recv().await {
            v.push(e);
        }
        v
    };
    let (r, evs) = tokio::join!(exec, collect);
    r.expect("execute_statements");
    check_invariants(&evs);
    evs
}

#[tokio::test]
async fn unsplit_multi_command_text_falls_back_to_text_results() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TABLE $S.t (a int)").await;
    let h = connect_single(&d).await;
    let evs = run_unsplit(&h, &fx.q("INSERT INTO $S.t VALUES (1), (2); SELECT a, a * 10 AS b FROM $S.t ORDER BY a; SELECT 'last' AS tail, NULL::int AS n"), ExecOptions::default()).await;
    assert_finished(&evs);
    match &evs[1] {
        QueryEvent::Columns { columns, .. } => assert_eq!(
            columns
                .iter()
                .map(|c| (c.name.as_str(), c.type_name.as_str()))
                .collect::<Vec<_>>(),
            [("tail", "text"), ("n", "text")]
        ),
        other => panic!("{other:?}"),
    }
    match &evs[2] {
        // Last result set only; values are text, NULL stays null.
        QueryEvent::Rows { rows, .. } => assert_eq!(rows, &vec![vec![json!("last"), Value::Null]]),
        other => panic!("{other:?}"),
    }
    // Both inserted rows landed, and the pooled connection is clean (autocommit).
    let r = h
        .execute_collect(&fx.q("SELECT count(*) FROM $S.t"), ExecOptions::default())
        .await
        .expect("count");
    assert_eq!(r.rows[0][0], json!("2"));

    // DML-only multi-command text reports the last command's row count.
    let evs = run_unsplit(
        &h,
        &fx.q("UPDATE $S.t SET a = a + 1; DELETE FROM $S.t WHERE a = 2"),
        ExecOptions::default(),
    )
    .await;
    assert!(
        matches!(
            dones(&evs)[0],
            QueryEvent::StatementDone {
                rows_affected: Some(1),
                row_count: 0,
                ..
            }
        ),
        "{evs:?}"
    );

    // BEGIN inside the text without COMMIT does not leak into the pool.
    assert_finished(
        &run_unsplit(
            &h,
            &fx.q("BEGIN; INSERT INTO $S.t VALUES (99)"),
            ExecOptions::default(),
        )
        .await,
    );
    let r = h
        .execute_collect(
            &fx.q("SELECT count(*) FROM $S.t WHERE a = 99"),
            ExecOptions::default(),
        )
        .await
        .expect("count");
    assert_eq!(r.rows[0][0], json!("0"));

    // A failure in the middle reports statement 0 and leaves the connection usable.
    let evs = run_unsplit(
        &h,
        "SELECT 1; SELECT * FROM missing_table_xyz",
        ExecOptions::default(),
    )
    .await;
    assert_eq!(failed(&evs).0, Some(0));
    assert_eq!(scalar(&h, "1::int4").await, json!(1));
}

#[tokio::test]
async fn read_only_never_uses_the_simple_protocol_fallback() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TABLE $S.t (a int)").await;
    let h = connect_single(&d).await;
    let evs = run_unsplit(
        &h,
        &fx.q("SELECT 1; COMMIT; INSERT INTO $S.t VALUES (1)"),
        ExecOptions {
            read_only: true,
            ..Default::default()
        },
    )
    .await;
    let (_, err) = failed(&evs);
    assert!(err.message.contains("multiple commands"), "{}", err.message);
    let r = h
        .execute_collect(&fx.q("SELECT count(*) FROM $S.t"), ExecOptions::default())
        .await
        .expect("count");
    assert_eq!(r.rows[0][0], json!("0"));
}

// ---- read-only escape via CR-terminated comments -------------------------------------------

/// `to_regclass` through an independent connection: does the table still exist, and how many rows?
async fn table_state(d: &Dsn, table: &str) -> (bool, Option<i64>) {
    let c = raw(d).await;
    let exists: bool = c
        .query_one(
            "SELECT to_regclass($1::text) IS NOT NULL",
            &[&table.to_string()],
        )
        .await
        .expect("to_regclass")
        .get(0);
    if !exists {
        return (false, None);
    }
    let n: i64 = c
        .query_one(&format!("SELECT count(*) FROM {table}"), &[])
        .await
        .expect("count")
        .get(0);
    (true, Some(n))
}

/// PostgreSQL ends a `--` comment at `\r` as well as `\n`. The reviewer's probes hid a `COMMIT`
/// and a `DROP TABLE` behind such comments and committed out of the READ ONLY transaction.
#[tokio::test]
async fn cr_terminated_comments_cannot_commit_out_of_read_only() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let setup = raw(&d).await;
    setup
        .batch_execute(
            "DROP TABLE IF EXISTS probe_victim; CREATE TABLE probe_victim (id int); \
             INSERT INTO probe_victim VALUES (1)",
        )
        .await
        .expect("fixture");
    let ro = ExecOptions {
        read_only: true,
        ..Default::default()
    };
    let cases = [
        // The exact probe.
        "--x\rCOMMIT /*\nSHOW x */;\n--x\rDROP TABLE probe_victim /*\nSHOW x */",
        "--x\rCOMMIT /*\nEXPLAIN SELECT 1 */;\n--x\rINSERT INTO probe_victim VALUES (2) /*\nEXPLAIN SELECT 1 */",
        "select 1 --\r; commit; drop table probe_victim",
        "\x0bCOMMIT;\n\x0bDROP TABLE probe_victim",
        "--x\rEND;\n--x\rDROP TABLE probe_victim",
        "--x\rROLLBACK;\n--x\rINSERT INTO probe_victim VALUES (3)",
        "/* a */--x\rBEGIN READ WRITE;\n--x\rINSERT INTO probe_victim VALUES (4)",
        "--x\rSET TRANSACTION READ WRITE;\n--x\rINSERT INTO probe_victim VALUES (5)",
    ];
    for sql in cases {
        // execute_stream
        let evs = run(&h, sql, ro.clone()).await;
        let (_, err) = failed(&evs);
        assert!(
            ["policy_denied", "database"].contains(&err.code.as_str()),
            "{sql:?}: {err:?}"
        );
        assert_eq!(
            table_state(&d, "probe_victim").await,
            (true, Some(1)),
            "stream escaped via {sql:?}"
        );
        // execute_collect
        let err = h
            .execute_collect(sql, ro.clone())
            .await
            .expect_err("must not succeed");
        assert!(
            matches!(err, CoreError::PolicyDenied(_) | CoreError::Database(_)),
            "{sql:?}: {err:?}"
        );
        assert_eq!(
            table_state(&d, "probe_victim").await,
            (true, Some(1)),
            "collect escaped via {sql:?}"
        );
    }
    // The first probe is refused by policy, not merely by a syntax accident.
    let evs = run(&h, cases[0], ro.clone()).await;
    let (idx, err) = failed(&evs);
    assert_eq!(
        (idx, err.code.as_str()),
        (Some(0), "policy_denied"),
        "{err:?}"
    );
    assert!(matches!(
        h.execute_collect(cases[0], ro).await,
        Err(CoreError::PolicyDenied(_))
    ));
    // The pooled connection is still usable and read-write again for normal runs.
    assert_eq!(
        scalar(&h, "current_setting('transaction_read_only')").await,
        json!("off")
    );
    setup
        .batch_execute("DROP TABLE probe_victim")
        .await
        .expect("cleanup");
}

// ---- dropped queries and pooled-connection hygiene -----------------------------------------

async fn backend_pid(h: &PgHandle) -> String {
    scalar(h, "pg_backend_pid()::text")
        .await
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Waits until no backend is running a statement containing `needle` (the cancel request that the
/// drop guard sends is asynchronous).
async fn wait_until_not_running(d: &Dsn, needle: &str) {
    let c = raw(d).await;
    let pattern = format!("%{needle}%");
    for _ in 0..50 {
        let n: i64 = c
            .query_one(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE pid <> pg_backend_pid() AND state = 'active' AND query LIKE $1",
                &[&pattern],
            )
            .await
            .expect("pg_stat_activity")
            .get(0);
        if n == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("a statement containing {needle:?} is still running");
}

/// Shared assertions after an agent-style write script was abandoned inside `BEGIN`.
async fn assert_pool_recovered(d: &Dsn, h: &PgHandle, fx: &Fixture, victim_pid: &str, sleep: &str) {
    // The abandoned backend is told to stop, not left sleeping for nobody.
    wait_until_not_running(d, sleep).await;

    // (i) The next autocommit insert really commits (it used to report rows_affected=1 and then
    // vanish with the leftover transaction).
    let r = h
        .execute_collect(&fx.q("INSERT INTO $S.t VALUES (2)"), ExecOptions::default())
        .await
        .expect("ui insert");
    assert_eq!(r.rows_affected, Some(1));
    let c = raw(d).await;
    let rows = c
        .query(&fx.q("SELECT a FROM $S.t ORDER BY a"), &[])
        .await
        .expect("rows");
    let seen: Vec<i32> = rows.iter().map(|r| r.get(0)).collect();
    assert_eq!(seen, [2], "only the UI insert may be committed");

    // (ii) A read-only run is a fresh read-only transaction, not the leftover read-write one.
    let ro = ExecOptions {
        read_only: true,
        ..Default::default()
    };
    let r = h
        .execute_collect(&fx.q("SELECT count(*)::text FROM $S.t"), ro.clone())
        .await
        .expect("ro count");
    assert_eq!(
        r.rows[0][0],
        json!("1"),
        "RO read saw uncommitted leftovers"
    );
    let err = h
        .execute_collect(&fx.q("INSERT INTO $S.t VALUES (3)"), ro)
        .await
        .expect_err("read-only write must fail");
    assert!(
        matches!(&err, CoreError::Database(m) if m.contains("read-only")),
        "{err:?}"
    );

    // The poisoned connection was discarded, not recycled.
    assert_ne!(backend_pid(h).await, victim_pid);
    assert_eq!(
        scalar(h, "current_setting('transaction_read_only')").await,
        json!("off")
    );
}

#[tokio::test]
async fn dropped_collect_inside_a_transaction_does_not_poison_the_pool() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TABLE $S.t (a int)").await;
    let h = connect_single(&d).await;
    let victim_pid = backend_pid(&h).await;
    let sleep = "pg_sleep(29.101)";
    let sql = fx.q(&format!(
        "begin; insert into $S.t values (1); select {sleep}"
    ));
    let r = tokio::time::timeout(
        Duration::from_millis(800),
        h.execute_collect(&sql, ExecOptions::default()),
    )
    .await;
    assert!(r.is_err(), "the query must still be running when dropped");
    assert_pool_recovered(&d, &h, &fx, &victim_pid, sleep).await;
}

#[tokio::test]
async fn aborted_stream_inside_a_transaction_does_not_poison_the_pool() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TABLE $S.t (a int)").await;
    let h = connect_single(&d).await;
    let victim_pid = backend_pid(&h).await;
    let sleep = "pg_sleep(29.202)";
    let sql = fx.q(&format!(
        "begin; insert into $S.t values (1); select {sleep}"
    ));
    let (tx, mut rx) = mpsc::channel(8);
    let task = {
        let h = h.clone();
        tokio::spawn(async move {
            h.execute_stream(
                "q".into(),
                &sql,
                ExecOptions::default(),
                CancellationToken::new(),
                tx,
            )
            .await
        })
    };
    // Wait until the script is inside the sleep (two statements done), then kill the task.
    let mut done = 0;
    while done < 2 {
        match rx.recv().await {
            Some(QueryEvent::StatementDone { .. }) => done += 1,
            Some(_) => {}
            None => panic!("stream ended early"),
        }
    }
    task.abort();
    let _ = task.await;
    assert_pool_recovered(&d, &h, &fx, &victim_pid, sleep).await;
}

#[tokio::test]
async fn dropped_read_only_collect_does_not_poison_the_pool() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TABLE $S.t (a int)").await;
    let h = connect_single(&d).await;
    let victim_pid = backend_pid(&h).await;
    let ro = ExecOptions {
        read_only: true,
        ..Default::default()
    };
    let r = tokio::time::timeout(
        Duration::from_millis(600),
        h.execute_collect("select pg_sleep(29.303)", ro),
    )
    .await;
    assert!(r.is_err());
    wait_until_not_running(&d, "pg_sleep(29.303)").await;
    assert_ne!(backend_pid(&h).await, victim_pid);
    // A normal write works: it is not running inside the abandoned READ ONLY transaction.
    h.execute_collect(&fx.q("INSERT INTO $S.t VALUES (1)"), ExecOptions::default())
        .await
        .expect("write after dropped read-only run");
    assert_eq!(table_state(&d, &fx.q("$S.t")).await, (true, Some(1)));
}

#[tokio::test]
async fn failed_script_with_its_own_begin_leaves_a_clean_connection() {
    let d = require_pg!();
    let fx = Fixture::new(&d, "CREATE TABLE $S.t (a int)").await;
    let h = connect_single(&d).await;
    let pid = backend_pid(&h).await;
    let evs = run(
        &h,
        &fx.q("BEGIN; INSERT INTO $S.t VALUES (1); SELECT 1/0"),
        ExecOptions::default(),
    )
    .await;
    assert_eq!(failed(&evs).0, Some(2));
    // Same connection, nothing left over: a write commits and a read-only run is read-only.
    h.execute_collect(&fx.q("INSERT INTO $S.t VALUES (2)"), ExecOptions::default())
        .await
        .expect("insert");
    assert_eq!(
        backend_pid(&h).await,
        pid,
        "expected the connection to be reused"
    );
    assert_eq!(table_state(&d, &fx.q("$S.t")).await, (true, Some(1)));
}

/// What a user script changes on its session must not be seen by the next checkout.
#[tokio::test]
async fn session_state_does_not_leak_between_checkouts() {
    let d = require_pg!();
    let fx = Fixture::new(
        &d,
        "CREATE TYPE $S.mood AS ENUM ('sad','ok'); CREATE TABLE $S.t (m $S.mood)",
    )
    .await;
    let h = connect_single(&d).await;
    let pid = backend_pid(&h).await;
    let default_path = scalar(&h, "current_setting('search_path')").await;

    h.execute_collect(
        "SET search_path = pg_catalog; SET statement_timeout = 12345; \
         SET application_name = 'leaked'; CREATE TEMP TABLE vg_leak (a int); \
         PREPARE vg_leak_p AS SELECT 1; SET ROLE postgres",
        ExecOptions::default(),
    )
    .await
    .expect("script that dirties the session");

    // Same physical connection, clean session.
    assert_eq!(
        backend_pid(&h).await,
        pid,
        "expected the connection to be reused"
    );
    assert_eq!(
        scalar(&h, "current_setting('search_path')").await,
        default_path
    );
    assert_eq!(
        scalar(&h, "current_setting('application_name')").await,
        json!(PgOptions::default().application_name)
    );
    assert_eq!(
        scalar(&h, "to_regclass('pg_temp.vg_leak') IS NULL").await,
        json!(true)
    );
    assert_eq!(
        scalar(
            &h,
            "(SELECT count(*) FROM pg_prepared_statements WHERE name = 'vg_leak_p')::int"
        )
        .await,
        json!(0)
    );
    // Read-only runs are reset too.
    let r = h
        .execute_collect(
            "SELECT current_setting('search_path')",
            ExecOptions {
                read_only: true,
                ..Default::default()
            },
        )
        .await
        .expect("ro");
    assert_eq!(r.rows[0][0], default_path);

    // The driver's own cached statements (custom-type look-ups) must survive the reset: querying
    // an enum on a recycled connection used to fail with "prepared statement does not exist".
    for _ in 0..3 {
        assert_eq!(scalar(&h, &fx.q("'ok'::$S.mood")).await, json!("ok"));
        assert_eq!(
            scalar(&h, &fx.q("ARRAY['sad','ok']::$S.mood[]")).await,
            json!(["sad", "ok"])
        );
    }
    assert_eq!(backend_pid(&h).await, pid);
}

/// Session-level advisory locks survive `ROLLBACK`; the session reset releases them as soon as the
/// run is over (in the background), not whenever the connection is next used.
#[tokio::test]
async fn advisory_locks_are_released_when_the_run_ends() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let key = i64::from(uuid::Uuid::new_v4().as_u128() as u32);
    let pid = backend_pid(&h).await;
    // The lock is really held by the pooled session while the script runs.
    let other = raw(&d).await;
    let try_lock = format!("SELECT pg_try_advisory_lock({key})");
    let r = h
        .execute_collect(
            &format!(
                "SELECT pg_advisory_lock({key}); ROLLBACK; \
                 SELECT count(*)::text FROM pg_locks WHERE locktype = 'advisory' AND pid = pg_backend_pid()"
            ),
            ExecOptions::default(),
        )
        .await
        .expect("lock");
    assert_eq!(r.rows[0][0], json!("1"), "ROLLBACK must not release it");
    // Once the run is over the lock is gone: poll (the reset runs in the background).
    let mut free = false;
    for _ in 0..50 {
        let got: bool = other.query_one(&try_lock, &[]).await.expect("try").get(0);
        if got {
            free = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(free, "advisory lock leaked past the end of the run");
    other
        .batch_execute(&format!("SELECT pg_advisory_unlock({key})"))
        .await
        .expect("unlock");
    assert_eq!(
        backend_pid(&h).await,
        pid,
        "expected the connection to be reused"
    );
}

/// Cost of the per-checkout reset. Run with `--ignored --nocapture`.
#[tokio::test]
#[ignore = "benchmark"]
async fn recycle_cost() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let raw = raw(&d).await;
    let n = 300;
    let t = Instant::now();
    for _ in 0..n {
        h.execute_collect("SELECT 1", ExecOptions::default())
            .await
            .expect("q");
    }
    let pooled = t.elapsed() / n;
    let t = Instant::now();
    for _ in 0..n {
        raw.simple_query("SELECT 1").await.expect("q");
    }
    eprintln!(
        "execute_collect(SELECT 1) incl. reset: {pooled:?}; bare simple_query: {:?}",
        t.elapsed() / n
    );
}

// ---- connection resilience and capitec/pg-proxy ------------------------------------------------

fn shunt_params(d: &Dsn) -> PgConnectParams {
    let mut p = params(d, TlsMode::Require);
    p.options.max_pool_size = 1;
    p.options.pgproxy = Some(PgProxyOptions {
        mode: PgProxyMode::Shunt,
        health_url: None,
    });
    p
}

#[tokio::test]
async fn released_sessions_reopen_on_the_next_query() {
    let d = require_pg!();
    let h = connect(&d).await;
    assert_eq!(scalar(&h, "1").await, json!(1));
    assert!(h.open_sessions() >= 1);
    // The session reset after a run happens in the background; let the connection return.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(h.release_idle_connections() >= 1);
    assert_eq!(h.open_sessions(), 0);
    assert_eq!(scalar(&h, "2").await, json!(2));
    assert!(h.open_sessions() >= 1);
}

#[tokio::test]
async fn a_pooled_connection_killed_by_the_server_is_replaced_transparently() {
    let d = require_pg!();
    let h = connect_single(&d).await;
    let pid = scalar(&h, "pg_backend_pid()").await;
    let pid = pid.as_i64().expect("pid");
    raw(&d)
        .await
        .execute("SELECT pg_terminate_backend($1)", &[&(pid as i32)])
        .await
        .expect("terminate");
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Neither the editor nor the agent sees "connection closed": the run goes to a new session.
    let evs = run(&h, "SELECT 7 AS n", ExecOptions::default()).await;
    assert_finished(&evs);
    let new_pid = scalar(&h, "pg_backend_pid()").await;
    assert_ne!(new_pid, json!(pid));
}

#[tokio::test]
async fn pgproxy_block_errors_are_policy_denials() {
    let d = require_pg!();
    let h = connect(&d).await;
    // What pg-proxy sends for a blocked query: P0001 "Query blocked by SQL Engineer: ...".
    let evs = run(
        &h,
        "DO $$ BEGIN RAISE EXCEPTION 'Query blocked by SQL Engineer: sequential scan on a large table'; END $$",
        ExecOptions::default(),
    )
    .await;
    let (_, err) = failed(&evs);
    assert_eq!(err.code, "policy_denied", "{err:?}");
    assert!(err.message.contains("sequential scan"), "{err:?}");
    // A user's own RAISE EXCEPTION is still a database error.
    let evs = run(
        &h,
        "DO $$ BEGIN RAISE EXCEPTION 'something else'; END $$",
        ExecOptions::default(),
    )
    .await;
    assert_eq!(failed(&evs).1.code, "database");
}

/// Review finding H2 in shunt mode: the after-statement check and the closing `SET` are queries
/// on the same connection too, so a truncated result must not be waited for there either.
#[tokio::test]
async fn shunt_mode_read_only_truncates_without_draining_the_stream() {
    let d = require_pg!();
    let mut p = shunt_params(&d);
    p.options.max_pool_size = 1;
    let h = connect_with(&d, p).await;
    let opts = || ExecOptions {
        max_rows: 5,
        read_only: true,
        ..Default::default()
    };
    let t0 = Instant::now();
    let evs = tokio::time::timeout(TRUNCATED_RUN_BUDGET * 2, run(&h, HUGE_RESULT, opts()))
        .await
        .unwrap_or_else(|_| panic!("still running after {:?}", t0.elapsed()));
    assert!(
        t0.elapsed() < TRUNCATED_RUN_BUDGET,
        "took {:?}",
        t0.elapsed()
    );
    assert!(matches!(
        dones(&evs)[0],
        QueryEvent::StatementDone {
            row_count: 5,
            truncated: true,
            ..
        }
    ));
    assert_finished(&evs);

    let script = format!("{HUGE_RESULT}; SELECT 42");
    let t0 = Instant::now();
    let evs = tokio::time::timeout(TRUNCATED_RUN_BUDGET * 2, run(&h, &script, opts()))
        .await
        .unwrap_or_else(|_| panic!("still running after {:?}", t0.elapsed()));
    assert!(
        t0.elapsed() < TRUNCATED_RUN_BUDGET,
        "took {:?}",
        t0.elapsed()
    );
    let (idx, err) = failed(&evs);
    assert_eq!(idx, Some(1));
    assert!(err.message.contains("statement 2 was not run"), "{err:?}");

    // The next run gets a fresh session: not read-only by a default the closed one left behind.
    let r = h
        .execute_collect(
            "SELECT current_setting('default_transaction_read_only')",
            ExecOptions::default(),
        )
        .await
        .expect("run after a truncated one");
    assert_eq!(r.rows, vec![vec![json!("off")]]);
}

#[tokio::test]
async fn shunt_mode_read_only_needs_no_transactions_and_cannot_be_escaped() {
    let d = require_pg!();
    let fx = Fixture::new(
        &d,
        "CREATE TABLE $S.t (a int); INSERT INTO $S.t VALUES (1); \
         CREATE PROCEDURE $S.p() LANGUAGE plpgsql AS $p$ BEGIN COMMIT; END $p$",
    )
    .await;
    let h = connect_with(&d, shunt_params(&d)).await;
    let ro = ExecOptions {
        read_only: true,
        ..Default::default()
    };
    let count = |h: Arc<PgHandle>, sql: String| async move {
        h.execute_collect(&sql, ExecOptions::default())
            .await
            .expect("count")
            .rows[0][0]
            .clone()
    };

    let evs = run(&h, &fx.q("SELECT a FROM $S.t; SELECT 2"), ro.clone()).await;
    assert_eq!(row_pages(&evs), [1, 1]);
    assert_finished(&evs);

    for sql in [
        "INSERT INTO $S.t VALUES (2)",
        "UPDATE $S.t SET a = 5",
        "CREATE TABLE $S.x (b int)",
    ] {
        let (idx, err) = failed(&run(&h, &fx.q(sql), ro.clone()).await);
        assert_eq!(idx, Some(0), "{sql}");
        assert!(err.message.contains("read-only"), "{sql}: {err:?}");
    }
    for sql in [
        "SET default_transaction_read_only = off; INSERT INTO $S.t VALUES (2)",
        "SELECT set_config('default_transaction_read_only', 'off', false); INSERT INTO $S.t VALUES (2)",
        "BEGIN READ WRITE; INSERT INTO $S.t VALUES (2)",
        "CALL $S.p(); INSERT INTO $S.t VALUES (2)",
        "DO $x$ BEGIN COMMIT; END $x$; INSERT INTO $S.t VALUES (2)",
    ] {
        let (_, err) = failed(&run(&h, &fx.q(sql), ro.clone()).await);
        assert_eq!(err.code, "policy_denied", "{sql}: {err:?}");
        assert_eq!(
            count(h.clone(), fx.q("SELECT count(*) FROM $S.t")).await,
            json!("1"),
            "escaped via: {sql}"
        );
    }
    // The session default is back to read-write afterwards, on the one pooled connection.
    assert_eq!(
        scalar(&h, "current_setting('default_transaction_read_only')").await,
        json!("off")
    );
    assert_finished(
        &run(
            &h,
            &fx.q("INSERT INTO $S.t VALUES (3)"),
            ExecOptions::default(),
        )
        .await,
    );
}
