import { useCallback } from "react";
import { api, asError } from "../ipc/commands";
import type { ApprovalDecision } from "../ipc/types";
import { useAgentStore } from "../store/useAgentStore";
import { useUIStore } from "../store/useUIStore";
import { useWorkspaceStore } from "../store/useWorkspaceStore";
import { useLastResultStore, withLastResult } from "../components/agent/lastResult";
import { buildAgentContext, currentAgentTarget, fileLabel, type ChipExclusions } from "./useAgentContext";

export interface SendArgs {
  prompt: string;
  providerId: string;
  exclude: ChipExclusions;
  /** Display name of the connection, for the "attached" summary in the transcript. */
  profileName: string | null;
  /**
   * Attach the last query result (columns and the first rows) to this message. The caller must
   * already have checked the connection's AI access; this only honours an explicit user choice.
   */
  attachResult?: boolean;
}

/** Starts, cancels and answers agent turns. Reads store state at call time, so it never goes stale. */
export function useAgentTurn() {
  const send = useCallback(async ({ prompt, providerId, exclude, profileName, attachResult = false }: SendArgs) => {
    const store = useAgentStore.getState();
    if (store.running || !prompt.trim()) return;
    const sessionId = store.sessionId;
    const tab = useWorkspaceStore.getState().activeTab();
    // Pinned: the stored target, whatever has focus now. Otherwise the focused connection.
    const target = currentAgentTarget();
    const context = buildAgentContext(target, tab, exclude);

    const attached: string[] = [];
    if (context.profileId) attached.push(profileName ?? "connection");
    if (context.openFile) attached.push(fileLabel(tab) ?? context.openFile.path);
    if (context.selectedText) attached.push("selection");

    const snapshot = attachResult ? useLastResultStore.getState().snapshot : null;
    if (snapshot && snapshot.profileId === target.profileId) attached.push("last result");
    const wire = withLastResult(prompt, snapshot && snapshot.profileId === target.profileId ? snapshot : null);

    store.dispatch({ type: "userPrompt", text: prompt.trim(), attached });
    try {
      await api.agentStartTurn({
        sessionId,
        providerId,
        prompt: wire,
        context,
        onEvent: (ev) => {
          const s = useAgentStore.getState();
          if (s.sessionId !== sessionId) return; // session was reset; drop stragglers
          s.dispatch(ev);
          if (ev.type === "fileWritten") {
            void useWorkspaceStore
              .getState()
              .openWorkspaceFile(ev.path, s.pinnedTarget?.profileId ?? useUIStore.getState().activeProfileId)
              .catch(() => undefined);
          }
        },
      });
    } catch (e) {
      if (useAgentStore.getState().sessionId === sessionId) useAgentStore.getState().dispatch({ type: "clientError", error: asError(e) });
    }
  }, []);

  const cancel = useCallback(async () => {
    const { sessionId, dispatch } = useAgentStore.getState();
    try {
      await api.agentCancel(sessionId);
      if (useAgentStore.getState().sessionId === sessionId) dispatch({ type: "cancelled" });
    } catch (e) {
      dispatch({ type: "noticeError", error: asError(e) });
    }
  }, []);

  /** Resolves true when the backend accepted the decision. */
  const resolveApproval = useCallback(async (callId: string, decision: ApprovalDecision): Promise<boolean> => {
    const { sessionId, dispatch } = useAgentStore.getState();
    try {
      await api.agentResolveApproval(sessionId, callId, decision);
      dispatch({
        type: "approvalResolved",
        callId,
        approved: decision.decision === "approve",
        reason: decision.decision === "reject" ? decision.reason : null,
      });
      return true;
    } catch (e) {
      dispatch({ type: "noticeError", error: asError(e) });
      return false;
    }
  }, []);

  return { send, cancel, resolveApproval };
}
