//! Integration tests for `ValkeyHandle` against a real Redis/Valkey.
//!
//! Skipped unless `VG_TEST_VALKEY` is set, e.g. `VG_TEST_VALKEY=redis://127.0.0.1:6379`.
//! Only logical databases 14 and 15 are touched; both are flushed at the start and end of
//! every test. Tests serialise on a global lock because they share those databases.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::FutureExt;
use secrecy::SecretString;
use serde_json::{Value as Json, json};
use tokio::sync::{Mutex, MutexGuard};
use vibelens_core::CoreError;
use vibelens_core::model::*;
use vibelens_core::ports::CredentialFn;
use vibelens_core::valkey::{ValkeyConnectParams, ValkeyHandle};

const DB_A: u8 = 14;
const DB_B: u8 = 15;

static LOCK: Mutex<()> = Mutex::const_new(());

struct Env {
    url: url::Url,
    calls: Arc<AtomicUsize>,
}

fn env() -> Option<Env> {
    let raw = std::env::var("VG_TEST_VALKEY")
        .ok()
        .filter(|s| !s.is_empty())?;
    Some(Env {
        url: url::Url::parse(&raw).expect("VG_TEST_VALKEY must be a URL"),
        calls: Arc::new(AtomicUsize::new(0)),
    })
}

impl Env {
    fn params(&self) -> ValkeyConnectParams {
        let host = self.url.host_str().unwrap_or("127.0.0.1").to_owned();
        ValkeyConnectParams {
            tls_server_name: host.clone(),
            host,
            port: self.url.port().unwrap_or(6379),
            tls: TlsConfig {
                mode: TlsMode::Disable,
                ca_cert_path: None,
                use_rds_bundle: false,
            },
            options: ValkeyOptions::default(),
        }
    }

    fn creds(&self) -> CredentialFn {
        let user = Some(self.url.username().to_owned()).filter(|u| !u.is_empty());
        let pass = self
            .url
            .password()
            .map(|p| SecretString::from(p.to_owned()));
        let calls = self.calls.clone();
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            let creds = ResolvedCredentials {
                username: user.clone(),
                password: pass.clone(),
                ..ResolvedCredentials::anonymous()
            };
            async move { Ok(creds) }.boxed()
        })
    }

    async fn connect(&self) -> Arc<ValkeyHandle> {
        ValkeyHandle::connect(self.params(), self.creds())
            .await
            .expect("connect")
    }

    /// An independent raw client on `db`, for fixtures the engine cannot create itself
    /// (binary values) and for killing connections.
    async fn raw(&self, db: u8) -> redis::aio::MultiplexedConnection {
        let host = self.url.host_str().unwrap_or("127.0.0.1");
        let port = self.url.port().unwrap_or(6379);
        redis::Client::open(format!("redis://{host}:{port}/{db}"))
            .expect("client")
            .get_multiplexed_async_connection()
            .await
            .expect("raw connection")
    }
}

/// Takes the global lock, connects and flushes db 14/15.
async fn setup() -> Option<(MutexGuard<'static, ()>, Env, Arc<ValkeyHandle>)> {
    let env = env()?;
    let guard = LOCK.lock().await;
    let h = env.connect().await;
    flush(&h).await;
    Some((guard, env, h))
}

async fn flush(h: &ValkeyHandle) {
    for db in [DB_A, DB_B] {
        h.command(db, argv(&["FLUSHDB"]), true)
            .await
            .expect("flushdb");
    }
}

