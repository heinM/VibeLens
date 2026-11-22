// The Redshift navigator model: schemas > tables and views > columns, lazy states, system schemas,
// column type labels and the query a table opens.
import { describe, expect, it } from "vitest";
import type { RedshiftColumn, RedshiftTableRef } from "../../../ipc/types";
import type { QueryState } from "../pgTreeModel";
import { buildRsRows, columnType, expandedRsSchemas, isSystemSchema, isViewKind, rsRefreshKeys, rsSchemaId, rsSelectAllSql, rsTableId, rsTableKey, type RsModelInput } from "../redshiftTreeModel";
import { QueryClient } from "@tanstack/react-query";
import { rsQueries } from "../../../hooks/useRedshift";

const ok = <T,>(data: T): QueryState<T> => ({ status: "success", data });
const c = (name: string, typeName: string, o: Partial<RedshiftColumn> = {}): RedshiftColumn => ({ name, typeName, nullable: true, length: null, precision: null, scale: null, defaultValue: null, ...o });
const t = (schema: string, name: string, kind = "TABLE"): RedshiftTableRef => ({ schema, name, kind });

const input = (over: Partial<RsModelInput> = {}): RsModelInput => ({
  schemas: ok(["sales", "public", "pg_catalog", "information_schema", "pg_automv"]),
  tables: new Map(),
  describes: new Map(),
  toggled: new Map(),
  showSystem: false,
  ...over,
});

describe("buildRsRows", () => {
  it("shows loading and error states for the schema list", () => {
    expect(buildRsRows(input({ schemas: { status: "pending" } }))[0]).toMatchObject({ kind: "status", statusKind: "loading" });
    expect(buildRsRows(input({ schemas: { status: "error", error: "AccessDenied" } }))[0]).toMatchObject({ kind: "status", statusKind: "error", label: "AccessDenied" });
  });
  it("lists user schemas sorted, hiding system schemas unless asked", () => {
    expect(buildRsRows(input()).map((r) => r.label)).toEqual(["public", "sales"]);
    expect(buildRsRows(input({ showSystem: true })).map((r) => r.label)).toEqual(["information_schema", "pg_automv", "pg_catalog", "public", "sales"]);
    expect(isSystemSchema("PG_INTERNAL")).toBe(true);
    expect(isSystemSchema("pgsales")).toBe(false);
  });
  it("expands a schema into its tables and views, with a loading row until they arrive", () => {
    const toggled = new Map([[rsSchemaId("public"), true]]);
    expect(buildRsRows(input({ toggled })).map((r) => [r.depth, r.kind, r.label])).toEqual([
      [0, "schema", "public"],
      [1, "status", "Loading…"],
      [0, "schema", "sales"],
    ]);
    const rows = buildRsRows(input({ toggled, tables: new Map([["public", ok([t("public", "orders"), t("public", "v_daily", "VIEW")])]]) }));
    expect(rows.map((r) => [r.depth, r.kind, r.label])).toEqual([
      [0, "schema", "public"],
      [1, "table", "orders"],
      [1, "table", "v_daily"],
      [0, "schema", "sales"],
    ]);
    expect(rows[1]).toMatchObject({ schema: "public", expandable: true, expanded: false });
  });
  it("says so when a schema is empty or its tables fail", () => {
    const toggled = new Map([[rsSchemaId("public"), true]]);
    expect(buildRsRows(input({ toggled, tables: new Map([["public", ok([])]]) }))[1]).toMatchObject({ statusKind: "empty" });
    expect(buildRsRows(input({ toggled, tables: new Map([["public", { status: "error", error: "boom" } as QueryState<RedshiftTableRef[]>]]) }))[1]).toMatchObject({ statusKind: "error", label: "boom" });
  });
  it("expands a table into its columns", () => {
    const toggled = new Map([
      [rsSchemaId("public"), true],
      [rsTableId("public", "orders"), true],
    ]);
    const rows = buildRsRows(
      input({
        toggled,
        tables: new Map([["public", ok([t("public", "orders")])]]),
        describes: new Map([[rsTableKey("public", "orders"), ok([c("id", "int8", { nullable: false }), c("note", "varchar", { length: 256 })])]]),
      }),
    );
    expect(rows.filter((r) => r.kind === "column").map((r) => [r.depth, r.label, r.column && columnType(r.column)])).toEqual([
      [2, "id", "int8"],
      [2, "note", "varchar(256)"],
    ]);
  });
  it("lists the expanded schemas whose tables to fetch", () => {
    expect(expandedRsSchemas(["a", "b"], new Map([[rsSchemaId("b"), true]]))).toEqual(["b"]);
    expect(expandedRsSchemas(undefined, new Map())).toEqual([]);
  });
});

