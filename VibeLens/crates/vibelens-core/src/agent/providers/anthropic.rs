//! Anthropic Messages API: request builder and SSE decoder.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{Flow, LlmRequest, LlmResponse, ProviderClient, SseDecoder, parse_tool_args, redact};
use crate::agent::sse::SseEvent;
use crate::error::{CoreError, Result};
use crate::model::{ChatMessage, ChatRole, ContentBlock, Usage};

const API_VERSION: &str = "2023-06-01";

pub(super) async fn stream(
    p: &ProviderClient,
    req: &LlmRequest,
    on_text: &mut (dyn FnMut(&str) + Send),
) -> Result<LlmResponse> {
    let headers = p
        .key_str()
        .map(|k| vec![("x-api-key", k.to_string())])
        .unwrap_or_default();
    stream_at(p, &p.url("/v1/messages"), headers, req, on_text).await
}

/// [`stream`] against an explicit URL with the caller's authentication headers (gateways).
/// `anthropic-version` is always added.
pub(super) async fn stream_at(
    p: &ProviderClient,
    url: &str,
    mut headers: Vec<(&'static str, String)>,
    req: &LlmRequest,
    on_text: &mut (dyn FnMut(&str) + Send),
) -> Result<LlmResponse> {
    let body = build_body(req);
    headers.push(("anthropic-version", API_VERSION.to_string()));
    let resp = p.post(url, headers, &body).await?;
    if !resp.status().is_success() {
        return Err(p.http_error(resp).await);
    }
    p.decode(resp, AnthropicDecoder::default(), on_text).await
}

/// Maps our provider-neutral transcript to Anthropic content blocks.
pub(super) fn build_body(req: &LlmRequest) -> Value {
    let mut messages = Vec::new();
    for m in &req.messages {
        let content = message_blocks(m);
        if content.is_empty() {
            continue;
        }
        let role = match m.role {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        };
        messages.push(json!({ "role": role, "content": content }));
    }
    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_tokens,
        "messages": messages,
        "stream": true,
    });
    if !req.system.is_empty() {
        body["system"] = json!(req.system);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema })).collect(),
        );
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    body
}

fn message_blocks(m: &ChatMessage) -> Vec<Value> {
    let mut results = Vec::new();
    let mut rest = Vec::new();
    for b in &m.content {
        match b {
            ContentBlock::Text { text } if text.trim().is_empty() => {}
            ContentBlock::Text { text } => rest.push(json!({ "type": "text", "text": text })),
            ContentBlock::ToolUse { id, name, input } => {
                rest.push(json!({ "type": "tool_use", "id": id, "name": name, "input": input }))
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut v = json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content });
                if *is_error {
                    v["is_error"] = json!(true);
                }
                results.push(v);
            }
        }
    }
    // The API requires tool_result blocks to come first in a user message.
    results.extend(rest);
    results
}

enum Block {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        json: String,
        input: Option<Value>,
    },
    /// thinking / redacted_thinking / server tools: not forwarded.
    Other,
}

#[derive(Default)]
pub(super) struct AnthropicDecoder {
    blocks: BTreeMap<usize, Block>,
    usage: Usage,
    stop_reason: Option<String>,
    stopped: bool,
}

fn idx(v: &Value) -> usize {
    v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize
}

