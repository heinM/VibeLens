import { describe, expect, it, vi } from "vitest";
import { buildCommands, type CommandActions, type CommandEnv } from "../commands";
import { catalogItems, connectionItems, fileItems, functionItems, groupRanked, historyItems, identForSql, insertionText, sqlPreview, timeAgo, compactRows, type GoActions } from "../items";
import { resolveErdTarget, schemasByTableCount } from "../erdTarget";
import { rankItems } from "../rank";
import { modeOf } from "../paletteStore";
import type { ConnectionProfile, SchemaMetadata } from "../../../ipc/types";
import { newProfile } from "../../../lib/defaults";
import { shopCatalog, table } from "../../erd/fixtures";

const prof = (id: string, name: string, engine: "postgres" | "valkey" = "postgres"): ConnectionProfile => ({ ...newProfile(engine), id, name, database: engine === "postgres" ? "shop_db" : null });
const goActions = (): GoActions => ({
  openConnection: vi.fn(),
  newTabOnConnection: vi.fn(),
  openTable: vi.fn(),
  insertName: vi.fn(),
  openErd: vi.fn(),
  openFile: vi.fn(),
  openHistory: vi.fn(),
  copy: vi.fn(),
});
const meta = (): SchemaMetadata => ({ database: "shop_db", serverVersion: "16", truncated: false, tables: [...shopCatalog(), table("v_totals", [], [], { kind: "view" })] });

describe("catalog items", () => {
  it("builds tables, views and schemas with context and actions", () => {
    const a = goActions();
    const p = prof("p1", "Demo Postgres");
    const items = catalogItems(p, "shop_db", meta(), a);
    expect(items.filter((i) => i.group === "Tables")).toHaveLength(5);
    expect(items.filter((i) => i.group === "Views")).toHaveLength(1);
    const schemas = items.filter((i) => i.group === "Schemas");
    expect(schemas).toHaveLength(1);
    expect(schemas[0].meta).toBe("6 tables");
    const orders = items.find((i) => i.title === "orders")!;
    expect(orders.crumbs).toEqual(["Demo Postgres", "shop"]);
    expect(orders.meta).toBe("~100 rows");
    orders.run();
    expect(a.openTable).toHaveBeenCalledWith(p, "shop_db", "shop", "orders");
    orders.alt!.run();
    expect(a.insertName).toHaveBeenCalledWith(p, "shop_db", "shop", "orders");
    schemas[0].run();
    expect(a.openErd).toHaveBeenCalledWith(p, "shop_db", "shop");
    expect(new Set(items.map((i) => i.id)).size).toBe(items.length);
  });

  it("functions carry their signature", () => {
    const a = goActions();
    const fns = functionItems(prof("p1", "X"), null, [
      { schema: "shop", name: "order_total", kind: "function", comment: null, estimatedRows: null, signature: "(order_id bigint)", isPartition: false, parentSchema: null, parentName: null },
      { schema: "shop", name: "orders", kind: "table", comment: null, estimatedRows: 1, signature: null, isPartition: false, parentSchema: null, parentName: null },
    ], a);
    expect(fns).toHaveLength(1);
    expect(fns[0].subtitle).toBe("(order_id bigint)");
    fns[0].run();
    expect(a.insertName).toHaveBeenCalled();
  });

  it("connections show state and offer a new tab as the secondary action", () => {
    const a = goActions();
    const items = connectionItems([prof("p1", "A"), prof("p2", "B", "valkey")], (id) => (id === "p1" ? "connected" : "disconnected"), a);
    expect(items[0].primary).toBe("Focus");
    expect(items[1].primary).toBe("Connect");
    expect(items[1].alt!.label).toBe("New console");
  });

  it("files and history items", () => {
    const a = goActions();
    const files = fileItems([{ path: "reports/monthly.sql", size: 1, modified: null }, { path: "cache.valkey", size: 1, modified: null }], a);
    expect(files[0].title).toBe("monthly.sql");
    expect(files[0].crumbs).toEqual(["reports"]);
    expect(files[1].icon).toBe("fileValkey");
    const h = historyItems([{ id: "1", sql: "-- note\nselect *\n  from orders", profileId: "p1", profileName: "A", database: null, at: 1000, ok: false, elapsedMs: 3, rowCount: null, error: "boom" }], a);
    expect(h[0].title).toBe("select * from orders");
    expect(h[0].icon).toBe("historyFail");
    expect(h[0].ts).toBe(1000);
  });
});

