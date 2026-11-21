import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  MAX_PERSISTED_TABS,
  MAX_PERSISTED_TEXT_BYTES,
  WORKSPACE_STORAGE_KEY,
  createDebouncedStorage,
  sanitizePersisted,
  shrinkWorkspace,
  toPersisted,
  toWorkspaceTab,
  unbindMissingProfiles,
  utf8Exceeds,
  type Backing,
  type PersistedWorkspace,
} from "../workspacePersist";
import type { WorkspaceTab } from "../useWorkspaceStore";

const tab = (id: string, over: Partial<WorkspaceTab> = {}): WorkspaceTab => ({
  id,
  kind: "sql",
  title: `${id}.sql`,
  profileId: "p1",
  database: "shop",
  valkeyDb: 0,
  path: null,
  contents: `select '${id}'`,
  dirty: true,
  selectedText: "sel",
  objectRef: null,
  ...over,
});

/** What the real path does: write out as JSON, read back, validate. */
const roundTrip = (tabs: WorkspaceTab[], active: string | null) => sanitizePersisted(JSON.parse(JSON.stringify(toPersisted(tabs, active))));

describe("persisted workspace round trip", () => {
  it("keeps tabs, order, active tab and every persisted field", () => {
    const tabs = [
      tab("a", { path: "reports/q1.sql", dirty: false, title: "q1.sql" }),
      tab("b", { kind: "valkey", title: "console.valkey", valkeyDb: 3, database: null, contents: "GET k" }),
      tab("c", { kind: "table", title: "orders", objectRef: { schema: "public", name: "orders" }, contents: "", dirty: false }),
    ];
    const back = roundTrip(tabs, "b");
    expect(back.activeTabId).toBe("b");
    expect(back.tabs.map((t) => t.id)).toEqual(["a", "b", "c"]);
    expect(back.tabs.map(toWorkspaceTab)).toEqual(tabs.map((t) => ({ ...t, selectedText: null })));
  });

  it("never writes selection, results, errors or run state", () => {
    const written = JSON.stringify(toPersisted([tab("a", { selectedText: "secret selection" })], "a"));
    expect(written).not.toContain("secret selection");
    expect(Object.keys(JSON.parse(written).tabs[0]).sort()).toEqual(["contents", "database", "dirty", "id", "kind", "objectRef", "path", "profileId", "title", "valkeyDb"]);
  });

  it("restores an empty workspace as empty", () => {
    expect(roundTrip([], null)).toEqual({ tabs: [], activeTabId: null });
  });

  it("falls back to the first tab when the active id is unknown", () => {
    expect(roundTrip([tab("a"), tab("b")], "gone").activeTabId).toBe("a");
  });
});

describe("text over 1 MiB", () => {
  const big = "x".repeat(MAX_PERSISTED_TEXT_BYTES + 1);

  it("is not kept, and the tab comes back clean", () => {
    const [t] = roundTrip([tab("a", { contents: big, dirty: true, path: "big.sql" })], "a").tabs;
    expect(t.contents).toBe("");
    expect(t.dirty).toBe(false);
    expect(t.path).toBe("big.sql");
  });

  it("counts bytes, not characters", () => {
    const exactlyOver = "é".repeat(MAX_PERSISTED_TEXT_BYTES / 2 + 1); // 2 bytes each
    expect(utf8Exceeds(exactlyOver, MAX_PERSISTED_TEXT_BYTES)).toBe(true);
    expect(utf8Exceeds("é".repeat(MAX_PERSISTED_TEXT_BYTES / 2), MAX_PERSISTED_TEXT_BYTES)).toBe(false);
    expect(utf8Exceeds("x".repeat(MAX_PERSISTED_TEXT_BYTES), MAX_PERSISTED_TEXT_BYTES)).toBe(false);
    expect(utf8Exceeds("x".repeat(MAX_PERSISTED_TEXT_BYTES + 1), MAX_PERSISTED_TEXT_BYTES)).toBe(true);
  });

  it("is dropped on read too, whatever wrote it", () => {
    const raw = { tabs: [{ ...toPersisted([tab("a")], "a").tabs[0], contents: big }], activeTabId: "a" };
    expect(sanitizePersisted(raw).tabs[0]).toMatchObject({ contents: "", dirty: false });
  });

  it("leaves the other tabs alone", () => {
    const back = roundTrip([tab("a", { contents: big }), tab("b")], "b");
    expect(back.tabs[1].contents).toBe("select 'b'");
    expect(back.tabs[1].dirty).toBe(true);
  });
});

