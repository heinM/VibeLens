import { describe, expect, it } from "vitest";
import {
  boundLabel,
  buildPgRows,
  collectPartitionRelations,
  compactCount,
  dbId,
  describeKey,
  groupId,
  objKey,
  objectId,
  partitionIntervalLabel,
  partitionObject,
  partitionStats,
  partitionSummary,
  partmanSummary,
  schemaId,
  type PgModelInput,
  type QueryState,
  type RelationRef,
} from "../pgTreeModel";
import type { DatabaseInfo, PartitionInfo, PartitionListing, PartmanConfig, SchemaInfo, SchemaObject, TableDescription } from "../../../ipc/types";

const ok = <T,>(data: T): QueryState<T> => ({ status: "success", data });
const db = (name: string, extra: Partial<DatabaseInfo> = {}): DatabaseInfo => ({ name, isTemplate: false, allowConnections: true, sizeBytes: null, ...extra });
const schema = (name: string, isSystem = false): SchemaInfo => ({ name, owner: null, isSystem });
const obj = (name: string, kind: SchemaObject["kind"]): SchemaObject => ({ schema: "public", name, kind, comment: null, estimatedRows: null, signature: null, isPartition: false, parentSchema: null, parentName: null });

const base = (over: Partial<PgModelInput> = {}): PgModelInput => ({
  databases: ok([db("app"), db("template1", { isTemplate: true })]),
  schemas: new Map([["app", ok([schema("public"), schema("pg_catalog", true)])]]),
  objects: new Map([[objKey("app", "public"), ok([obj("users", "table"), obj("v_users", "view"), obj("f", "function")])]]),
  describes: new Map(),
  partitions: new Map(),
  toggled: new Map(),
  showSystem: false,
  currentDb: "app",
  ...over,
});

describe("buildPgRows", () => {
  it("hides template databases and collapsed children by default", () => {
    const rows = buildPgRows(base());
    expect(rows.map((r) => r.label)).toEqual(["app"]);
  });
  it("shows system things when asked", () => {
    const rows = buildPgRows(base({ showSystem: true, toggled: new Map([[dbId("app"), true]]) }));
    expect(rows.some((r) => r.label === "template1")).toBe(true);
    expect(rows.some((r) => r.label === "pg_catalog")).toBe(true);
  });
  it("groups objects and expands Tables by default", () => {
    const toggled = new Map([
      [dbId("app"), true],
      [schemaId("app", "public"), true],
    ]);
    const rows = buildPgRows(base({ toggled }));
    expect(rows.map((r) => `${r.depth}:${r.label}`)).toEqual(["0:app", "1:public", "2:Tables", "3:users", "2:Views", "2:Functions"]);
    expect(rows.find((r) => r.label === "Tables")?.count).toBe(1);
  });
  it("shows columns of an expanded table, ordered by ordinal", () => {
    const users = obj("users", "table");
    const desc: TableDescription = {
      schema: "public",
      name: "users",
      kind: "table",
      comment: null,
      estimatedRows: 3,
      indexes: [],
      foreignKeys: [],
      columns: [
        { name: "name", dataType: "text", nullable: true, default: null, isPrimaryKey: false, ordinal: 2, comment: null },
        { name: "id", dataType: "int4", nullable: false, default: null, isPrimaryKey: true, ordinal: 1, comment: null },
      ],
    };
    const toggled = new Map([
      [dbId("app"), true],
      [schemaId("app", "public"), true],
      [objectId("app", users), true],
    ]);
    const rows = buildPgRows(base({ toggled, describes: new Map([[describeKey("app", "public", "users"), ok(desc)]]) }));
    const cols = rows.filter((r) => r.kind === "column").map((r) => r.label);
    expect(cols).toEqual(["id", "name"]);
  });
  it("emits loading and error placeholders", () => {
    const toggled = new Map([[dbId("app"), true]]);
    expect(buildPgRows(base({ toggled, schemas: new Map() })).map((r) => r.statusKind)).toEqual([undefined, "loading"]);
    const err = buildPgRows(base({ toggled, schemas: new Map([["app", { status: "error", error: "boom" } as QueryState<SchemaInfo[]>]]) }));
    expect(err[1]).toMatchObject({ statusKind: "error", label: "boom" });
  });
  it("lets a group be collapsed explicitly", () => {
    const toggled = new Map([
      [dbId("app"), true],
      [schemaId("app", "public"), true],
      [groupId("app", "public", "tables"), false],
    ]);
    expect(buildPgRows(base({ toggled })).some((r) => r.label === "users")).toBe(false);
  });
});

