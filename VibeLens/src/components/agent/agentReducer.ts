// Pure transcript reducer for the AI agent. No React, no IPC: unit-tested in
// src/store/__tests__/agentReducer.test.ts.
import type { AgentEvent, ErrorPayload, ToolRisk, Usage } from "../../ipc/types";

export type ToolStatus = "proposed" | "awaitingApproval" | "running" | "ok" | "error" | "denied" | "rejected";

export interface UserItem {
  kind: "user";
  id: string;
  text: string;
  /** Human-readable chips that were attached, e.g. ["orders-db", "report.sql"]. */
  attached: string[];
}
export interface AssistantItem {
  kind: "assistant";
  id: string;
  text: string;
  streaming: boolean;
}
export interface ToolItem {
  kind: "tool";
  id: string;
  callId: string;
  tool: string;
  args: unknown;
  risk: ToolRisk;
  /** Why it was denied / flagged. */
  reason: string | null;
  status: ToolStatus;
  summary: string | null;
  elapsedMs: number | null;
}
export interface ApprovalItem {
  kind: "approval";
  id: string;
  callId: string;
  tool: string;
  args: unknown;
  reason: string;
  preview: string | null;
  status: "pending" | "approved" | "rejected";
  rejectReason: string | null;
}
export interface ErrorItem {
  kind: "error";
  id: string;
  error: ErrorPayload;
}
export interface UsageItem {
  kind: "usage";
  id: string;
  usage: Usage | null;
  stopReason: string;
}
export type TranscriptItem = UserItem | AssistantItem | ToolItem | ApprovalItem | ErrorItem | UsageItem;

export interface AgentCore {
  transcript: TranscriptItem[];
  running: boolean;
  turnId: string | null;
  /** callId of the approval awaiting a decision, if any. */
  pendingApproval: string | null;
  seq: number;
}

export const initialAgentCore = (): AgentCore => ({ transcript: [], running: false, turnId: null, pendingApproval: null, seq: 0 });

/** Events that originate on the client rather than from the Rust agent loop. */
export type ClientEvent =
  | { type: "userPrompt"; text: string; attached: string[] }
  | { type: "approvalResolved"; callId: string; approved: boolean; reason: string | null }
  | { type: "clientError"; error: ErrorPayload }
  /** An error that does not end the turn (e.g. a failed cancel or approval call). */
  | { type: "noticeError"; error: ErrorPayload }
  | { type: "cancelled" };

export type AgentStoreEvent = AgentEvent | ClientEvent;

const nextId = (s: AgentCore): [string, number] => [`i${s.seq}`, s.seq + 1];

function closeStreaming(items: TranscriptItem[]): TranscriptItem[] {
  if (!items.some((i) => i.kind === "assistant" && i.streaming)) return items;
  return items.map((i) => (i.kind === "assistant" && i.streaming ? { ...i, streaming: false } : i));
}

/** Marks tools that never reported back (turn ended / failed / cancelled) as finished-with-error. */
function settleTools(items: TranscriptItem[], summary: string): TranscriptItem[] {
  return items.map((i) => {
    if (i.kind === "tool" && (i.status === "running" || i.status === "proposed" || i.status === "awaitingApproval")) {
      return { ...i, status: "error", summary: i.summary ?? summary };
    }
    if (i.kind === "approval" && i.status === "pending") return { ...i, status: "rejected", rejectReason: summary };
    return i;
  });
}

function mapTool(items: TranscriptItem[], callId: string, fn: (t: ToolItem) => ToolItem): TranscriptItem[] {
  return items.map((i) => (i.kind === "tool" && i.callId === callId ? fn(i) : i));
}

