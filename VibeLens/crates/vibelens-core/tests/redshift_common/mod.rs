//! Shared helpers for the `redshift_*` integration tests: an in-process HTTP server that speaks
//! enough of the Redshift Data API's awsJson1.1 protocol for the real `aws-sdk-redshiftdata`
//! client to run offline. Same pattern as `tests/athena_common`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_credential_types::Credentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use vibelens_core::model::*;
use vibelens_core::redshift::{RedshiftEngine, RedshiftTiming};

/// The static test credentials. Neither may ever appear in a request body or an error.
pub const ACCESS_KEY: &str = "AKIDREDSHIFTTEST";
pub const SECRET_KEY: &str = "secret-redshift-test-key";

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    pub path: String,
    /// Header names are lower-cased.
    pub headers: HashMap<String, String>,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Resp {
    pub fn ok(body: &Value) -> Self {
        Self {
            status: 200,
            headers: vec![("content-type".into(), "application/x-amz-json-1.1".into())],
            body: body.to_string(),
        }
    }
    /// A modeled AWS error.
    pub fn error(status: u16, error_type: &str, message: &str) -> Self {
        Self {
            status,
            headers: vec![
                ("content-type".into(), "application/x-amz-json-1.1".into()),
                ("x-amzn-errortype".into(), error_type.into()),
            ],
            body: json!({"__type": error_type, "message": message}).to_string(),
        }
    }
}

pub type Handler = Arc<dyn Fn(&Req) -> Resp + Send + Sync>;

pub struct MockServer {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Req>>>,
}

impl MockServer {
    pub fn all(&self) -> Vec<Req> {
        self.requests.lock().unwrap().clone()
    }
    /// Parsed bodies of the requests for one operation.
    pub fn calls(&self, operation: &str) -> Vec<Value> {
        self.all()
            .iter()
            .filter(|r| op(r) == operation)
            .map(body)
            .collect()
    }
    pub fn count(&self, operation: &str) -> usize {
        self.calls(operation).len()
    }
}

/// The Data API operation a request calls (`ExecuteStatement`, …).
pub fn op(req: &Req) -> String {
    req.headers
        .get("x-amz-target")
        .and_then(|t| t.strip_prefix("RedshiftData."))
        .unwrap_or("")
        .to_string()
}

pub fn body(req: &Req) -> Value {
    serde_json::from_str(&req.body).unwrap_or(Value::Null)
}

pub async fn serve(handler: Handler) -> MockServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests: Arc<Mutex<Vec<Req>>> = Arc::default();
    let recorded = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            let recorded = recorded.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let header_end = loop {
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p;
                    }
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let mut lines = head.split("\r\n");
                let mut first = lines.next().unwrap_or("").split(' ');
                let method = first.next().unwrap_or("").to_string();
                let path = first.next().unwrap_or("").to_string();
                let headers: HashMap<String, String> = lines
                    .filter_map(|l| {
                        l.split_once(':')
                            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                    })
                    .collect();
                let len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = buf[header_end + 4..].to_vec();
                while body.len() < len {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => body.extend_from_slice(&tmp[..n]),
                    }
                }
                let req = Req {
                    method,
                    path,
                    headers,
                    body: String::from_utf8_lossy(&body).to_string(),
                };
                recorded.lock().unwrap().push(req.clone());
                let resp = handler(&req);
                let mut out = format!(
                    "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                    resp.status,
                    resp.body.len()
                );
                for (k, v) in &resp.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                out.push_str(&resp.body);
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    MockServer {
        url: format!("http://{addr}"),
        requests,
    }
}

// ---------------------------------------------------------------------------------------------
// Profiles and the engine
// ---------------------------------------------------------------------------------------------

pub fn aws() -> AwsTarget {
    AwsTarget {
        identity: AwsIdentity::DefaultChain,
        region: "eu-west-1".into(),
    }
}

pub fn provisioned(auth: RedshiftAuth) -> RedshiftOptions {
    RedshiftOptions {
        target: RedshiftTarget::Provisioned {
            cluster_identifier: "analytics-1".into(),
        },
        database: "dev".into(),
        aws: aws(),
        auth,
    }
}

pub fn serverless(auth: RedshiftAuth) -> RedshiftOptions {
    RedshiftOptions {
        target: RedshiftTarget::Serverless {
            workgroup_name: "default-wg".into(),
        },
        database: "dev".into(),
        aws: aws(),
        auth,
    }
}