describe("rejecting malformed stored state", () => {
  const good = () => toPersisted([tab("a")], "a").tabs[0];

  it("gives an empty workspace for anything that is not the expected object", () => {
    for (const raw of [null, undefined, 42, "tabs", [], true, {}, { tabs: "x" }, { tabs: {} }, { tabs: null }])
      expect(sanitizePersisted(raw)).toEqual({ tabs: [], activeTabId: null });
  });

  it("drops a tab with a wrong or missing required field, and keeps the good ones", () => {
    const bad: unknown[] = [
      null,
      "tab",
      [],
      { ...good(), id: "" },
      { ...good(), id: 7 },
      { ...good(), kind: "pivot" },
      { ...good(), title: undefined },
      { ...good(), contents: 5 },
      { ...good(), dirty: "yes" },
      { ...good(), profileId: 5 },
      { ...good(), database: {} },
      { ...good(), path: "" },
      { ...good(), path: 3 },
      { ...good(), valkeyDb: -1 },
      { ...good(), valkeyDb: 1.5 },
      { ...good(), valkeyDb: "0" },
      { ...good(), objectRef: "orders" },
      { ...good(), objectRef: { schema: "public" } },
      { ...good(), title: "t".repeat(5000) },
    ];
    const out = sanitizePersisted({ tabs: [...bad, { ...good(), id: "ok" }], activeTabId: "ok" });
    expect(out.tabs.map((t) => t.id)).toEqual(["ok"]);
    expect(out.activeTabId).toBe("ok");
  });

  it("fills in what an older or hand-edited payload left out", () => {
    const { id, kind, title, contents, dirty } = good();
    const [t] = sanitizePersisted({ tabs: [{ id, kind, title, contents, dirty }], activeTabId: id }).tabs;
    expect(t).toMatchObject({ profileId: null, database: null, valkeyDb: 0, path: null, objectRef: null });
  });

  it("drops duplicate ids and caps the number of tabs", () => {
    expect(sanitizePersisted({ tabs: [good(), good()], activeTabId: "a" }).tabs).toHaveLength(1);
    const many = Array.from({ length: MAX_PERSISTED_TABS + 20 }, (_, i) => ({ ...good(), id: `t${i}` }));
    expect(sanitizePersisted({ tabs: many, activeTabId: null }).tabs).toHaveLength(MAX_PERSISTED_TABS);
  });

  it("ignores extra fields instead of carrying them into the store", () => {
    const [t] = sanitizePersisted({ tabs: [{ ...good(), results: [1, 2], error: "boom", running: true }], activeTabId: "a" }).tabs;
    expect(t).not.toHaveProperty("results");
    expect(t).not.toHaveProperty("error");
    expect(t).not.toHaveProperty("running");
  });

  it("does not trust an active id that is not a string", () => {
    expect(sanitizePersisted({ tabs: [good()], activeTabId: { id: "a" } }).activeTabId).toBe("a");
  });
});

describe("unbindMissingProfiles", () => {
  it("unbinds only tabs whose profile is gone", () => {
    const tabs = [tab("a", { profileId: "p1" }), tab("b", { profileId: "gone", database: "x", valkeyDb: 4 }), tab("c", { profileId: null })];
    const out = unbindMissingProfiles(tabs, new Set(["p1"]));
    expect(out[0]).toBe(tabs[0]);
    expect(out[1]).toMatchObject({ profileId: null, database: null, valkeyDb: 0, contents: tabs[1].contents });
    expect(out[2]).toBe(tabs[2]);
  });

  it("returns the same array when nothing is missing", () => {
    const tabs = [tab("a")];
    expect(unbindMissingProfiles(tabs, new Set(["p1"]))).toBe(tabs);
  });
});

