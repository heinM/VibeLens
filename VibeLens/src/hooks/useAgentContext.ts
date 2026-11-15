import { useQuery } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import type { AgentContext, ConnectionProfile } from "../ipc/types";
import { useUIStore } from "../store/useUIStore";
import { useWorkspaceStore, type WorkspaceTab } from "../store/useWorkspaceStore";
import { useLastResultStore, type LastResultSnapshot } from "../components/agent/lastResult";

export interface ChipExclusions {
  connection: boolean;
  file: boolean;
  selection: boolean;
}
export const NO_EXCLUSIONS: ChipExclusions = { connection: false, file: false, selection: false };

export interface Target {
  profileId: string | null;
  database: string | null;
}

/** The active tab's connection wins; otherwise fall back to the sidebar selection. */
export function resolveTarget(tab: Pick<WorkspaceTab, "profileId" | "database" | "kind"> | null, uiProfileId: string | null, uiDatabase: string | null): Target {
  if (tab?.profileId) return { profileId: tab.profileId, database: tab.kind === "sql" ? tab.database : null };
  return { profileId: uiProfileId, database: uiDatabase };
}

export function fileLabel(tab: Pick<WorkspaceTab, "path" | "title"> | null): string | null {
  if (!tab) return null;
  const p = tab.path ?? tab.title;
  return p.split("/").pop() || p;
}

/** Builds the context sent with a turn, honouring chips the user removed. */
export function buildAgentContext(
  target: Target,
  tab: Pick<WorkspaceTab, "path" | "title" | "contents" | "selectedText"> | null,
  exclude: ChipExclusions,
): AgentContext {
  return {
    profileId: exclude.connection ? null : target.profileId,
    database: exclude.connection ? null : target.database,
    openFile: exclude.file || !tab ? null : { path: tab.path ?? tab.title, contents: tab.contents },
    selectedText: exclude.selection || !tab?.selectedText ? null : tab.selectedText,
  };
}

export interface AgentContextInfo extends Target {
  profile: ConnectionProfile | null;
  tab: WorkspaceTab | null;
  fileName: string | null;
  selectionChars: number;
  /** Valkey logical db of the active tab, shown in the chip only. */
  valkeyDb: number | null;
  /** The newest query result, when it came from the connection the agent is looking at. */
  lastResult: LastResultSnapshot | null;
}

/** Live view of what the agent would see right now (for the header chips). */
export function useAgentContextInfo(): AgentContextInfo {
  const uiProfileId = useUIStore((s) => s.activeProfileId);
  const uiDatabase = useUIStore((s) => s.activeDatabase);
  const tab = useWorkspaceStore((s) => s.tabs.find((t) => t.id === s.activeTabId) ?? null);
  const profiles = useQuery({ queryKey: ["profiles"], queryFn: () => api.listProfiles() });
  const target = resolveTarget(tab, uiProfileId, uiDatabase);
  const profile = profiles.data?.find((p) => p.id === target.profileId) ?? null;
  const snapshot = useLastResultStore((st) => st.snapshot);
  return {
    ...target,
    profile,
    tab,
    fileName: fileLabel(tab),
    selectionChars: tab?.selectedText?.length ?? 0,
    valkeyDb: tab?.kind === "valkey" && tab.profileId ? tab.valkeyDb : null,
    lastResult: snapshot && target.profileId && snapshot.profileId === target.profileId ? snapshot : null,
  };
}
