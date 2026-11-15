//! LLM providers: two wire formats cover everything we support.
//!
//! * [`anthropic`]: Anthropic Messages API (`/v1/messages`, SSE with `content_block_*` events)
//! * [`openai`]: OpenAI-compatible Chat Completions (`/chat/completions`, SSE with `delta` chunks)
//!   which also serves xAI, Gemini's compatibility endpoint, Ollama, LM Studio and vLLM.
//! * [`portkey`]: Portkey AI Gateway, which speaks either of the two above; only the URL,
//!   authentication and routing headers differ.
//!
//! Each wire format is split into a pure *request builder* and a pure *stream decoder*
//! (a state machine fed one [`SseEvent`] at a time), so both are tested against recorded
//! streams without any network.

pub(crate) mod anthropic;
pub(crate) mod openai;
pub(crate) mod portkey;

use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use super::sse::{SseEvent, SseParser};
use crate::error::{CoreError, Result};
use crate::model::{ContentBlock, ProviderConfig, ProviderKind, Usage};

/// Longest we wait for the next network chunk. Generous: local models may need to load first.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// How much of an error body we read.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// A tool the model may call. `input_schema` is a JSON Schema object.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Everything a provider needs for one model call.
#[derive(Debug, Clone)]
pub(crate) struct LlmRequest {
    pub system: String,
    pub messages: Vec<crate::model::ChatMessage>,
    pub tools: Vec<ToolSpec>,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
}

/// One completed model response.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LlmResponse {
    pub content: Vec<ContentBlock>,
    /// Normalised: `end_turn`, `tool_use`, `max_tokens`, `stop_sequence`, or the provider's own word.
    pub stop_reason: String,
    pub usage: Usage,
}

impl LlmResponse {
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }
}

pub(crate) enum Flow {
    Continue,
    Done,
}

/// A wire-format stream decoder.
pub(crate) trait SseDecoder: Sized {
    fn on_event(&mut self, ev: &SseEvent, on_text: &mut dyn FnMut(&str)) -> Result<Flow>;
    fn finish(self) -> Result<LlmResponse>;
}