fn argv(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

async fn run(h: &ValkeyHandle, db: u8, a: &[&str]) -> Json {
    match h
        .command(db, argv(a), true)
        .await
        .unwrap_or_else(|e| panic!("{a:?}: {e}"))
    {
        CommandOutcome::Ok { result, .. } => result,
        other => panic!("{a:?}: unexpected {other:?}"),
    }
}

macro_rules! fixture {
    () => {
        match setup().await {
            Some(f) => f,
            None => {
                eprintln!("VG_TEST_VALKEY not set; skipping");
                return;
            }
        }
    };
}

#[tokio::test]
async fn connects_without_auth_and_reports_server_info() {
    let (_g, _env, h) = fixture!();
    let info = h.server_info().await.unwrap();
    let version = info.version.expect("version");
    assert!(
        version.chars().next().is_some_and(|c| c.is_ascii_digit()),
        "{version}"
    );
    assert!(
        matches!(
            info.mode.as_deref(),
            Some("standalone" | "cluster" | "sentinel")
        ),
        "{:?}",
        info.mode
    );
    assert!(
        matches!(info.role.as_deref(), Some("master" | "slave" | "replica")),
        "{:?}",
        info.role
    );
    assert!(info.used_memory_human.is_some());
    assert!(info.extra.contains_key("tcp_port"));
    assert!(
        info.extra.contains_key("connected_slaves") || info.extra.contains_key("master_replid")
    );
    flush(&h).await;
}

#[tokio::test]
async fn list_dbs_counts_keys() {
    let (_g, _env, h) = fixture!();
    run(&h, DB_A, &["SET", "k1", "v"]).await;
    run(&h, DB_A, &["SET", "k2", "v"]).await;
    run(&h, DB_B, &["SET", "k1", "v"]).await;
    let dbs = h.list_dbs().await.unwrap();
    assert!(dbs.len() >= 16, "{}", dbs.len());
    assert!(
        dbs.windows(2).all(|w| w[0].index + 1 == w[1].index),
        "indexes are contiguous"
    );
    let count = |i: u8| dbs.iter().find(|d| d.index == i).and_then(|d| d.keys);
    assert_eq!(count(DB_A), Some(2));
    assert_eq!(count(DB_B), Some(1));
    flush(&h).await;
}

#[tokio::test]
async fn scan_pages_through_1200_keys_exactly_once_with_types() {
    let (_g, env, h) = fixture!();
    // Build the fixture through a raw pipeline (fast), then read it back through the engine.
    let mut raw = env.raw(DB_A).await;
    let mut p = redis::pipe();
    let mut expected: HashMap<String, KeyType> = HashMap::new();
    for i in 0..1000 {
        p.cmd("SET").arg(format!("str:{i:04}")).arg("v").ignore();
        expected.insert(format!("str:{i:04}"), KeyType::String);
    }
    for i in 0..100 {
        p.cmd("HSET")
            .arg(format!("hash:{i:03}"))
            .arg("f")
            .arg("v")
            .ignore();
        expected.insert(format!("hash:{i:03}"), KeyType::Hash);
    }
    for i in 0..50 {
        p.cmd("RPUSH").arg(format!("list:{i:03}")).arg("a").ignore();
        expected.insert(format!("list:{i:03}"), KeyType::List);
    }
    for i in 0..30 {
        p.cmd("SADD").arg(format!("set:{i:03}")).arg("a").ignore();
        expected.insert(format!("set:{i:03}"), KeyType::Set);
    }
    for i in 0..10 {
        p.cmd("ZADD")
            .arg(format!("zset:{i:03}"))
            .arg(1)
            .arg("a")
            .ignore();
        expected.insert(format!("zset:{i:03}"), KeyType::Zset);
    }
    for i in 0..10 {
        p.cmd("XADD")
            .arg(format!("stream:{i:03}"))
            .arg("*")
            .arg("f")
            .arg("v")
            .ignore();
        expected.insert(format!("stream:{i:03}"), KeyType::Stream);
    }
    p.query_async::<()>(&mut raw).await.unwrap();
    assert_eq!(expected.len(), 1200);

    let mut seen: Vec<KeyEntry> = Vec::new();
    let mut cursor = "0".to_string();
    let mut pages = 0;
    loop {
        let page = h.scan(DB_A, "*", &cursor, 100).await.unwrap();
        pages += 1;
        seen.extend(page.keys);
        cursor = page.cursor;
        if page.done {
            break;
        }
        assert_ne!(
            cursor, "0",
            "an unfinished scan must not hand back the start cursor"
        );
        assert!(pages < 10_000, "scan does not terminate");
    }
    assert!(
        pages > 1,
        "COUNT 100 over 1200 keys must take several pages, got {pages}"
    );

    let names: Vec<&str> = seen.iter().map(|k| k.key.as_str()).collect();
    let unique: BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(names.len(), unique.len(), "duplicate keys across pages");
    assert_eq!(unique.len(), 1200);
    for entry in &seen {
        assert_eq!(
            entry.key_type,
            Some(expected[&entry.key]),
            "type of {}",
            entry.key
        );
    }

    // MATCH narrows the result; the start cursor is accepted as "" too.
    let mut hashes = 0;
    let mut cursor = String::new();
    loop {
        let page = h.scan(DB_A, "hash:*", &cursor, 500).await.unwrap();
        assert!(
            page.keys
                .iter()
                .all(|k| k.key.starts_with("hash:") && k.key_type == Some(KeyType::Hash))
        );
        hashes += page.keys.len();
        cursor = page.cursor;
        if page.done {
            break;
        }
    }
    assert_eq!(hashes, 100);
    flush(&h).await;
}

#[tokio::test]
async fn scan_rejects_garbage_cursor_and_survives_binary_keys() {
    let (_g, env, h) = fixture!();
    assert!(matches!(
        h.scan(DB_A, "*", "not-a-cursor", 10).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(
        matches!(
            h.scan(DB_A, "*", "1:2", 10).await,
            Err(CoreError::Invalid(_))
        ),
        "cluster-style cursor on standalone"
    );

    let mut raw = env.raw(DB_A).await;
    redis::cmd("SET")
        .arg(&b"bin\xff\xfekey"[..])
        .arg("v")
        .exec_async(&mut raw)
        .await
        .unwrap();
    redis::cmd("SET")
        .arg("plain")
        .arg("v")
        .exec_async(&mut raw)
        .await
        .unwrap();
    let page = h.scan(DB_A, "*", "0", 100).await.unwrap();
    assert_eq!(page.keys.len(), 2);
    assert!(page.done);
    let bin = page
        .keys
        .iter()
        .find(|k| k.key.starts_with("bin"))
        .expect("binary key listed");
    assert!(bin.key.contains('\u{fffd}'), "lossy display: {:?}", bin.key);
    assert_eq!(
        bin.key_type,
        Some(KeyType::String),
        "TYPE must use the raw key bytes"
    );
    flush(&h).await;
}

#[tokio::test]
async fn get_value_for_every_type() {
    let (_g, env, h) = fixture!();
    run(&h, DB_A, &["SET", "s", "héllo ✓"]).await;
    run(&h, DB_A, &["HSET", "h", "a", "1", "b", "2"]).await;
    run(&h, DB_A, &["RPUSH", "l", "x", "y", "z"]).await;
    run(&h, DB_A, &["SADD", "set", "m1", "m2"]).await;
    run(&h, DB_A, &["ZADD", "z", "1.5", "one", "2", "two"]).await;
    run(&h, DB_A, &["XADD", "x", "1-1", "f1", "v1", "f2", "v2"]).await;

    let s = h.get_value(DB_A, "s", 100).await.unwrap();
    assert_eq!(s.key_type, KeyType::String);
    assert_eq!(s.value, json!("héllo ✓"));
    assert_eq!(s.size, Some("héllo ✓".len() as u64), "STRLEN counts bytes");
    assert_eq!(s.ttl_ms, -1);
    assert!(!s.truncated);

    let hv = h.get_value(DB_A, "h", 100).await.unwrap();
    assert_eq!(hv.key_type, KeyType::Hash);
    assert_eq!(hv.value, json!({"a": "1", "b": "2"}));
    assert_eq!(hv.size, Some(2));

    let l = h.get_value(DB_A, "l", 100).await.unwrap();
    assert_eq!(
        (l.key_type, l.value, l.size),
        (KeyType::List, json!(["x", "y", "z"]), Some(3))
    );

    let st = h.get_value(DB_A, "set", 100).await.unwrap();
    assert_eq!(st.key_type, KeyType::Set);
    let mut members: Vec<String> = serde_json::from_value(st.value).unwrap();
    members.sort();
    assert_eq!(members, ["m1", "m2"]);
    assert_eq!(st.size, Some(2));

    let z = h.get_value(DB_A, "z", 100).await.unwrap();
    assert_eq!(z.key_type, KeyType::Zset);
    assert_eq!(
        z.value,
        json!([{"member": "one", "score": 1.5}, {"member": "two", "score": 2.0}])
    );
    assert_eq!(z.size, Some(2));

    let x = h.get_value(DB_A, "x", 100).await.unwrap();
    assert_eq!(x.key_type, KeyType::Stream);
    assert_eq!(
        x.value,
        json!([{"id": "1-1", "fields": {"f1": "v1", "f2": "v2"}}])
    );
    assert_eq!(x.size, Some(1));

    // TTL
    run(&h, DB_A, &["SET", "ttl", "v", "PX", "100000"]).await;
    let t = h.get_value(DB_A, "ttl", 10).await.unwrap();
    assert!(t.ttl_ms > 0 && t.ttl_ms <= 100_000, "{}", t.ttl_ms);

    // Missing key
    assert!(matches!(
        h.get_value(DB_A, "nope", 10).await,
        Err(CoreError::NotFound(_))
    ));
    // Right key, wrong db
    assert!(matches!(
        h.get_value(DB_B, "s", 10).await,
        Err(CoreError::NotFound(_))
    ));

    // Binary string, binary hash value
    let mut raw = env.raw(DB_A).await;
    redis::cmd("SET")
        .arg("bin")
        .arg(&[0xffu8, 0xfe, 0x00, 0x41][..])
        .exec_async(&mut raw)
        .await
        .unwrap();
    redis::cmd("HSET")
        .arg("hbin")
        .arg("f")
        .arg(&[0x80u8, 0x81][..])
        .exec_async(&mut raw)
        .await
        .unwrap();
    let b = h.get_value(DB_A, "bin", 10).await.unwrap();
    assert_eq!(b.value, json!({"$bytes": "//4AQQ==", "len": 4}));
    assert_eq!(b.size, Some(4));
    let hb = h.get_value(DB_A, "hbin", 10).await.unwrap();
    assert_eq!(hb.value, json!({"f": {"$bytes": "gIE=", "len": 2}}));
    flush(&h).await;
}

#[tokio::test]
async fn get_value_truncates_collections_and_strings() {
    let (_g, env, h) = fixture!();
    let mut raw = env.raw(DB_A).await;
    let mut p = redis::pipe();
    for i in 0..500 {
        p.cmd("RPUSH").arg("list").arg(format!("e{i}")).ignore();
        p.cmd("HSET")
            .arg("hash")
            .arg(format!("f{i}"))
            .arg(i)
            .ignore();
        p.cmd("SADD").arg("set").arg(format!("m{i}")).ignore();
        p.cmd("ZADD")
            .arg("zset")
            .arg(i)
            .arg(format!("m{i}"))
            .ignore();
        p.cmd("XADD")
            .arg("stream")
            .arg("*")
            .arg("i")
            .arg(i)
            .ignore();
    }
    p.query_async::<()>(&mut raw).await.unwrap();
    // 1.5 MiB string, cut at the 1 MiB read limit
    redis::cmd("SET")
        .arg("big")
        .arg("a".repeat(1_572_864))
        .exec_async(&mut raw)
        .await
        .unwrap();
    // A multi-byte character straddling the cut must not turn the value into base64
    let mut straddle = "a".repeat(1_048_575);
    straddle.push('€'); // bytes 1_048_575..1_048_578
    straddle.push_str("tail");
    redis::cmd("SET")
        .arg("straddle")
        .arg(straddle)
        .exec_async(&mut raw)
        .await
        .unwrap();

    let l = h.get_value(DB_A, "list", 10).await.unwrap();
    assert!(l.truncated);
    assert_eq!(l.size, Some(500));
    assert_eq!(l.value.as_array().map(Vec::len), Some(10));
    assert_eq!(l.value[0], "e0");

    // 500 > 50: goes through HSCAN, still capped at exactly 50
    let hv = h.get_value(DB_A, "hash", 50).await.unwrap();
    assert!(hv.truncated);
    assert_eq!(hv.size, Some(500));
    assert_eq!(hv.value.as_object().map(|o| o.len()), Some(50));

    let st = h.get_value(DB_A, "set", 50).await.unwrap();
    assert!(st.truncated);
    assert_eq!(st.size, Some(500));
    assert_eq!(st.value.as_array().map(Vec::len), Some(50));

    let z = h.get_value(DB_A, "zset", 25).await.unwrap();
    assert!(z.truncated);
    assert_eq!(z.size, Some(500));
    assert_eq!(z.value.as_array().map(Vec::len), Some(25));
    assert_eq!(z.value[0]["member"], "m0");

    let x = h.get_value(DB_A, "stream", 20).await.unwrap();
    assert!(x.truncated);
    assert_eq!(x.size, Some(500));
    assert_eq!(x.value.as_array().map(Vec::len), Some(20));

    // exactly at the cap is not truncated
    let exact = h.get_value(DB_A, "list", 500).await.unwrap();
    assert!(!exact.truncated);
    assert_eq!(exact.value.as_array().map(Vec::len), Some(500));
    let hall = h.get_value(DB_A, "hash", 500).await.unwrap();
    assert!(!hall.truncated);
    assert_eq!(hall.value.as_object().map(|o| o.len()), Some(500));

    let big = h.get_value(DB_A, "big", 10).await.unwrap();
    assert!(big.truncated);
    assert_eq!(big.size, Some(1_572_864));
    assert_eq!(big.value.as_str().map(str::len), Some(1_048_576));

    let sv = h.get_value(DB_A, "straddle", 10).await.unwrap();
    assert!(sv.truncated);
    let text = sv
        .value
        .as_str()
        .expect("still text after trimming the partial character");
    assert_eq!(text.len(), 1_048_575);
    flush(&h).await;
}

#[tokio::test]
async fn is_write_command_is_server_authoritative() {
    let (_g, _env, h) = fixture!();
    let w = |a: &[&str]| {
        let a = argv(a);
        let h = h.clone();
        async move { h.is_write_command(&a).await.unwrap() }
    };
    assert!(!w(&["GET", "k"]).await);
    assert!(!w(&["get", "k"]).await, "case-insensitive");
    assert!(w(&["SET", "k", "v"]).await);
    assert!(!w(&["CONFIG", "GET", "maxmemory"]).await);
    assert!(w(&["CONFIG", "SET", "maxmemory", "0"]).await);
    assert!(w(&["FLUSHALL"]).await);
    assert!(w(&["EVAL", "return 1", "0"]).await);
    assert!(!w(&["EVAL_RO", "return 1", "0"]).await);
    assert!(w(&["FCALL", "f", "0"]).await);
    assert!(!w(&["INFO"]).await);
    assert!(!w(&["SCAN", "0"]).await);
    assert!(!w(&["KEYS", "*"]).await);
    assert!(!w(&["TTL", "k"]).await);
    assert!(!w(&["CLIENT", "LIST"]).await);
    assert!(w(&["CLIENT", "KILL", "ID", "1"]).await);
    assert!(w(&["SCRIPT", "FLUSH"]).await);
    assert!(!w(&["SCRIPT", "EXISTS", "abc"]).await);
    assert!(w(&["DEL", "k"]).await);
    assert!(w(&["PUBLISH", "c", "m"]).await);
    assert!(w(&["DEBUG", "SLEEP", "0"]).await);
    assert!(w(&["SHUTDOWN"]).await);
    // Unknown commands cannot be proven read-only
    assert!(w(&["NOSUCHCOMMAND", "x"]).await);
    assert!(w(&["CONFIG", "NOSUCHSUB"]).await);
    assert!(matches!(
        h.is_write_command(&[]).await,
        Err(CoreError::Invalid(_))
    ));
    flush(&h).await;
}

#[tokio::test]
async fn command_requires_confirmation_for_writes() {
    let (_g, _env, h) = fixture!();
    match h
        .command(DB_A, argv(&["SET", "k", "v"]), false)
        .await
        .unwrap()
    {
        CommandOutcome::RequiresConfirmation { reason } => {
            assert!(reason.contains("SET"), "{reason}");
            assert!(reason.contains("write"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    // Not executed
    assert!(matches!(
        h.get_value(DB_A, "k", 10).await,
        Err(CoreError::NotFound(_))
    ));

    assert_eq!(run(&h, DB_A, &["SET", "k", "v"]).await, json!("OK"));
    assert_eq!(run(&h, DB_A, &["GET", "k"]).await, json!("v"));
    // reads never need confirmation
    match h.command(DB_A, argv(&["GET", "k"]), false).await.unwrap() {
        CommandOutcome::Ok { result, .. } => assert_eq!(result, json!("v")),
        other => panic!("{other:?}"),
    }
    // subcommand-aware
    assert!(matches!(
        h.command(DB_A, argv(&["CONFIG", "SET", "maxmemory", "0"]), false)
            .await
            .unwrap(),
        CommandOutcome::RequiresConfirmation { .. }
    ));
    assert!(matches!(
        h.command(DB_A, argv(&["CONFIG", "GET", "maxmemory"]), false)
            .await
            .unwrap(),
        CommandOutcome::Ok { .. }
    ));
    flush(&h).await;
}

#[tokio::test]
async fn command_rejects_empty_and_blocked() {
    let (_g, _env, h) = fixture!();
    assert!(matches!(
        h.command(DB_A, vec![], true).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        h.command(DB_A, argv(&[""]), true).await,
        Err(CoreError::Invalid(_))
    ));
    for blocked in [
        &["SELECT", "3"][..],
        &["select", "3"],
        &["MONITOR"],
        &["SUBSCRIBE", "ch"],
        &["PSUBSCRIBE", "ch*"],
        &["SSUBSCRIBE", "ch"],
        &["MULTI"],
        &["HELLO", "2"],
    ] {
        // even with allow_write=true
        match h.command(DB_A, argv(blocked), true).await {
            Err(CoreError::Invalid(msg)) => assert!(!msg.is_empty()),
            other => panic!("{blocked:?} should be rejected, got {other:?}"),
        }
    }
    match h.command(DB_A, argv(&["SELECT", "1"]), true).await {
        Err(CoreError::Invalid(msg)) => {
            assert!(msg.contains("UI"), "should point at the db switcher: {msg}")
        }
        other => panic!("{other:?}"),
    }
    // The db did not change underneath us.
    run(&h, DB_A, &["SET", "still-14", "1"]).await;
    assert_eq!(
        h.get_value(DB_A, "still-14", 1).await.unwrap().key,
        "still-14"
    );
    flush(&h).await;
}

#[tokio::test]
async fn databases_are_isolated() {
    let (_g, _env, h) = fixture!();
    run(&h, DB_A, &["SET", "shared-name", "in-14"]).await;
    assert_eq!(run(&h, DB_B, &["GET", "shared-name"]).await, Json::Null);
    run(&h, DB_B, &["SET", "shared-name", "in-15"]).await;
    assert_eq!(run(&h, DB_A, &["GET", "shared-name"]).await, json!("in-14"));
    assert_eq!(run(&h, DB_B, &["GET", "shared-name"]).await, json!("in-15"));
    assert_eq!(run(&h, DB_A, &["DBSIZE"]).await, json!(1));

    let a = h.scan(DB_A, "*", "0", 100).await.unwrap();
    let b = h.scan(DB_B, "*", "0", 100).await.unwrap();
    assert_eq!(a.keys.len(), 1);
    assert_eq!(b.keys.len(), 1);

    // interleaved use from concurrent tasks never crosses databases
    let mut tasks = Vec::new();
    for i in 0..40 {
        let h = h.clone();
        tasks.push(tokio::spawn(async move {
            let (db, want) = if i % 2 == 0 {
                (DB_A, "in-14")
            } else {
                (DB_B, "in-15")
            };
            let got = run(&h, db, &["GET", "shared-name"]).await;
            assert_eq!(got, json!(want));
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    flush(&h).await;
}

#[tokio::test]
async fn resp3_replies_become_json_objects() {
    let (_g, _env, h) = fixture!();
    // CONFIG GET is a map under RESP3
    let cfg = run(&h, DB_A, &["CONFIG", "GET", "maxmemory-policy"]).await;
    assert!(cfg.is_object(), "{cfg}");
    assert!(cfg["maxmemory-policy"].is_string(), "{cfg}");

    run(&h, DB_A, &["HSET", "h", "a", "1"]).await;
    assert_eq!(run(&h, DB_A, &["HGETALL", "h"]).await, json!({"a": "1"}));

    run(&h, DB_A, &["ZADD", "z", "1.5", "m"]).await;
    assert_eq!(
        run(&h, DB_A, &["ZRANGE", "z", "0", "-1", "WITHSCORES"]).await,
        json!([["m", 1.5]])
    );
    assert_eq!(run(&h, DB_A, &["ZSCORE", "z", "m"]).await, json!(1.5));

    run(&h, DB_A, &["SADD", "s", "x"]).await;
    assert_eq!(run(&h, DB_A, &["SMEMBERS", "s"]).await, json!(["x"]));

    // nested map inside a stream info reply
    run(&h, DB_A, &["XADD", "x", "1-1", "f", "v"]).await;
    let xi = run(&h, DB_A, &["XINFO", "STREAM", "x"]).await;
    assert!(xi.is_object(), "{xi}");
    assert_eq!(xi["length"], json!(1));

    // INFO is a verbatim string
    let info = run(&h, DB_A, &["INFO", "server"]).await;
    assert!(
        info.as_str()
            .is_some_and(|s| s.contains("redis_version") || s.contains("valkey_version"))
    );

    // integers, nil, status
    assert_eq!(run(&h, DB_A, &["DBSIZE"]).await, json!(4));
    assert_eq!(run(&h, DB_A, &["GET", "missing"]).await, Json::Null);
    assert_eq!(run(&h, DB_A, &["PING"]).await, json!("PONG"));

    // server errors surface as errors, not panics
    match h.command(DB_A, argv(&["GET", "h"]), true).await {
        Err(CoreError::Valkey(m)) => assert!(m.contains("WRONGTYPE"), "{m}"),
        other => panic!("{other:?}"),
    }
    // nested errors are values: EVAL returning an error table inside an array
    let nested = run(
        &h,
        DB_A,
        &["EVAL", "return {1, redis.error_reply('MY custom')}", "0"],
    )
    .await;
    assert_eq!(nested[0], json!(1));
    flush(&h).await;
}

#[tokio::test]
async fn reconnects_with_fresh_credentials_after_the_server_drops_us() {
    let (_g, env, h) = fixture!();
    let id = run(&h, DB_A, &["CLIENT", "ID"])
        .await
        .as_i64()
        .expect("client id");
    let before = env.calls.load(Ordering::SeqCst);
    assert!(before >= 1);

    // Kill only our db-14 connection, from a different connection.
    let mut raw = env.raw(DB_A).await;
    let killed: i64 = redis::cmd("CLIENT")
        .arg("KILL")
        .arg("ID")
        .arg(id)
        .query_async(&mut raw)
        .await
        .unwrap();
    assert_eq!(killed, 1);

    // A read is transparently retried on a rebuilt connection.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(run(&h, DB_A, &["GET", "after"]).await, Json::Null);
    assert!(
        env.calls.load(Ordering::SeqCst) > before,
        "CredentialFn must be consulted again on rebuild"
    );
    flush(&h).await;
}

#[tokio::test]
async fn reads_survive_a_dropped_connection_and_writes_report_it() {
    let (_g, env, h) = fixture!();
    run(&h, DB_A, &["SET", "k", "v"]).await;
    let kill = |id: i64| {
        let env_raw = env.raw(DB_A);
        async move {
            let mut raw = env_raw.await;
            let n: i64 = redis::cmd("CLIENT")
                .arg("KILL")
                .arg("ID")
                .arg(id)
                .query_async(&mut raw)
                .await
                .unwrap();
            assert_eq!(n, 1);
            // let the driver notice the EOF
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    };

    // read after kill: retried transparently
    let id = run(&h, DB_A, &["CLIENT", "ID"]).await.as_i64().unwrap();
    kill(id).await;
    assert_eq!(run(&h, DB_A, &["GET", "k"]).await, json!("v"));

    // write after kill: the error is reported (it may have been applied), and the next call works
    let id = run(&h, DB_A, &["CLIENT", "ID"]).await.as_i64().unwrap();
    kill(id).await;
    match h.command(DB_A, argv(&["SET", "k2", "v2"]), true).await {
        Err(CoreError::NotConnected(m)) => assert!(m.contains("may or may not"), "{m}"),
        // The kill can be noticed before the send, in which case the write is simply
        // retried by nobody and fails the same way; success would mean silent replay.
        Ok(_) => panic!("a write must never be silently replayed after a dropped connection"),
        Err(other) => panic!("{other:?}"),
    }
    assert_eq!(run(&h, DB_A, &["GET", "k"]).await, json!("v"));
    flush(&h).await;
}

#[tokio::test]
async fn idle_secs_and_close() {
    let (_g, _env, h) = fixture!();
    run(&h, DB_A, &["PING"]).await;
    assert!(h.idle_secs() <= 1);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(h.idle_secs() >= 1, "idle_secs = {}", h.idle_secs());
    run(&h, DB_A, &["PING"]).await;
    assert!(h.idle_secs() <= 1);

    flush(&h).await;
    h.close().await;
    assert!(matches!(
        h.command(DB_A, argv(&["PING"]), true).await,
        Err(CoreError::NotConnected(_))
    ));
    assert!(matches!(
        h.scan(DB_A, "*", "0", 10).await,
        Err(CoreError::NotConnected(_))
    ));
}

#[tokio::test]
async fn wrong_credentials_are_an_auth_error_and_unreachable_is_not_connected() {
    let Some(env) = env() else {
        eprintln!("VG_TEST_VALKEY not set; skipping");
        return;
    };
    // Dead port
    let mut p = env.params();
    p.port = 1;
    p.options.connect_timeout_secs = 3;
    let err = ValkeyHandle::connect(p, env.creds())
        .await
        .err()
        .expect("must fail");
    assert!(
        matches!(err, CoreError::NotConnected(_) | CoreError::Timeout(_)),
        "{err:?}"
    );

    // A password against a server that has none configured is rejected by the server.
    let creds: CredentialFn = Arc::new(|| {
        async {
            Ok(ResolvedCredentials {
                username: Some("nobody".into()),
                password: Some(SecretString::from("definitely-wrong".to_owned())),
                ..ResolvedCredentials::anonymous()
            })
        }
        .boxed()
    });
    match ValkeyHandle::connect(env.params(), creds).await {
        Err(CoreError::Auth(m)) => assert!(
            !m.contains("definitely-wrong"),
            "secret leaked into error: {m}"
        ),
        Err(other) => panic!("expected Auth, got {other:?}"),
        Ok(_) => panic!("auth must fail"),
    }
}

#[tokio::test]
async fn cluster_options_reject_nonzero_db_without_a_cluster() {
    // Pure option plumbing: asking for cluster mode against a standalone server fails to connect
    // cleanly (no panic, a typed error).
    let Some(env) = env() else {
        eprintln!("VG_TEST_VALKEY not set; skipping");
        return;
    };
    let mut p = env.params();
    p.options.cluster = true;
    p.options.connect_timeout_secs = 5;
    let r = ValkeyHandle::connect(p, env.creds()).await;
    assert!(r.is_err(), "a standalone server is not a cluster");
}

/// An ACL user that may not call COMMAND: `COMMAND INFO` is unusable, so the static read-only
/// table decides (allow-list: unlisted commands count as writes). Also exercises username +
/// password auth. The user is removed afterwards.
#[tokio::test]
async fn write_detection_falls_back_to_the_static_table_without_command_info() {
    let (_g, env, h) = fixture!();
    let user = "vgtest_nocommand";
    run(
        &h,
        DB_A,
        &[
            "ACL",
            "SETUSER",
            user,
            "on",
            ">pw-for-test",
            "+@all",
            "-command",
            "~*",
            "&*",
        ],
    )
    .await;

    let creds: CredentialFn = Arc::new(move || {
        async move {
            Ok(ResolvedCredentials {
                username: Some(user.into()),
                password: Some(SecretString::from("pw-for-test".to_owned())),
                ..ResolvedCredentials::anonymous()
            })
        }
        .boxed()
    });
    let restricted = ValkeyHandle::connect(env.params(), creds)
        .await
        .expect("ACL user can connect");
    let w = |a: &[&str]| {
        let a = argv(a);
        let r = restricted.clone();
        async move { r.is_write_command(&a).await.unwrap() }
    };
    assert!(!w(&["GET", "k"]).await);
    assert!(!w(&["CONFIG", "GET", "maxmemory"]).await);
    assert!(!w(&["TTL", "k"]).await);
    assert!(w(&["SET", "k", "v"]).await);
    assert!(w(&["CONFIG", "SET", "maxmemory", "0"]).await);
    assert!(w(&["FLUSHALL"]).await);
    assert!(w(&["EVAL", "return 1", "0"]).await);
    assert!(
        w(&["SOMETHING-UNKNOWN"]).await,
        "unlisted commands fail safe"
    );
    match restricted
        .command(DB_A, argv(&["SET", "k", "v"]), false)
        .await
        .unwrap()
    {
        CommandOutcome::RequiresConfirmation { reason } => {
            assert!(reason.contains("COMMAND INFO is unavailable"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    restricted.close().await;

    run(&h, DB_A, &["ACL", "DELUSER", user]).await;
    flush(&h).await;
}