describe("compactCount", () => {
  it("abbreviates", () => {
    expect(compactCount(12)).toBe("~12");
    expect(compactCount(1200)).toBe("~1.2k");
    expect(compactCount(45_000)).toBe("~45k");
    expect(compactCount(2_500_000)).toBe("~2.5M");
    expect(compactCount(null)).toBe("");
  });
});

const part = (name: string, over: Partial<PartitionInfo> = {}): PartitionInfo => ({
  schema: "public",
  name,
  bound: "FOR VALUES FROM ('2024-01-01') TO ('2024-02-01')",
  isDefault: false,
  isPartitioned: false,
  estimatedRows: 1200,
  sizeBytes: 8192,
  ...over,
});
const managed: PartmanConfig = { control: null, partitionInterval: "1 day", premake: 4, retention: "90 days", retentionKeepTable: false, automaticMaintenance: null };
const listing = (partitions: PartitionInfo[], over: Partial<PartitionListing> = {}): PartitionListing => ({ partitionKey: "RANGE (created_at)", partitions, truncated: false, partman: null, ...over });
const open = (...ids: string[]) => new Map<string, boolean>([[dbId("app"), true], [schemaId("app", "public"), true], ...ids.map((i) => [i, true] as [string, boolean])]);
const events = obj("events", "partitionedTable");
const childOf = (name: string, extra: Partial<SchemaObject> = {}): SchemaObject => ({ ...obj(name, "table"), isPartition: true, parentSchema: "public", parentName: "events", ...extra });

describe("partitioned tables", () => {
  const objects = new Map([[objKey("app", "public"), ok([events, childOf("events_2024_01"), childOf("events_default"), obj("users", "table")])]]);

  it("hides partitions from the flat Tables list", () => {
    const rows = buildPgRows(base({ objects, toggled: open() }));
    expect(rows.map((r) => r.label)).toEqual(["app", "public", "Tables", "events", "users"]);
    expect(rows.find((r) => r.label === "Tables")?.count).toBe(2);
  });

  it("says so when a schema holds only partitions of tables elsewhere", () => {
    const only = new Map([[objKey("app", "public"), ok([childOf("p1", { parentSchema: "other" })])]]);
    const rows = buildPgRows(base({ objects: only, toggled: open() }));
    expect(rows.at(-1)).toMatchObject({ kind: "status", statusKind: "empty", label: "Only partitions of tables in other schemas" });
  });

  it("shows the summary row, then the partitions with bound and size, under the expanded parent", () => {
    const l = listing([part("events_2024_01"), part("events_default", { bound: "DEFAULT", isDefault: true, sizeBytes: null, estimatedRows: null })], { partman: managed });
    const rows = buildPgRows(base({ objects, toggled: open(objectId("app", events)), partitions: new Map([[describeKey("app", "public", "events"), ok(l)]]) }));
    const under = rows.slice(rows.findIndex((r) => r.label === "events") + 1);
    // Columns are still pending here; the partition rows follow the summary at one level deeper.
    const summary = under.find((r) => r.kind === "partitionSummary");
    expect(summary).toMatchObject({ depth: 4, label: "RANGE (created_at) · 2 partitions · pg_partman · daily · premake 4 · retention 90 days" });
    const parts = under.filter((r) => r.partition);
    expect(parts.map((r) => `${r.depth}:${r.label}`)).toEqual(["4:events_2024_01", "4:events_default"]);
    expect(parts[0].object).toMatchObject({ kind: "table", isPartition: true, parentName: "events", schema: "public" });
    expect(under.indexOf(summary!)).toBeLessThan(under.indexOf(parts[0]));
    expect(boundLabel(parts[0].partition!.bound)).toBe("FROM ('2024-01-01') TO ('2024-02-01')");
    expect(boundLabel(parts[1].partition!.bound)).toBe("DEFAULT");
  });

  it("shows loading, error and empty states for the partition list without hiding the columns", () => {
    const toggled = open(objectId("app", events));
    const at = (q: QueryState<PartitionListing>) => buildPgRows(base({ objects, toggled, partitions: new Map([[describeKey("app", "public", "events"), q]]) }));
    expect(at({ status: "pending" }).some((r) => r.label === "Loading partitions…")).toBe(true);
    expect(at({ status: "error", error: "denied" }).some((r) => r.statusKind === "error" && r.label === "denied")).toBe(true);
    expect(at(ok(listing([]))).some((r) => r.statusKind === "empty" && r.label === "No partitions")).toBe(true);
    // Nothing requested yet (collapsed parent) shows no partition rows at all.
    expect(buildPgRows(base({ objects, toggled: open() })).some((r) => r.kind === "partitionSummary")).toBe(false);
  });

  it("expands a partition like a table: columns from its own schema, and nested partitions when it is partitioned", () => {
    const sub = part("events_2024", { schema: "archive", isPartitioned: true, bound: "FOR VALUES FROM (2024) TO (2025)" });
    const subObj = partitionObject(sub, events);
    const leaf = part("events_2024_q1", { schema: "archive", bound: "FOR VALUES IN ('q1')" });
    const desc: TableDescription = {
      schema: "archive",
      name: "events_2024",
      kind: "partitionedTable",
      comment: null,
      estimatedRows: null,
      indexes: [],
      foreignKeys: [],
      columns: [{ name: "id", dataType: "int8", nullable: false, default: null, isPrimaryKey: true, ordinal: 1, comment: null }],
    };
    const rows = buildPgRows(
      base({
        objects,
        toggled: open(objectId("app", events), objectId("app", subObj)),
        partitions: new Map([
          [describeKey("app", "public", "events"), ok(listing([sub]))],
          [describeKey("app", "archive", "events_2024"), ok(listing([leaf], { partitionKey: "LIST (region)" }))],
        ]),
        describes: new Map([[describeKey("app", "archive", "events_2024"), ok(desc)]]),
      }),
    );
    const i = rows.findIndex((r) => r.label === "events_2024");
    expect(rows[i]).toMatchObject({ depth: 4, expandable: true, expanded: true, schema: "archive" });
    expect(rows.slice(i + 1, i + 4).map((r) => `${r.depth}:${r.kind}:${r.label}`)).toEqual(["5:column:id", "5:partitionSummary:LIST (region) · 1 partition", "5:object:events_2024_q1"]);
  });

  it("collects only what the expanded nodes need, level by level", () => {
    const sub = part("events_2024", { schema: "archive", isPartitioned: true });
    const subObj = partitionObject(sub, events);
    const cache = new Map<string, PartitionListing>([[describeKey("app", "public", "events"), listing([sub, part("events_other")])]]);
    const get = (r: RelationRef) => cache.get(describeKey(r.db, r.schema, r.table));
    const collect = (toggled: Map<string, boolean>) => {
      const out = { listings: [] as RelationRef[], describes: [] as RelationRef[] };
      collectPartitionRelations("app", events, toggled, get, out);
      return out;
    };
    // Only the parent is expanded: one listing, no column fetches.
    expect(collect(new Map())).toEqual({ listings: [{ db: "app", schema: "public", table: "events" }], describes: [] });
    // The sub-partition is expanded: its columns and its own listing are wanted.
    const out = collect(new Map([[objectId("app", subObj), true]]));
    expect(out.describes).toEqual([{ db: "app", schema: "archive", table: "events_2024" }]);
    expect(out.listings.map((r) => r.table)).toEqual(["events", "events_2024"]);
    // Before the parent's listing has arrived nothing deeper can be known.
    cache.clear();
    expect(collect(new Map([[objectId("app", subObj), true]])).listings).toHaveLength(1);
  });
});

