// One-shot AI requests for inline features (⌘K in the editor, "Fix with AI", "Explain this plan").
// Runs a normal agent turn in its own throwaway session, so the same policy, approval gate and
// AI-data-access rules apply as in the agent panel. Returns the final assistant text.
import { api, asError } from "../ipc/commands";
import type { AgentContext, AgentEvent, ErrorPayload, ProviderSummary } from "../ipc/types";
import { useAgentStore } from "../store/useAgentStore";
import { uid } from "./id";

const usable = (p: ProviderSummary) => p.config.model.trim() !== "" && (!p.config.requiresApiKey || p.hasApiKey);

/** Picks the user's default provider if usable, else the first usable one. */
export async function pickProvider(): Promise<ProviderSummary | null> {
  const providers = await api.agentListProviders();
  const preferred = useAgentStore.getState().defaultProviderId;
  return providers.find((p) => p.config.id === preferred && usable(p)) ?? providers.find(usable) ?? null;
}

/** Shown wherever an AI affordance is unavailable because the connection's AI access is `none`. */
export const AI_OFF_HINT = "AI access is off for this connection";

export interface InlineAiOptions {
  prompt: string;
  context: AgentContext;
  /** Streaming text as it arrives. */
  onText?: (fullTextSoFar: string) => void;
  /** Any non-text event (tool calls, approvals) for optional progress UI. `sessionId` lets a caller
   *  resolve an `approvalRequired` via `api.agentResolveApproval(sessionId, callId, …)`. */
  onEvent?: (ev: AgentEvent, sessionId: string) => void;
  signal?: AbortSignal;
}

export interface InlineAiResult {
  text: string;
  /** First fenced code block in the answer (```sql … ```), if any. */
  code: string | null;
}

export class InlineAiError extends Error {
  constructor(public payload: ErrorPayload) {
    super(payload.message);
  }
}

export function firstCodeBlock(text: string): string | null {
  const m = /```[a-zA-Z0-9_+-]*\s*\n([\s\S]*?)```/.exec(text);
  return m ? m[1].trimEnd() : null;
}

/**
 * Runs a single agent turn and resolves with the answer. Tool calls that need approval surface
 * in the agent panel's approval flow only if that session is shown; inline callers should phrase
 * prompts so the agent only needs read-only tools (schema lookups), or handle `approvalRequired`
 * via `onEvent` and call `api.agentResolveApproval(sessionId, …)`.
 */
export async function askAgentOnce(opts: InlineAiOptions): Promise<InlineAiResult & { sessionId: string }> {
  const provider = await pickProvider();
  if (!provider) throw new InlineAiError({ code: "invalid", message: "No AI provider is configured. Open Settings → AI providers." });
  const sessionId = `inline-${uid()}`;
  let text = "";
  return new Promise((resolve, reject) => {
    const finish = (fn: () => void) => {
      opts.signal?.removeEventListener("abort", onAbort);
      void api.agentReset(sessionId).catch(() => {});
      fn();
    };
    const onAbort = () => {
      void api.agentCancel(sessionId).catch(() => {});
      finish(() => reject(new InlineAiError({ code: "cancelled", message: "cancelled" })));
    };
    opts.signal?.addEventListener("abort", onAbort);
    api
      .agentStartTurn({
        sessionId,
        providerId: provider.config.id,
        prompt: opts.prompt,
        context: opts.context,
        onEvent: (ev) => {
          if (ev.type === "textDelta") {
            text += ev.text;
            opts.onText?.(text);
          } else {
            opts.onEvent?.(ev, sessionId);
          }
          if (ev.type === "turnFinished") finish(() => resolve({ text, code: firstCodeBlock(text), sessionId }));
          if (ev.type === "failed") finish(() => reject(new InlineAiError(ev.error)));
        },
      })
      .catch((e) => finish(() => reject(new InlineAiError(asError(e)))));
  });
}
