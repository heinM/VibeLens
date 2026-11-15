//! OpenAI-compatible Chat Completions: request builder and SSE decoder.
//! Serves OpenAI-shaped endpoints generally: xAI, Gemini's compatibility endpoint, Ollama,
//! LM Studio, vLLM, gateways. Servers differ in small ways (missing `index`, empty ids,
//! object-valued `arguments`, `finish_reason: "stop"` alongside tool calls); the decoder is
//! lenient about all of them.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{Flow, LlmRequest, LlmResponse, ProviderClient, SseDecoder, parse_tool_args, redact};
use crate::agent::sse::SseEvent;
use crate::error::{CoreError, Result};
use crate::model::{ChatRole, ContentBlock, Usage};

pub(super) async fn stream(
    p: &ProviderClient,
    req: &LlmRequest,
    on_text: &mut (dyn FnMut(&str) + Send),
) -> Result<LlmResponse> {
    let headers = p
        .key_str()
        .map(|k| vec![("authorization", format!("Bearer {k}"))])
        .unwrap_or_default();
    stream_at(p, &p.url("/chat/completions"), headers, req, on_text).await
}

/// [`stream`] against an explicit URL with the caller's authentication headers (gateways).
pub(super) async fn stream_at(
    p: &ProviderClient,
    url: &str,
    headers: Vec<(&'static str, String)>,
    req: &LlmRequest,
    on_text: &mut (dyn FnMut(&str) + Send),
) -> Result<LlmResponse> {
    let mut with_usage = true;
    loop {
        let body = build_body(req, with_usage);
        let resp = p.post(url, headers.clone(), &body).await?;
        if resp.status().is_success() {
            return p.decode(resp, OpenAiDecoder::default(), on_text).await;
        }
        let (status, text) = p.read_error_body(resp).await;
        // Some compatible servers reject `stream_options`; retry once without usage reporting.
        if with_usage && status == 400 && text.to_ascii_lowercase().contains("stream_options") {
            with_usage = false;
            continue;
        }
        return Err(p.error_from(status, &text));
    }
}

pub(super) fn build_body(req: &LlmRequest, include_usage: bool) -> Value {
    let mut messages = Vec::new();
    if !req.system.is_empty() {
        messages.push(json!({ "role": "system", "content": req.system }));
    }
    for m in &req.messages {
        match m.role {
            ChatRole::Assistant => {
                let text: String = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let calls: Vec<Value> = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolUse { id, name, input } => Some(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": input.to_string() },
                        })),
                        _ => None,
                    })
                    .collect();
                if text.trim().is_empty() && calls.is_empty() {
                    continue;
                }
                let mut msg = json!({ "role": "assistant", "content": if text.trim().is_empty() { Value::Null } else { json!(text) } });
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                messages.push(msg);
            }
            ChatRole::User => {
                // Tool results must directly follow the assistant message that asked for them.
                for b in &m.content {
                    if let ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = b
                    {
                        messages.push(json!({ "role": "tool", "tool_call_id": tool_use_id, "content": content }));
                    }
                }
                let text: String = m
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.trim().is_empty() {
                    messages.push(json!({ "role": "user", "content": text }));
                }
            }
        }
    }
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "max_tokens": req.max_tokens,
    });
    if include_usage {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if !req.tools.is_empty() {
        body["tools"] = Value::Array(
            req.tools
                .iter()
                .map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.input_schema } }))
                .collect(),
        );
    }
    body
}

#[derive(Default)]
struct CallAcc {
    id: String,
    name: String,
    args: String,
}

#[derive(Default)]
pub(super) struct OpenAiDecoder {
    text: String,
    calls: Vec<CallAcc>,
    /// Provider `index` → position in `calls`.
    by_index: HashMap<i64, usize>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
    saw_done: bool,
    saw_chunk: bool,
}

