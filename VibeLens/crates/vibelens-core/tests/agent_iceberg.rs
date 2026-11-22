//! Agent loop over Apache Iceberg connections: a scripted mock LLM server (real HTTP + SSE), a
//! fake `ToolBackend` serving an Iceberg-shaped catalog and an in-memory vault. Asserts the policy
//! outcome (safe / approval / denied), the `read_only` flag the backend receives, the approval
//! card text and what the model is told, for both the embedded engine and Athena.
#![cfg(feature = "iceberg")]

mod services;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use vibelens_core::agent::AgentService;
use vibelens_core::error::{CoreError, Result};
use vibelens_core::model::*;
use vibelens_core::ports::{FileVersion, SecretVault, ToolBackend};

// ---------------------------------------------------------------------------------------------
// Mock LLM server (same shape as tests/agent_loop.rs; integration tests are separate crates)
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Recorded {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

enum Reply {
    /// 200 text/event-stream, body delivered in a few separate writes.
    Sse(String),
}

struct MockLlm {
    base_url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockLlm {
    async fn start(replies: Vec<Reply>) -> MockLlm {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (q, r) = (queue.clone(), requests.clone());
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let (q, r) = (q.clone(), r.clone());
                tokio::spawn(async move {
                    let _ = serve(sock, q, r).await;
                });
            }
        });
        MockLlm {
            base_url: format!("http://{addr}"),
            requests,
        }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

async fn serve(
    mut sock: TcpStream,
    queue: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
) -> std::io::Result<()> {
    // Read headers.
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let header_end = loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while buf.len() < header_end + len {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body: Value =
        serde_json::from_slice(&buf[header_end..header_end + len.min(buf.len() - header_end)])
            .unwrap_or(Value::Null);
    requests.lock().unwrap().push(Recorded {
        path,
        headers,
        body,
    });

    let reply = queue.lock().unwrap().pop_front();
    match reply {
        Some(Reply::Sse(body)) => {
            sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n").await?;
            // Deliver in three pieces, splitting mid-line on purpose.
            let bytes = body.as_bytes();
            let third = bytes.len() / 3;
            for part in [
                &bytes[..third],
                &bytes[third..2 * third],
                &bytes[2 * third..],
            ] {
                sock.write_all(part).await?;
                sock.flush().await?;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        None => {
            let resp = "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
            sock.write_all(resp.as_bytes()).await?;
        }
    }
    Ok(())
}

// ---- SSE builders -----------------------------------------------------------------------------

fn anthropic_sse(
    text: &str,
    calls: &[(&str, &str, Value)],
    stop: &str,
    usage: (u64, u64),
) -> String {
    let mut s = String::new();
    let mut ev = |name: &str, data: Value| s.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    ev(
        "message_start",
        json!({"type":"message_start","message":{"id":"m","role":"assistant","content":[],"usage":{"input_tokens":usage.0,"output_tokens":1}}}),
    );
    let mut idx = 0;
    if !text.is_empty() {
        ev(
            "content_block_start",
            json!({"type":"content_block_start","index":idx,"content_block":{"type":"text","text":""}}),
        );
        // Two deltas so streaming is visible.
        let mid = text
            .char_indices()
            .nth(text.chars().count() / 2)
            .map(|(i, _)| i)
            .unwrap_or(0);
        for part in [&text[..mid], &text[mid..]] {
            if !part.is_empty() {
                ev(
                    "content_block_delta",
                    json!({"type":"content_block_delta","index":idx,"delta":{"type":"text_delta","text":part}}),
                );
            }
        }
        ev(
            "content_block_stop",
            json!({"type":"content_block_stop","index":idx}),
        );
        idx += 1;
    }
    for (id, name, input) in calls {
        ev(
            "content_block_start",
            json!({"type":"content_block_start","index":idx,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
        );
        let raw = input.to_string();
        let cut = raw.len() / 2;
        let cut = (0..=cut)
            .rev()
            .find(|i| raw.is_char_boundary(*i))
            .unwrap_or(0);
        for part in [&raw[..cut], &raw[cut..]] {
            ev(
                "content_block_delta",
                json!({"type":"content_block_delta","index":idx,"delta":{"type":"input_json_delta","partial_json":part}}),
            );
        }
        ev(
            "content_block_stop",
            json!({"type":"content_block_stop","index":idx}),
        );
        idx += 1;
    }
    ev(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":usage.1}}),
    );
    ev("message_stop", json!({"type":"message_stop"}));
    s
}

// ---------------------------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct TestVault(Mutex<HashMap<String, String>>);

#[async_trait]
impl SecretVault for TestVault {
    async fn get(&self, key: &str) -> Result<Option<SecretString>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(key)
            .map(|s| SecretString::from(s.clone())))
    }
    async fn set(&self, key: &str, value: &SecretString) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert(key.to_string(), value.expose_secret().to_string());
        Ok(())
    }
    async fn delete(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
struct SqlCall {
    profile: String,
    database: Option<String>,
    sql: String,
    read_only: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct SchemaCall {
    profile: String,
    database: Option<String>,
    filter: Option<String>,
}

#[derive(Default)]
struct FakeBackend {
    profiles: Mutex<BTreeMap<String, ConnectionProfile>>,
    sql_calls: Mutex<Vec<SqlCall>>,
    schema_calls: Mutex<Vec<SchemaCall>>,
    valkey_calls: Mutex<usize>,
    /// What `describe_iceberg_table` returns; `None` = the trait default (`NotImplemented`).
    table_info: Mutex<Option<IcebergTableInfo>>,
    describe_calls: Mutex<Vec<(Vec<String>, String)>>,
}

fn iceberg_profile(
    id: &str,
    athena: bool,
    env: Environment,
    access: AiDataAccess,
    default_namespace: Option<&str>,
) -> ConnectionProfile {
    ConnectionProfile {
        id: id.into(),
        name: format!("{id}-lake"),
        engine: EngineKind::Iceberg,
        host: String::new(),
        port: 0,
        database: None,
        auth: AuthMethod::None,
        tls: Default::default(),
        tunnel: None,
        environment: env,
        ai_data_access: access,
        color: None,
        pg: Default::default(),
        valkey: Default::default(),
        created_at: None,
        updated_at: None,
        iceberg: Some(IcebergOptions {
            catalog: IcebergCatalog::Rest {
                uri: "https://secret-catalog.internal.example/api".into(),
                warehouse: Some("secret-warehouse".into()),
                auth: RestAuth::None,
                headers: Default::default(),
            },
            storage: IcebergStorage::Auto,
            query_engine: if athena {
                IcebergQueryEngine::Athena {
                    workgroup: "secret-workgroup".into(),
                    output_location: None,
                    aws: None,
                }
            } else {
                IcebergQueryEngine::Local
            },
            default_namespace: default_namespace.map(String::from),
        }),
        redshift: None,
        mysql: Default::default(),
    }
}

fn column(name: &str, ty: &str, ordinal: i32, nullable: bool) -> ColumnInfo {
    ColumnInfo {
        name: name.into(),
        data_type: ty.into(),
        nullable,
        default: None,
        is_primary_key: false,
        ordinal,
        comment: None,
    }
}

fn table(ns: &str, name: &str, cols: Vec<ColumnInfo>) -> TableDescription {
    TableDescription {
        schema: ns.into(),
        name: name.into(),
        kind: ObjectKind::Table,
        comment: None,
        estimated_rows: None,
        columns: cols,
        indexes: vec![],
        foreign_keys: vec![],
    }
}

impl FakeBackend {
    fn with(profiles: Vec<ConnectionProfile>) -> Arc<Self> {
        let b = FakeBackend::default();
        for p in profiles {
            b.profiles.lock().unwrap().insert(p.id.clone(), p);
        }
        Arc::new(b)
    }
    fn sql(&self) -> Vec<SqlCall> {
        self.sql_calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ToolBackend for FakeBackend {
    fn profile(&self, profile_id: &str) -> Result<ConnectionProfile> {
        self.profiles
            .lock()
            .unwrap()
            .get(profile_id)
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("profile {profile_id}")))
    }
    fn list_profiles(&self) -> Result<Vec<ConnectionProfile>> {
        Ok(self.profiles.lock().unwrap().values().cloned().collect())
    }
    async fn execute_sql(
        &self,
        profile_id: &str,
        database: Option<&str>,
        sql: &str,
        read_only: bool,
        _max_rows: usize,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<QueryResult> {
        self.sql_calls.lock().unwrap().push(SqlCall {
            profile: profile_id.into(),
            database: database.map(String::from),
            sql: sql.into(),
            read_only,
        });
        Ok(QueryResult {
            columns: vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "long".into(),
                    type_oid: 0,
                },
                ColumnMeta {
                    name: "name".into(),
                    type_name: "string".into(),
                    type_oid: 0,
                },
            ],
            rows: vec![
                vec![json!("1"), json!("alice")],
                vec![json!("2"), json!("bob")],
            ],
            row_count: 2,
            truncated: false,
            command_tag: None,
            rows_affected: None,
            elapsed_ms: 1,
        })
    }
    async fn schema_metadata(
        &self,
        profile_id: &str,
        database: Option<&str>,
        filter: Option<&str>,
        _max_tables: usize,
    ) -> Result<SchemaMetadata> {
        self.schema_calls.lock().unwrap().push(SchemaCall {
            profile: profile_id.into(),
            database: database.map(String::from),
            filter: filter.map(String::from),
        });
        let all = vec![
            table(
                "shop",
                "customers",
                vec![
                    column("id", "long", 1, false),
                    column("name", "string", 2, true),
                ],
            ),
            table(
                "shop",
                "orders",
                vec![
                    column("order_id", "long", 1, false),
                    column("placed_at", "timestamptz", 2, false),
                    column("total", "decimal(12, 2)", 3, true),
                ],
            ),
            table(
                "analytics.daily",
                "revenue",
                vec![column("day", "date", 1, false)],
            ),
        ];
        Ok(SchemaMetadata {
            database: "iceberg".into(),
            server_version: None,
            tables: all
                .into_iter()
                .filter(|t| filter.is_none_or(|f| t.schema == f))
                .collect(),
            truncated: false,
        })
    }
    async fn describe_iceberg_table(
        &self,
        _profile_id: &str,
        namespace: Vec<String>,
        name: &str,
    ) -> Result<IcebergTableInfo> {
        self.describe_calls
            .lock()
            .unwrap()
            .push((namespace, name.to_string()));
        self.table_info
            .lock()
            .unwrap()
            .clone()
            .ok_or(CoreError::NotImplemented("describe_iceberg_table"))
    }
    async fn valkey_scan(&self, _: &str, _: u8, _: &str, _: usize) -> Result<Vec<KeyEntry>> {
        *self.valkey_calls.lock().unwrap() += 1;
        Ok(vec![])
    }
    async fn valkey_get(&self, _: &str, _: u8, _: &str) -> Result<ValkeyValue> {
        *self.valkey_calls.lock().unwrap() += 1;
        Err(CoreError::NotImplemented("fake"))
    }
    async fn valkey_is_write(&self, _: &str, _: &[String]) -> Result<bool> {
        *self.valkey_calls.lock().unwrap() += 1;
        Ok(true)
    }
    async fn valkey_command(
        &self,
        _: &str,
        _: u8,
        _: Vec<String>,
        _: bool,
    ) -> Result<CommandOutcome> {
        *self.valkey_calls.lock().unwrap() += 1;
        Err(CoreError::NotImplemented("fake"))
    }
    async fn list_files(&self) -> Result<Vec<WorkspaceFile>> {
        Ok(vec![])
    }
    async fn read_file(&self, path: &str) -> Result<String> {
        Err(CoreError::NotFound(format!("file {path}")))
    }
    async fn write_file(&self, _: &str, _: &str, _: &FileVersion) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------------

struct Harness {
    svc: Arc<AgentService>,
    backend: Arc<FakeBackend>,
    llm: MockLlm,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new(replies: Vec<Reply>, profiles: Vec<ConnectionProfile>) -> Harness {
        let llm = MockLlm::start(replies).await;
        let dir = tempfile::tempdir().unwrap();
        let backend = FakeBackend::with(profiles);
        let svc = AgentService::new(
            Arc::new(TestVault::default()),
            backend.clone(),
            dir.path().to_path_buf(),
        );
        svc.save_provider(
            ProviderConfig {
                id: "anthropic".into(),
                label: "Anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: llm.base_url.clone(),
                model: "claude-test".into(),
                max_tokens: 1024,
                temperature: None,
                extra_headers: Default::default(),
                requires_api_key: true,
                portkey: None,
                context_window: None,
            },
            Some(SecretString::from("sk-ant-test-key-0123456789".to_string())),
        )
        .await
        .unwrap();
        Harness {
            svc,
            backend,
            llm,
            _dir: dir,
        }
    }

    /// Runs a turn to its terminal event; `decide` answers approval cards (default: reject).
    async fn run(
        &self,
        profile: &str,
        mut decide: impl FnMut(&str, &str, Option<&str>) -> ApprovalDecision,
    ) -> Vec<AgentEvent> {
        let (tx, mut rx) = mpsc::channel(256);
        self.svc
            .start_turn(
                "s1".into(),
                "anthropic".into(),
                "look at the lake".into(),
                AgentContext {
                    profile_id: Some(profile.into()),
                    ..Default::default()
                },
                tx,
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(15), rx.recv())
                .await
                .expect("timed out waiting for agent event")
                .expect("event channel closed before a terminal event");
            if let AgentEvent::ApprovalRequired {
                call_id,
                reason,
                preview,
                ..
            } = &ev
            {
                let d = decide(call_id, reason, preview.as_deref());
                self.svc.resolve_approval("s1", call_id, d).await.unwrap();
            }
            let done = matches!(
                ev,
                AgentEvent::TurnFinished { .. } | AgentEvent::Failed { .. }
            );
            out.push(ev);
            if done {
                return out;
            }
        }
    }

    fn system_prompt(&self) -> String {
        self.llm.requests()[0].body["system"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// The tool_result contents the model received in the last request, in order.
    fn results_seen(&self) -> Vec<(String, bool)> {
        let reqs = self.llm.requests();
        reqs.last().unwrap().body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .filter(|b| b["type"] == "tool_result")
            .map(|b| {
                (
                    b["content"].as_str().unwrap_or_default().to_string(),
                    b["is_error"] == true,
                )
            })
            .collect()
    }
}

fn shape(events: &[AgentEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCallProposed {
                call_id,
                tool,
                risk,
                ..
            } => Some(format!("proposed {call_id} {tool} {risk:?}")),
            AgentEvent::ApprovalRequired { call_id, tool, .. } => {
                Some(format!("approval {call_id} {tool}"))
            }
            AgentEvent::ToolFinished {
                call_id,
                ok,
                summary,
                ..
            } => Some(format!("finished {call_id} ok={ok} {summary}")),
            AgentEvent::TurnFinished { stop_reason, .. } => Some(format!("turn {stop_reason}")),
            AgentEvent::Failed { error } => Some(format!("failed {}", error.code)),
            _ => None,
        })
        .collect()
}

fn calls(calls: Vec<(&str, &str, Value)>) -> Vec<Reply> {
    vec![
        Reply::Sse(anthropic_sse("", &calls, "tool_use", (1, 1))),
        Reply::Sse(anthropic_sse("done", &[], "end_turn", (1, 1))),
    ]
}

// ---------------------------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn local_engine_reads_run_read_only_and_schema_tools_use_the_namespace() {
    let h = Harness::new(
        calls(vec![
            ("c1", "get_schema", json!({"profile_id":"i1"})),
            ("c2", "get_schema", json!({"profile_id":"i1","schema":"analytics.daily","database":"ignored"})),
            ("c3", "describe_table", json!({"profile_id":"i1","table":"orders"})),
            ("c4", "describe_table", json!({"profile_id":"i1","schema":"analytics.daily","table":"revenue"})),
            ("c5", "describe_table", json!({"profile_id":"i1","table":"shop.customers","database":"ignored"})),
            ("c6", "run_sql", json!({"profile_id":"i1","sql":"select id, name from shop.customers limit 2","purpose":"peek","database":"ignored"})),
            ("c7", "run_sql", json!({"profile_id":"i1","sql":"select * from shop.\"orders@123\" limit 1","purpose":"time travel"})),
        ]),
        vec![iceberg_profile("i1", false, Environment::Dev, AiDataAccess::Results, Some("shop"))],
    )
    .await;
    let events = h.run("i1", |_, _, _| panic!("no approvals expected")).await;
    assert_eq!(
        shape(&events),
        vec![
            "proposed c1 get_schema Safe",
            "finished c1 ok=true 3 tables",
            "proposed c2 get_schema Safe",
            "finished c2 ok=true 1 table",
            "proposed c3 describe_table Safe",
            "finished c3 ok=true shop.orders (3 columns)",
            "proposed c4 describe_table Safe",
            "finished c4 ok=true analytics.daily.revenue (1 columns)",
            "proposed c5 describe_table Safe",
            "finished c5 ok=true shop.customers (2 columns)",
            "proposed c6 run_sql Safe",
            "finished c6 ok=true 2 rows",
            "proposed c7 run_sql Safe",
            "finished c7 ok=true 2 rows",
            "turn end_turn",
        ]
    );

    // The namespace filter reaches the backend; `database` never does for describe_table.
    let schema_calls = h.backend.schema_calls.lock().unwrap().clone();
    let filters: Vec<_> = schema_calls.iter().map(|c| c.filter.as_deref()).collect();
    assert_eq!(
        filters,
        vec![
            None,
            Some("analytics.daily"),
            // describe_table: default namespace, explicit nested namespace, `ns.table` form.
            Some("shop"),
            Some("analytics.daily"),
            Some("shop"),
        ]
    );
    assert!(
        schema_calls[2..].iter().all(|c| c.database.is_none()),
        "{schema_calls:?}"
    );

    // Reads run read-only; the SQL is passed through untouched (time-travel name included).
    let sql = h.backend.sql();
    assert_eq!(sql.len(), 2);
    assert!(sql.iter().all(|c| c.read_only && c.profile == "i1"));
    assert!(sql[1].sql.contains("\"orders@123\""));

    // The model sees Iceberg types in the schema text.
    let results = h.results_seen();
    assert!(results[0].0.contains("shop.orders") && results[0].0.contains("timestamptz"));
    assert!(results[0].0.contains("analytics.daily.revenue"));
    assert!(
        results[2].0.contains("total decimal(12, 2)"),
        "{}",
        results[2].0
    );

    // The turn really went to the (mock) provider with the app's key.
    let first = &h.llm.requests()[0];
    assert!(first.path.contains("messages"), "{}", first.path);
    assert!(first.headers.contains_key("x-api-key"));

    // The prompt names the engine and the time-travel syntax, and never the catalog.
    let system = h.system_prompt();
    assert!(system.contains("Apache Iceberg") && system.contains("table@<snapshot_id>"));
    assert!(system.contains("Default namespace: shop"));
    for secret in ["secret-catalog", "secret-warehouse", "secret-workgroup"] {
        assert!(!system.contains(secret), "{secret} leaked into the prompt");
        for r in h.llm.requests() {
            assert!(!r.body.to_string().contains(secret), "{secret} leaked");
        }
    }
}

fn orders_info() -> IcebergTableInfo {
    let field = |id: i32, name: &str, ty: &str, required: bool| IcebergField {
        id,
        name: name.into(),
        type_name: ty.into(),
        required,
        doc: None,
        children: vec![],
    };
    IcebergTableInfo {
        namespace: vec!["shop".into()],
        name: "orders".into(),
        kind: IcebergObjectKind::Table,
        location: "s3://secret-bucket/shop/orders".into(),
        metadata_location: None,
        format_version: 2,
        table_uuid: None,
        last_updated_ms: None,
        current_snapshot_id: Some("2".into()),
        current_schema_id: 0,
        fields: vec![
            field(1, "order_id", "long", true),
            field(2, "placed_at", "timestamptz", true),
        ],
        schema_count: 1,
        partition_spec: vec![IcebergPartitionField {
            source_id: 2,
            field_id: 1000,
            name: "placed_at_day".into(),
            transform: "day".into(),
        }],
        sort_order: vec![],
        properties: [
            ("write.parquet.compression".to_string(), "zstd".to_string()),
            (
                "s3.secret-access-key".to_string(),
                "AKIA-SUPER-SECRET".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        snapshots: vec![
            IcebergSnapshot {
                snapshot_id: "2".into(),
                parent_snapshot_id: Some("1".into()),
                sequence_number: None,
                timestamp_ms: 1_700_000_100_000,
                operation: Some("overwrite".into()),
                summary: Default::default(),
                manifest_list: None,
                schema_id: None,
            },
            IcebergSnapshot {
                snapshot_id: "1".into(),
                parent_snapshot_id: None,
                sequence_number: None,
                timestamp_ms: 1_700_000_000_000,
                operation: Some("append".into()),
                summary: Default::default(),
                manifest_list: None,
                schema_id: None,
            },
        ],
        refs: vec![],
    }
}

#[tokio::test]
async fn describe_table_uses_the_rich_iceberg_call_when_the_backend_has_it() {
    for access in [AiDataAccess::SchemaOnly, AiDataAccess::Results] {
        let h = Harness::new(
            calls(vec![(
                "d",
                "describe_table",
                json!({"profile_id":"i1","schema":"shop","table":"orders"}),
            )]),
            vec![iceberg_profile("i1", false, Environment::Dev, access, None)],
        )
        .await;
        *h.backend.table_info.lock().unwrap() = Some(orders_info());
        let events = h.run("i1", |_, _, _| panic!("no approval")).await;
        let shape = shape(&events);
        assert_eq!(shape[0], "proposed d describe_table Safe");
        assert_eq!(
            shape[1],
            "finished d ok=true shop.orders (2 columns, 2 snapshots)"
        );
        assert_eq!(
            h.backend.describe_calls.lock().unwrap().clone(),
            vec![(vec!["shop".to_string()], "orders".to_string())]
        );
        assert!(
            h.backend.schema_calls.lock().unwrap().is_empty(),
            "no fallback needed"
        );
        let (text, is_error) = h.results_seen().remove(0);
        assert!(!is_error);
        assert!(text.contains("partitioning: day(placed_at)"), "{text}");
        assert!(text.contains("snapshots: 2, current 2"), "{text}");
        assert!(
            text.contains("overwrite") && text.contains("append"),
            "{text}"
        );
        for hidden in ["AKIA-SUPER-SECRET", "secret-bucket", "s3.secret", "zstd"] {
            assert!(!text.contains(hidden), "{hidden} leaked: {text}");
        }
    }
}

#[tokio::test]
async fn describe_table_passes_nested_namespaces_as_levels_and_falls_back_on_not_found() {
    let h = Harness::new(
        calls(vec![(
            "d",
            "describe_table",
            json!({"profile_id":"i1","table":"analytics.daily.revenue"}),
        )]),
        vec![iceberg_profile(
            "i1",
            false,
            Environment::Dev,
            AiDataAccess::Results,
            None,
        )],
    )
    .await;
    // Default NotImplemented: the schema listing answers instead.
    let events = h.run("i1", |_, _, _| panic!("no approval")).await;
    assert_eq!(
        shape(&events)[1],
        "finished d ok=true analytics.daily.revenue (1 columns)"
    );
    assert_eq!(
        h.backend.describe_calls.lock().unwrap().clone(),
        vec![(
            vec!["analytics".to_string(), "daily".to_string()],
            "revenue".to_string()
        )]
    );
    assert_eq!(
        h.backend.schema_calls.lock().unwrap()[0].filter.as_deref(),
        Some("analytics.daily")
    );
}

#[tokio::test]
async fn describe_table_without_a_namespace_asks_for_one() {
    let h = Harness::new(
        calls(vec![(
            "c1",
            "describe_table",
            json!({"profile_id":"i1","table":"orders"}),
        )]),
        vec![iceberg_profile(
            "i1",
            false,
            Environment::Dev,
            AiDataAccess::Results,
            None,
        )],
    )
    .await;
    let events = h.run("i1", |_, _, _| panic!("no approval")).await;
    let shape = shape(&events);
    assert_eq!(shape[0], "proposed c1 describe_table Safe");
    assert!(
        shape[1].starts_with("finished c1 ok=false") && shape[1].contains("no default namespace")
    );
    assert_eq!(shape[2], "turn end_turn");
    assert!(h.backend.schema_calls.lock().unwrap().is_empty());
    let (text, is_error) = h.results_seen().remove(0);
    assert!(
        is_error && text.contains("no default namespace") && text.contains("get_schema"),
        "{text}"
    );
}

#[tokio::test]
async fn local_insert_waits_for_approval_and_runs_read_write_once_approved() {
    let insert = "insert into shop.customers values (501, 'zed')";
    let h = Harness::new(
        calls(vec![(
            "w1",
            "run_sql",
            json!({"profile_id":"i1","sql":insert,"purpose":"add zed"}),
        )]),
        vec![iceberg_profile(
            "i1",
            false,
            Environment::Prod,
            AiDataAccess::Results,
            None,
        )],
    )
    .await;
    let backend = h.backend.clone();
    let events = h
        .run("i1", |_, reason, preview| {
            assert!(
                reason.contains("PRODUCTION connection 'i1-lake'"),
                "{reason}"
            );
            assert!(
                reason.contains("Iceberg writes create a new table snapshot"),
                "{reason}"
            );
            assert!(!reason.contains("billed per TB"), "{reason}");
            assert!(reason.contains("add zed"));
            assert_eq!(preview, Some(insert));
            assert!(backend.sql().is_empty(), "nothing ran before approval");
            ApprovalDecision::Approve
        })
        .await;
    assert!(shape(&events).contains(&"approval w1 run_sql".to_string()));
    let sql = h.backend.sql();
    assert_eq!(sql.len(), 1);
    assert!(!sql[0].read_only, "an approved write runs read-write");
}

#[tokio::test]
async fn local_update_is_flagged_as_needing_athena_and_a_rejection_runs_nothing() {
    let h = Harness::new(
        calls(vec![(
            "u1",
            "run_sql",
            json!({"profile_id":"i1","sql":"update shop.orders set total = 0","purpose":"zero it"}),
        )]),
        vec![iceberg_profile(
            "i1",
            false,
            Environment::Dev,
            AiDataAccess::Results,
            None,
        )],
    )
    .await;
    let events = h
        .run("i1", |_, reason, _| {
            assert!(reason.contains("need an Athena connection"), "{reason}");
            ApprovalDecision::Reject { reason: None }
        })
        .await;
    assert!(
        shape(&events)
            .iter()
            .any(|s| s.starts_with("proposed u1 run_sql NeedsApproval"))
    );
    assert!(
        h.backend.sql().is_empty(),
        "rejected writes never reach the backend"
    );
    assert_eq!(*h.backend.valkey_calls.lock().unwrap(), 0);
}

#[tokio::test]
async fn athena_dml_and_ctas_mention_the_scan_bill() {
    let h = Harness::new(
        calls(vec![
            ("a1", "run_sql", json!({"profile_id":"i1","sql":"delete from shop.orders where total = 0","purpose":"clean"})),
            ("a2", "run_sql", json!({"profile_id":"i1","sql":"create table shop.top as select * from shop.orders","purpose":"ctas"})),
        ]),
        vec![iceberg_profile("i1", true, Environment::Staging, AiDataAccess::Results, None)],
    )
    .await;
    let mut reasons = Vec::new();
    let events = h
        .run("i1", |_, reason, _| {
            reasons.push(reason.to_string());
            ApprovalDecision::Approve
        })
        .await;
    assert_eq!(reasons.len(), 2);
    for r in &reasons {
        assert!(
            r.contains("Athena is billed per TB scanned")
                && r.contains("Iceberg writes create a new table snapshot")
                && !r.contains("PRODUCTION"),
            "{r}"
        );
    }
    assert_eq!(shape(&events).last().unwrap(), "turn end_turn");
    assert!(h.backend.sql().iter().all(|c| !c.read_only));
    let system = h.system_prompt();
    assert!(system.contains("billed per TB scanned") && !system.contains("cannot UPDATE"));
}

#[tokio::test]
async fn athena_reads_under_results_with_approval_mention_cost_and_run_read_only() {
    let h = Harness::new(
        calls(vec![(
            "r1",
            "run_sql",
            json!({"profile_id":"i1","sql":"select count(*) from shop.orders","purpose":"count"}),
        )]),
        vec![iceberg_profile(
            "i1",
            true,
            Environment::Dev,
            AiDataAccess::ResultsWithApproval,
            None,
        )],
    )
    .await;
    let events = h
        .run("i1", |_, reason, _| {
            assert!(
                reason.contains("sent to the LLM provider") && reason.contains("billed per TB"),
                "{reason}"
            );
            ApprovalDecision::Approve
        })
        .await;
    assert!(shape(&events).contains(&"approval r1 run_sql".to_string()));
    let sql = h.backend.sql();
    assert_eq!(sql.len(), 1);
    assert!(sql[0].read_only, "approved reads still run read-only");
}

#[tokio::test]
async fn schema_only_iceberg_shares_structure_but_never_rows() {
    let h = Harness::new(
        calls(vec![
            ("s1", "get_schema", json!({"profile_id":"i1","schema":"shop"})),
            ("s2", "describe_table", json!({"profile_id":"i1","schema":"shop","table":"orders"})),
            ("s3", "run_sql", json!({"profile_id":"i1","sql":"select * from shop.orders","purpose":"look"})),
            ("s4", "run_sql", json!({"profile_id":"i1","sql":"insert into shop.customers select * from shop.customers","purpose":"dup"})),
        ]),
        vec![iceberg_profile("i1", false, Environment::Dev, AiDataAccess::SchemaOnly, None)],
    )
    .await;
    let events = h.run("i1", |_, _, _| ApprovalDecision::Approve).await;
    let shape = shape(&events);
    assert_eq!(shape[0], "proposed s1 get_schema Safe");
    assert!(shape.contains(&"proposed s2 describe_table Safe".to_string()));
    assert!(shape.contains(&"proposed s3 run_sql Denied".to_string()));
    assert!(shape.contains(&"proposed s4 run_sql NeedsApproval".to_string()));
    // Only the approved write reached the backend, read-write, and its rows are withheld.
    let sql = h.backend.sql();
    assert_eq!(sql.len(), 1);
    assert!(sql[0].sql.starts_with("insert") && !sql[0].read_only);
    let results = h.results_seen();
    assert!(
        results[2].1 && results[2].0.contains("schema-only"),
        "{:?}",
        results[2]
    );
    assert!(results[3].0.contains("withheld"), "{}", results[3].0);
    assert!(
        !results[3].0.contains("alice"),
        "values must not reach the model"
    );
}

#[tokio::test]
async fn access_none_and_valkey_tools_are_refused_on_iceberg() {
    let h = Harness::new(
        calls(vec![
            (
                "n1",
                "run_sql",
                json!({"profile_id":"off","sql":"select 1","purpose":"x"}),
            ),
            ("n2", "get_schema", json!({"profile_id":"off"})),
            ("n3", "valkey_scan", json!({"profile_id":"i1"})),
            (
                "n4",
                "valkey_command",
                json!({"profile_id":"i1","argv":["GET","k"]}),
            ),
        ]),
        vec![
            iceberg_profile("off", false, Environment::Dev, AiDataAccess::None, None),
            iceberg_profile("i1", false, Environment::Dev, AiDataAccess::Results, None),
        ],
    )
    .await;
    let events = h.run("i1", |_, _, _| panic!("no approval")).await;
    assert_eq!(
        shape(&events),
        vec![
            "proposed n1 run_sql Denied",
            "finished n1 ok=false denied",
            "proposed n2 get_schema Denied",
            "finished n2 ok=false denied",
            "proposed n3 valkey_scan Denied",
            "finished n3 ok=false denied",
            "proposed n4 valkey_command Denied",
            "finished n4 ok=false denied",
            "turn end_turn",
        ]
    );
    assert!(h.backend.sql().is_empty() && h.backend.schema_calls.lock().unwrap().is_empty());
    assert_eq!(
        *h.backend.valkey_calls.lock().unwrap(),
        0,
        "even the write probe is skipped"
    );
    let results = h.results_seen();
    assert!(results[0].0.contains("aiDataAccess = none"));
    assert!(results[2].0.contains("Iceberg") && results[2].0.contains("run_sql"));
}

#[tokio::test]
async fn unverifiable_athena_sql_goes_to_approval_instead_of_running() {
    let h = Harness::new(
        calls(vec![
            ("v1", "run_sql", json!({"profile_id":"i1","sql":"OPTIMIZE shop.orders REWRITE DATA USING BIN_PACK","purpose":"compact"})),
            ("v2", "run_sql", json!({"profile_id":"i1","sql":"select * from shop.orders for version as of 42","purpose":"tt"})),
        ]),
        vec![iceberg_profile("i1", true, Environment::Prod, AiDataAccess::Results, None)],
    )
    .await;
    let mut asked = 0;
    let events = h
        .run("i1", |_, reason, _| {
            asked += 1;
            assert!(reason.contains("PRODUCTION"), "{reason}");
            ApprovalDecision::Reject { reason: None }
        })
        .await;
    assert_eq!(asked, 2);
    assert!(h.backend.sql().is_empty());
    assert_eq!(shape(&events).last().unwrap(), "turn end_turn");
}

// ---------------------------------------------------------------------------------------------
// End to end over the real AppToolBackend and the pyiceberg fixture (skips when unset)
// ---------------------------------------------------------------------------------------------

mod real {
    use super::*;
    use vibelens_core::aws::AwsService;
    use vibelens_core::connections::{AppToolBackend, ConnectionManager};
    use vibelens_core::iceberg::IcebergDeps;
    use vibelens_core::ports::{CredentialSource, ProfileStore, TunnelHandle, TunnelProvider};
    use vibelens_core::vault::MemoryVault;
    use vibelens_core::workspace::WorkspaceFs;

    struct OneProfile(ConnectionProfile);

    impl ProfileStore for OneProfile {
        fn list(&self) -> Result<Vec<ConnectionProfile>> {
            Ok(vec![self.0.clone()])
        }
        fn get(&self, id: &str) -> Result<ConnectionProfile> {
            (id == self.0.id)
                .then(|| self.0.clone())
                .ok_or_else(|| CoreError::NotFound(id.into()))
        }
        fn upsert(&self, p: ConnectionProfile) -> Result<ConnectionProfile> {
            Ok(p)
        }
        fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    struct NoCreds;
    #[async_trait]
    impl CredentialSource for NoCreds {
        async fn resolve(&self, _: &ConnectionProfile) -> Result<ResolvedCredentials> {
            Ok(ResolvedCredentials::anonymous())
        }
    }

    struct NoTunnels;
    #[async_trait]
    impl TunnelProvider for NoTunnels {
        async fn open(&self, _: &str, _: &TunnelConfig, _: &str, _: u16) -> Result<TunnelHandle> {
            Err(CoreError::Tunnel("unused".into()))
        }
    }

    #[tokio::test]
    async fn agent_explores_and_queries_a_real_iceberg_catalog() {
        let (Ok(db), Ok(warehouse)) = (
            std::env::var("VG_TEST_ICEBERG_SQL_DB"),
            std::env::var("VG_TEST_ICEBERG_WAREHOUSE"),
        ) else {
            crate::services::skip(
                "iceberg",
                "VG_TEST_ICEBERG_SQL_DB and VG_TEST_ICEBERG_WAREHOUSE are not both set",
            );
            return;
        };
        let mut profile = iceberg_profile(
            "lake",
            false,
            Environment::Dev,
            AiDataAccess::Results,
            Some("shop"),
        );
        profile.iceberg = Some(IcebergOptions {
            catalog: IcebergCatalog::Sql {
                uri: format!("sqlite://{db}"),
                warehouse: format!("file://{warehouse}"),
                catalog_name: "vg_test".into(),
            },
            storage: IcebergStorage::Local,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: Some("shop".into()),
        });
        let mgr = ConnectionManager::new(
            Arc::new(OneProfile(profile)),
            Arc::new(NoCreds),
            Arc::new(NoTunnels),
        );
        mgr.set_iceberg_deps(IcebergDeps {
            aws: AwsService::new(),
            vault: Arc::new(MemoryVault::new()),
        });
        let ws = tempfile::tempdir().unwrap();
        let backend = AppToolBackend::new(
            mgr,
            Arc::new(WorkspaceFs::new(ws.path().join("w")).unwrap()),
        );

        let llm = MockLlm::start(calls(vec![
            ("g", "get_schema", json!({"profile_id":"lake","schema":"shop"})),
            ("d", "describe_table", json!({"profile_id":"lake","table":"orders"})),
            ("c", "run_sql", json!({"profile_id":"lake","sql":"select count(*) as n from customers","purpose":"count"})),
            ("w", "run_sql", json!({"profile_id":"lake","sql":"insert into shop.customers select * from shop.customers limit 1","purpose":"dup"})),
        ]))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let svc = AgentService::new(
            Arc::new(TestVault::default()),
            backend,
            dir.path().to_path_buf(),
        );
        svc.save_provider(
            ProviderConfig {
                id: "anthropic".into(),
                label: "Anthropic".into(),
                kind: ProviderKind::Anthropic,
                base_url: llm.base_url.clone(),
                model: "claude-test".into(),
                max_tokens: 1024,
                temperature: None,
                extra_headers: Default::default(),
                requires_api_key: true,
                portkey: None,
                context_window: None,
            },
            Some(SecretString::from("sk-ant-test-key-0123456789".to_string())),
        )
        .await
        .unwrap();
        let (tx, mut rx) = mpsc::channel(256);
        svc.start_turn(
            "s1".into(),
            "anthropic".into(),
            "explore".into(),
            AgentContext {
                profile_id: Some("lake".into()),
                ..Default::default()
            },
            tx,
        )
        .await
        .unwrap();
        let mut events = Vec::new();
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(60), rx.recv())
                .await
                .expect("timed out")
                .expect("channel closed");
            if let AgentEvent::ApprovalRequired { call_id, .. } = &ev {
                // The write is rejected: the local engine appends, but this test must not change the fixture.
                svc.resolve_approval("s1", call_id, ApprovalDecision::Reject { reason: None })
                    .await
                    .unwrap();
            }
            let done = matches!(
                ev,
                AgentEvent::TurnFinished { .. } | AgentEvent::Failed { .. }
            );
            events.push(ev);
            if done {
                break;
            }
        }
        let shape = shape(&events);
        assert_eq!(shape.last().unwrap(), "turn end_turn", "{shape:?}");
        assert!(shape.contains(&"proposed g get_schema Safe".to_string()));
        assert!(
            shape.contains(&"finished g ok=true 3 tables".to_string()),
            "{shape:?}"
        );
        assert!(shape.contains(&"proposed w run_sql NeedsApproval".to_string()));
        assert!(
            shape
                .iter()
                .any(|s| s.starts_with("finished d ok=true shop.orders (")),
            "{shape:?}"
        );
        assert!(
            shape.contains(&"finished c ok=true 1 row".to_string()),
            "{shape:?}"
        );

        let body = llm.requests().last().unwrap().body.clone();
        let results: Vec<String> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
            .filter(|b| b["type"] == "tool_result")
            .map(|b| b["content"].as_str().unwrap_or_default().to_string())
            .collect();
        assert!(results[0].contains("shop.customers") && results[0].contains("shop.events"));
        assert!(
            results[1].contains("placed_at")
                && results[1].contains("timestamp")
                && results[1].contains("partitioning: day(placed_at)")
                && results[1].contains("snapshots: 4"),
            "{}",
            results[1]
        );
        assert!(
            results[2].contains("\"500\""),
            "int64 counts cross as strings: {}",
            results[2]
        );
    }
}
