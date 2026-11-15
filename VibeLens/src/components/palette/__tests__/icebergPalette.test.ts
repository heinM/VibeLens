// Iceberg entries of the command palette: visibility, target resolution, the table picker.
import { QueryClient } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ConnectionProfile, IcebergNamespace, IcebergTableRef } from "../../../ipc/types";

const api = vi.hoisted(() => ({
  iceListNamespaces: vi.fn(async (_id: string, _parent: string[] | null) => [] as IcebergNamespace[]),
  iceListTables: vi.fn(async (_id: string, _ns: string[]) => [] as IcebergTableRef[]),
  wsRead: vi.fn(async () => ""),
}));
vi.mock("../../../ipc/commands", () => ({ api, asError: (e: unknown) => e }));

import { newProfile } from "../../../lib/defaults";
import { useUIStore } from "../../../store/useUIStore";
import { useWorkspaceStore } from "../../../store/useWorkspaceStore";
import { consumeAutoRun } from "../../editor/autoRun";
import { buildCommands, COMMAND_GROUP_ORDER, type CommandActions, type CommandEnv } from "../commands";
import { connectionItems, type GoActions } from "../items";
import { ICEBERG_TABLES_GROUP, defaultIcebergTableActions, icebergTableItems, listIcebergTables, openIcebergQueryTab, resolveIcebergTarget } from "../icebergPalette";
import { modeOf, usePaletteStore } from "../paletteStore";
import { rankItems } from "../rank";

const prof = (id: string, name: string, engine: "postgres" | "valkey" | "iceberg"): ConnectionProfile => ({ ...newProfile(engine), id, name });
const pg = prof("pg1", "Orders DB", "postgres");
const ice = prof("ice1", "Lakehouse", "iceberg");
const ice2 = prof("ice2", "Analytics lake", "iceberg");
const vk = prof("vk1", "Cache", "valkey");

const ns = (...levels: string[]): IcebergNamespace => ({ levels, properties: {} });
const tref = (namespace: string[], name: string, kind: "table" | "view" = "table"): IcebergTableRef => ({ namespace, name, kind });

beforeEach(() => {
  vi.resetAllMocks();
  api.iceListNamespaces.mockResolvedValue([]);
  api.iceListTables.mockResolvedValue([]);
  api.wsRead.mockResolvedValue("");
  useWorkspaceStore.setState({ tabs: [], activeTabId: null });
  usePaletteStore.setState({ open: false, query: "", scope: null, armed: false });
});

describe("resolveIcebergTarget", () => {
  const stateOf = (connected: string[]) => (id: string) => (connected.includes(id) ? ("connected" as const) : ("disconnected" as const));
  const all = [pg, ice, ice2, vk];

  it("is null without an Iceberg connection", () => {
    expect(resolveIcebergTarget({ profiles: [pg, vk], activeTabProfileId: pg.id, activeProfileId: pg.id, stateOf: stateOf([pg.id]) })).toBeNull();
    expect(resolveIcebergTarget({ profiles: [], activeTabProfileId: null, activeProfileId: null, stateOf: stateOf([]) })).toBeNull();
  });
  it("prefers the active tab's connection, then the active connection", () => {
    expect(resolveIcebergTarget({ profiles: all, activeTabProfileId: ice2.id, activeProfileId: ice.id, stateOf: stateOf([]) })?.id).toBe(ice2.id);
    expect(resolveIcebergTarget({ profiles: all, activeTabProfileId: pg.id, activeProfileId: ice.id, stateOf: stateOf([]) })?.id).toBe(ice.id);
    expect(resolveIcebergTarget({ profiles: all, activeTabProfileId: null, activeProfileId: null, stateOf: stateOf([ice2.id]) })?.id).toBe(ice2.id);
  });
  it("does not pick a disconnected Iceberg connection just because it exists", () => {
    expect(resolveIcebergTarget({ profiles: all, activeTabProfileId: pg.id, activeProfileId: pg.id, stateOf: stateOf([pg.id]) })).toBeNull();
  });
});

