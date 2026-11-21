// The real store, wired to a fake localStorage: hydration on creation, what is written, what is not.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { WORKSPACE_STORAGE_KEY } from "../workspacePersist";

vi.mock("../../ipc/commands", () => ({ api: { wsRead: vi.fn(), wsWrite: vi.fn() }, asError: (e: unknown) => e }));

let data: Map<string, string>;

/** Loads a fresh copy of the store module, as a webview reload would. */
async function freshStore() {
  vi.resetModules();
  const mod = await import("../useWorkspaceStore");
  const { api } = await import("../../ipc/commands");
  return { ...mod, api: api as unknown as { wsRead: ReturnType<typeof vi.fn> } };
}

beforeEach(() => {
  data = new Map();
  vi.stubGlobal("window", { localStorage: { getItem: (k: string) => data.get(k) ?? null, setItem: (k: string, v: string) => void data.set(k, v), removeItem: (k: string) => void data.delete(k) } });
});
afterEach(() => vi.unstubAllGlobals());

describe("workspace store persistence", () => {
  it("restores open tabs, the active tab and their editor text after a reload", async () => {
    const a = await freshStore();
    const s = a.useWorkspaceStore.getState();
    const t1 = s.openTab({ kind: "sql", title: "scratch.sql", profileId: "p1", database: "shop", contents: "select 1", dirty: true });
    s.openTab({ kind: "valkey", profileId: "p2", valkeyDb: 2 });
    s.setActive(t1);
    a.workspaceStorage.flush();

    const b = await freshStore();
    const { tabs, activeTabId } = b.useWorkspaceStore.getState();
    expect(activeTabId).toBe(t1);
    expect(tabs.map((t) => [t.kind, t.title, t.profileId, t.database, t.valkeyDb, t.contents, t.dirty])).toEqual([
      ["sql", "scratch.sql", "p1", "shop", 0, "select 1", true],
      ["valkey", "console.valkey", "p2", null, 2, "", false],
    ]);
    expect(tabs.every((t) => t.selectedText === null)).toBe(true);
  });

  it("does not write on every keystroke", async () => {
    vi.useFakeTimers();
    try {
      const a = await freshStore();
      const id = a.useWorkspaceStore.getState().openTab({ kind: "sql" });
      a.workspaceStorage.flush();
      const setItem = vi.spyOn(window.localStorage, "setItem");
      for (let i = 0; i < 100; i++) a.useWorkspaceStore.getState().updateTab(id, { contents: "x".repeat(i), dirty: true });
      expect(setItem).not.toHaveBeenCalled();
      vi.advanceTimersByTime(600);
      expect(setItem).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("starts empty when the stored state is corrupt", async () => {
    for (const text of ["{oops", '{"state":"junk","version":1}', '{"state":{"tabs":[{"id":1}]},"version":1}', '{"state":{"tabs":[{"id":"a","kind":"nope"}]},"version":0}']) {
      data.set(WORKSPACE_STORAGE_KEY, text);
      const { useWorkspaceStore } = await freshStore();
      expect(useWorkspaceStore.getState().tabs).toEqual([]);
      expect(useWorkspaceStore.getState().activeTabId).toBeNull();
    }
  });

  it("unbinds tabs whose connection no longer exists, once", async () => {
    const a = await freshStore();
    a.useWorkspaceStore.getState().openTab({ kind: "sql", profileId: "gone", database: "d" });
    a.useWorkspaceStore.getState().openTab({ kind: "sql", profileId: "p1", database: "d" });
    a.workspaceStorage.flush();

    const b = await freshStore();
    b.useWorkspaceStore.getState().reconcileRestored(["p1"]);
    expect(b.useWorkspaceStore.getState().tabs.map((t) => [t.profileId, t.database])).toEqual([[null, null], ["p1", "d"]]);
    b.useWorkspaceStore.getState().reconcileRestored([]); // a second call is ignored
    expect(b.useWorkspaceStore.getState().tabs[1].profileId).toBe("p1");
  });

  it("reads clean file-backed tabs from their file again, but not dirty ones", async () => {
    const a = await freshStore();
    const s = a.useWorkspaceStore.getState();
    const clean = s.openTab({ kind: "sql", path: "a.sql", contents: "old", dirty: false });
    const dirty = s.openTab({ kind: "sql", path: "b.sql", contents: "my edit", dirty: true });
    a.workspaceStorage.flush();

    const b = await freshStore();
    b.api.wsRead.mockImplementation(async (p: string) => (p === "a.sql" ? "new on disk" : "disk b"));
    await b.useWorkspaceStore.getState().refreshFileTabs();
    const byId = new Map(b.useWorkspaceStore.getState().tabs.map((t) => [t.id, t]));
    expect(byId.get(clean)?.contents).toBe("new on disk");
    expect(byId.get(dirty)?.contents).toBe("my edit");
    expect(b.api.wsRead).toHaveBeenCalledTimes(1);
  });

  it("brings a file-backed tab whose text was too big back from the file", async () => {
    const a = await freshStore();
    const id = a.useWorkspaceStore.getState().openTab({ kind: "sql", path: "big.sql", contents: "z".repeat(1024 * 1024 + 1), dirty: true });
    a.workspaceStorage.flush();

    const b = await freshStore();
    expect(b.useWorkspaceStore.getState().tabs[0]).toMatchObject({ id, contents: "", dirty: false, path: "big.sql" });
    b.api.wsRead.mockResolvedValue("from file");
    await b.useWorkspaceStore.getState().refreshFileTabs();
    expect(b.useWorkspaceStore.getState().tabs[0].contents).toBe("from file");
  });

  it("keeps the restored text when the file cannot be read", async () => {
    const a = await freshStore();
    a.useWorkspaceStore.getState().openTab({ kind: "sql", path: "gone.sql", contents: "kept", dirty: false });
    a.workspaceStorage.flush();
    const b = await freshStore();
    b.api.wsRead.mockRejectedValue(new Error("not found"));
    await b.useWorkspaceStore.getState().refreshFileTabs();
    expect(b.useWorkspaceStore.getState().tabs[0].contents).toBe("kept");
  });
});
