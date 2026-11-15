//! A tiny in-process Iceberg REST catalog: just enough of the spec for VibeLens's tests.
//!
//! Serves `/v1/config`, `/v1/oauth/tokens`, namespaces, tables and `load table`, with table
//! metadata taken from the pyiceberg-written fixture. Every request is recorded.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

#[derive(Default)]
struct State {
    requests: Vec<Recorded>,
    /// Tokens the server currently accepts (`None` = no auth required).
    valid_tokens: Option<HashSet<String>>,
    next_token: u32,
    /// `s3.endpoint` etc. the server vends in `/v1/config` and in load-table.
    vended: BTreeMap<String, String>,
}

pub struct MockRest {
    pub uri: String,
    state: Arc<Mutex<State>>,
    warehouse: PathBuf,
    _task: tokio::task::JoinHandle<()>,
}

impl MockRest {
    /// `require_auth`: reject requests without a currently valid bearer token.
    pub async fn start(warehouse: &str, require_auth: bool, vended: &[(&str, &str)]) -> MockRest {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State {
            valid_tokens: require_auth.then(HashSet::new),
            vended: vended
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..Default::default()
        }));
        let wh = PathBuf::from(warehouse);
        let (st, w2) = (state.clone(), wh.clone());
        let task = tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let (st, w) = (st.clone(), w2.clone());
                tokio::spawn(async move {
                    let _ = serve(sock, st, w).await;
                });
            }
        });
        MockRest {
            uri,
            state,
            warehouse: wh,
            _task: task,
        }
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.state.lock().unwrap().requests.clone()
    }

    /// Makes every token issued so far invalid (the next call must re-authenticate).
    pub fn revoke_tokens(&self) {
        if let Some(v) = self.state.lock().unwrap().valid_tokens.as_mut() {
            v.clear();
        }
    }

    /// Accept this static bearer token.
    pub fn accept_token(&self, token: &str) {
        self.state
            .lock()
            .unwrap()
            .valid_tokens
            .get_or_insert_with(HashSet::new)
            .insert(token.to_string());
    }

    pub fn token_requests(&self) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|r| r.path.starts_with("/v1/oauth/tokens"))
            .collect()
    }
}

async fn serve(
    mut sock: TcpStream,
    st: Arc<Mutex<State>>,
    warehouse: PathBuf,
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        // Read one request (headers + content-length body); loop for keep-alive.
        let head_end = loop {
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
            let mut chunk = [0u8; 4096];
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default().to_string();
        let mut parts = request_line.split_whitespace();
        let (method, path) = (
            parts.next().unwrap_or_default().to_string(),
            parts.next().unwrap_or_default().to_string(),
        );
        let headers: BTreeMap<String, String> = lines
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            })
            .collect();
        let len: usize = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let mut chunk = [0u8; 4096];
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&buf[head_end..head_end + len]).to_string();
        buf.drain(..head_end + len);

        let rec = Recorded {
            method: method.clone(),
            path: path.clone(),
            headers: headers.clone(),
            body: body.clone(),
        };
        let (status, payload) = route(&st, &warehouse, rec);
        let text = payload.to_string();
        let resp = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{text}",
            text.len()
        );
        sock.write_all(resp.as_bytes()).await?;
    }
}

fn error(code: u16, kind: &str, msg: &str) -> (String, Value) {
    let reason = match code {
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    (
        format!("{code} {reason}"),
        json!({"error": {"message": msg, "type": kind, "code": code}}),
    )
}

fn route(st: &Arc<Mutex<State>>, warehouse: &std::path::Path, rec: Recorded) -> (String, Value) {
    let mut s = st.lock().unwrap();
    s.requests.push(rec.clone());
    let path = rec.path.split('?').next().unwrap_or_default().to_string();

    if path == "/v1/oauth/tokens" {
        s.next_token += 1;
        let tok = format!("tok-{}", s.next_token);
        if let Some(v) = s.valid_tokens.as_mut() {
            v.insert(tok.clone());
        }
        return (
            "200 OK".into(),
            json!({"access_token": tok, "token_type": "Bearer", "expires_in": 3600}),
        );
    }
    if let Some(valid) = &s.valid_tokens {
        let ok = rec
            .headers
            .get("authorization")
            .and_then(|h| h.strip_prefix("Bearer "))
            .is_some_and(|t| valid.contains(t));
        if !ok {
            return error(401, "NotAuthorizedException", "token missing or expired");
        }
    }
    let vended = s.vended.clone();
    drop(s);
    let segs: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match segs.as_slice() {
        ["v1", "config"] => (
            "200 OK".into(),
            json!({"defaults": vended, "overrides": {}}),
        ),
        ["v1", "namespaces"] => ("200 OK".into(), json!({"namespaces": [["shop"]]})),
        ["v1", "namespaces", ns] if *ns == "shop" => (
            "200 OK".into(),
            json!({"namespace": ["shop"], "properties": {"owner": "mock"}}),
        ),
        ["v1", "namespaces", ns, "tables"] if *ns == "shop" => (
            "200 OK".into(),
            json!({"identifiers": [{"namespace": ["shop"], "name": "customers"}, {"namespace": ["shop"], "name": "orders"}]}),
        ),
        ["v1", "namespaces", ns, "tables", name] if *ns == "shop" => {
            match latest_metadata(warehouse, name) {
                Some((location, metadata)) => (
                    "200 OK".into(),
                    json!({"metadata-location": location, "metadata": metadata, "config": vended}),
                ),
                None => error(404, "NoSuchTableException", "no such table"),
            }
        }
        _ => error(404, "NoSuchNamespaceException", &format!("no route {path}")),
    }
}

/// The newest `*.metadata.json` of `shop/<name>` in the warehouse.
fn latest_metadata(warehouse: &std::path::Path, name: &str) -> Option<(String, Value)> {
    let dir = warehouse.join("shop").join(name).join("metadata");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".metadata.json"))
        })
        .collect();
    files.sort();
    let last = files.pop()?;
    let v: Value = serde_json::from_slice(&std::fs::read(&last).ok()?).ok()?;
    Some((format!("file://{}", last.display()), v))
}
