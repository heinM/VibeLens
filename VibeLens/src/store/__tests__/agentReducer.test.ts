import { describe, expect, it } from "vitest";
import { applyAgentEvent, initialAgentCore, userPrompts, type AgentCore, type AgentStoreEvent, type ToolItem, type ApprovalItem } from "../../components/agent/agentReducer";

const run = (events: AgentStoreEvent[], from: AgentCore = initialAgentCore()) => events.reduce(applyAgentEvent, from);
const kinds = (s: AgentCore) => s.transcript.map((i) => i.kind);
const tool = (s: AgentCore, callId = "c1") => s.transcript.find((i): i is ToolItem => i.kind === "tool" && i.callId === callId);
const approval = (s: AgentCore) => s.transcript.find((i): i is ApprovalItem => i.kind === "approval");

const started: AgentStoreEvent = { type: "turnStarted", turnId: "t1", providerId: "p", model: "m" };
const prompt: AgentStoreEvent = { type: "userPrompt", text: "hello", attached: ["db"] };

describe("applyAgentEvent: streaming", () => {
  it("adds the user prompt and marks the turn running", () => {
    const s = run([prompt]);
    expect(s.running).toBe(true);
    expect(s.transcript).toEqual([{ kind: "user", id: "i0", text: "hello", attached: ["db"] }]);
  });

  it("appends text deltas to one streaming assistant item", () => {
    const s = run([prompt, started, { type: "textDelta", text: "Hel" }, { type: "textDelta", text: "lo " }, { type: "textDelta", text: "there" }]);
    expect(s.turnId).toBe("t1");
    expect(kinds(s)).toEqual(["user", "assistant"]);
    const a = s.transcript[1];
    expect(a.kind === "assistant" && a.text).toBe("Hello there");
    expect(a.kind === "assistant" && a.streaming).toBe(true);
  });

  it("ignores empty deltas", () => {
    const s = run([prompt, { type: "textDelta", text: "" }]);
    expect(kinds(s)).toEqual(["user"]);
  });

  it("starts a new assistant item after a tool call, and closes the previous one", () => {
    const s = run([
      prompt,
      started,
      { type: "textDelta", text: "Let me look." },
      { type: "toolCallProposed", callId: "c1", tool: "get_schema", arguments: {}, risk: "safe", reason: null },
      { type: "textDelta", text: "Done." },
    ]);
    expect(kinds(s)).toEqual(["user", "assistant", "tool", "assistant"]);
    const [, first, , second] = s.transcript;
    expect(first.kind === "assistant" && first.streaming).toBe(false);
    expect(second.kind === "assistant" && second.text).toBe("Done.");
  });

  it("finishing the turn stops streaming, records usage and clears running", () => {
    const s = run([prompt, started, { type: "textDelta", text: "ok" }, { type: "turnFinished", stopReason: "end_turn", usage: { inputTokens: 10, outputTokens: 3 } }]);
    expect(s.running).toBe(false);
    const a = s.transcript.find((i) => i.kind === "assistant");
    expect(a?.kind === "assistant" && a.streaming).toBe(false);
    const u = s.transcript[s.transcript.length - 1];
    expect(u).toMatchObject({ kind: "usage", usage: { inputTokens: 10, outputTokens: 3 }, stopReason: "end_turn" });
  });

  it("ids stay unique across items", () => {
    const s = run([prompt, started, { type: "textDelta", text: "a" }, { type: "toolCallProposed", callId: "c1", tool: "x", arguments: null, risk: "safe", reason: null }, { type: "turnFinished", stopReason: "end_turn", usage: null }]);
    const ids = s.transcript.map((i) => i.id);
    expect(new Set(ids).size).toBe(ids.length);
  });
});

describe("applyAgentEvent: tool lifecycle", () => {
  it("proposed -> running -> ok", () => {
    let s = run([prompt, started, { type: "toolCallProposed", callId: "c1", tool: "run_sql", arguments: { sql: "select 1" }, risk: "safe", reason: null }]);
    expect(tool(s)?.status).toBe("proposed");
    s = run([{ type: "toolStarted", callId: "c1" }], s);
    expect(tool(s)?.status).toBe("running");
    s = run([{ type: "toolFinished", callId: "c1", ok: true, summary: "1 row", elapsedMs: 12 }], s);
    expect(tool(s)).toMatchObject({ status: "ok", summary: "1 row", elapsedMs: 12 });
  });

  it("a failed tool is marked error", () => {
    const s = run([prompt, { type: "toolCallProposed", callId: "c1", tool: "run_sql", arguments: {}, risk: "safe", reason: null }, { type: "toolStarted", callId: "c1" }, { type: "toolFinished", callId: "c1", ok: false, summary: "syntax error", elapsedMs: 3 }]);
    expect(tool(s)).toMatchObject({ status: "error", summary: "syntax error" });
  });

  it("denied calls stay denied and keep their reason", () => {
    const s = run([
      prompt,
      { type: "toolCallProposed", callId: "c1", tool: "get_schema", arguments: {}, risk: "denied", reason: "AI access is off for this connection" },
      { type: "toolFinished", callId: "c1", ok: false, summary: "denied", elapsedMs: 0 },
    ]);
    expect(tool(s)).toMatchObject({ status: "denied", reason: "AI access is off for this connection" });
  });

  it("toolStarted for an unknown call is a no-op", () => {
    const before = run([prompt]);
    expect(run([{ type: "toolStarted", callId: "nope" }], before).transcript).toEqual(before.transcript);
  });

  it("fileWritten does not change state", () => {
    const before = run([prompt]);
    expect(run([{ type: "fileWritten", path: "a.sql" }], before)).toEqual(before);
  });
});

