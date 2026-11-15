import { describe, expect, it } from "vitest";
import { buildPgRows, compactCount, dbId, groupId, objKey, objectId, schemaId, describeKey, type PgModelInput, type QueryState } from "../pgTreeModel";
import type { DatabaseInfo, SchemaInfo, SchemaObject, TableDescription } from "../../../ipc/types";

const ok = <T,>(data: T): QueryState<T> => ({ status: "success", data });
const db = (name: string, extra: Partial<DatabaseInfo> = {}): DatabaseInfo => ({ name, isTemplate: false, allowConnections: true, sizeBytes: null, ...extra });
const schema = (name: string, isSystem = false): SchemaInfo => ({ name, owner: null, isSystem });
const obj = (name: string, kind: SchemaObject["kind"]): SchemaObject => ({ schema: "public", name, kind, comment: null, estimatedRows: null, signature: null });

const base = (over: Partial<PgModelInput> = {}): PgModelInput => ({
  databases: ok([db("app"), db("template1", { isTemplate: true })]),
  schemas: new Map([["app", ok([schema("public"), schema("pg_catalog", true)])]]),
  objects: new Map([[objKey("app", "public"), ok([obj("users", "table"), obj("v_users", "view"), obj("f", "function")])]]),
  describes: new Map(),
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