describe("debounced storage", () => {
  let data: Map<string, string>;
  let backing: Backing;
  let setItem: ReturnType<typeof vi.fn>;
  const value = (n: number) => ({ state: { tabs: [], activeTabId: null, n }, version: 1 });
  type V = ReturnType<typeof value>;
  const make = (o: Partial<Parameters<typeof createDebouncedStorage<V["state"]>>[1]> = {}) => createDebouncedStorage<V["state"]>(() => backing, { delayMs: 500, maxWaitMs: 3000, ...o });

  beforeEach(() => {
    vi.useFakeTimers();
    data = new Map();
    setItem = vi.fn((k: string, v: string) => void data.set(k, v));
    backing = { getItem: (k) => data.get(k) ?? null, setItem: setItem as unknown as Backing["setItem"], removeItem: (k) => void data.delete(k) };
  });
  afterEach(() => vi.useRealTimers());

  it("writes once after typing stops, not per change", () => {
    const s = make();
    for (let i = 0; i < 20; i++) {
      s.setItem("k", value(i));
      vi.advanceTimersByTime(100);
    }
    expect(setItem).not.toHaveBeenCalled();
    vi.advanceTimersByTime(500);
    expect(setItem).toHaveBeenCalledTimes(1);
    expect(JSON.parse(data.get("k")!).state.n).toBe(19);
  });

  it("forces a write during a long burst", () => {
    const s = make();
    for (let i = 0; i < 40; i++) {
      s.setItem("k", value(i));
      vi.advanceTimersByTime(200);
    }
    expect(setItem.mock.calls.length).toBeGreaterThanOrEqual(2);
  });

  it("reads see the pending value, and flush writes it", () => {
    const s = make();
    s.setItem("k", value(1));
    expect((s.getItem("k") as V).state.n).toBe(1);
    expect(data.has("k")).toBe(false);
    s.flush();
    expect(JSON.parse(data.get("k")!).state.n).toBe(1);
    vi.advanceTimersByTime(1000);
    expect(setItem).toHaveBeenCalledTimes(1);
  });

  it("does not rewrite an unchanged payload", () => {
    const s = make();
    s.setItem("k", value(1));
    s.flush();
    s.setItem("k", value(1));
    s.flush();
    expect(setItem).toHaveBeenCalledTimes(1);
  });

  it("reads unreadable or non-object storage as empty", () => {
    const s = make();
    for (const text of ["{not json", "42", "null", '"x"', "[]", '{"version":1}']) {
      data.set("k", text);
      expect(s.getItem("k")).toBeNull();
    }
    data.delete("k");
    expect(s.getItem("k")).toBeNull();
  });

  it("never throws when storage fails, and retries once with a smaller value", () => {
    const big = (n: number): PersistedWorkspace => ({
      activeTabId: "a",
      tabs: ["a", "b", "c"].map((id) => toPersisted([tab(id, { dirty: id === "c", contents: "y".repeat(n) })], null).tabs[0]),
    });
    let quota = 100;
    backing = {
      getItem: () => null,
      setItem: (k, v) => {
        if (v.length > quota) throw new Error("QuotaExceededError");
        data.set(k, v);
      },
      removeItem: () => undefined,
    };
    const s = createDebouncedStorage<PersistedWorkspace>(() => backing, { delayMs: 10, maxWaitMs: 100, shrink: shrinkWorkspace });
    quota = 3000;
    s.setItem(WORKSPACE_STORAGE_KEY, { state: big(1000), version: 1 });
    expect(() => s.flush()).not.toThrow();
    const kept = JSON.parse(data.get(WORKSPACE_STORAGE_KEY)!).state.tabs as Array<{ id: string; contents: string }>;
    // the active tab "a" and the dirty tab "c" keep their text; "b" is the one that went
    expect(kept.map((t) => t.contents.length > 0)).toEqual([true, false, true]);

    quota = 10;
    s.setItem(WORKSPACE_STORAGE_KEY, { state: big(1000), version: 1 });
    expect(() => s.flush()).not.toThrow();
  });

  it("survives a backing store that throws on every call", () => {
    backing = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
      removeItem: () => {
        throw new Error("denied");
      },
    };
    const s = make();
    expect(s.getItem("k")).toBeNull();
    s.setItem("k", value(1));
    expect(() => s.flush()).not.toThrow();
    expect(() => s.removeItem("k")).not.toThrow();
  });
});