/// Feeds bytes through the SSE parser into the decoder. Returns true once the stream is over.
pub(crate) fn feed_chunk<D: SseDecoder>(
    parser: &mut SseParser,
    dec: &mut D,
    bytes: &[u8],
    on_text: &mut dyn FnMut(&str),
) -> Result<bool> {
    for ev in parser.feed(bytes) {
        if let Flow::Done = dec.on_event(&ev, on_text)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Decodes a complete recorded stream (tests, fixtures).
#[cfg(test)]
pub(crate) fn decode_chunks<D: SseDecoder>(
    mut dec: D,
    chunks: &[&[u8]],
    on_text: &mut dyn FnMut(&str),
) -> Result<LlmResponse> {
    let mut parser = SseParser::new();
    let mut done = false;
    for c in chunks {
        if feed_chunk(&mut parser, &mut dec, c, on_text)? {
            done = true;
            break;
        }
    }
    if !done {
        for ev in parser.finish() {
            if let Flow::Done = dec.on_event(&ev, on_text)? {
                break;
            }
        }
    }
    dec.finish()
}

/// Reads the response body as an SSE stream into `dec`.
async fn read_stream<D: SseDecoder>(
    resp: reqwest::Response,
    mut dec: D,
    on_text: &mut (dyn FnMut(&str) + Send),
    key: Option<&str>,
) -> Result<LlmResponse> {
    use futures_util::StreamExt;
    let mut parser = SseParser::new();
    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::time::timeout(IDLE_TIMEOUT, stream.next())
            .await
            .map_err(|_| CoreError::Llm("the provider stopped sending data (timed out)".into()))?;
        match next {
            Some(Ok(bytes)) => {
                if feed_chunk(&mut parser, &mut dec, &bytes, on_text)? {
                    return dec.finish();
                }
            }
            Some(Err(e)) => {
                return Err(CoreError::Llm(format!(
                    "stream interrupted: {}",
                    redact(&describe_reqwest_error(&e), key)
                )));
            }
            None => break,
        }
    }
    for ev in parser.finish() {
        if let Flow::Done = dec.on_event(&ev, on_text)? {
            break;
        }
    }
    dec.finish()
}

/// A provider bound to its config, key and HTTP client.
pub(crate) struct ProviderClient {
    pub(crate) cfg: ProviderConfig,
    key: Option<SecretString>,
    http: reqwest::Client,
}

impl ProviderClient {
    /// Fails with `Invalid` when the config is unusable (no model, missing key).
    pub fn new(
        cfg: ProviderConfig,
        key: Option<SecretString>,
        http: reqwest::Client,
    ) -> Result<Self> {
        if cfg.model.trim().is_empty() {
            return Err(CoreError::Invalid(format!(
                "set a model id for {} in Settings",
                cfg.label
            )));
        }
        if cfg.kind == ProviderKind::Portkey {
            portkey::validate(&cfg)?;
        }
        let key = key.filter(|k| !k.expose_secret().trim().is_empty());
        if cfg.requires_api_key && key.is_none() {
            return Err(CoreError::Invalid(format!(
                "add an API key for {} in Settings",
                cfg.label
            )));
        }
        Ok(Self { cfg, key, http })
    }

    pub fn model(&self) -> &str {
        &self.cfg.model
    }

    pub fn max_tokens(&self) -> u32 {
        self.cfg.max_tokens
    }

    pub fn temperature(&self) -> Option<f32> {
        self.cfg.temperature
    }

    /// Streams one completion. `on_text` receives text deltas as they arrive.
    pub async fn stream(
        &self,
        req: &LlmRequest,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<LlmResponse> {
        match self.cfg.kind {
            ProviderKind::Anthropic => anthropic::stream(self, req, on_text).await,
            ProviderKind::OpenAiCompatible => openai::stream(self, req, on_text).await,
            ProviderKind::Portkey => {
                let headers = portkey::headers(self.cfg.portkey.as_ref(), self.key_str());
                match portkey::api_format(&self.cfg) {
                    crate::model::PortkeyApiFormat::ChatCompletions => {
                        let url = portkey::chat_url(&self.cfg.base_url);
                        openai::stream_at(self, &url, headers, req, on_text).await
                    }
                    crate::model::PortkeyApiFormat::AnthropicMessages => {
                        let url = portkey::messages_url(&self.cfg.base_url);
                        anthropic::stream_at(self, &url, headers, req, on_text).await
                    }
                }
            }
        }
    }

    pub(super) fn key_str(&self) -> Option<&str> {
        self.key.as_ref().map(|k| k.expose_secret())
    }

    /// `base_url` + `path`, tolerating a trailing slash on the base.
    pub(super) fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    /// POSTs `body` as JSON with `headers` plus the config's `extra_headers`.
    pub(super) async fn post(
        &self,
        url: &str,
        headers: Vec<(&'static str, String)>,
        body: &Value,
    ) -> Result<reqwest::Response> {
        let mut rb = self
            .http
            .post(url)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        for (k, v) in headers {
            rb = rb.header(k, v);
        }
        for (k, v) in &self.cfg.extra_headers {
            rb = rb.header(k.as_str(), v.as_str());
        }
        let body_bytes = serde_json::to_vec(body)?;
        let resp = rb.body(body_bytes).send().await.map_err(|e| {
            CoreError::Llm(format!(
                "could not reach {}: {}",
                self.cfg.label,
                redact(&describe_reqwest_error(&e), self.key_str())
            ))
        })?;
        Ok(resp)
    }

    /// Reads a non-2xx response into an `Llm` error carrying the provider's own message.
    pub(super) async fn http_error(&self, resp: reqwest::Response) -> CoreError {
        let (status, body) = self.read_error_body(resp).await;
        self.error_from(status, &body)
    }

    /// Builds the user-facing error for a non-2xx status and its body.
    pub(super) fn error_from(&self, status: u16, body: &str) -> CoreError {
        CoreError::Llm(format!(
            "{} returned HTTP {}: {}",
            self.cfg.label,
            status,
            error_message_from_body(body, self.key_str())
        ))
    }

    pub(super) async fn read_error_body(&self, resp: reqwest::Response) -> (u16, String) {
        use futures_util::StreamExt;
        let status = resp.status().as_u16();
        let mut body = Vec::new();
        let mut stream = resp.bytes_stream();
        while let Ok(Some(Ok(chunk))) =
            tokio::time::timeout(Duration::from_secs(15), stream.next()).await
        {
            body.extend_from_slice(&chunk);
            if body.len() >= MAX_ERROR_BODY {
                break;
            }
        }
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    pub(super) async fn decode<D: SseDecoder>(
        &self,
        resp: reqwest::Response,
        dec: D,
        on_text: &mut (dyn FnMut(&str) + Send),
    ) -> Result<LlmResponse> {
        read_stream(resp, dec, on_text, self.key_str()).await
    }
}

/// `reqwest::Error` with its source chain flattened and the URL removed.
fn describe_reqwest_error(e: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        let part = s.to_string();
        if !msg.contains(&part) {
            msg.push_str(": ");
            msg.push_str(&part);
        }
        src = s.source();
    }
    msg
}

/// Pulls a human message out of a provider error body (Anthropic, OpenAI and Gemini shapes),
/// falling back to the raw text. Always redacted and bounded.
pub(crate) fn error_message_from_body(body: &str, key: Option<&str>) -> String {
    fn find(v: &Value) -> Option<String> {
        match v {
            Value::Array(items) => items.iter().find_map(find),
            Value::Object(map) => {
                if let Some(err) = map.get("error") {
                    match err {
                        Value::String(s) => return Some(s.clone()),
                        Value::Object(e) => {
                            if let Some(Value::String(m)) = e.get("message") {
                                return Some(m.clone());
                            }
                        }
                        _ => {}
                    }
                }
                map.get("message")
                    .and_then(|m| m.as_str())
                    .map(String::from)
            }
            _ => None,
        }
    }
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .as_ref()
        .and_then(find)
        .unwrap_or_else(|| {
            let t = body.trim();
            if t.is_empty() {
                "(empty response body)".to_string()
            } else {
                t.to_string()
            }
        });
    let mut msg = redact(&msg, key);
    if msg.chars().count() > 600 {
        msg = msg.chars().take(600).collect::<String>() + "…";
    }
    msg
}

/// Removes anything that looks like an API key from text destined for logs or the UI.
pub(crate) fn redact(text: &str, key: Option<&str>) -> String {
    let mut s = text.to_string();
    if let Some(k) = key.map(str::trim).filter(|k| k.len() >= 8) {
        s = s.replace(k, "[redacted]");
    }
    let mut out = String::with_capacity(s.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if !token.is_empty() {
            if looks_like_secret(token) {
                out.push_str("[redacted]");
            } else {
                out.push_str(token);
            }
            token.clear();
        }
    };
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            token.push(c);
        } else {
            flush(&mut token, &mut out);
            out.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

fn looks_like_secret(t: &str) -> bool {
    let prefixed = ["sk-", "xai-", "AIza", "gsk_", "pk-", "key-"]
        .iter()
        .any(|p| t.starts_with(p))
        && t.len() >= 12;
    let long_mixed = t.len() >= 32
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars().any(|c| c.is_ascii_alphabetic());
    prefixed || long_mixed
}

/// Parses accumulated tool-call argument text. Empty means "no arguments".
pub(crate) fn parse_tool_args(raw: &str) -> Value {
    let t = raw.trim();
    if t.is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(t).unwrap_or_else(|_| {
        // Truncated or malformed JSON: surface it so the tool layer reports a clear error.
        let preview: String = t.chars().take(300).collect();
        serde_json::json!({ "_invalid_arguments": preview })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_keys() {
        let s = redact(
            "Incorrect API key provided: sk-proj-abcdefghijklmnop. Bearer 0123456789abcdef0123456789abcdef01",
            None,
        );
        assert!(!s.contains("sk-proj"));
        assert!(!s.contains("0123456789abcdef"));
        assert!(s.contains("Incorrect API key provided"));
        let s = redact("bad key hunter2hunter2", Some("hunter2hunter2"));
        assert_eq!(s, "bad key [redacted]");
        // Ordinary words and short ids survive.
        assert_eq!(
            redact("model `foo-bar` not found (req_123)", None),
            "model `foo-bar` not found (req_123)"
        );
    }

    #[test]
    fn extracts_error_messages() {
        let a = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(error_message_from_body(a, None), "invalid x-api-key");
        let o = r#"{"error":{"message":"The model `x` does not exist","type":"invalid_request_error"}}"#;
        assert_eq!(
            error_message_from_body(o, None),
            "The model `x` does not exist"
        );
        let g = r#"[{"error":{"code":400,"message":"API key not valid. Please pass a valid API key.","status":"INVALID_ARGUMENT"}}]"#;
        assert_eq!(
            error_message_from_body(g, None),
            "API key not valid. Please pass a valid API key."
        );
        assert_eq!(
            error_message_from_body(r#"{"error":"model not loaded"}"#, None),
            "model not loaded"
        );
        assert_eq!(
            error_message_from_body("<html>502 Bad Gateway</html>", None),
            "<html>502 Bad Gateway</html>"
        );
        assert_eq!(error_message_from_body("", None), "(empty response body)");
    }

    #[test]
    fn tool_args() {
        assert_eq!(parse_tool_args(""), serde_json::json!({}));
        assert_eq!(parse_tool_args("  "), serde_json::json!({}));
        assert_eq!(parse_tool_args(r#"{"a":1}"#), serde_json::json!({"a":1}));
        assert!(
            parse_tool_args(r#"{"a":"#)
                .get("_invalid_arguments")
                .is_some()
        );
    }
}