describe("partition text", () => {
  it("puts the key, the count and the partman line in one summary", () => {
    expect(partitionSummary(listing([part("a")]))).toBe("RANGE (created_at) · 1 partition");
    expect(partitionSummary(listing([part("a"), part("b")], { truncated: true }))).toBe("RANGE (created_at) · 2+ partitions");
    expect(partitionSummary(listing([], { partitionKey: null }))).toBe("0 partitions");
  });
  it("describes the partman configuration", () => {
    expect(partmanSummary(managed)).toBe("pg_partman · daily · premake 4 · retention 90 days");
    expect(partmanSummary({ control: "id", partitionInterval: "100000", premake: null, retention: "10", retentionKeepTable: true, automaticMaintenance: "off" })).toBe(
      "pg_partman · control id · every 100000 ids · retention 10, keep tables · maintenance off",
    );
    expect(partmanSummary({ control: null, partitionInterval: null, premake: null, retention: null, retentionKeepTable: null, automaticMaintenance: null })).toBe("pg_partman");
  });
  it("words partition intervals", () => {
    const cases: Record<string, string> = {
      "1 day": "daily",
      "7 days": "weekly",
      "1 week": "weekly",
      "1 mon": "monthly",
      "3 mons": "every 3 months",
      "1 year": "yearly",
      "01:00:00": "hourly",
      "4 hours": "every 4 hours",
      monthly: "monthly",
      "100000": "every 100000 ids",
      "something odd": "something odd",
    };
    for (const [raw, label] of Object.entries(cases)) expect(partitionIntervalLabel(raw), raw).toBe(label);
  });
  it("shows rows and size beside a partition", () => {
    expect(partitionStats(part("a", { estimatedRows: 1200, sizeBytes: 8192 }))).toBe("~1.2k · 8.00 KB");
    expect(partitionStats(part("a", { estimatedRows: null, sizeBytes: null }))).toBe("");
  });
});
