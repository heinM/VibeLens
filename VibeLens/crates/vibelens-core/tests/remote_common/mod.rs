//! Shared helpers for the `remote_*` integration tests: an in-process HTTP/1.1 server (the
//! pattern of `athena_common`) that records every request and answers from a closure, with an
//! optional delay per answer, plus event helpers.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vibelens_core::model::*;

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
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// Wait this long before answering.
    pub delay: Option<Duration>,
}

impl Resp {
    pub fn json(status: u16, body: &serde_json::Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.to_string(),
            delay: None,
        }
    }
    pub fn ok(body: &serde_json::Value) -> Self {
        Self::json(200, body)
    }
    pub fn text(status: u16, body: &str) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), "text/plain".into())],
            body: body.to_string(),
            delay: None,
        }
    }
    pub fn redirect(location: &str) -> Self {
        Self {
            status: 307,
            headers: vec![("location".into(), location.into())],
            body: String::new(),
            delay: None,
        }
    }
    pub fn after(mut self, d: Duration) -> Self {
        self.delay = Some(d);
        self
    }
}

pub type Handler = Arc<dyn Fn(&Req) -> Resp + Send + Sync>;

pub struct MockServer {
    /// `http://127.0.0.1:<port>`
    pub url: String,
    pub port: u16,
    pub requests: Arc<Mutex<Vec<Req>>>,
}

impl MockServer {
    pub fn all(&self) -> Vec<Req> {
        self.requests.lock().unwrap().clone()
    }
    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
    /// Requests with this method whose path (without query) equals `path`.
    pub fn calls(&self, method: &str, path: &str) -> Vec<Req> {
        self.all()
            .into_iter()
            .filter(|r| r.method == method && r.path.split('?').next() == Some(path))
            .collect()
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
                if let Some(d) = resp.delay {
                    tokio::time::sleep(d).await;
                }
                let mut out = format!(
                    "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                    resp.status,
                    resp.body.len()
                );
                for (k, v) in &resp.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.write_all(resp.body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    MockServer {
        url: format!("http://{addr}"),
        port: addr.port(),
        requests,
    }
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

pub fn rows(events: &[QueryEvent]) -> Vec<Vec<Cell>> {
    events
        .iter()
        .filter_map(|e| match e {
            QueryEvent::Rows { rows, .. } => Some(rows.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

pub fn opts() -> ExecOptions {
    ExecOptions {
        page_size: 2,
        ..ExecOptions::default()
    }
}

pub fn timing() -> vibelens_core::remote::RemoteTiming {
    vibelens_core::remote::RemoteTiming {
        poll_initial: Duration::from_millis(5),
        poll_max: Duration::from_millis(20),
        retry_base: Duration::from_millis(5),
        cancel_timeout: Duration::from_secs(2),
    }
}

/// Runs `run` with a fresh channel and collects every event. `cancel_after` cancels the run
/// after that long.
pub async fn collect_events<F, Fut>(cancel_after: Option<Duration>, run: F) -> Vec<QueryEvent>
where
    F: FnOnce(CancellationToken, mpsc::Sender<QueryEvent>) -> Fut,
    Fut: std::future::Future<Output = vibelens_core::Result<()>>,
{
    let (tx, mut rx) = mpsc::channel(256);
    let cancel = CancellationToken::new();
    if let Some(d) = cancel_after {
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(d).await;
            c.cancel();
        });
    }
    run(cancel, tx).await.unwrap();
    let mut out = Vec::new();
    while let Some(ev) = rx.recv().await {
        out.push(ev);
    }
    out
}
