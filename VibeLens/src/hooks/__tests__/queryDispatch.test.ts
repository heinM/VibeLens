// Engine dispatch of the SQL runner: Postgres -> pgExecute, Iceberg -> iceExecute, Valkey -> refused;
// cancel always goes through pgCancel; DDL invalidates the right catalog.
import { QueryClient } from "@tanstack/react-query";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { QueryEvent } from "../../ipc/types";

const api = vi.hoisted(() => ({
  pgExecute: vi.fn(async (_a: unknown) => "pg-1"),
  iceExecute: vi.fn(async (_a: unknown) => "ice-1"),
  rsExecute: vi.fn(async (_a: unknown) => "rs-1"),
  myExecute: vi.fn(async (_a: unknown) => "my-1"),
  pgCancel: vi.fn(async (_id: string) => undefined),
}));
vi.mock("../../ipc/commands", () => ({ api, asError: (e: unknown) => e }));

import { DDL_TAG, cancelQuery, executeOnEngine, invalidateAfterDdl, runsSql, statsFromEvent } from "../queryDispatch";
import { record, type HistoryCtx } from "../useQueryRunner";
import { useHistoryStore } from "../../store/useHistoryStore";
import { schemaMetadataKey } from "../../lib/schemaCache";

const onEvent = vi.fn();
const base = { profileId: "p1", database: "orders", sql: "select 1", onEvent };

beforeEach(() => {
  vi.clearAllMocks();
  useHistoryStore.setState({ entries: [] });
});

describe("executeOnEngine", () => {
  it("runs Postgres through pgExecute with the database, and is the default", async () => {
    expect(await executeOnEngine("postgres", { ...base, options: { readOnly: true }, confirmed: true })).toBe("pg-1");
    expect(await executeOnEngine(undefined, base)).toBe("pg-1");
    expect(await executeOnEngine(null, base)).toBe("pg-1");
    expect(api.iceExecute).not.toHaveBeenCalled();
    expect(api.pgExecute).toHaveBeenCalledTimes(3);
    expect(api.pgExecute).toHaveBeenNthCalledWith(1, { profileId: "p1", database: "orders", sql: "select 1", options: { readOnly: true }, confirmed: true, onEvent });
  });

  it("runs Iceberg through iceExecute and never sends a database", async () => {
    expect(await executeOnEngine("iceberg", { ...base, options: { readOnly: true, maxRows: 10 }, confirmed: true })).toBe("ice-1");
    expect(api.pgExecute).not.toHaveBeenCalled();
    const arg = api.iceExecute.mock.calls[0][0] as Record<string, unknown>;
    expect(arg).toEqual({ profileId: "p1", sql: "select 1", options: { readOnly: true, maxRows: 10 }, confirmed: true, onEvent });
    expect("database" in arg).toBe(false);
  });

  it("hands the same event callback to both so the stream is handled identically", async () => {
    await executeOnEngine("iceberg", base);
    await executeOnEngine("postgres", base);
    expect((api.iceExecute.mock.calls[0][0] as { onEvent: unknown }).onEvent).toBe(onEvent);
    expect((api.pgExecute.mock.calls[0][0] as { onEvent: unknown }).onEvent).toBe(onEvent);
  });

  it("passes the prod write-guard rejection through for the confirm flow", async () => {
    api.iceExecute.mockRejectedValueOnce({ code: "confirm_required", message: "writes to prod" });
    await expect(executeOnEngine("iceberg", base)).rejects.toEqual({ code: "confirm_required", message: "writes to prod" });
  });

  it("refuses to run SQL on Valkey without calling any executor", async () => {
    await expect(executeOnEngine("valkey", base)).rejects.toMatchObject({ code: "unsupported" });
    expect(api.pgExecute).not.toHaveBeenCalled();
    expect(api.iceExecute).not.toHaveBeenCalled();
    expect(runsSql("valkey")).toBe(false);
    expect(runsSql("iceberg")).toBe(true);
    expect(runsSql("postgres")).toBe(true);
    expect(runsSql(undefined)).toBe(true);
  });
});