impl SseDecoder for AnthropicDecoder {
    fn on_event(&mut self, ev: &SseEvent, on_text: &mut dyn FnMut(&str)) -> Result<Flow> {
        let kind = ev.event.as_deref().unwrap_or("");
        if kind == "ping" || ev.data.trim().is_empty() {
            return Ok(Flow::Continue);
        }
        let v: Value = serde_json::from_str(&ev.data)
            .map_err(|e| CoreError::Llm(format!("malformed event from Anthropic: {e}")))?;
        // The event name is redundant with data.type; trust data.type, fall back to the name.
        let ty = v.get("type").and_then(Value::as_str).unwrap_or(kind);
        match ty {
            "message_start" => {
                if let Some(u) = v.pointer("/message/usage") {
                    self.absorb_usage(u);
                }
            }
            "content_block_start" => {
                let cb = v.get("content_block").cloned().unwrap_or(Value::Null);
                let block = match cb.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        let initial = cb.get("text").and_then(Value::as_str).unwrap_or("");
                        if !initial.is_empty() {
                            on_text(initial);
                        }
                        Block::Text(initial.to_string())
                    }
                    Some("tool_use") => Block::ToolUse {
                        id: cb
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: cb
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        json: String::new(),
                        input: None,
                    },
                    _ => Block::Other,
                };
                self.blocks.insert(idx(&v), block);
            }
            "content_block_delta" => {
                let delta = v.get("delta").cloned().unwrap_or(Value::Null);
                match (
                    delta.get("type").and_then(Value::as_str),
                    self.blocks.get_mut(&idx(&v)),
                ) {
                    (Some("text_delta"), Some(Block::Text(t))) => {
                        if let Some(s) = delta.get("text").and_then(Value::as_str) {
                            t.push_str(s);
                            on_text(s);
                        }
                    }
                    (Some("input_json_delta"), Some(Block::ToolUse { json, .. })) => {
                        if let Some(s) = delta.get("partial_json").and_then(Value::as_str) {
                            json.push_str(s);
                        }
                    }
                    _ => {} // thinking_delta, signature_delta, citations…
                }
            }
            "content_block_stop" => {
                if let Some(Block::ToolUse { json, input, .. }) = self.blocks.get_mut(&idx(&v)) {
                    *input = Some(parse_tool_args(json));
                }
            }
            "message_delta" => {
                if let Some(r) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(r.to_string());
                }
                if let Some(u) = v.get("usage") {
                    self.absorb_usage(u);
                }
            }
            "message_stop" => {
                self.stopped = true;
                return Ok(Flow::Done);
            }
            "error" => {
                let etype = v
                    .pointer("/error/type")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                let msg = v
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                return Err(CoreError::Llm(format!(
                    "Anthropic stream error ({etype}): {}",
                    redact(msg, None)
                )));
            }
            _ => {} // unknown event types are ignored for forward compatibility
        }
        Ok(Flow::Continue)
    }

    fn finish(self) -> Result<LlmResponse> {
        if !self.stopped && self.stop_reason.is_none() {
            return Err(CoreError::Llm(
                "the stream from Anthropic ended before the message completed".into(),
            ));
        }
        let mut content = Vec::new();
        for (_, block) in self.blocks {
            match block {
                Block::Text(t) if !t.is_empty() => content.push(ContentBlock::Text { text: t }),
                Block::Text(_) | Block::Other => {}
                Block::ToolUse {
                    id,
                    name,
                    json,
                    input,
                } => {
                    let input = input.unwrap_or_else(|| parse_tool_args(&json));
                    content.push(ContentBlock::ToolUse { id, name, input });
                }
            }
        }
        Ok(LlmResponse {
            content,
            stop_reason: self.stop_reason.unwrap_or_else(|| "end_turn".into()),
            usage: self.usage,
        })
    }
}