describe("helpers", () => {
  it("sqlPreview, timeAgo, compactRows", () => {
    expect(sqlPreview("\n\n  SELECT   1  ")).toBe("SELECT 1");
    expect(sqlPreview("x".repeat(300), 20)).toHaveLength(20);
    expect(timeAgo(10_000, 9_000)).toBe("just now");
    expect(timeAgo(3 * 3600_000, 0)).toBe("3 h ago");
    expect(compactRows(1250000)).toBe("~1.3M rows");
    expect(compactRows(null)).toBe("");
  });

  it("insertionText quotes only when needed", () => {
    expect(insertionText("shop", "orders")).toBe("shop.orders");
    expect(insertionText("Shop", "Order Items")).toBe('"Shop"."Order Items"');
    expect(insertionText("public", "user")).toBe('public."user"');
    expect(insertionText("shop", "")).toBe("shop");
    expect(identForSql('a"b')).toBe('"a""b"');
  });

  it("modeOf follows the leading >", () => {
    expect(modeOf(">new")).toBe("commands");
    expect(modeOf("orders >")).toBe("go");
  });
});

describe("groupRanked", () => {
  const mk = (id: string, title: string, group: string) => ({ id, title, search: title, group });
  const items = [mk("1", "orders", "Tables"), mk("2", "order_total", "Functions"), mk("3", "customers", "Tables"), mk("4", "ord", "Views")];

  it("orders groups by best score when searching", () => {
    const rows = groupRanked(rankItems(items, "ord"));
    expect(rows[0]).toMatchObject({ type: "header" });
    const firstItem = rows.find((r) => r.type === "item")!;
    expect(firstItem.entry!.item.title).toBe("ord"); // exact match wins, so Views leads
    const idx = rows.filter((r) => r.type === "item").map((r) => r.index);
    expect(idx).toEqual([...idx.keys()]);
  });

  it("uses a fixed order and caps when browsing", () => {
    const rows = groupRanked(rankItems(items, ""), { order: ["Tables", "Views", "Functions"], caps: { Tables: 1 } });
    expect(rows.filter((r) => r.type === "header").map((r) => `${r.label}:${r.count}`)).toEqual(["Tables:2", "Views:1", "Functions:1"]);
    expect(rows.filter((r) => r.type === "item")).toHaveLength(3);
  });
});

