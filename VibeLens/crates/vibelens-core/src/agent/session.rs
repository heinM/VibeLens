//! Session state and the turn loop: stream → propose tools → policy → approval → run → repeat.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::diff::unified_diff;
use super::policy::{Assessment, ProfileFacts, assess};
use super::prompt::build_system_prompt;
use super::providers::{LlmRequest, LlmResponse, ProviderClient};
use super::redact::model_error_text;
use super::tools::{ToolCall, ToolOutput, frame_result, run as run_tool, tool_specs};
use crate::error::{CoreError, ErrorPayload};
use crate::model::*;
use crate::ports::ToolBackend;

/// At most this many rounds of tool calls per turn.
pub(super) const MAX_STEPS: usize = 16;
/// Approximate token budget for a session's history (characters / 4).
pub(super) const HISTORY_BUDGET_TOKENS: usize = 150_000;
const TOOL_TIMEOUT: Duration = Duration::from_secs(180);
const ELIDED: &str = "[elided]";

/// One conversation. Lives in the service's map; the running turn works on a copy of `history`
/// and commits it back when it ends.
pub(super) struct SessionState {
    pub history: Vec<ChatMessage>,
    pub cancel: CancellationToken,
    pub pending: HashMap<String, oneshot::Sender<ApprovalDecision>>,
    pub running: bool,
    /// Bumped by `reset`; a turn that started before a reset must not write its history back.
    pub generation: u64,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            history: Vec::new(),
            cancel: CancellationToken::new(),
            pending: HashMap::new(),
            running: false,
            generation: 0,
        }
    }
}

pub(super) type Sessions = Arc<Mutex<HashMap<String, SessionState>>>;

/// Locks, tolerating poison (a panicked turn must not brick the service).
pub(super) fn lock(s: &Sessions) -> MutexGuard<'_, HashMap<String, SessionState>> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------------
// History budget
// ---------------------------------------------------------------------------------------------

fn block_chars(b: &ContentBlock) -> usize {
    match b {
        ContentBlock::Text { text } => text.len(),
        ContentBlock::ToolUse { name, input, .. } => name.len() + input.to_string().len(),
        ContentBlock::ToolResult { content, .. } => content.len(),
    }
}

#[cfg(test)]
pub(super) fn estimate_tokens(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .flat_map(|m| m.content.iter())
        .map(block_chars)
        .sum::<usize>()
        / 4
}

fn starts_clean_turn(m: &ChatMessage) -> bool {
    m.role == ChatRole::User
        && !m
            .content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
}

/// Keeps `history` under `budget_tokens` (≈ chars / 4): first elides the oldest tool results,
/// then drops the oldest whole exchanges. Never leaves an orphaned tool_use / tool_result pair.
pub(super) fn trim_history(history: &mut Vec<ChatMessage>, budget_tokens: usize) {
    let budget = budget_tokens * 4;
    let mut total: usize = history
        .iter()
        .flat_map(|m| m.content.iter())
        .map(block_chars)
        .sum();
    if total <= budget {
        return;
    }
    'elide: for m in history.iter_mut() {
        for b in m.content.iter_mut() {
            if let ContentBlock::ToolResult { content, .. } = b
                && content != ELIDED
            {
                total = total.saturating_sub(content.len()) + ELIDED.len();
                *content = ELIDED.to_string();
                if total <= budget {
                    break 'elide;
                }
            }
        }
    }
    while total > budget {
        let Some(k) = history
            .iter()
            .skip(1)
            .position(starts_clean_turn)
            .map(|i| i + 1)
        else {
            break;
        };
        let dropped: usize = history
            .drain(..k)
            .flat_map(|m| m.content.into_iter())
            .map(|b| block_chars(&b))
            .sum();
        total = total.saturating_sub(dropped);
    }
}

// ---------------------------------------------------------------------------------------------
// The turn
// ---------------------------------------------------------------------------------------------

enum Outcome {
    Finished(String),
    Failed(CoreError),
    Cancelled,
}

enum CallOutcome {
    Result(ContentBlock),
    Cancelled,
}

