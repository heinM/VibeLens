import { useEffect } from "react";
import { useQuery } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import type { AgentContext, ConnectionProfile } from "../ipc/types";
import { useAgentStore, type PinnedTarget } from "../store/useAgentStore";
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

/**
 * The target of the next turn: the pinned one while the agent is pinned, otherwise whatever has
 * focus ({@link resolveTarget}).
 */
export function resolveAgentTarget(
  pinned: PinnedTarget | null,
  tab: Pick<WorkspaceTab, "profileId" | "database" | "kind"> | null,
  uiProfileId: string | null,
  uiDatabase: string | null,
): Target {
  if (pinned) return { profileId: pinned.profileId, database: pinned.database };
  return resolveTarget(tab, uiProfileId, uiDatabase);
}

/** False once the pinned connection is gone from a loaded profile list (deleted). */
export function pinStillValid(pinned: PinnedTarget | null, profiles: Pick<ConnectionProfile, "id">[] | undefined): boolean {
  if (!pinned || !profiles) return true; // nothing pinned, or not loaded yet: no verdict
  return profiles.some((p) => p.id === pinned.profileId);
}

/** The target of the next turn, read from the stores at call time (event handlers, IPC callbacks). */
export function currentAgentTarget(): Target {
  const ui = useUIStore.getState();
  return resolveAgentTarget(useAgentStore.getState().pinnedTarget, useWorkspaceStore.getState().activeTab(), ui.activeProfileId, ui.activeDatabase);
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
  /** True while the agent is pinned to this target rather than following focus. */
  pinned: boolean;
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
  const pinnedTarget = useAgentStore((s) => s.pinnedTarget);
  const profiles = useQuery({ queryKey: ["profiles"], queryFn: () => api.listProfiles() });
  // A pin on a connection that has since been deleted lets go by itself.
  const stale = !pinStillValid(pinnedTarget, profiles.data);
  useEffect(() => {
    if (stale) useAgentStore.getState().unpin();
  }, [stale]);
  const pinned = stale ? null : pinnedTarget;
  const target = resolveAgentTarget(pinned, tab, uiProfileId, uiDatabase);
  const profile = profiles.data?.find((p) => p.id === target.profileId) ?? null;
  const snapshot = useLastResultStore((st) => st.snapshot);
  return {
    ...target,
    pinned: !!pinned,
    profile,
    tab,
    fileName: fileLabel(tab),
    selectionChars: tab?.selectedText?.length ?? 0,
    // The Valkey db shown is the active tab's, so only when that tab is on the target connection.
    valkeyDb: tab?.kind === "valkey" && tab.profileId && tab.profileId === target.profileId ? tab.valkeyDb : null,
    lastResult: snapshot && target.profileId && snapshot.profileId === target.profileId ? snapshot : null,
  };
}