describe("palette commands", () => {
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
    newIcebergQuery: vi.fn(),
    pickIcebergTable: vi.fn(),
  });
  const env = (over: Partial<CommandEnv> = {}, a = actions()): CommandEnv => ({
    aiMode: "assist",
    theme: "dark",
    profiles: [pg, ice],
    stateOf: () => "connected",
    connectedPg: 1,
    hasValkeyProfile: false,
    historyCount: 0,
    iceberg: ice,
    erdTarget: null,
    mod: "Ctrl",
    actions: a,
    ...over,
  });
  const titles = (e: CommandEnv) => buildCommands(e).map((c) => c.title);

  it("shows both Iceberg commands when an Iceberg connection is active", () => {
    const t = titles(env());
    expect(t).toContain("Iceberg: new query on Lakehouse");
    expect(t).toContain("Iceberg: open table…");
  });
  it("hides them otherwise", () => {
    for (const e of [env({ iceberg: null }), env({ iceberg: undefined }), env({ profiles: [pg], iceberg: null })]) {
      expect(titles(e).filter((x) => x.startsWith("Iceberg"))).toEqual([]);
    }
  });
  it("hides them when the actions are not wired", () => {
    const a = actions();
    a.pickIcebergTable = undefined;
    expect(titles(env({}, a)).filter((x) => x.startsWith("Iceberg"))).toEqual([]);
  });
  it("names the connection and runs the right action with it", () => {
    const a = actions();
    const cmds = buildCommands(env({}, a));
    cmds.find((c) => c.title.startsWith("Iceberg: new query"))!.run();
    expect(a.newIcebergQuery).toHaveBeenCalledWith(ice);
    const open = cmds.find((c) => c.title === "Iceberg: open table…")!;
    expect(open.primary).toBe("Choose");
    expect(open.meta).toBe("Lakehouse");
    open.run();
    expect(a.pickIcebergTable).toHaveBeenCalledWith(ice);
  });
  it("lists them in their own group, found by 'iceberg'", () => {
    const cmds = buildCommands(env());
    const ice = cmds.filter((c) => c.title.startsWith("Iceberg"));
    expect(ice.every((c) => c.group === "Iceberg")).toBe(true);
    expect(COMMAND_GROUP_ORDER).toContain("Iceberg");
    const ranked = rankItems(cmds, "iceberg table", new Map(), 10);
    expect(ranked[0].item.title).toBe("Iceberg: open table…");
  });
  it("labels Iceberg connections in the connect / disconnect commands", () => {
    const c = buildCommands(env({ stateOf: () => "disconnected" })).find((x) => x.id === `cmd:connect:${ice.id}`)!;
    expect(c.meta).toBe("Iceberg");
  });
});

describe("connection items", () => {
  const a = (): GoActions => ({ openConnection: vi.fn(), newTabOnConnection: vi.fn(), openTable: vi.fn(), insertName: vi.fn(), openErd: vi.fn(), openFile: vi.fn(), openHistory: vi.fn(), copy: vi.fn() });
  it("does not mislabel an Iceberg connection as Valkey", () => {
    const actions = a();
    const [item] = connectionItems([ice], () => "connected", actions);
    expect(item.icon).toBe("iceberg");
    expect(item.search).toMatch(/iceberg/);
    expect(item.search).not.toMatch(/valkey|redis/);
    expect(item.crumbs?.[0]).not.toBe(":0");
    expect(item.alt?.label).toBe("New SQL tab");
    item.alt!.run();
    expect(actions.newTabOnConnection).toHaveBeenCalledWith(ice);
  });
  it("describes the catalog location", () => {
    const rest = { ...ice, iceberg: { ...ice.iceberg!, catalog: { kind: "rest" as const, uri: "https://polaris.example/api", warehouse: null, auth: { kind: "none" as const }, headers: {} } } };
    expect(connectionItems([rest], () => "disconnected", a())[0].crumbs).toEqual(["https://polaris.example/api"]);
    const glue = { ...ice, iceberg: { ...ice.iceberg!, catalog: { kind: "glue" as const, aws: { identity: { kind: "defaultChain" as const }, region: "eu-west-1" }, catalogId: null, warehouse: null } } };
    expect(connectionItems([glue], () => "disconnected", a())[0].crumbs).toEqual(["AWS Glue · eu-west-1"]);
  });
  it("leaves Postgres and Valkey rows as they were", () => {
    const [p, v] = connectionItems([pg, vk], () => "connected", a());
    expect(p.icon).toBe("pg");
    expect(v.icon).toBe("valkey");
  });
});

