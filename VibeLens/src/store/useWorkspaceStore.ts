// Editor tabs. Shared contract: the agent terminal reads the active tab for context and
// calls `openWorkspaceFile` when the agent writes a file.
import { create } from "zustand";
import { api } from "../ipc/commands";
import { uid } from "../lib/id";

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
  activeTab: () => WorkspaceTab | null;
}

const kindForPath = (p: string): TabKind => (/\.(valkey|redis)$/i.test(p) ? "valkey" : "sql");
const basename = (p: string) => p.split("/").pop() ?? p;

export const useWorkspaceStore = create<WorkspaceState>((set, get) => ({
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
  activeTab: () => get().tabs.find((t) => t.id === get().activeTabId) ?? null,
}));