describe("executeOnEngine (redshift)", () => {
  it("runs Redshift through rsExecute with the tab's database, never through pgExecute", async () => {
    expect(await executeOnEngine("redshift", { ...base, options: { readOnly: true }, confirmed: true })).toBe("rs-1");
    expect(api.pgExecute).not.toHaveBeenCalled();
    expect(api.iceExecute).not.toHaveBeenCalled();
    expect(api.rsExecute).toHaveBeenCalledWith({ profileId: "p1", database: "orders", sql: "select 1", options: { readOnly: true }, confirmed: true, onEvent });
  });

  it("passes a null database through (the connection's own database)", async () => {
    await executeOnEngine("redshift", { ...base, database: null });
    expect((api.rsExecute.mock.calls[0][0] as { database: unknown }).database).toBeNull();
  });

  it("keeps the confirm_required rejection for the production confirm flow", async () => {
    api.rsExecute.mockRejectedValueOnce({ code: "confirm_required", message: "writes to prod" });
    await expect(executeOnEngine("redshift", base)).rejects.toEqual({ code: "confirm_required", message: "writes to prod" });
  });

  it("is a SQL engine, and an unknown engine is refused rather than run as Postgres", async () => {
    expect(runsSql("redshift")).toBe(true);
    await expect(executeOnEngine("mystery" as never, base)).rejects.toMatchObject({ code: "unsupported" });
    expect(api.pgExecute).not.toHaveBeenCalled();
  });

  it("is cancelled with pgCancel like the other engines", async () => {
    await cancelQuery("rs-1");
    expect(api.pgCancel).toHaveBeenCalledWith("rs-1");
  });
});

describe("cancel", () => {
  it("uses pgCancel for Iceberg queries too (shared running-queries map)", async () => {
    await cancelQuery("ice-1");
    expect(api.pgCancel).toHaveBeenCalledWith("ice-1");
  });
});

describe("stats events", () => {
  it("keeps all four fields", () => {
    const ev: Extract<QueryEvent, { type: "stats" }> = { type: "stats", statementIndex: 2, engine: "athena", bytesScanned: 123, costUsdEstimate: 0.5, engineQueryId: "q-abc" };
    expect(statsFromEvent(ev)).toEqual({ engine: "athena", bytesScanned: 123, costUsdEstimate: 0.5, engineQueryId: "q-abc" });
    expect(statsFromEvent({ ...ev, bytesScanned: null, costUsdEstimate: null, engineQueryId: null })).toEqual({ engine: "athena", bytesScanned: null, costUsdEstimate: null, engineQueryId: null });
  });
});

describe("DDL invalidation", () => {
  const seed = (qc: QueryClient) => {
    qc.setQueryData(schemaMetadataKey("p1", "orders"), { n: 1 });
    qc.setQueryData(schemaMetadataKey("p1", null, "iceberg"), { n: 2 });
    qc.setQueryData(["ice", "p1", "tables", "shop"], []);
    qc.setQueryData(["ice", "p2", "tables", "shop"], []);
  };
  const stale = (qc: QueryClient, key: readonly unknown[]) => qc.getQueryState(key)?.isInvalidated === true;

  it("matches the DDL command tags", () => {
    for (const t of ["CREATE TABLE", "ALTER TABLE", "DROP TABLE", "COMMENT", "TRUNCATE", "REFRESH MATERIALIZED VIEW"]) expect(DDL_TAG.test(t), t).toBe(true);
    for (const t of ["SELECT 1", "INSERT 0 1", "UPDATE 3", "DELETE 1"]) expect(DDL_TAG.test(t), t).toBe(false);
  });

  it("refreshes only the Iceberg caches of that connection after Iceberg DDL", () => {
    const qc = new QueryClient();
    seed(qc);
    invalidateAfterDdl(qc, "iceberg", "p1");
    expect(stale(qc, schemaMetadataKey("p1", null, "iceberg"))).toBe(true);
    expect(stale(qc, ["ice", "p1", "tables", "shop"])).toBe(true);
    expect(stale(qc, ["ice", "p2", "tables", "shop"])).toBe(false);
    expect(stale(qc, schemaMetadataKey("p1", "orders"))).toBe(false);
  });

  it("keeps the Redshift catalog apart from Postgres and refreshes only it after Redshift DDL", () => {
    const qc = new QueryClient();
    seed(qc);
    qc.setQueryData(schemaMetadataKey("p1", "dev", "redshift"), { n: 3 });
    qc.setQueryData(["rs", "p1", null, "tables", "public"], []);
    expect(schemaMetadataKey("p1", "dev", "redshift")[0]).toBe("rs");
    invalidateAfterDdl(qc, "redshift", "p1");
    expect(stale(qc, schemaMetadataKey("p1", "dev", "redshift"))).toBe(true);
    expect(stale(qc, ["rs", "p1", null, "tables", "public"])).toBe(true);
    expect(stale(qc, schemaMetadataKey("p1", "orders"))).toBe(false);
    expect(stale(qc, schemaMetadataKey("p1", null, "iceberg"))).toBe(false);
  });

  it("refreshes only the Postgres catalog after Postgres DDL", () => {
    const qc = new QueryClient();
    seed(qc);
    invalidateAfterDdl(qc, "postgres", "p1");
    expect(stale(qc, schemaMetadataKey("p1", "orders"))).toBe(true);
    expect(stale(qc, schemaMetadataKey("p1", null, "iceberg"))).toBe(false);
    expect(stale(qc, ["ice", "p1", "tables", "shop"])).toBe(false);
  });
});

