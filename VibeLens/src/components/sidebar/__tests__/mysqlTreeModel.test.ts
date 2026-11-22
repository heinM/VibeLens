// The MySQL navigator model: databases > groups > objects > columns, system databases, refresh keys and
// the SQL the tree writes (backtick-quoted, so no name can break out of its identifier).
import { describe, expect, it } from "vitest";
import type { SchemaObject, TableDescription } from "../../../ipc/types";
import { buildMyRows, expandedMyDatabases, isMySystemDatabase, myDbId, myGroupId, myObjectId, myQualifiedName, myRefreshKeys, mySelectAllSql, myShowCreateSql, myTableKey } from "../mysqlTreeModel";

const obj = (name: string, kind: SchemaObject["kind"]): SchemaObject => ({ schema: "shop", name, kind, comment: null, estimatedRows: null, signature: null, isPartition: false, parentSchema: null, parentName: null });
const ok = <T,>(data: T) => ({ status: "success" as const, data });
const desc = (name: string): TableDescription => ({
  schema: "shop",
  name,
  kind: "table",
  comment: null,
  estimatedRows: null,
  columns: [{ name: "id", dataType: "int", nullable: false, default: null, isPrimaryKey: true, ordinal: 1, comment: null }],
  indexes: [],
  foreignKeys: [],
});

describe("buildMyRows", () => {
  it("shows loading and errors at the top", () => {
    expect(buildMyRows({ databases: { status: "pending" }, objects: new Map(), describes: new Map(), toggled: new Map(), showSystem: false })[0].statusKind).toBe("loading");
    const e = buildMyRows({ databases: { status: "error", error: "boom" }, objects: new Map(), describes: new Map(), toggled: new Map(), showSystem: false });
    expect(e[0]).toMatchObject({ statusKind: "error", label: "boom" });
  });
  it("hides MySQL's own databases unless asked, but never the connection's default", () => {
    const dbs = ok(["shop", "mysql", "sys", "information_schema", "performance_schema", "analytics"]);
    const labels = (showSystem: boolean, defaultDb: string | null = null) =>
      buildMyRows({ databases: dbs, objects: new Map(), describes: new Map(), toggled: new Map(), showSystem, defaultDb }).map((r) => r.label);
    expect(labels(false)).toEqual(["analytics", "shop"]);
    expect(labels(true)).toHaveLength(6);
    expect(labels(false, "sys")).toEqual(["analytics", "shop", "sys"]);
    expect(isMySystemDatabase("Performance_Schema")).toBe(true);
  });
  it("groups objects, opens groups by default, and lists columns of an expanded table only", () => {
    const toggled = new Map([
      [myDbId("shop"), true],
      [myObjectId("shop", obj("orders", "table")), true],
      [myObjectId("shop", obj("add_order", "procedure")), true],
    ]);
    const rows = buildMyRows({
      databases: ok(["shop"]),
      objects: new Map([["shop", ok([obj("orders", "table"), obj("v_orders", "view"), obj("total", "function"), obj("add_order", "procedure")])]]),
      describes: new Map([[myTableKey("shop", "orders"), ok(desc("orders"))]]),
      toggled,
      showSystem: false,
    });
    expect(rows.map((r) => `${r.depth}:${r.kind}:${r.label}`)).toEqual([
      "0:db:shop",
      "1:group:Tables",
      "2:object:orders",
      "3:column:id",
      "1:group:Views",
      "2:object:v_orders",
      "1:group:Functions",
      "2:object:total",
      "1:group:Procedures",
      "2:object:add_order",
    ]);
    expect(rows.find((r) => r.label === "add_order")?.expandable).toBe(false);
    expect(rows.find((r) => r.label === "Tables")?.count).toBe(1);
    // A closed group hides its members.
    const closed = buildMyRows({ databases: ok(["shop"]), objects: new Map([["shop", ok([obj("orders", "table")])]]), describes: new Map(), toggled: new Map([[myDbId("shop"), true], [myGroupId("shop", "tables"), false]]), showSystem: false });
    expect(closed.map((r) => r.label)).toEqual(["shop", "Tables"]);
  });
  it("says so when a database is empty, and loads lazily", () => {
    const toggled = new Map([[myDbId("shop"), true]]);
    expect(buildMyRows({ databases: ok(["shop"]), objects: new Map([["shop", ok([])]]), describes: new Map(), toggled, showSystem: false })[1].statusKind).toBe("empty");
    expect(buildMyRows({ databases: ok(["shop"]), objects: new Map(), describes: new Map(), toggled, showSystem: false })[1].statusKind).toBe("loading");
    expect(expandedMyDatabases(["shop", "other"], toggled)).toEqual(["shop"]);
  });
});

describe("SQL the tree writes", () => {
  it("quotes with backticks and doubles any backtick in a name", () => {
    expect(mySelectAllSql("shop", "orders")).toBe("SELECT * FROM `shop`.`orders` LIMIT 100;");
    expect(myQualifiedName("we`ird", "t`; DROP TABLE x; -- ")).toBe("`we``ird`.`t``; DROP TABLE x; -- `");
    expect(myShowCreateSql("shop", "v", "view")).toBe("SHOW CREATE VIEW `shop`.`v`;");
    expect(myShowCreateSql("shop", "p", "procedure")).toBe("SHOW CREATE PROCEDURE `shop`.`p`;");
  });
  it("refreshes the narrowest set of cached answers", () => {
    expect(myRefreshKeys("p1", { kind: "object", db: "shop", object: obj("orders", "table") })).toEqual([["my", "p1", "shop", "describe", "orders"]]);
    expect(myRefreshKeys("p1", { kind: "db", db: "shop" })).toEqual([["my", "p1", "shop"]]);
    expect(myRefreshKeys("p1")).toEqual([["my", "p1"]]);
  });
});