/// Everything one turn needs. Consumed by [`TurnRunner::run`].
pub(super) struct TurnRunner {
    pub sessions: Sessions,
    pub session_id: String,
    pub generation: u64,
    pub turn_id: String,
    pub provider: ProviderClient,
    pub backend: Arc<dyn ToolBackend>,
    pub tx: mpsc::Sender<AgentEvent>,
    pub cancel: CancellationToken,
    pub ctx: AgentContext,
    pub history: Vec<ChatMessage>,
    pub usage: Usage,
    /// Set once the terminal event has been sent and state committed.
    pub done: bool,
}

impl Drop for TurnRunner {
    /// A turn that dies without finishing (panic, runtime shutdown) must not leave the session
    /// stuck in "running", and the UI must get a terminal event.
    fn drop(&mut self) {
        if self.done {
            return;
        }
        self.release();
        let err = CoreError::Other("the agent turn ended unexpectedly".into());
        let _ = self.tx.try_send(AgentEvent::Failed {
            error: ErrorPayload::from(&err),
        });
    }
}

fn user_text(text: String) -> ContentBlock {
    ContentBlock::Text { text }
}

fn tool_error_block(id: &str, msg: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.to_string(),
        content: frame_result(msg),
        is_error: true,
    }
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        line.chars().take(max).collect::<String>() + "…"
    } else {
        line.to_string()
    }
}

impl TurnRunner {
    /// Sends an event. A closed receiver means the UI is gone: cancel the turn.
    async fn emit(&self, ev: AgentEvent) {
        if self.tx.send(ev).await.is_err() {
            self.cancel.cancel();
        }
    }

    /// Commits history and clears the running flag (unless a `reset` intervened).
    fn release(&mut self) {
        let mut map = lock(&self.sessions);
        if let Some(s) = map.get_mut(&self.session_id)
            && s.generation == self.generation
        {
            s.history = std::mem::take(&mut self.history);
            s.running = false;
            s.pending.clear();
        }
    }

    /// Runs the turn to completion. Always ends with `TurnFinished` or `Failed`; state is
    /// committed *before* that event so a client can immediately start the next turn.
    pub async fn run(mut self, prompt: String) {
        self.emit(AgentEvent::TurnStarted {
            turn_id: self.turn_id.clone(),
            provider_id: self.provider.cfg.id.clone(),
            model: self.provider.model().to_string(),
        })
        .await;
        let outcome = self.drive(prompt).await;
        let usage = Some(self.usage.clone());
        self.release();
        self.done = true;
        let terminal = match outcome {
            Outcome::Finished(stop_reason) => AgentEvent::TurnFinished { stop_reason, usage },
            Outcome::Cancelled => AgentEvent::TurnFinished {
                stop_reason: "cancelled".into(),
                usage,
            },
            Outcome::Failed(e) => AgentEvent::Failed {
                error: ErrorPayload::from(&e),
            },
        };
        // Best effort: if the UI is gone there is nobody to tell.
        let _ = self.tx.send(terminal).await;
    }

