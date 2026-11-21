// AI agent transcript + session state. The transcript is an append-only event log, so it lives
// here rather than in TanStack Query. Only `defaultProviderId` is persisted (non-secret).
// The pinned target is deliberately not persisted: a restart starts out following focus again.
import { create } from "zustand";
import { persist } from "zustand/middleware";
import { api } from "../ipc/commands";
import { uid } from "../lib/id";
import { applyAgentEvent, initialAgentCore, type AgentCore, type AgentStoreEvent } from "../components/agent/agentReducer";

export { applyAgentEvent, initialAgentCore, userPrompts } from "../components/agent/agentReducer";
export type * from "../components/agent/agentReducer";

/** A connection (and database) the agent stays on whatever tab or sidebar item has focus. */
export interface PinnedTarget {
  profileId: string;
  database: string | null;
}

interface AgentState extends AgentCore {
  sessionId: string;
  defaultProviderId: string | null;
  setDefaultProviderId: (id: string | null) => void;
  /** null = follow focus (active tab, else the sidebar selection). Survives "New session". */
  pinnedTarget: PinnedTarget | null;
  pinTarget: (t: PinnedTarget) => void;
  unpin: () => void;
  dispatch: (ev: AgentStoreEvent) => void;
  /** Ends the current session (cancelling any running turn) and starts a fresh one. */
  reset: () => void;
}

export const useAgentStore = create<AgentState>()(
  persist(
    (set, get) => ({
      ...initialAgentCore(),
      sessionId: uid(),
      defaultProviderId: null,
      setDefaultProviderId: (defaultProviderId) => set({ defaultProviderId }),
      pinnedTarget: null,
      pinTarget: ({ profileId, database }) => set({ pinnedTarget: { profileId, database } }),
      unpin: () => set({ pinnedTarget: null }),
      dispatch: (ev) => set((s) => applyAgentEvent(s, ev)),
      reset: () => {
        const old = get().sessionId;
        if (get().running) void api.agentCancel(old).catch(() => undefined);
        void api.agentReset(old).catch(() => undefined);
        set({ ...initialAgentCore(), sessionId: uid() });
      },
    }),
    {
      name: "vibelens.agent",
      version: 1,
      partialize: (s) => ({ defaultProviderId: s.defaultProviderId }),
    },
  ),
);
