import { describe, expect, it } from "vitest";
import type { IcebergNamespace, IcebergTableInfo, IcebergTableRef } from "../../../ipc/types";
import type { QueryState } from "../pgTreeModel";
import { CATALOG_ID, buildIceRows, expandedNamespaces, nsId, partitionExpr, partitionSummary, shortType, tableId, tableKey, totalRecords } from "../icebergTreeModel";

const ns = (...levels: string[]): IcebergNamespace => ({ levels, properties: {} });
const ok = <T,>(data: T): QueryState<T> => ({ status: "success", data });
const pending = <T,>(): QueryState<T> => ({ status: "pending" });
const tbl = (namespace: string[], name: string, kind: "table" | "view" = "table"): IcebergTableRef => ({ namespace, name, kind });

const info = (over: Partial<IcebergTableInfo> = {}): IcebergTableInfo => ({
  namespace: ["shop"],
  name: "events",
  kind: "table",
  location: "",
  metadataLocation: null,
  formatVersion: 2,
  tableUuid: null,
  lastUpdatedMs: null,
  currentSnapshotId: "1",
  currentSchemaId: 0,
  fields: [
    { id: 1, name: "event_id", typeName: "long", required: true, doc: null, children: [] },
    { id: 5, name: "ctx", typeName: "struct<device: string>", required: false, doc: null, children: [{ id: 8, name: "device", typeName: "string", required: false, doc: null, children: [] }] },
  ],
  schemaCount: 1,
  partitionSpec: [{ sourceId: 1, fieldId: 1000, name: "event_bucket", transform: "bucket[8]" }],
  sortOrder: [],
  properties: {},
  snapshots: [{ snapshotId: "1", parentSnapshotId: null, sequenceNumber: null, timestampMs: 0, operation: "append", summary: { "total-records": "800" }, manifestList: null, schemaId: 0 }],
  refs: [],
  ...over,
});

const base = (over: Partial<Parameters<typeof buildIceRows>[0]> = {}) => ({
  catalogLabel: "REST · localhost:8181",
  topLevel: ok([ns("shop"), ns("lake")]),
  children: new Map<string, QueryState<IcebergNamespace[]>>(),
  tables: new Map<string, QueryState<IcebergTableRef[]>>(),
  describes: new Map<string, QueryState<IcebergTableInfo>>(),
  toggled: new Map<string, boolean>(),
  ...over,
});