    async fn drive(&mut self, prompt: String) -> Outcome {
        let profile = self
            .ctx
            .profile_id
            .as_deref()
            .and_then(|id| self.backend.profile(id).ok());
        let system = build_system_prompt(profile.as_ref(), &self.ctx);
        let tools = tool_specs();

        // A previous turn that failed before the model answered leaves a trailing user message;
        // fold the new prompt into it rather than sending two user turns in a row.
        match self.history.last_mut() {
            Some(last) if last.role == ChatRole::User => last.content.push(user_text(prompt)),
            _ => self.history.push(ChatMessage {
                role: ChatRole::User,
                content: vec![user_text(prompt)],
            }),
        }

        let mut steps = 0usize;
        loop {
            if self.cancel.is_cancelled() {
                return Outcome::Cancelled;
            }
            trim_history(&mut self.history, HISTORY_BUDGET_TOKENS);
            let req = LlmRequest {
                system: system.clone(),
                messages: self.history.clone(),
                tools: tools.clone(),
                model: self.provider.model().to_string(),
                max_tokens: self.provider.max_tokens(),
                temperature: self.provider.temperature(),
            };
            let resp = match self.call_model(&req).await {
                Ok(r) => r,
                Err(outcome) => return outcome,
            };
            self.usage.input_tokens += resp.usage.input_tokens;
            self.usage.output_tokens += resp.usage.output_tokens;

            let calls: Vec<(String, String, Value)> = resp
                .tool_uses()
                .map(|(i, n, v)| (i.to_string(), n.to_string(), v.clone()))
                .collect();
            if !resp.content.is_empty() {
                self.history.push(ChatMessage {
                    role: ChatRole::Assistant,
                    content: resp.content.clone(),
                });
            }
            // Continue whenever the model asked for tools, whatever stop_reason says
            // (some compatible servers report "stop" alongside tool calls).
            if calls.is_empty() {
                return Outcome::Finished(resp.stop_reason);
            }

            let limit_hit = steps >= MAX_STEPS;
            let mut results = Vec::with_capacity(calls.len());
            let mut cancelled = false;
            for (id, name, input) in &calls {
                if cancelled {
                    results.push(tool_error_block(
                        id,
                        "Cancelled by the user before this tool ran.",
                    ));
                    continue;
                }
                if limit_hit {
                    let reason = format!(
                        "Step limit reached ({MAX_STEPS} tool rounds per turn). This tool was not run; summarise what you found and ask the user how to proceed."
                    );
                    results.push(self.refuse(id, name, input, &reason).await);
                    continue;
                }
                match self.handle_call(id, name, input).await {
                    CallOutcome::Result(block) => results.push(block),
                    CallOutcome::Cancelled => {
                        results.push(tool_error_block(id, "Cancelled by the user."));
                        cancelled = true;
                    }
                }
            }
            self.history.push(ChatMessage {
                role: ChatRole::User,
                content: results,
            });
            if cancelled {
                return Outcome::Cancelled;
            }
            if limit_hit {
                return Outcome::Finished("max_steps".into());
            }
            steps += 1;
        }
    }