describe("applyAgentEvent: approvals", () => {
  const ask: AgentStoreEvent[] = [
    prompt,
    started,
    { type: "toolCallProposed", callId: "c1", tool: "run_sql", arguments: { sql: "delete from t" }, risk: "needsApproval", reason: "writes" },
    { type: "approvalRequired", callId: "c1", tool: "run_sql", arguments: { sql: "delete from t" }, reason: "Deletes rows in production", preview: "delete from t" },
  ];

  it("a pending approval is tracked and the tool awaits it", () => {
    const s = run(ask);
    expect(s.pendingApproval).toBe("c1");
    expect(approval(s)).toMatchObject({ status: "pending", reason: "Deletes rows in production", preview: "delete from t" });
    expect(tool(s)?.status).toBe("awaitingApproval");
  });

  it("approving resumes the tool and clears the pending approval", () => {
    const s = run([...ask, { type: "approvalResolved", callId: "c1", approved: true, reason: null }]);
    expect(s.pendingApproval).toBeNull();
    expect(approval(s)?.status).toBe("approved");
    expect(tool(s)?.status).toBe("running");
  });

  it("rejecting keeps the reason and mutes the tool", () => {
    const s = run([...ask, { type: "approvalResolved", callId: "c1", approved: false, reason: "too risky" }]);
    expect(approval(s)).toMatchObject({ status: "rejected", rejectReason: "too risky" });
    expect(tool(s)).toMatchObject({ status: "rejected", summary: "Rejected: too risky" });
    // The backend then reports the tool result; the muted state survives.
    const after = run([{ type: "toolFinished", callId: "c1", ok: false, summary: "rejected by user", elapsedMs: 0 }], s);
    expect(tool(after)?.status).toBe("rejected");
  });

  it("creates the tool card if approvalRequired arrives first", () => {
    const s = run([prompt, { type: "approvalRequired", callId: "z", tool: "write_file", arguments: { path: "m.sql" }, reason: "Writes a file", preview: "--- a\n+++ b" }]);
    expect(tool(s, "z")).toMatchObject({ tool: "write_file", status: "awaitingApproval" });
    expect(s.pendingApproval).toBe("z");
  });

  it("finishing the turn while pending settles the approval", () => {
    const s = run([...ask, { type: "turnFinished", stopReason: "cancelled", usage: null }]);
    expect(s.pendingApproval).toBeNull();
    expect(approval(s)?.status).toBe("rejected");
    expect(tool(s)?.status).toBe("error");
  });
});

describe("applyAgentEvent: failures and cancel", () => {
  it("failed adds an error item, stops running and settles in-flight tools", () => {
    const s = run([
      prompt,
      started,
      { type: "textDelta", text: "partial" },
      { type: "toolCallProposed", callId: "c1", tool: "run_sql", arguments: {}, risk: "safe", reason: null },
      { type: "toolStarted", callId: "c1" },
      { type: "failed", error: { code: "provider", message: "401 unauthorized" } },
    ]);
    expect(s.running).toBe(false);
    expect(tool(s)?.status).toBe("error");
    expect(s.transcript[s.transcript.length - 1]).toEqual({ kind: "error", id: expect.any(String), error: { code: "provider", message: "401 unauthorized" } });
  });

  it("a rejected start promise becomes an error item", () => {
    const s = run([prompt, { type: "clientError", error: { code: "not_found", message: "no such provider" } }]);
    expect(s.running).toBe(false);
    expect(kinds(s)).toEqual(["user", "error"]);
  });

  it("a user cancel ends the turn quietly, and the backend's cancelled failure adds nothing", () => {
    let s = run([prompt, started, { type: "textDelta", text: "thinking" }, { type: "cancelled" }]);
    expect(s.running).toBe(false);
    const n = s.transcript.length;
    s = run([{ type: "failed", error: { code: "cancelled", message: "turn cancelled" } }], s);
    expect(s.transcript.length).toBe(n);
  });

  it("noticeError appends an error without ending the turn", () => {
    const s = run([prompt, started, { type: "noticeError", error: { code: "other", message: "cancel failed" } }]);
    expect(s.running).toBe(true);
    expect(kinds(s)).toEqual(["user", "error"]);
  });

  it("a second prompt starts a new running turn after a finished one", () => {
    const s = run([prompt, started, { type: "turnFinished", stopReason: "end_turn", usage: null }, { type: "userPrompt", text: "again", attached: [] }]);
    expect(s.running).toBe(true);
    expect(userPrompts(s.transcript)).toEqual(["hello", "again"]);
  });
});
