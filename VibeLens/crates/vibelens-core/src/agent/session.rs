//! Session state and the turn loop: stream → propose tools → policy → approval → run → repeat.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::diff::{DiffPreview, count_hidden_chars, grouped, unified_diff, visible};
use super::policy::{Assessment, ProfileFacts, approval_target, assess};
use super::prompt::build_system_prompt;
use super::providers::{LlmRequest, LlmResponse, ProviderClient, ToolSpec};
use super::redact::model_error_text;
use super::tools::{ToolCall, ToolOutput, frame_result, run as run_tool, tool_specs};
use crate::error::{CoreError, ErrorPayload};
use crate::model::*;
use crate::ports::{FileVersion, ToolBackend};

/// At most this many rounds of tool calls per turn.
pub(super) const MAX_STEPS: usize = 16;
/// Approximate token budget for a session's history (characters / 4).
pub(super) const HISTORY_BUDGET_TOKENS: usize = 150_000;
/// The least history budget a configured context window can produce: below this not even the
/// current question and one tool result fit, and trimming further would not help.
pub(super) const MIN_HISTORY_BUDGET_TOKENS: usize = 2_048;
/// Added to the estimated size of the system prompt and tool specs: chars / 4 under-counts
/// SQL, JSON and non-English text, and providers wrap every message in template tokens.
const PROMPT_RESERVE_MARGIN_TOKENS: usize = 1_024;
const TOOL_TIMEOUT: Duration = Duration::from_secs(180);
/// How long a cancelled tool may take to stop its work on the server before it is dropped.
const CANCEL_GRACE: Duration = Duration::from_secs(15);
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

/// Tokens set aside for what is sent each turn besides the history: the system prompt and the
/// tool specs, estimated like the history (chars / 4), plus a margin.
pub(super) fn prompt_reserve_tokens(system: &str, tools: &[ToolSpec]) -> usize {
    let tool_chars: usize = tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.input_schema.to_string().len())
        .sum();
    (system.len() + tool_chars) / 4 + PROMPT_RESERVE_MARGIN_TOKENS
}

