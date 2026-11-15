//! Shared helpers for the `aws_*` integration tests: a tiny in-process HTTP server that plays the
//! role of SSO-OIDC / SSO portal / Secrets Manager, so the real SDK clients can be exercised offline.
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
