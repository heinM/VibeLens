// "Explain with AI": one agent turn that streams a diagnosis for the current plan.
// State lives here (not in the panel) so switching tabs in PlanView doesn't drop a stream.
import { useCallback, useEffect, useRef, useState } from "react";
import { InlineAiError, askAgentOnce } from "../../lib/aiInline";
import type { AiDataAccess } from "../../ipc/types";

export type ExplainAiState =
  | { status: "idle" }
  | { status: "running"; text: string; activity: string | null }
  | { status: "done"; text: string }
  /** Stopped by the user, or because the model asked for something that needs approval. */
  | { status: "stopped"; text: string; reason: "cancelled" | "approval" }
  | { status: "error"; text: string; code: string; message: string };

const ACTIVITY: Record<string, string> = {
  get_schema: "Reading the schema",
  describe_table: "Reading table definitions",
  list_connections: "Checking connections",
};

export function useExplainAi({ prompt, profileId, database, access, blocked = false }: { prompt: string; profileId: string | null; database: string | null; access: AiDataAccess | null; blocked?: boolean }) {
  const [state, setState] = useState<ExplainAiState>({ status: "idle" });
  const abort = useRef<AbortController | null>(null);
  const reason = useRef<"cancelled" | "approval">("cancelled");
  const live = useRef(true);
  const textRef = useRef("");

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
      abort.current?.abort();
    };
  }, []);

  const cancel = useCallback(() => {
    reason.current = "cancelled";
    abort.current?.abort();
  }, []);

  const reset = useCallback(() => {
    abort.current?.abort();
    setState({ status: "idle" });
  }, []);

  const run = useCallback(async () => {
    // AI access `none`: the assistant learns nothing about this connection, plan or SQL. `blocked`
    // means the prompt could not be prepared safely (see PlanView).
    if (access === "none" || blocked) return;
    abort.current?.abort();
    const ac = new AbortController();
    abort.current = ac;
    reason.current = "cancelled";
    textRef.current = "";
    setState({ status: "running", text: "", activity: null });
    const set = (s: ExplainAiState) => {
      if (live.current && abort.current === ac) setState(s);
    };
    try {
      const res = await askAgentOnce({
        prompt,
        context: { profileId, database, openFile: null, selectedText: null },
        signal: ac.signal,
        onText: (t) => {
          textRef.current = t;
          set({ status: "running", text: t, activity: null });
        },
        onEvent: (ev) => {
          if (ev.type === "toolCallProposed") set({ status: "running", text: textRef.current, activity: ACTIVITY[ev.tool] ?? `Using ${ev.tool}` });
          if (ev.type === "approvalRequired") {
            // Inline explanations only use read-only lookups; anything needing approval is out of scope here.
            reason.current = "approval";
            ac.abort();
          }
        },
      });
      set({ status: "done", text: res.text });
    } catch (e) {
      if (e instanceof InlineAiError) {
        if (e.payload.code === "cancelled") set({ status: "stopped", text: textRef.current, reason: reason.current });
        else set({ status: "error", text: textRef.current, code: e.payload.code, message: e.payload.message });
      } else {
        set({ status: "error", text: textRef.current, code: "other", message: e instanceof Error ? e.message : String(e) });
      }
    }
  }, [prompt, profileId, database, access, blocked]);

  return { state, run, cancel, reset };
}