    /// Streams one model response, forwarding text deltas as `TextDelta` events.
    async fn call_model(&self, req: &LlmRequest) -> Result<LlmResponse, Outcome> {
        let (ttx, mut trx) = mpsc::unbounded_channel::<String>();
        let mut on_text = |t: &str| {
            let _ = ttx.send(t.to_string());
        };
        let fut = self.provider.stream(req, &mut on_text);
        tokio::pin!(fut);
        let result = loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(Outcome::Cancelled),
                Some(mut text) = trx.recv() => {
                    // Coalesce whatever else is already waiting into one event.
                    while let Ok(more) = trx.try_recv() {
                        text.push_str(&more);
                    }
                    self.emit(AgentEvent::TextDelta { text }).await;
                }
                r = &mut fut => break r,
            }
        };
        let mut tail = String::new();
        while let Ok(more) = trx.try_recv() {
            tail.push_str(&more);
        }
        if !tail.is_empty() {
            self.emit(AgentEvent::TextDelta { text: tail }).await;
        }
        result.map_err(Outcome::Failed)
    }

    /// Proposes a call and immediately closes it as denied.
    async fn refuse(&self, id: &str, name: &str, input: &Value, reason: &str) -> ContentBlock {
        self.emit(AgentEvent::ToolCallProposed {
            call_id: id.to_string(),
            tool: name.to_string(),
            arguments: input.clone(),
            risk: ToolRisk::Denied,
            reason: Some(reason.to_string()),
        })
        .await;
        self.emit(AgentEvent::ToolFinished {
            call_id: id.to_string(),
            ok: false,
            summary: "denied".into(),
            elapsed_ms: 0,
        })
        .await;
        tool_error_block(id, reason)
    }

    async fn handle_call(&mut self, id: &str, name: &str, input: &Value) -> CallOutcome {
        let default_profile = self.ctx.profile_id.as_deref();
        let call = match ToolCall::parse(name, input, default_profile) {
            Ok(c) => c,
            Err(msg) => {
                return CallOutcome::Result(
                    self.refuse(id, name, input, &format!("Invalid tool call: {msg}"))
                        .await,
                );
            }
        };

        // Resolve the connection this call touches.
        let facts = match call.profile_id() {
            Some(pid) => match self.backend.profile(pid) {
                Ok(p) => Some(ProfileFacts::from(&p)),
                Err(_) => {
                    let reason = format!(
                        "Unknown connection id '{pid}'. Call list_connections and use one of the returned ids."
                    );
                    return CallOutcome::Result(self.refuse(id, name, input, &reason).await);
                }
            },
            None => None,
        };

        // Ask the server whether a Valkey command writes, but only when it could matter (a Valkey
        // connection that may be used at all; policy refuses the call on any other engine).
        let valkey_write = match (&call, &facts) {
            (
                ToolCall::ValkeyCommand {
                    profile_id, argv, ..
                },
                Some(f),
            ) if f.access != AiDataAccess::None && f.engine == EngineKind::Valkey => {
                // Fail closed: if the server cannot tell us, treat it as a write.
                self.backend
                    .valkey_is_write(profile_id, argv)
                    .await
                    .unwrap_or(true)
            }
            _ => false,
        };

        let mut plan = assess(&call, facts.as_ref(), valkey_write);
        let mut preview = plan.preview.clone();

        // write_file: build the diff against what is on disk now.
        if let ToolCall::WriteFile { path, contents } = &call {
            match self.backend.read_file(path).await {
                Ok(old) => preview = Some(unified_diff(path, Some(&old), contents)),
                Err(CoreError::NotFound(_)) => preview = Some(unified_diff(path, None, contents)),
                Err(CoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    preview = Some(unified_diff(path, None, contents))
                }
                // The workspace jail / extension allow-list speaking: refuse before asking the user.
                Err(e @ (CoreError::Invalid(_) | CoreError::PolicyDenied(_))) => {
                    plan = Assessment {
                        risk: ToolRisk::Denied,
                        reason: Some(format!("cannot write '{path}': {e}")),
                        preview: None,
                        allow_write: false,
                        redact_values: false,
                    };
                }
                Err(_) => preview = Some(unified_diff(path, None, contents)),
            }
        }

        self.emit(AgentEvent::ToolCallProposed {
            call_id: id.to_string(),
            tool: name.to_string(),
            arguments: input.clone(),
            risk: plan.risk,
            reason: plan.reason.clone(),
        })
        .await;

        match plan.risk {
            ToolRisk::Denied => {
                self.emit(AgentEvent::ToolFinished {
                    call_id: id.to_string(),
                    ok: false,
                    summary: "denied".into(),
                    elapsed_ms: 0,
                })
                .await;
                let reason = plan.reason.unwrap_or_else(|| "denied by policy".into());
                return CallOutcome::Result(tool_error_block(id, &reason));
            }
            ToolRisk::NeedsApproval => {
                let (otx, orx) = oneshot::channel();
                {
                    // Register before announcing, so a fast UI cannot resolve a call we have not parked yet.
                    let mut map = lock(&self.sessions);
                    match map.get_mut(&self.session_id) {
                        Some(s) if s.generation == self.generation => {
                            s.pending.insert(id.to_string(), otx);
                        }
                        _ => return CallOutcome::Cancelled,
                    }
                }
                self.emit(AgentEvent::ApprovalRequired {
                    call_id: id.to_string(),
                    tool: name.to_string(),
                    arguments: input.clone(),
                    reason: plan.reason.clone().unwrap_or_default(),
                    preview,
                })
                .await;
                let decision = tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => None,
                    d = orx => d.ok(),
                };
                {
                    let mut map = lock(&self.sessions);
                    if let Some(s) = map.get_mut(&self.session_id) {
                        s.pending.remove(id);
                    }
                }
                match decision {
                    None => return CallOutcome::Cancelled,
                    Some(ApprovalDecision::Approve) => {}
                    Some(ApprovalDecision::Reject { reason }) => {
                        self.emit(AgentEvent::ToolFinished {
                            call_id: id.to_string(),
                            ok: false,
                            summary: "rejected by user".into(),
                            elapsed_ms: 0,
                        })
                        .await;
                        let why = reason
                            .filter(|r| !r.trim().is_empty())
                            .unwrap_or_else(|| "no reason given".into());
                        return CallOutcome::Result(tool_error_block(
                            id,
                            &format!("User rejected: {why}"),
                        ));
                    }
                }
            }
            ToolRisk::Safe => {}
        }

        self.emit(AgentEvent::ToolStarted {
            call_id: id.to_string(),
        })
        .await;
        let started = Instant::now();
        let result = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return CallOutcome::Cancelled,
            r = tokio::time::timeout(TOOL_TIMEOUT, run_tool(&call, self.backend.as_ref(), &plan)) => {
                r.unwrap_or_else(|_| Err(CoreError::Timeout(format!("{name} did not finish within {} seconds", TOOL_TIMEOUT.as_secs()))))
            }
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(ToolOutput {
                body,
                summary,
                wrote_path,
            }) => {
                self.emit(AgentEvent::ToolFinished {
                    call_id: id.to_string(),
                    ok: true,
                    summary,
                    elapsed_ms,
                })
                .await;
                if let Some(path) = wrote_path {
                    self.emit(AgentEvent::FileWritten { path }).await;
                }
                CallOutcome::Result(ContentBlock::ToolResult {
                    tool_use_id: id.to_string(),
                    content: frame_result(&body),
                    is_error: false,
                })
            }
            Err(e) => {
                // The UI sees the real error; the model sees it with connection details removed
                // and, where values are withheld, with no data-bearing text at all.
                let msg = e.to_string();
                let withhold_values = plan.redact_values
                    || facts
                        .as_ref()
                        .is_some_and(|f| f.access == AiDataAccess::SchemaOnly);
                self.emit(AgentEvent::ToolFinished {
                    call_id: id.to_string(),
                    ok: false,
                    summary: first_line(&msg, 200),
                    elapsed_ms,
                })
                .await;
                CallOutcome::Result(tool_error_block(
                    id,
                    &model_error_text(&e, withhold_values, facts.as_ref().map(|f| f.engine)),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(t: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::User,
            content: vec![ContentBlock::Text { text: t.into() }],
        }
    }
    fn assistant_call(id: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: "run_sql".into(),
                input: serde_json::json!({"sql":"select 1"}),
            }],
        }
    }
    fn result(id: &str, size: usize) -> ChatMessage {
        ChatMessage {
            role: ChatRole::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.into(),
                content: "x".repeat(size),
                is_error: false,
            }],
        }
    }
    fn assistant_text(t: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::Assistant,
            content: vec![ContentBlock::Text { text: t.into() }],
        }
    }

    #[test]
    fn under_budget_is_untouched() {
        let mut h = vec![
            user("hi"),
            assistant_call("a"),
            result("a", 100),
            assistant_text("ok"),
        ];
        let before = h.clone();
        trim_history(&mut h, 1000);
        assert_eq!(h, before);
    }

    #[test]
    fn oldest_tool_results_are_elided_first() {
        let mut h = vec![
            user("q1"),
            assistant_call("a"),
            result("a", 4000),
            assistant_text("r1"),
            user("q2"),
            assistant_call("b"),
            result("b", 4000),
            assistant_text("r2"),
        ];
        // 8000 chars of results ≈ 2000 tokens; budget forces exactly the oldest to go.
        trim_history(&mut h, 1200);
        let contents: Vec<_> = h
            .iter()
            .flat_map(|m| m.content.iter())
            .filter_map(|b| {
                if let ContentBlock::ToolResult { content, .. } = b {
                    Some(content.clone())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(contents[0], ELIDED);
        assert_eq!(contents[1].len(), 4000, "newer result kept");
        assert_eq!(
            h.len(),
            8,
            "structure preserved so tool_use/tool_result pairs stay valid"
        );
    }

    #[test]
    fn whole_exchanges_dropped_when_eliding_is_not_enough() {
        let mut h = vec![
            user(&"a".repeat(8000)),
            assistant_text(&"b".repeat(8000)),
            user("second question"),
            assistant_call("c"),
            result("c", 10),
            assistant_text("answer"),
        ];
        trim_history(&mut h, 1000);
        assert_eq!(h.len(), 4, "first exchange dropped as a unit");
        assert!(
            matches!(&h[0].content[0], ContentBlock::Text { text } if text == "second question")
        );
        // Never starts with an orphaned tool_result.
        assert!(starts_clean_turn(&h[0]));
    }

    #[test]
    fn single_giant_turn_cannot_be_dropped_to_nothing() {
        let mut h = vec![user(&"a".repeat(100_000))];
        trim_history(&mut h, 100);
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn estimate_counts_all_block_kinds() {
        let h = vec![
            user(&"a".repeat(400)),
            assistant_call("a"),
            result("a", 400),
        ];
        assert!(estimate_tokens(&h) >= 200);
    }

    #[test]
    fn first_line_helper() {
        assert_eq!(first_line("one\ntwo", 10), "one");
        assert_eq!(first_line(&"x".repeat(300), 5), "xxxxx…");
    }
}