describe("buildCommands", () => {
  const actions = (): CommandActions => ({
    newSqlTab: vi.fn(),
    newValkeyConsole: vi.fn(),
    newConnection: vi.fn(),
    openSettings: vi.fn(),
    setAiMode: vi.fn(),
    toggleTheme: vi.fn(),
    toggleSidebar: vi.fn(),
    toggleAgent: vi.fn(),
    connect: vi.fn(),
    disconnect: vi.fn(),
    refreshSchema: vi.fn(),
    refreshFiles: vi.fn(),
    openErd: vi.fn(),
    clearHistory: vi.fn(),
  });
  const env = (over: Partial<CommandEnv> = {}): CommandEnv => ({
    aiMode: "assist",
    theme: "dark",
    profiles: [prof("p1", "Demo Postgres"), prof("p2", "Demo Valkey", "valkey")],
    stateOf: (id) => (id === "p1" ? "connected" : "disconnected"),
    connectedPg: 1,
    hasValkeyProfile: true,
    historyCount: 3,
    erdTarget: { profileId: "p1", database: null, schema: "shop" },
    mod: "⌘",
    actions: actions(),
    ...over,
  });
  const titles = (e: CommandEnv) => buildCommands(e).map((c) => c.title);

  it("offers the full command set", () => {
    const t = titles(env());
    for (const want of [
      "New SQL tab",
      "New connection…",
      "Open settings",
      "Switch to light theme",
      "Toggle navigator",
      "Toggle agent panel",
      "Open ER diagram for shop",
      "Refresh schema cache",
      "Clear query history…",
      "Disconnect Demo Postgres",
      "Connect Demo Valkey",
      "Interface mode: Classic",
      "Interface mode: Assist",
      "Interface mode: Agent-first",
    ])
      expect(t).toContain(want);
  });

  it("offers workspace file import only when the action is wired, and runs it", () => {
    expect(titles(env())).not.toContain("Import files into workspace");
    const importWorkspaceFiles = vi.fn();
    const e = env({ actions: { ...actions(), importWorkspaceFiles } });
    expect(titles(e)).toContain("Import files into workspace");
    buildCommands(e).find((c) => c.id === "cmd:import-workspace-files")!.run();
    expect(importWorkspaceFiles).toHaveBeenCalledTimes(1);
  });

  it("hides the agent panel toggle in classic mode", () => {
    expect(titles(env({ aiMode: "classic" }))).not.toContain("Toggle agent panel");
    expect(titles(env({ aiMode: null }))).not.toContain("Toggle agent panel");
  });

  it("omits ER and refresh when there is nothing to act on", () => {
    const t = titles(env({ erdTarget: null, connectedPg: 0 }));
    expect(t.some((x) => x.startsWith("Open ER diagram"))).toBe(false);
    expect(t).not.toContain("Refresh schema cache");
  });

  it("marks the current mode and wires actions", () => {
    const e = env();
    const items = buildCommands(e);
    expect(items.find((c) => c.id === "cmd:mode:assist")!.meta).toBe("current");
    items.find((c) => c.id === "cmd:mode:classic")!.run();
    expect(e.actions.setAiMode).toHaveBeenCalledWith("classic");
    items.find((c) => c.id === "cmd:disconnect:p1")!.run();
    expect(e.actions.disconnect).toHaveBeenCalledWith("p1");
    items.find((c) => c.title.startsWith("Open ER"))!.run();
    expect(e.actions.openErd).toHaveBeenCalledWith({ profileId: "p1", database: null, schema: "shop" });
  });

  it("finds commands by fuzzy query", () => {
    const r = rankItems(buildCommands(env()), "tgl thm");
    expect(r[0]?.item.id).toBe("cmd:theme");
    expect(rankItems(buildCommands(env()), "er diag")[0].item.title).toContain("ER diagram");
  });
});

describe("resolveErdTarget", () => {
  const profiles = [prof("p1", "A"), prof("p2", "V", "valkey")];
  const base = { activeProfileId: null, activeDatabase: null, profiles };

  it("uses the table tab's own schema", () => {
    expect(resolveErdTarget({ ...base, activeTab: { kind: "table", profileId: "p1", database: "d", objectRef: { schema: "shop", name: "orders" } } })).toEqual({ profileId: "p1", database: "d", schema: "shop" });
  });

  it("falls back to the sidebar selection and the catalog's biggest schema", () => {
    const sel = { ...base, activeTab: null, activeProfileId: "p1", activeDatabase: "x" };
    expect(resolveErdTarget(sel)).toEqual({ profileId: "p1", database: "x", schema: "public" });
    expect(resolveErdTarget({ ...sel, schemasOf: () => ["shop", "audit"] })?.schema).toBe("shop");
  });

  it("schemasByTableCount ranks by size and lets public win ties", () => {
    const t = (schema: string, n: number) => Array.from({ length: n }, () => ({ schema }));
    expect(schemasByTableCount([...t("public", 1), ...t("shop", 20)])).toEqual(["shop", "public"]);
    expect(schemasByTableCount([...t("a", 2), ...t("public", 2)])).toEqual(["public", "a"]);
    expect(schemasByTableCount(undefined)).toEqual([]);
  });

  it("is null for Valkey or no selection", () => {
    expect(resolveErdTarget({ ...base, activeTab: null, activeProfileId: "p2" })).toBeNull();
    expect(resolveErdTarget({ ...base, activeTab: { kind: "valkey", profileId: "p2", database: null, objectRef: null } })).toBeNull();
    expect(resolveErdTarget({ ...base, activeTab: null })).toBeNull();
  });

  it("a SQL tab supplies its connection but not a schema", () => {
    expect(resolveErdTarget({ ...base, activeTab: { kind: "sql", profileId: "p1", database: null, objectRef: null }, schemasOf: () => ["shop"] })).toEqual({ profileId: "p1", database: null, schema: "shop" });
  });
});
