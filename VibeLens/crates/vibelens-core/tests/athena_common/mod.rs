//! Shared helpers for the `athena_*` integration tests: an in-process HTTP server that speaks
//! enough of Athena's awsJson1.1 protocol for the real `aws-sdk-athena` client to run offline.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Req {
    pub method: String,
    /// Path including the query string.
    pub path: String,
    /// Header names are lower-cased.
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Req {
    pub fn path_only(&self) -> &str {
        self.path.split('?').next().unwrap_or("")
    }
    pub fn query(&self, key: &str) -> Option<String> {
        let q = self.path.split_once('?')?.1;
        q.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            (k == key).then(|| v.to_string())
        })
    }
}

#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Resp {
    pub fn json(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.to_string(),
        }
    }
    pub fn ok(body: &str) -> Self {
        Self::json(200, body)
    }
    /// A modeled AWS error (restJson1 / awsJson1.1).
    pub fn error(status: u16, error_type: &str, message: &str) -> Self {
        Self {
            status,
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("x-amzn-errortype".into(), error_type.into()),
            ],
            body: format!(r#"{{"__type":"{error_type}","message":"{message}"}}"#),
        }
    }
}

pub type Handler = Arc<dyn Fn(&Req) -> Resp + Send + Sync>;

pub struct MockServer {
    pub url: String,
    pub requests: Arc<Mutex<Vec<Req>>>,
}

impl MockServer {
    /// Number of recorded requests whose path (without query) equals `path`.
    pub fn count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path_only() == path)
            .count()
    }
    pub fn all(&self) -> Vec<Req> {
        self.requests.lock().unwrap().clone()
    }
    pub fn to_target(&self, target: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.headers.get("x-amz-target").is_some_and(|t| t == target))
            .count()
    }
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
                let mut tmp = [0u8; 4096];
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
// Athena specifics
// ---------------------------------------------------------------------------------------------

use std::time::Duration;

use aws_credential_types::Credentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use vibelens_core::athena::{AthenaConfig, AthenaEngine, AthenaTiming};
use vibelens_core::model::QueryEvent;

/// The Athena operation a request calls (`StartQueryExecution`, …).
pub fn op(req: &Req) -> String {
    req.headers
        .get("x-amz-target")
        .and_then(|t| t.strip_prefix("AmazonAthena."))
        .unwrap_or("")
        .to_string()
}

pub fn body(req: &Req) -> Value {
    serde_json::from_str(&req.body).unwrap_or(Value::Null)
}

/// Requests for one operation, bodies parsed.
pub fn calls(server: &MockServer, operation: &str) -> Vec<Value> {
    server
        .all()
        .iter()
        .filter(|r| op(r) == operation)
        .map(body)
        .collect()
}

pub fn config() -> AthenaConfig {
    AthenaConfig {
        region: "eu-west-1".into(),
        workgroup: "wg-test".into(),
        output_location: Some("s3://results-bucket/athena/".into()),
        catalog: "AwsDataCatalog".into(),
        database: Some("shop".into()),
    }
}

pub fn engine(server: &MockServer) -> AthenaEngine {
    let creds = Credentials::new("AKIDTEST", "secret-test", None, None, "athena-test");
    AthenaEngine::new(config(), SharedCredentialsProvider::new(creds))
        .with_endpoint_url(&server.url)
        .with_timing(AthenaTiming {
            poll_initial: Duration::from_millis(5),
            poll_max: Duration::from_millis(20),
            retry_base: Duration::from_millis(5),
        })
}

pub fn started(id: &str) -> Resp {
    Resp::ok(&json!({ "QueryExecutionId": id }).to_string())
}

/// `GetQueryExecution` response.
pub fn execution(state: &str, statement_type: &str, scanned: i64) -> Resp {
    Resp::ok(
        &json!({"QueryExecution": {
            "QueryExecutionId": "qid-1",
            "StatementType": statement_type,
            "Status": {"State": state},
            "Statistics": {"DataScannedInBytes": scanned},
        }})
        .to_string(),
    )
}

pub fn failed_execution(reason: &str, athena_message: &str) -> Resp {
    Resp::ok(
        &json!({"QueryExecution": {
            "QueryExecutionId": "qid-1",
            "StatementType": "DML",
            "Status": {"State": "FAILED", "StateChangeReason": reason,
                       "AthenaError": {"ErrorCategory": 2, "ErrorType": 1006, "Retryable": false,
                                       "ErrorMessage": athena_message}},
        }})
        .to_string(),
    )
}

/// `(name, type)` column definitions.
pub fn column_info(cols: &[(&str, &str)]) -> Value {
    Value::Array(
        cols.iter()
            .map(|(n, t)| {
                json!({"Name": n, "Label": n, "Type": t, "Precision": 0, "Scale": 0,
                       "Nullable": "UNKNOWN", "CaseSensitive": false})
            })
            .collect(),
    )
}

/// A row of optional cells; `None` is NULL (the `VarCharValue` key is absent).
pub fn row(cells: &[Option<&str>]) -> Value {
    json!({"Data": cells.iter().map(|c| match c {
        Some(v) => json!({"VarCharValue": v}),
        None => json!({}),
    }).collect::<Vec<_>>()})
}

pub fn results_page(
    cols: &[(&str, &str)],
    rows: Vec<Value>,
    next: Option<&str>,
    update_count: Option<i64>,
) -> Resp {
    let mut v = json!({"ResultSet": {
        "Rows": rows,
        "ResultSetMetadata": {"ColumnInfo": column_info(cols)},
    }});
    if let Some(n) = next {
        v["NextToken"] = json!(n);
    }
    if let Some(n) = update_count {
        v["UpdateCount"] = json!(n);
    }
    Resp::ok(&v.to_string())
}

/// Runs a script through `execute_stream` and collects every event.
pub async fn stream(
    e: &AthenaEngine,
    sql: &str,
    opts: vibelens_core::model::ExecOptions,
) -> Vec<QueryEvent> {
    let (tx, mut rx) = mpsc::channel(64);
    let cancel = tokio_util::sync::CancellationToken::new();
    e.execute_stream("q1".into(), sql, opts, cancel, tx)
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(ev) = rx.recv().await {
        out.push(ev);
    }
    out
}
