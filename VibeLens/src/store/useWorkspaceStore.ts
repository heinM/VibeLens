// Editor tabs. Shared contract: the agent terminal reads the active tab for context and
// calls `openWorkspaceFile` when the agent writes a file.
//
// The tab list is kept in localStorage and restored after a reload or restart (what is kept, and what
// never is, is in `workspacePersist.ts`). Connections live in the Rust process, so a restored tab only
// needs its binding back: `reconcileRestored` unbinds tabs whose profile is gone, and
// `refreshFileTabs` re-reads clean file-backed tabs from their file.
import { create } from "zustand";
import { persist } from "zustand/middleware";
import { api } from "../ipc/commands";
import { uid } from "../lib/id";
import {
  WORKSPACE_PERSIST_VERSION,
  WORKSPACE_STORAGE_KEY,
  createDebouncedStorage,
  sanitizePersisted,
  shrinkWorkspace,
  toPersisted,
  toWorkspaceTab,
  unbindMissingProfiles,
  type Backing,
  type PersistedWorkspace,
} from "./workspacePersist";

export type TabKind = "sql" | "valkey" | "table" | "erd" | "icetable";

export interface WorkspaceTab {
  id: string;
  kind: TabKind;
  title: string;
  profileId: string | null;
  database: string | null; // postgres
  valkeyDb: number; // valkey
  /** Workspace-relative path when backed by a file, else null (scratch). */
  path: string | null;
  contents: string;
  dirty: boolean;
  /** Current editor selection (for agent context). */
  selectedText: string | null;
  /** "table": the table/view being browsed. "erd": `name` is unused; `schema` is the schema drawn.
   *  "icetable": Iceberg table; `schema` is the namespace levels joined with "." (use `namespaceLevels`). */
  objectRef: { schema: string; name: string } | null;
}

interface WorkspaceState {
  tabs: WorkspaceTab[];
  activeTabId: string | null;
  openTab: (t: Partial<WorkspaceTab> & { kind: TabKind }) => string;
  closeTab: (id: string) => void;
  setActive: (id: string) => void;
  updateTab: (id: string, patch: Partial<WorkspaceTab>) => void;
  /** Reads the file and opens it (or refreshes the open tab if not dirty). */
  openWorkspaceFile: (path: string, profileId?: string | null) => Promise<string>;
  saveTab: (id: string, path?: string) => Promise<void>;
  /** Once per page load, after the profile list is known: tabs bound to a profile that no longer exists become unbound. */
  reconcileRestored: (profileIds: readonly string[]) => void;
  /** Once per page load: clean tabs backed by a workspace file are read from the file again (it is the truth, and a tab whose text was too big to keep has none). */
  refreshFileTabs: () => Promise<void>;
  activeTab: () => WorkspaceTab | null;
}

const kindForPath = (p: string): TabKind => (/\.(valkey|redis)$/i.test(p) ? "valkey" : "sql");
const basename = (p: string) => p.split("/").pop() ?? p;

const memory = new Map<string, string>();
const memoryBacking: Backing = { getItem: (k) => memory.get(k) ?? null, setItem: (k, v) => void memory.set(k, v), removeItem: (k) => void memory.delete(k) };

function localBacking(): Backing {
  try {
    return window.localStorage;
  } catch {
    return memoryBacking;
  }
}

/** Typing costs one write per pause (and at least one every few seconds), not one per keystroke. */
export const workspaceStorage = createDebouncedStorage<PersistedWorkspace>(localBacking, { delayMs: 500, maxWaitMs: 3000, shrink: shrinkWorkspace });

// A reload or the window going away must not lose the last half second of typing.
try {
  if (typeof window !== "undefined" && typeof window.addEventListener === "function") {
    window.addEventListener("pagehide", workspaceStorage.flush);
    window.addEventListener("beforeunload", workspaceStorage.flush);
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "hidden") workspaceStorage.flush();
    });
  }
} catch {
  /* no window (tests): nothing to flush on */
}

let reconciled = false;
let fileTabsRefreshed = false;

export const useWorkspaceStore = create<WorkspaceState>()(
  persist(
    (set, get) => ({
      tabs: [],
      activeTabId: null,

      openTab: (t) => {
        const id = t.id ?? uid();
        const tab: WorkspaceTab = {
          id,
          kind: t.kind,
          title: t.title ?? (t.kind === "sql" ? "untitled.sql" : "console.valkey"),
          profileId: t.profileId ?? null,
          database: t.database ?? null,
          valkeyDb: t.valkeyDb ?? 0,
          path: t.path ?? null,
          contents: t.contents ?? "",
          dirty: t.dirty ?? false,
          selectedText: null,
          objectRef: t.objectRef ?? null,
        };
        set((s) => ({ tabs: [...s.tabs, tab], activeTabId: id }));
        return id;
      },
      closeTab: (id) =>
        set((s) => {
          const idx = s.tabs.findIndex((t) => t.id === id);
          const tabs = s.tabs.filter((t) => t.id !== id);
          const activeTabId = s.activeTabId === id ? (tabs[Math.max(0, idx - 1)]?.id ?? null) : s.activeTabId;
          return { tabs, activeTabId };
        }),
      setActive: (id) => set({ activeTabId: id }),
      updateTab: (id, patch) => set((s) => ({ tabs: s.tabs.map((t) => (t.id === id ? { ...t, ...patch } : t)) })),
      openWorkspaceFile: async (path, profileId = null) => {
        const contents = await api.wsRead(path);
        const existing = get().tabs.find((t) => t.path === path);
        if (existing) {
          if (!existing.dirty) get().updateTab(existing.id, { contents });
          set({ activeTabId: existing.id });
          return existing.id;
        }
        return get().openTab({ kind: kindForPath(path), title: basename(path), path, contents, profileId });
      },
      saveTab: async (id, path) => {
        const tab = get().tabs.find((t) => t.id === id);
        if (!tab) return;
        const target = path ?? tab.path;
        if (!target) throw new Error("No path to save to");
        await api.wsWrite(target, tab.contents);
        get().updateTab(id, { path: target, title: basename(target), dirty: false });
      },
      reconcileRestored: (profileIds) => {
        if (reconciled) return;
        reconciled = true;
        const next = unbindMissingProfiles(get().tabs, new Set(profileIds));
        if (next !== get().tabs) set({ tabs: next });
      },
      refreshFileTabs: async () => {
        if (fileTabsRefreshed) return;
        fileTabsRefreshed = true;
        const targets = get().tabs.filter((t) => t.path !== null && !t.dirty);
        await Promise.all(
          targets.map(async (t) => {
            try {
              const contents = await api.wsRead(t.path as string);
              const now = get().tabs.find((x) => x.id === t.id);
              // Not if the user has started typing, or the tab was re-pointed, while the read was out.
              if (now && !now.dirty && now.path === t.path && now.contents !== contents) get().updateTab(t.id, { contents });
            } catch {
              /* the file is gone or unreadable: keep what was restored */
            }
          }),
        );
      },
      activeTab: () => get().tabs.find((t) => t.id === get().activeTabId) ?? null,
    }),
    {
      name: WORKSPACE_STORAGE_KEY,
      version: WORKSPACE_PERSIST_VERSION,
      storage: workspaceStorage,
      partialize: (s) => toPersisted(s.tabs, s.activeTabId),
      // Whatever version wrote it, only what validates survives.
      migrate: (persisted) => sanitizePersisted(persisted),
      merge: (persisted, current) => {
        const restored = sanitizePersisted(persisted);
        return { ...current, tabs: restored.tabs.map(toWorkspaceTab), activeTabId: restored.activeTabId };
      },
    },
  ),
);