pub fn fast() -> RedshiftTiming {
    RedshiftTiming {
        poll_initial: Duration::from_millis(5),
        poll_max: Duration::from_millis(20),
        retry_base: Duration::from_millis(5),
        ..RedshiftTiming::default()
    }
}

pub fn creds() -> SharedCredentialsProvider {
    SharedCredentialsProvider::new(Credentials::new(
        ACCESS_KEY,
        SECRET_KEY,
        None,
        None,
        "redshift-test",
    ))
}

pub fn engine_for(server: &MockServer, opts: RedshiftOptions) -> RedshiftEngine {
    RedshiftEngine::new(opts, creds())
        .unwrap()
        .with_endpoint_url(&server.url)
        .with_timing(fast())
}

pub fn engine(server: &MockServer) -> RedshiftEngine {
    engine_for(server, provisioned(RedshiftAuth::IamIdentity))
}

// ---------------------------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------------------------

pub fn executed(id: &str) -> Resp {
    Resp::ok(&json!({"Id": id, "CreatedAt": 1_700_000_000.0, "Database": "dev"}))
}

/// `DescribeStatement` response.
pub fn described(id: &str, status: &str, has_result_set: bool, result_rows: i64) -> Resp {
    Resp::ok(&json!({
        "Id": id,
        "Status": status,
        "HasResultSet": has_result_set,
        "ResultRows": result_rows,
        "ResultSize": 0,
        "RedshiftQueryId": 4242,
        "Duration": 1_000_000,
    }))
}

pub fn failed(id: &str, error: &str) -> Resp {
    Resp::ok(&json!({"Id": id, "Status": "FAILED", "Error": error, "RedshiftQueryId": 0}))
}

/// `(name, typeName)` column metadata.
pub fn columns(cols: &[(&str, &str)]) -> Value {
    Value::Array(
        cols.iter()
            .map(|(n, t)| {
                json!({"name": n, "label": n, "typeName": t, "precision": 0, "scale": 0,
                       "nullable": 1, "length": 0, "isCaseSensitive": false,
                       "isCurrency": false, "isSigned": true})
            })
            .collect(),
    )
}

pub fn long(n: i64) -> Value {
    json!({"longValue": n})
}
pub fn text(s: &str) -> Value {
    json!({"stringValue": s})
}
pub fn null() -> Value {
    json!({"isNull": true})
}
pub fn double(d: f64) -> Value {
    json!({"doubleValue": d})
}
pub fn boolean(b: bool) -> Value {
    json!({"booleanValue": b})
}

pub fn result_page(cols: &[(&str, &str)], records: Vec<Vec<Value>>, next: Option<&str>) -> Resp {
    let mut v = json!({
        "Records": records,
        "ColumnMetadata": columns(cols),
        "TotalNumRows": 0,
    });
    if let Some(n) = next {
        v["NextToken"] = json!(n);
    }
    Resp::ok(&v)
}

/// A server that answers one SELECT with `records`, in one page.
pub fn one_result(
    cols: &'static [(&'static str, &'static str)],
    records: Vec<Vec<Value>>,
) -> Handler {
    Arc::new(move |req: &Req| match op(req).as_str() {
        "ExecuteStatement" => executed("stmt-1"),
        "DescribeStatement" => described("stmt-1", "FINISHED", true, records.len() as i64),
        "GetStatementResult" => result_page(cols, records.clone(), None),
        other => Resp::error(400, "ValidationException", &format!("unexpected {other}")),
    })
}

/// Runs a script through `execute_stream` and collects every event.
pub async fn stream(e: &RedshiftEngine, sql: &str, opts: ExecOptions) -> Vec<QueryEvent> {
    let (tx, mut rx) = mpsc::channel(64);
    let cancel = tokio_util::sync::CancellationToken::new();
    e.execute_stream("q1".into(), sql, None, opts, cancel, tx)
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(ev) = rx.recv().await {
        out.push(ev);
    }
    out
}

pub fn kinds(events: &[QueryEvent]) -> Vec<&'static str> {
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

/// `(statement_index, code, message)` of the terminal `Failed` event.
pub fn failure(events: &[QueryEvent]) -> (Option<usize>, String, String) {
    match events.last() {
        Some(QueryEvent::Failed {
            statement_index,
            error,
        }) => (*statement_index, error.code.clone(), error.message.clone()),
        other => panic!("expected Failed, got {other:?}"),
    }
}

pub fn rows_of(events: &[QueryEvent]) -> Vec<Vec<Vec<Cell>>> {
    events
        .iter()
        .filter_map(|e| match e {
            QueryEvent::Rows { rows, .. } => Some(rows.clone()),
            _ => None,
        })
        .collect()
}