impl OpenAiDecoder {
    fn apply_tool_call(&mut self, tc: &Value) {
        let index = tc.get("index").and_then(Value::as_i64);
        let id = tc
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let pos = match (index, id) {
            (Some(i), _) => match self.by_index.get(&i).copied() {
                // Same index but a different non-empty id: some servers reuse index 0 for
                // every parallel call. Treat it as a new call.
                Some(p)
                    if id.is_some_and(|id| {
                        !self.calls[p].id.is_empty() && self.calls[p].id != id
                    }) =>
                {
                    self.calls.push(CallAcc::default());
                    self.by_index.insert(i, self.calls.len() - 1);
                    self.calls.len() - 1
                }
                Some(p) => p,
                None => {
                    self.calls.push(CallAcc::default());
                    self.by_index.insert(i, self.calls.len() - 1);
                    self.calls.len() - 1
                }
            },
            // No index: a new id starts a new call, otherwise continue the last one.
            (None, Some(_)) => {
                self.calls.push(CallAcc::default());
                self.calls.len() - 1
            }
            (None, None) => {
                if self.calls.is_empty() {
                    self.calls.push(CallAcc::default());
                }
                self.calls.len() - 1
            }
        };
        let call = &mut self.calls[pos];
        if let Some(id) = id
            && call.id.is_empty()
        {
            call.id = id.to_string();
        }
        if let Some(f) = tc.get("function") {
            if let Some(n) = f
                .get("name")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
                && call.name.is_empty()
            {
                call.name = n.to_string();
            }
            match f.get("arguments") {
                Some(Value::String(s)) => call.args.push_str(s),
                // Ollama and a few others send the parsed object in one piece.
                Some(v @ Value::Object(_)) => call.args = v.to_string(),
                _ => {}
            }
        }
    }
}