describe("listIcebergTables", () => {
  const tree = () => {
    api.iceListNamespaces.mockImplementation(async (_id, parent) => {
      const k = (parent ?? []).join(".");
      if (k === "") return [ns("shop"), ns("lake")];
      if (k === "lake") return [ns("lake", "sales"), ns("lake", "marketing")];
      return [];
    });
    api.iceListTables.mockImplementation(async (_id, n) => {
      const k = n.join(".");
      if (k === "shop") return [tref(["shop"], "orders"), tref(["shop"], "v_recent", "view")];
      if (k === "lake.sales") return [tref(["lake", "sales"], "daily_revenue")];
      return [];
    });
  };

  it("walks nested namespaces and collects every table and view", async () => {
    tree();
    const r = await listIcebergTables(new QueryClient(), "ice1");
    expect(r.truncated).toBe(false);
    expect(r.tables.map((t) => `${t.namespace.join(".")}.${t.name}`).sort()).toEqual(["lake.sales.daily_revenue", "shop.orders", "shop.v_recent"]);
    // top level asked with a null parent, children with the parent's levels
    expect(api.iceListNamespaces).toHaveBeenCalledWith("ice1", null);
    expect(api.iceListNamespaces).toHaveBeenCalledWith("ice1", ["lake"]);
    expect(api.iceListTables).toHaveBeenCalledTimes(4);
  });
  it("stops at the namespace limit and says so", async () => {
    api.iceListNamespaces.mockImplementation(async (_id, parent) => ((parent ?? []).length === 0 ? Array.from({ length: 50 }, (_, i) => ns(`ns${i}`)) : []));
    const r = await listIcebergTables(new QueryClient(), "ice1", { maxNamespaces: 10 });
    expect(r.truncated).toBe(true);
    expect(api.iceListTables).toHaveBeenCalledTimes(10);
  });
  it("shares the navigator's cache instead of listing twice", async () => {
    tree();
    const qc = new QueryClient();
    await listIcebergTables(qc, "ice1");
    await listIcebergTables(qc, "ice1");
    expect(api.iceListTables).toHaveBeenCalledTimes(4);
  });
  it("propagates a catalog error", async () => {
    api.iceListNamespaces.mockRejectedValueOnce({ code: "connection", message: "catalog unreachable" });
    await expect(listIcebergTables(new QueryClient(), "ice1")).rejects.toMatchObject({ message: "catalog unreachable" });
  });
  it("copes with an empty catalog", async () => {
    expect(await listIcebergTables(new QueryClient(), "ice1")).toEqual({ tables: [], truncated: false });
  });
});