export function applyAgentEvent(state: AgentCore, ev: AgentStoreEvent): AgentCore {
  switch (ev.type) {
    case "userPrompt": {
      const [id, seq] = nextId(state);
      const item: UserItem = { kind: "user", id, text: ev.text, attached: ev.attached };
      return { ...state, seq, running: true, turnId: null, pendingApproval: null, transcript: [...closeStreaming(state.transcript), item] };
    }

    case "turnStarted":
      return { ...state, running: true, turnId: ev.turnId };

    case "textDelta": {
      if (ev.text === "") return state;
      const last = state.transcript[state.transcript.length - 1];
      if (last?.kind === "assistant" && last.streaming) {
        const transcript = state.transcript.slice(0, -1);
        transcript.push({ ...last, text: last.text + ev.text });
        return { ...state, transcript };
      }
      const [id, seq] = nextId(state);
      return { ...state, seq, transcript: [...state.transcript, { kind: "assistant", id, text: ev.text, streaming: true }] };
    }

    case "toolCallProposed": {
      const [id, seq] = nextId(state);
      const existing = state.transcript.find((i) => i.kind === "tool" && i.callId === ev.callId);
      if (existing) {
        return {
          ...state,
          transcript: mapTool(state.transcript, ev.callId, (t) => ({ ...t, tool: ev.tool, args: ev.arguments, risk: ev.risk, reason: ev.reason })),
        };
      }
      const status: ToolStatus = ev.risk === "denied" ? "denied" : ev.risk === "needsApproval" ? "awaitingApproval" : "proposed";
      const item: ToolItem = { kind: "tool", id, callId: ev.callId, tool: ev.tool, args: ev.arguments, risk: ev.risk, reason: ev.reason, status, summary: null, elapsedMs: null };
      return { ...state, seq, transcript: [...closeStreaming(state.transcript), item] };
    }

    case "approvalRequired": {
      let { seq } = state;
      let transcript = closeStreaming(state.transcript);
      if (!transcript.some((i) => i.kind === "tool" && i.callId === ev.callId)) {
        const tid = `i${seq++}`;
        transcript = [
          ...transcript,
          { kind: "tool", id: tid, callId: ev.callId, tool: ev.tool, args: ev.arguments, risk: "needsApproval", reason: ev.reason, status: "awaitingApproval", summary: null, elapsedMs: null },
        ];
      } else {
        transcript = mapTool(transcript, ev.callId, (t) => (t.status === "proposed" ? { ...t, status: "awaitingApproval", risk: "needsApproval" } : t));
      }
      const item: ApprovalItem = {
        kind: "approval",
        id: `i${seq++}`,
        callId: ev.callId,
        tool: ev.tool,
        args: ev.arguments,
        reason: ev.reason,
        preview: ev.preview,
        status: "pending",
        rejectReason: null,
      };
      return { ...state, seq, transcript: [...transcript, item], pendingApproval: ev.callId };
    }

    case "approvalResolved": {
      const transcript = state.transcript.map((i): TranscriptItem => {
        if (i.kind === "approval" && i.callId === ev.callId && i.status === "pending") {
          return { ...i, status: ev.approved ? "approved" : "rejected", rejectReason: ev.approved ? null : ev.reason };
        }
        if (i.kind === "tool" && i.callId === ev.callId && i.status === "awaitingApproval") {
          return ev.approved ? { ...i, status: "running" } : { ...i, status: "rejected", summary: ev.reason ? `Rejected: ${ev.reason}` : "Rejected by you" };
        }
        return i;
      });
      return { ...state, transcript, pendingApproval: state.pendingApproval === ev.callId ? null : state.pendingApproval };
    }

    case "toolStarted":
      return { ...state, transcript: mapTool(state.transcript, ev.callId, (t) => (t.status === "denied" || t.status === "rejected" ? t : { ...t, status: "running" })) };

    case "toolFinished":
      return {
        ...state,
        transcript: mapTool(state.transcript, ev.callId, (t) => {
          // A call the policy denied or the user rejected stays muted; just keep the explanation.
          if (t.status === "denied" || t.status === "rejected") return { ...t, summary: t.summary ?? ev.summary, elapsedMs: ev.elapsedMs };
          return { ...t, status: ev.ok ? "ok" : "error", summary: ev.summary, elapsedMs: ev.elapsedMs };
        }),
        pendingApproval: state.pendingApproval === ev.callId ? null : state.pendingApproval,
      };

    case "fileWritten":
      return state; // handled as a side effect in useAgentTurn

    case "turnFinished": {
      let transcript = settleTools(closeStreaming(state.transcript), "No result was reported.");
      let seq = state.seq;
      if (ev.usage || ev.stopReason) {
        transcript = [...transcript, { kind: "usage", id: `i${seq++}`, usage: ev.usage, stopReason: ev.stopReason }];
      }
      return { ...state, seq, transcript, running: false, pendingApproval: null };
    }

    case "failed": {
      const transcript = settleTools(closeStreaming(state.transcript), "Turn failed before this finished.");
      // A user-requested cancel already ended the turn locally; don't add noise.
      if (ev.error.code === "cancelled") return { ...state, transcript, running: false, pendingApproval: null };
      const [id, seq] = nextId(state);
      return { ...state, seq, transcript: [...transcript, { kind: "error", id, error: ev.error }], running: false, pendingApproval: null };
    }

    case "clientError": {
      const [id, seq] = nextId(state);
      const transcript = settleTools(closeStreaming(state.transcript), "Turn failed before this finished.");
      return { ...state, seq, transcript: [...transcript, { kind: "error", id, error: ev.error }], running: false, pendingApproval: null };
    }

    case "noticeError": {
      const [id, seq] = nextId(state);
      return { ...state, seq, transcript: [...state.transcript, { kind: "error", id, error: ev.error }] };
    }

    case "cancelled":
      return {
        ...state,
        transcript: settleTools(closeStreaming(state.transcript), "Cancelled."),
        running: false,
        pendingApproval: null,
      };
  }
}

/** Prompts the user has sent, oldest first (for ArrowUp recall). */
export const userPrompts = (items: TranscriptItem[]): string[] => items.flatMap((i) => (i.kind === "user" ? [i.text] : []));