describe("helpers", () => {
  it("labels column types with length, precision and scale", () => {
    expect(columnType(c("a", "numeric", { precision: 18, scale: 2 }))).toBe("numeric(18,2)");
    expect(columnType(c("a", "character varying", { length: 64 }))).toBe("character varying(64)");
    expect(columnType(c("a", "varchar(10)", { length: 10 }))).toBe("varchar(10)");
    expect(columnType(c("a", "timestamp"))).toBe("timestamp");
  });
  it("recognises views", () => {
    expect(isViewKind("VIEW")).toBe(true);
    expect(isViewKind("MATERIALIZED VIEW")).toBe(true);
    expect(isViewKind("TABLE")).toBe(false);
    expect(isViewKind("EXTERNAL TABLE")).toBe(false);
  });
  it("opens a table as SELECT * FROM \"schema\".\"table\" LIMIT 100", () => {
    expect(rsSelectAllSql("public", "orders")).toBe('SELECT * FROM "public"."orders" LIMIT 100;');
    expect(rsSelectAllSql("Sales", 'we"ird')).toBe('SELECT * FROM "Sales"."we""ird" LIMIT 100;');
  });
});

describe("Refresh (rsRefreshKeys)", () => {
  const stale = (qc: QueryClient, key: readonly unknown[]) => qc.getQueryState(key)?.isInvalidated === true;
  const seed = () => {
    const qc = new QueryClient();
    for (const o of [rsQueries.tables("p1", null, "public"), rsQueries.describe("p1", null, "public", "orders"), rsQueries.describe("p1", null, "public", "items"), rsQueries.describe("p1", null, "sales", "orders"), rsQueries.tables("p1", null, "sales")]) qc.setQueryData(o.queryKey, []);
    return qc;
  };
  const run = (qc: QueryClient, keys: (readonly unknown[])[]) => keys.forEach((queryKey) => void qc.invalidateQueries({ queryKey }));

  it("a schema refresh also refreshes the columns of every table under it, and nothing in other schemas", () => {
    const qc = seed();
    run(qc, rsRefreshKeys("p1", null, { kind: "schema", schema: "public" }));
    expect(stale(qc, rsQueries.tables("p1", null, "public").queryKey)).toBe(true);
    expect(stale(qc, rsQueries.describe("p1", null, "public", "orders").queryKey)).toBe(true);
    expect(stale(qc, rsQueries.describe("p1", null, "public", "items").queryKey)).toBe(true);
    expect(stale(qc, rsQueries.describe("p1", null, "sales", "orders").queryKey)).toBe(false);
    expect(stale(qc, rsQueries.tables("p1", null, "sales").queryKey)).toBe(false);
  });
  it("a table refresh refreshes that table's columns only; anything else the whole connection", () => {
    const qc = seed();
    run(qc, rsRefreshKeys("p1", null, { kind: "table", schema: "public", table: { schema: "public", name: "orders", kind: "TABLE" } }));
    expect(stale(qc, rsQueries.describe("p1", null, "public", "orders").queryKey)).toBe(true);
    expect(stale(qc, rsQueries.describe("p1", null, "public", "items").queryKey)).toBe(false);
    expect(rsRefreshKeys("p1", null)).toEqual([["rs", "p1"]]);
  });
});