/// The history budget for one provider. Without a configured context window it is
/// [`HISTORY_BUDGET_TOKENS`]. With one, it is what the window leaves after the output
/// (`max_tokens`) and `reserve`, never more than [`HISTORY_BUDGET_TOKENS`] and never less than
/// [`MIN_HISTORY_BUDGET_TOKENS`].
pub(super) fn history_budget(
    context_window: Option<u32>,
    max_tokens: u32,
    reserve: usize,
) -> usize {
    let Some(window) = context_window else {
        return HISTORY_BUDGET_TOKENS;
    };
    (window as usize)
        .saturating_sub(max_tokens as usize)
        .saturating_sub(reserve)
        .clamp(MIN_HISTORY_BUDGET_TOKENS, HISTORY_BUDGET_TOKENS)
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
        let budget = history_budget(
            self.provider.cfg.context_window,
            self.provider.max_tokens(),
            prompt_reserve_tokens(&system, &tools),
        );

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
            trim_history(&mut self.history, budget);
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
            sql_run: None,
        })
        .await;
        tool_error_block(id, reason)
    }

    async fn handle_call(&mut self, id: &str, name: &str, input: &Value) -> CallOutcome {
        let default_profile = self.ctx.profile_id.as_deref();
        let mut call = match ToolCall::parse(name, input, default_profile) {
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

        // The model picks the database; on the active connection a call that names none works
        // in the active database. Resolve it once, here, with the rule execution uses: the
        // approval card shows this value and the backend is handed this value.
        if let Some(f) = facts
            .as_ref()
            .filter(|f| matches!(f.engine, EngineKind::Postgres | EngineKind::Redshift))
        {
            call.default_database(self.ctx.profile_id.as_deref(), self.ctx.database.as_deref());
            call.pin_database(f.database.as_deref());
        }
        let target = facts.as_ref().and_then(|f| approval_target(&call, f));

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

        // write_file: the diff is built against what is on disk now, and that version goes with
        // the call to execution, which writes only if the file is still the one shown here.
        let mut write_baseline: Option<FileVersion> = None;
        if let ToolCall::WriteFile { path, contents } = &call {
            let existing = match self.backend.read_file(path).await {
                Ok(old) => Ok(Some(old)),
                Err(CoreError::NotFound(_)) => Ok(None),
                Err(CoreError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            };
            match existing {
                Ok(old) => {
                    let diff = unified_diff(path, old.as_deref(), contents);
                    write_baseline = Some(FileVersion::of(old.as_deref()));
                    if let Some(note) = write_notes(&diff, contents) {
                        plan.reason =
                            Some(format!("{} {note}", plan.reason.take().unwrap_or_default()));
                    }
                    preview = Some(diff.text);
                }
                Err(e) => {
                    let reason = match &e {
                        // The workspace jail / extension allow-list speaking.
                        CoreError::Invalid(_) | CoreError::PolicyDenied(_) => {
                            format!("cannot write '{}': {e}", visible(path))
                        }
                        // There is a file (or something) there that cannot be read, so no true
                        // diff exists. Showing it as a new file would have the user approve
                        // "create" for what is in fact a replacement.
                        _ => format!(
                            "cannot write '{}': what is at that path now could not be read ({e}), so the user cannot be shown what would change. Nothing was written. Use another path, or tell the user what went wrong.",
                            visible(path)
                        ),
                    };
                    // Refuse before asking the user.
                    plan = Assessment {
                        risk: ToolRisk::Denied,
                        reason: Some(reason),
                        preview: None,
                        allow_write: false,
                        redact_values: false,
                    };
                    preview = None;
                }
            }
        } else if let Some(text) = preview.take() {
            // A statement or command shown before it runs: the same rule as for a diff. What is
            // run is the call's own text; only what is displayed changes.
            // The note goes first: the reason ends with the model's `purpose`, which is free
            // text and need not end a sentence.
            let hidden = count_hidden_chars(&text);
            if hidden > 0 {
                plan.reason = Some(format!(
                    "{} {}",
                    hidden_chars_note("The text below", hidden, "below"),
                    plan.reason.take().unwrap_or_default()
                ));
            }
            preview = Some(visible(&text).into_owned());
        }

        // The reason is read by the user and carries text the model wrote (a purpose, a command
        // line), so it follows the same rule as the preview. A refusal keeps its own text: it
        // goes back to the model.
        if plan.risk == ToolRisk::NeedsApproval
            && let Some(reason) = plan.reason.as_mut()
            && count_hidden_chars(reason) > 0
        {
            *reason = visible(reason).into_owned();
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
                    sql_run: None,
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
                    target,
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
                            sql_run: None,
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
        // The tool gets its own token, cancelled with the turn or at the tool timeout. On either,
        // the tool future is awaited a little longer instead of dropped, so the engine can stop
        // the statement on the server (Trino DELETE, Livy cancel, StopQueryExecution, …).
        let tool_cancel = self.cancel.child_token();
        let tool = run_tool(
            &call,
            self.backend.as_ref(),
            &plan,
            write_baseline.as_ref(),
            &tool_cancel,
        );
        tokio::pin!(tool);
        let result = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => {
                let _ = tokio::time::timeout(CANCEL_GRACE, &mut tool).await;
                return CallOutcome::Cancelled;
            }
            r = tokio::time::timeout(TOOL_TIMEOUT, &mut tool) => match r {
                Ok(r) => r,
                Err(_) => {
                    tool_cancel.cancel();
                    let _ = tokio::time::timeout(CANCEL_GRACE, &mut tool).await;
                    Err(CoreError::Timeout(format!(
                        "{name} did not finish within {} seconds",
                        TOOL_TIMEOUT.as_secs()
                    )))
                }
            },
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        // Where a SQL call ran, from the resolved call and the policy verdict, so the user can
        // reopen it against the same connection and database. No rows go with it.
        let sql_run = match &call {
            ToolCall::RunSql {
                profile_id,
                database,
                sql,
                ..
            } => Some(SqlRun {
                profile_id: profile_id.clone(),
                database: database.clone(),
                sql: sql.clone(),
                read_only: !plan.allow_write,
            }),
            _ => None,
        };
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
                    sql_run,
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
                    sql_run,
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

/// The sentence the approval card adds when text holds characters that are shown by code point.
fn hidden_chars_note(subject: &str, n: usize, place: &str) -> String {
    if n == 1 {
        format!(
            "{subject} has 1 control, invisible or direction-changing character; it is shown as <U+…> {place}."
        )
    } else {
        format!(
            "{subject} has {} control, invisible or direction-changing characters; each is shown as <U+…> {place}.",
            grouped(n)
        )
    }
}

/// What the user has to know about a `write_file` diff that the diff alone does not say: how
/// much of it is not on screen, and that the new content holds characters they cannot see.
fn write_notes(diff: &DiffPreview, contents: &str) -> Option<String> {
    let mut notes = Vec::new();
    if diff.hidden_lines > 0 {
        notes.push(format!(
            "The diff is too long to show in full: {} of its lines {} not shown ({} added, {} removed). Approving writes the whole file, including that part.",
            grouped(diff.hidden_lines),
            if diff.hidden_lines == 1 { "is" } else { "are" },
            grouped(diff.hidden_added),
            grouped(diff.hidden_removed),
        ));
    }
    let hidden = count_hidden_chars(contents);
    if hidden > 0 {
        notes.push(hidden_chars_note("The new content", hidden, "in the diff"));
    }
    (!notes.is_empty()).then(|| notes.join(" "))
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
    fn history_budget_follows_the_context_window() {
        // Unset: today's budget, whatever the output limit.
        assert_eq!(history_budget(None, 8192, 5_000), HISTORY_BUDGET_TOKENS);
        // Window minus output minus reserve.
        assert_eq!(history_budget(Some(16_384), 4096, 4_000), 8_288);
        // A huge window never raises the budget above the default.
        assert_eq!(
            history_budget(Some(2_000_000), 8192, 5_000),
            HISTORY_BUDGET_TOKENS
        );
        // A window too small for output and prompt falls back to the floor, without underflow.
        assert_eq!(
            history_budget(Some(4096), 8192, 5_000),
            MIN_HISTORY_BUDGET_TOKENS
        );
        assert_eq!(
            history_budget(Some(8192), 4096, 3_000),
            MIN_HISTORY_BUDGET_TOKENS
        );
    }

    #[test]
    fn prompt_reserve_counts_system_prompt_and_tool_specs() {
        let none = prompt_reserve_tokens("", &[]);
        assert_eq!(none, PROMPT_RESERVE_MARGIN_TOKENS);
        let sys = "s".repeat(4_000);
        assert_eq!(prompt_reserve_tokens(&sys, &[]), 1_000 + none);
        let specs = tool_specs();
        let with_tools = prompt_reserve_tokens(&sys, &specs);
        assert!(
            with_tools > 1_000 + none + 1_000,
            "the tool catalogue is several thousand characters: {with_tools}"
        );
    }

    #[test]
    fn first_line_helper() {
        assert_eq!(first_line("one\ntwo", 10), "one");
        assert_eq!(first_line(&"x".repeat(300), 5), "xxxxx…");
    }
}