describe("history is tagged with the engine", () => {
  const ctx = (over: Partial<HistoryCtx> = {}): HistoryCtx => ({ profileId: "p1", profileName: "Lake", environment: "dev", database: null, sql: "select 1", queue: null, texts: null, count: 1, stmtStart: 0, recorded: false, ...over });

  it("records the engine of an Iceberg run", () => {
    record(ctx({ engine: "iceberg" }), "select * from shop.orders", true, 12, 5, null);
    expect(useHistoryStore.getState().entries[0]).toMatchObject({ engine: "iceberg", database: null, profileName: "Lake", ok: true });
  });

  it("keeps entries without an engine as they were (Postgres callers that predate the tag)", () => {
    record(ctx(), "select 1", true, 1, 1, null);
    expect("engine" in useHistoryStore.getState().entries[0]).toBe(false);
  });

  it("still redacts, strips errors and skips prod for Iceberg", () => {
    record(ctx({ engine: "iceberg" }), "select 1", false, 1, null, "failed\nDETAIL: Key (v)=(secret)");
    expect(useHistoryStore.getState().entries[0].error).toBe("failed");
    useHistoryStore.setState({ entries: [] });
    record(ctx({ engine: "iceberg", environment: "prod" }), "select 1", true, 1, 1, null);
    expect(useHistoryStore.getState().entries).toHaveLength(0);
  });
});

describe("MySQL dispatch", () => {
  it("runs MySQL through myExecute with the tab's database, never through pgExecute", async () => {
    expect(await executeOnEngine("mysql", { ...base, options: { readOnly: true }, confirmed: true })).toBe("my-1");
    expect(api.pgExecute).not.toHaveBeenCalled();
    expect(api.myExecute).toHaveBeenCalledWith({ profileId: "p1", database: "orders", sql: "select 1", options: { readOnly: true }, confirmed: true, onEvent });
    expect(runsSql("mysql")).toBe(true);
  });
  it("passes a null database through so Rust uses the profile's default", async () => {
    await executeOnEngine("mysql", { ...base, database: null });
    expect((api.myExecute.mock.calls[0][0] as { database: unknown }).database).toBeNull();
  });
  it("cancels through pgCancel like every engine", async () => {
    await cancelQuery("my-1");
    expect(api.pgCancel).toHaveBeenCalledWith("my-1");
  });
  it("after DDL refreshes only the MySQL catalog of that connection", () => {
    const qc = new QueryClient();
    qc.setQueryData(schemaMetadataKey("p1", "shop", "mysql"), { database: "shop", serverVersion: null, tables: [], truncated: false });
    qc.setQueryData(["my", "p1", "shop", "objects"], []);
    qc.setQueryData(schemaMetadataKey("p2", "shop", "mysql"), { database: "shop", serverVersion: null, tables: [], truncated: false });
    qc.setQueryData(schemaMetadataKey("p1", "shop"), { database: "shop", serverVersion: null, tables: [], truncated: false });
    invalidateAfterDdl(qc, "mysql", "p1");
    expect(qc.getQueryState(schemaMetadataKey("p1", "shop", "mysql"))?.isInvalidated).toBe(true);
    expect(qc.getQueryState(["my", "p1", "shop", "objects"])?.isInvalidated).toBe(true);
    expect(qc.getQueryState(schemaMetadataKey("p2", "shop", "mysql"))?.isInvalidated).toBe(false);
    expect(qc.getQueryState(schemaMetadataKey("p1", "shop"))?.isInvalidated).toBe(false);
    expect(DDL_TAG.test("RENAME TABLE")).toBe(true);
  });
});