impl AnthropicDecoder {
    fn absorb_usage(&mut self, u: &Value) {
        let get = |k: &str| u.get(k).and_then(Value::as_u64);
        if let Some(i) = get("input_tokens") {
            // Cached prompt tokens are still prompt tokens for accounting purposes.
            self.usage.input_tokens = i
                + get("cache_creation_input_tokens").unwrap_or(0)
                + get("cache_read_input_tokens").unwrap_or(0);
        }
        if let Some(o) = get("output_tokens") {
            self.usage.output_tokens = o;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::providers::{ToolSpec, decode_chunks};

    /// A recorded-style stream: text, then two tool calls whose JSON arrives in fragments.
    pub(crate) const FIXTURE: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_01\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-test\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":472,\"output_tokens\":2}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: ping\n",
        "data: {\"type\": \"ping\"}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Let me look \"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"at the schema and a sample.\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01A\",\"name\":\"get_schema\",\"input\":{}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"profile_id\\\": \\\"p\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"1\\\", \\\"schema\\\":\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\" \\\"public\\\"}\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":1}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":2,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01B\",\"name\":\"list_files\",\"input\":{}}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":2}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":89}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    fn run(chunks: &[&[u8]]) -> (Result<LlmResponse>, String) {
        let mut text = String::new();
        let r = decode_chunks(AnthropicDecoder::default(), chunks, &mut |t| {
            text.push_str(t)
        });
        (r, text)
    }

    #[test]
    fn decodes_text_and_two_fragmented_tool_calls() {
        let (r, text) = run(&[FIXTURE.as_bytes()]);
        let r = r.unwrap();
        assert_eq!(text, "Let me look at the schema and a sample.");
        assert_eq!(r.stop_reason, "tool_use");
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 472,
                output_tokens: 89
            }
        );
        assert_eq!(
            r.content,
            vec![
                ContentBlock::Text {
                    text: "Let me look at the schema and a sample.".into()
                },
                ContentBlock::ToolUse {
                    id: "toolu_01A".into(),
                    name: "get_schema".into(),
                    input: json!({"profile_id": "p1", "schema": "public"})
                },
                ContentBlock::ToolUse {
                    id: "toolu_01B".into(),
                    name: "list_files".into(),
                    input: json!({})
                },
            ]
        );
    }

    #[test]
    fn decodes_when_chunked_arbitrarily() {
        let bytes = FIXTURE.as_bytes();
        for size in [1usize, 3, 7, 64, 500] {
            let chunks: Vec<&[u8]> = bytes.chunks(size).collect();
            let (r, text) = run(&chunks);
            let r = r.unwrap_or_else(|e| panic!("chunk size {size}: {e}"));
            assert_eq!(
                text, "Let me look at the schema and a sample.",
                "chunk size {size}"
            );
            assert_eq!(r.tool_uses().count(), 2);
        }
    }

    #[test]
    fn plain_text_end_turn() {
        let s = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":1,\"cache_read_input_tokens\":5}}}\n\n",
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Done.\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let (r, text) = run(&[s.as_bytes()]);
        let r = r.unwrap();
        assert_eq!(text, "Done.");
        assert_eq!(r.stop_reason, "end_turn");
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 15,
                output_tokens: 3
            }
        );
    }

    #[test]
    fn crlf_stream_and_thinking_blocks_ignored() {
        let s = FIXTURE.replace('\n', "\r\n");
        let with_thinking = format!(
            "event: content_block_start\r\ndata: {{\"type\":\"content_block_start\",\"index\":9,\"content_block\":{{\"type\":\"thinking\",\"thinking\":\"\"}}}}\r\n\r\nevent: content_block_delta\r\ndata: {{\"type\":\"content_block_delta\",\"index\":9,\"delta\":{{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}}}\r\n\r\n{s}"
        );
        let (r, text) = run(&[with_thinking.as_bytes()]);
        let r = r.unwrap();
        assert_eq!(text, "Let me look at the schema and a sample.");
        assert_eq!(r.content.len(), 3);
    }

    #[test]
    fn error_event_is_surfaced() {
        let s = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let (r, _) = run(&[s.as_bytes()]);
        let e = r.unwrap_err();
        assert!(matches!(e, CoreError::Llm(_)));
        assert!(
            e.to_string().contains("overloaded_error") && e.to_string().contains("Overloaded"),
            "{e}"
        );
    }

    #[test]
    fn truncated_stream_is_an_error() {
        let cut = &FIXTURE[..FIXTURE.find("event: message_delta").unwrap()];
        let (r, _) = run(&[cut.as_bytes()]);
        assert!(matches!(r, Err(CoreError::Llm(_))));
    }

    #[test]
    fn invalid_tool_json_is_reported_not_fatal() {
        let s = concat!(
            "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t\",\"name\":\"run_sql\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"sql\\\": \\\"sel\"}}\n\n",
            "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":5}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        );
        let (r, _) = run(&[s.as_bytes()]);
        let r = r.unwrap();
        assert_eq!(r.stop_reason, "max_tokens");
        let (_, _, input) = r.tool_uses().next().unwrap();
        assert!(input.get("_invalid_arguments").is_some());
    }

    #[test]
    fn request_body_shape() {
        let req = LlmRequest {
            system: "sys".into(),
            messages: vec![
                ChatMessage {
                    role: ChatRole::User,
                    content: vec![ContentBlock::Text { text: "hi".into() }],
                },
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: vec![
                        ContentBlock::Text { text: "".into() },
                        ContentBlock::Text {
                            text: "checking".into(),
                        },
                        ContentBlock::ToolUse {
                            id: "t1".into(),
                            name: "list_files".into(),
                            input: json!({}),
                        },
                    ],
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: vec![
                        ContentBlock::Text {
                            text: "also".into(),
                        },
                        ContentBlock::ToolResult {
                            tool_use_id: "t1".into(),
                            content: "[]".into(),
                            is_error: false,
                        },
                        ContentBlock::ToolResult {
                            tool_use_id: "t2".into(),
                            content: "boom".into(),
                            is_error: true,
                        },
                    ],
                },
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: vec![],
                },
            ],
            tools: vec![ToolSpec {
                name: "list_files".into(),
                description: "d".into(),
                input_schema: json!({"type":"object","properties":{}}),
            }],
            model: "claude-x".into(),
            max_tokens: 1000,
            temperature: Some(0.2),
        };
        let b = build_body(&req);
        assert_eq!(b["model"], "claude-x");
        assert_eq!(b["max_tokens"], 1000);
        assert_eq!(b["stream"], true);
        assert_eq!(b["system"], "sys");
        assert!((b["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "empty assistant message dropped");
        assert_eq!(
            msgs[1]["content"].as_array().unwrap().len(),
            2,
            "empty text block dropped"
        );
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        // tool_results first, then text.
        assert_eq!(
            msgs[2]["content"][0],
            json!({"type":"tool_result","tool_use_id":"t1","content":"[]"})
        );
        assert_eq!(
            msgs[2]["content"][1],
            json!({"type":"tool_result","tool_use_id":"t2","content":"boom","is_error":true})
        );
        assert_eq!(msgs[2]["content"][2]["type"], "text");
    }

    #[test]
    fn minimal_body_omits_optional_fields() {
        let req = LlmRequest {
            system: String::new(),
            messages: vec![],
            tools: vec![],
            model: "m".into(),
            max_tokens: 5,
            temperature: None,
        };
        let b = build_body(&req);
        assert!(
            b.get("system").is_none() && b.get("tools").is_none() && b.get("temperature").is_none()
        );
    }
}