describe("buildIceRows", () => {
  it("shows the catalog, expanded, with namespaces sorted by name", () => {
    const rows = buildIceRows(base());
    expect(rows.map((r) => [r.kind, r.label, r.depth])).toEqual([["catalog", "REST · localhost:8181", 0], ["namespace", "lake", 1], ["namespace", "shop", 1]]);
    expect(rows[1].expanded).toBe(false);
  });
  it("loading, error and empty states for the top level", () => {
    expect(buildIceRows(base({ topLevel: pending() }))[1]).toMatchObject({ kind: "status", statusKind: "loading" });
    expect(buildIceRows(base({ topLevel: { status: "error", error: "no route" } }))[1]).toMatchObject({ statusKind: "error", label: "no route" });
    expect(buildIceRows(base({ topLevel: ok([]) }))[1]).toMatchObject({ statusKind: "empty" });
    expect(buildIceRows(base({ toggled: new Map([[CATALOG_ID, false]]) }))).toHaveLength(1);
  });
  it("lists nested namespaces before tables and views, and badges come from describe", () => {
    const toggled = new Map([[nsId(["shop"]), true]]);
    const rows = buildIceRows(
      base({
        toggled,
        children: new Map([["shop", ok([ns("shop", "archive")])]]),
        tables: new Map([["shop", ok([tbl(["shop"], "orders"), tbl(["shop"], "events"), tbl(["shop"], "v_recent", "view")])]]),
        describes: new Map([[tableKey(["shop"], "events"), ok(info())]]),
      }),
    );
    const shop = rows.slice(rows.findIndex((r) => r.label === "shop"));
    expect(shop.map((r) => [r.kind, r.label, r.depth])).toEqual([
      ["namespace", "shop", 1],
      ["namespace", "archive", 2],
      ["table", "events", 2],
      ["table", "orders", 2],
      ["table", "v_recent", 2],
    ]);
    expect(shop.find((r) => r.label === "events")?.info?.partitionSpec).toHaveLength(1);
    expect(shop.find((r) => r.label === "v_recent")?.table?.kind).toBe("view");
    expect(shop.find((r) => r.label === "archive")?.expanded).toBe(false);
  });
  it("expands a table into its fields, with nested structs opening on demand", () => {
    const toggled = new Map<string, boolean>([[nsId(["shop"]), true], [tableId(["shop"], "events"), true]]);
    const input = base({ toggled, children: new Map([["shop", ok([])]]), tables: new Map([["shop", ok([tbl(["shop"], "events")])]]), describes: new Map([[tableKey(["shop"], "events"), ok(info())]]) });
    const rows = buildIceRows(input);
    expect(rows.filter((r) => r.kind === "field").map((r) => [r.label, r.depth, r.expandable, r.expanded])).toEqual([["event_id", 3, false, false], ["ctx", 3, true, false]]);
    const ctx = rows.find((r) => r.label === "ctx");
    const open = buildIceRows({ ...input, toggled: new Map(toggled).set(ctx?.id ?? "", true) }).filter((r) => r.kind === "field");
    expect(open.map((r) => [r.label, r.depth])).toEqual([["event_id", 3], ["ctx", 3], ["device", 4]]);
    expect(open[2].fieldPath).toBe("ctx.device");
  });
  it("shows loading and error rows under an expanded table", () => {
    const toggled = new Map<string, boolean>([[nsId(["shop"]), true], [tableId(["shop"], "events"), true]]);
    const mk = (d?: QueryState<IcebergTableInfo>) =>
      buildIceRows(base({ toggled, children: new Map([["shop", ok([])]]), tables: new Map([["shop", ok([tbl(["shop"], "events")])]]), describes: new Map(d ? [[tableKey(["shop"], "events"), d]] : []) }));
    expect(mk().at(-1)).toMatchObject({ statusKind: "loading" });
    expect(mk({ status: "error", error: "AccessDenied" }).at(-1)).toMatchObject({ statusKind: "error", label: "AccessDenied" });
  });
  it("empty namespace and per-namespace errors", () => {
    const toggled = new Map([[nsId(["shop"]), true]]);
    const empty = buildIceRows(base({ toggled, children: new Map([["shop", ok([])]]), tables: new Map([["shop", ok([])]]) }));
    expect(empty.at(-1)).toMatchObject({ statusKind: "empty", depth: 2 });
    const failing = buildIceRows(base({ toggled, children: new Map([["shop", ok([])]]), tables: new Map([["shop", { status: "error", error: "forbidden" } as QueryState<IcebergTableRef[]>]]) }));
    expect(failing.filter((r) => r.statusKind === "error")).toHaveLength(1);
    expect(failing.some((r) => r.statusKind === "empty")).toBe(false);
    const loading = buildIceRows(base({ toggled }));
    expect(loading.at(-1)).toMatchObject({ statusKind: "loading" });
  });
});

describe("expandedNamespaces", () => {
  it("returns expanded namespaces in tree order, descending only through expanded parents", () => {
    const top = [ns("a"), ns("b")];
    const kids = new Map<string, IcebergNamespace[] | undefined>([["a", [ns("a", "x"), ns("a", "y")]]]);
    const toggled = new Map<string, boolean>([[nsId(["a"]), true], [nsId(["a", "y"]), true], [nsId(["b"]), true], [nsId(["a", "x"]), false]]);
    expect(expandedNamespaces(top, kids, toggled)).toEqual([["a"], ["a", "y"], ["b"]]);
    expect(expandedNamespaces(top, kids, new Map([[CATALOG_ID, false], [nsId(["a"]), true]]))).toEqual([]);
  });
});

describe("table helpers", () => {
  it("formats partition expressions", () => {
    expect(partitionExpr("bucket[16]", "id")).toBe("bucket(16, id)");
    expect(partitionExpr("truncate[4]", "name")).toBe("truncate(4, name)");
    expect(partitionExpr("identity", "kind")).toBe("kind");
    expect(partitionExpr("month", "ts")).toBe("month(ts)");
    expect(partitionSummary(info())).toBe("bucket(8, event_id) → event_bucket");
    expect(partitionSummary(info({ partitionSpec: [] }))).toBe("");
  });
  it("reads the row count of the current snapshot, if it has one", () => {
    expect(totalRecords(info())).toBe(800);
    expect(totalRecords(info({ currentSnapshotId: null }))).toBeNull();
  });
  it("collapses struct type text", () => {
    expect(shortType(info().fields[1])).toBe("struct");
    expect(shortType(info().fields[0])).toBe("long");
  });
});