describe("table picker items", () => {
  const refs = [tref(["shop"], "orders"), tref(["shop"], "v_recent", "view"), tref(["lake", "sales"], "Q4 forecast")];
  const openTable = vi.fn();
  const queryTable = vi.fn();
  const items = () => icebergTableItems(ice, refs, { openTable, queryTable });

  it("builds one row per table with namespace crumbs", () => {
    const it = items();
    expect(it.map((i) => i.title)).toEqual(["orders", "v_recent", "Q4 forecast"]);
    expect(it.every((i) => i.group === ICEBERG_TABLES_GROUP)).toBe(true);
    expect(it[0].crumbs).toEqual(["Lakehouse", "shop"]);
    expect(it[2].crumbs).toEqual(["Lakehouse", "lake.sales"]);
    expect(new Set(it.map((i) => i.id)).size).toBe(3);
    expect(it[1].icon).toBe("view");
    expect(it[0].primary).toBe("Open table");
    expect(it[1].primary).toBe("Open view");
  });
  it("finds a table by its dotted path", () => {
    const hit = rankItems(items(), "lake sales q4", new Map(), 10);
    expect(hit[0].item.title).toBe("Q4 forecast");
    expect(rankItems(items(), "shop.ord", new Map(), 10)[0].item.title).toBe("orders");
  });
  it("opens the explorer tab, and a query on alt", () => {
    const it = items();
    it[2].run();
    expect(openTable).toHaveBeenCalledWith(ice, ["lake", "sales"], "Q4 forecast");
    it[0].alt!.run();
    expect(queryTable).toHaveBeenCalledWith(ice, ["shop"], "orders");
  });
  it("tells keeps escaped namespace levels distinct in ids", () => {
    const a = icebergTableItems(ice, [tref(["a.b"], "t"), tref(["a", "b"], "t")], { openTable, queryTable });
    expect(new Set(a.map((i) => i.id)).size).toBe(2);
  });
});

describe("default actions", () => {
  it("open the table through openIcebergTableTab", () => {
    const open = vi.fn();
    defaultIcebergTableActions(open).openTable(ice, ["shop"], "orders");
    expect(open).toHaveBeenCalledWith("ice1", ["shop"], "orders");
    expect(useUIStore.getState().activeProfileId).toBe("ice1");
  });
  it("query the table in a new worksheet bound to the connection, and run it", () => {
    defaultIcebergTableActions(vi.fn()).queryTable(ice, ["lake", "sales"], "Q4 forecast");
    const tab = useWorkspaceStore.getState().tabs[0];
    expect(tab).toMatchObject({ kind: "sql", profileId: "ice1", database: null, title: "Q4 forecast.sql" });
    expect(tab.contents).toBe('SELECT * FROM lake.sales."Q4 forecast" LIMIT 100;');
    expect(consumeAutoRun(tab.id)).toBe(true);
  });
  it("new blank worksheets get unique names and keep the Iceberg binding", () => {
    const a = openIcebergQueryTab(ice);
    const b = openIcebergQueryTab(ice);
    const tabs = useWorkspaceStore.getState().tabs;
    expect(tabs.map((t) => t.title)).toEqual(["query-1.sql", "query-2.sql"]);
    expect(tabs.every((t) => t.profileId === "ice1" && t.database === null && t.kind === "sql")).toBe(true);
    expect(a).not.toBe(b);
    expect(consumeAutoRun(a)).toBe(false);
  });
});

describe("palette store scope", () => {
  it("opens straight into a picker and leaves it again", () => {
    usePaletteStore.getState().openScoped({ kind: "iceTables", profileId: "ice1" });
    expect(usePaletteStore.getState()).toMatchObject({ open: true, query: "", scope: { kind: "iceTables", profileId: "ice1" } });
    usePaletteStore.getState().leaveScope();
    expect(usePaletteStore.getState().scope).toBeNull();
    expect(usePaletteStore.getState().open).toBe(true);
  });
  it("a normal shortcut leaves the picker instead of toggling the palette shut", () => {
    usePaletteStore.getState().openScoped({ kind: "iceTables", profileId: "ice1" });
    usePaletteStore.getState().show("go");
    expect(usePaletteStore.getState()).toMatchObject({ open: true, scope: null, query: "" });
    usePaletteStore.getState().show("go"); // now it toggles as before
    expect(usePaletteStore.getState().open).toBe(false);
  });
  it("modes still come from the query", () => {
    expect(modeOf(">x")).toBe("commands");
    expect(modeOf("x")).toBe("go");
  });
});