impl SseDecoder for OpenAiDecoder {
    fn on_event(&mut self, ev: &SseEvent, on_text: &mut dyn FnMut(&str)) -> Result<Flow> {
        let data = ev.data.trim();
        if data == "[DONE]" {
            self.saw_done = true;
            return Ok(Flow::Done);
        }
        if data.is_empty() {
            return Ok(Flow::Continue);
        }
        let v: Value = serde_json::from_str(data)
            .map_err(|e| CoreError::Llm(format!("malformed chunk from provider: {e}")))?;
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| err.as_str())
                .unwrap_or("unknown error");
            return Err(CoreError::Llm(format!(
                "provider stream error: {}",
                redact(msg, None)
            )));
        }
        self.saw_chunk = true;
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
            self.usage = Some(Usage {
                input_tokens: get("prompt_tokens"),
                output_tokens: get("completion_tokens"),
            });
        }
        // Only the first choice is used (we never request n > 1).
        let Some(choice) = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return Ok(Flow::Continue);
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(t) = delta
                .get("content")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                self.text.push_str(t);
                on_text(t);
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in calls {
                    self.apply_tool_call(tc);
                }
            }
        }
        if let Some(r) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(r.to_string());
        }
        Ok(Flow::Continue)
    }

    fn finish(self) -> Result<LlmResponse> {
        if !self.saw_done && self.finish_reason.is_none() {
            return Err(CoreError::Llm(if self.saw_chunk {
                "the provider's stream ended before the response completed".into()
            } else {
                "the provider returned an empty response".into()
            }));
        }
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(ContentBlock::Text { text: self.text });
        }
        let mut has_calls = false;
        for c in self.calls {
            if c.name.is_empty() {
                continue;
            }
            has_calls = true;
            let id = if c.id.is_empty() {
                format!("call_{}", uuid::Uuid::new_v4().simple())
            } else {
                c.id
            };
            content.push(ContentBlock::ToolUse {
                id,
                name: c.name,
                input: parse_tool_args(&c.args),
            });
        }
        let stop_reason = match self.finish_reason.as_deref() {
            Some("length") => "max_tokens".to_string(),
            // Ollama and others report "stop" even when tool calls are present.
            _ if has_calls => "tool_use".to_string(),
            Some("tool_calls" | "function_call") => "tool_use".to_string(),
            Some("stop") | None => "end_turn".to_string(),
            Some(other) => other.to_string(),
        };
        Ok(LlmResponse {
            content,
            stop_reason,
            usage: self.usage.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::providers::{ToolSpec, decode_chunks};
    use crate::model::ChatMessage;

    /// Text, then two tool calls whose `arguments` are fragmented, then a usage-only chunk.
    const FIXTURE: &str = concat!(
        "data: {\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"I'll check \"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"both.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"run_sql\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"profile_id\\\":\\\"p1\\\",\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"sql\\\":\\\"select 1\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"th\\\":\\\"a.sql\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: {\"id\":\"c1\",\"choices\":[],\"usage\":{\"prompt_tokens\":321,\"completion_tokens\":47,\"total_tokens\":368}}\n\n",
        "data: [DONE]\n\n",
    );

    fn run(chunks: &[&[u8]]) -> (Result<LlmResponse>, String) {
        let mut text = String::new();
        let r = decode_chunks(OpenAiDecoder::default(), chunks, &mut |t| text.push_str(t));
        (r, text)
    }

    #[test]
    fn decodes_fragmented_tool_calls_and_final_usage_chunk() {
        let (r, text) = run(&[FIXTURE.as_bytes()]);
        let r = r.unwrap();
        assert_eq!(text, "I'll check both.");
        assert_eq!(r.stop_reason, "tool_use");
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 321,
                output_tokens: 47
            }
        );
        assert_eq!(
            r.content,
            vec![
                ContentBlock::Text {
                    text: "I'll check both.".into()
                },
                ContentBlock::ToolUse {
                    id: "call_a".into(),
                    name: "run_sql".into(),
                    input: json!({"profile_id":"p1","sql":"select 1"})
                },
                ContentBlock::ToolUse {
                    id: "call_b".into(),
                    name: "read_file".into(),
                    input: json!({"path":"a.sql"})
                },
            ]
        );
    }

    #[test]
    fn decodes_with_odd_chunking_and_crlf() {
        for size in [1usize, 5, 31, 200] {
            let chunks: Vec<&[u8]> = FIXTURE.as_bytes().chunks(size).collect();
            let (r, text) = run(&chunks);
            assert_eq!(r.unwrap().tool_uses().count(), 2, "chunk size {size}");
            assert_eq!(text, "I'll check both.");
        }
        let crlf = FIXTURE.replace('\n', "\r\n");
        let (r, _) = run(&[crlf.as_bytes()]);
        assert_eq!(r.unwrap().tool_uses().count(), 2);
    }

    #[test]
    fn plain_text_stop() {
        let s = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\" there\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let (r, text) = run(&[s.as_bytes()]);
        let r = r.unwrap();
        assert_eq!(text, "Hello there");
        assert_eq!(r.stop_reason, "end_turn");
        assert_eq!(r.usage, Usage::default());
    }

    #[test]
    fn length_maps_to_max_tokens() {
        let s = "data: {\"choices\":[{\"delta\":{\"content\":\"abc\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";
        assert_eq!(run(&[s.as_bytes()]).0.unwrap().stop_reason, "max_tokens");
    }

    #[test]
    fn ollama_style_whole_calls_with_stop_and_object_arguments() {
        let s = concat!(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[{\"id\":\"\",\"type\":\"function\",\"function\":{\"name\":\"list_files\",\"arguments\":{}}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let r = run(&[s.as_bytes()]).0.unwrap();
        assert_eq!(
            r.stop_reason, "tool_use",
            "tool calls win over finish_reason=stop"
        );
        let (id, name, input) = r.tool_uses().next().unwrap();
        assert!(
            id.starts_with("call_") && id.len() > 10,
            "generated id: {id}"
        );
        assert_eq!(name, "list_files");
        assert_eq!(input, &json!({}));
    }

    #[test]
    fn parallel_calls_that_all_claim_index_zero() {
        let s = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"x1\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"x2\",\"function\":{\"name\":\"list_connections\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let r = run(&[s.as_bytes()]).0.unwrap();
        let names: Vec<_> = r
            .tool_uses()
            .map(|(i, n, _)| (i.to_string(), n.to_string()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("x1".into(), "list_files".into()),
                ("x2".into(), "list_connections".into())
            ]
        );
    }

    #[test]
    fn calls_without_index() {
        let s = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"\\\"x.sql\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"b\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let r = run(&[s.as_bytes()]).0.unwrap();
        let calls: Vec<_> = r.tool_uses().collect();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].2, &json!({"path":"x.sql"}));
        assert_eq!(calls[1].1, "list_files");
    }

    #[test]
    fn stream_error_chunk_and_truncation() {
        let s = "data: {\"error\":{\"message\":\"quota exceeded\",\"type\":\"insufficient_quota\"}}\n\n";
        let e = run(&[s.as_bytes()]).0.unwrap_err();
        assert!(e.to_string().contains("quota exceeded"));
        let cut = "data: {\"choices\":[{\"delta\":{\"content\":\"abc\"}}]}\n\n";
        assert!(matches!(run(&[cut.as_bytes()]).0, Err(CoreError::Llm(_))));
        assert!(matches!(run(&[b"" as &[u8]]).0, Err(CoreError::Llm(_))));
    }

    #[test]
    fn done_without_finish_reason_is_ok() {
        let s = "data: {\"choices\":[{\"delta\":{\"content\":\"hey\"}}]}\n\ndata: [DONE]\n\n";
        assert_eq!(run(&[s.as_bytes()]).0.unwrap().stop_reason, "end_turn");
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
                        ContentBlock::Text {
                            text: "checking".into(),
                        },
                        ContentBlock::ToolUse {
                            id: "c1".into(),
                            name: "run_sql".into(),
                            input: json!({"sql":"select 1"}),
                        },
                        ContentBlock::ToolUse {
                            id: "c2".into(),
                            name: "list_files".into(),
                            input: json!({}),
                        },
                    ],
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: vec![
                        ContentBlock::ToolResult {
                            tool_use_id: "c1".into(),
                            content: "rows".into(),
                            is_error: false,
                        },
                        ContentBlock::ToolResult {
                            tool_use_id: "c2".into(),
                            content: "err".into(),
                            is_error: true,
                        },
                        ContentBlock::Text {
                            text: "and then?".into(),
                        },
                    ],
                },
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: "c3".into(),
                        name: "list_files".into(),
                        input: json!({}),
                    }],
                },
            ],
            tools: vec![ToolSpec {
                name: "run_sql".into(),
                description: "d".into(),
                input_schema: json!({"type":"object"}),
            }],
            model: "grok-x".into(),
            max_tokens: 777,
            temperature: None,
        };
        let b = build_body(&req, true);
        assert_eq!(b["stream"], true);
        assert_eq!(b["stream_options"]["include_usage"], true);
        assert_eq!(b["max_tokens"], 777);
        assert_eq!(
            b["tools"][0],
            json!({"type":"function","function":{"name":"run_sql","description":"d","parameters":{"type":"object"}}})
        );
        let m = b["messages"].as_array().unwrap();
        assert_eq!(m[0], json!({"role":"system","content":"sys"}));
        assert_eq!(m[1], json!({"role":"user","content":"hi"}));
        assert_eq!(m[2]["role"], "assistant");
        assert_eq!(m[2]["content"], "checking");
        assert_eq!(
            m[2]["tool_calls"][0]["function"]["arguments"],
            "{\"sql\":\"select 1\"}"
        );
        assert_eq!(m[2]["tool_calls"][1]["id"], "c2");
        assert_eq!(
            m[3],
            json!({"role":"tool","tool_call_id":"c1","content":"rows"})
        );
        assert_eq!(m[4]["tool_call_id"], "c2");
        assert_eq!(m[5], json!({"role":"user","content":"and then?"}));
        assert_eq!(m[6]["content"], Value::Null);
        assert!(build_body(&req, false).get("stream_options").is_none());
    }
}
