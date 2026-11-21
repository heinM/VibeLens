// Identifier quoting per engine, for everything that writes SQL for the user:
//  * Spark SQL reads "x" as a string literal, so `SELECT "user" FROM t` returns the text on every row:
//    completion, join hints, the palette and the Iceberg helpers must quote with backticks there.
//  * Trino / Athena read `analytics.daily.orders` as catalog.schema.table: a nested Iceberg namespace is
//    one quoted schema there, `"analytics.daily".orders`.
import { describe, expect, it } from "vitest";
import type { ColumnInfo, ForeignKeyInfo, SchemaMetadata, TableDescription } from "../../../ipc/types";
import { ident, indexCatalog, quoteIdent, schemaIdent } from "../catalog";
import { complete } from "../complete";
import { quoteIdentFor, type SqlDialect } from "../dialect";
import { identForSql, insertionText } from "../../../components/palette/items";
import { namespaceSql, qualifiedName, quoteAlways, selectAsOfSnapshotSql } from "../../../components/iceberg/iceSql";
import { cur } from "./fixtures";

let ord = 0;
const col = (name: string, dataType = "string", o: Partial<ColumnInfo> = {}): ColumnInfo => ({ name, dataType, nullable: true, default: null, isPrimaryKey: false, ordinal: ++ord, comment: null, ...o });
const table = (schema: string, name: string, columns: ColumnInfo[], foreignKeys: ForeignKeyInfo[] = []): TableDescription => ({ schema, name, kind: "table", comment: null, estimatedRows: null, columns, indexes: [], foreignKeys });
const META: SchemaMetadata = {
  database: "x",
  serverVersion: null,
  truncated: false,
  tables: [
    table("shop", "orders", [col("id", "long", { isPrimaryKey: true }), col("user"), col("Total"), col("customer_id", "long")], [{ name: "fk", columns: ["customer_id"], referencedSchema: "shop", referencedTable: "Customers", referencedColumns: ["id"] }]),
    table("shop", "Customers", [col("id", "long", { isPrimaryKey: true })]),
    table("analytics.daily", "orders", [col("day", "date")]),
  ],
};
const dotted = indexCatalog(META, { dotted: true, defaultSchema: null });
const at = (src: string, dialect: SqlDialect) => {
  const { text, offset } = cur(src);
  return complete(dotted, text, offset, { dialect })?.suggestions ?? [];
};
const insertOf = (src: string, dialect: SqlDialect, label: string) => at(src, dialect).find((s) => s.label === label)?.insertText;

describe("the shared quoting helper", () => {
  it("uses backticks (doubling a backtick) for Spark and double quotes elsewhere", () => {
    expect(quoteIdentFor("user", "spark")).toBe("`user`");
    expect(quoteIdentFor("a`b", "spark")).toBe("`a``b`");
    expect(quoteIdentFor('a"b', "spark")).toBe('`a"b`');
    for (const d of ["postgres", "trino", "athena", "datafusion", "redshift", null] as const) expect(quoteIdentFor('a"b', d)).toBe('"a""b"');
  });
  it("is the one catalog.ts and iceSql.ts use", () => {
    expect(quoteIdent("user", "spark")).toBe(quoteIdentFor("user", "spark"));
    expect(quoteAlways("user", "spark")).toBe(quoteIdentFor("user", "spark"));
    expect(ident("user", false, "spark")).toBe("`user`");
    expect(ident("user")).toBe('"user"');
    expect(ident("a$b", false, "spark")).toBe("`a$b`"); // `$` is not a bare Spark identifier character
    expect(ident("a$b")).toBe("a$b");
  });
});

describe("Spark completion quotes with backticks", () => {
  it("columns: a reserved or mixed-case column is never inserted as a string literal", () => {
    expect(insertOf("select | from shop.orders", "spark", "user")).toBe("`user`");
    expect(insertOf("select | from shop.orders", "spark", "Total")).toBe("`Total`");
    expect(insertOf("select | from shop.orders", "trino", "user")).toBe('"user"');
    expect(insertOf("select o.| from shop.orders o", "spark", "user")).toBe("`user`");
  });
  it("tables and namespaces", () => {
    expect(insertOf("select * from shop.|", "spark", "Customers")).toBe("`Customers`");
    expect(insertOf("select * from |", "spark", "Customers")).toBe("shop.`Customers`");
  });
  it("join conditions", () => {
    const j = at("select * from shop.orders o join shop.Customers c on |", "spark").find((s) => s.kind === "join");
    expect(j?.insertText).toBe("o.customer_id = c.id");
    const k = at("select * from shop.orders join shop.Customers on |", "spark").find((s) => s.kind === "join");
    expect(k?.insertText).toContain("`Customers`.id");
    expect(k?.insertText).not.toContain('"');
  });
  it("the palette's insert text", () => {
    expect(identForSql("user", "spark")).toBe("`user`");
    expect(insertionText("Sales", "order", "spark")).toBe("`Sales`.`order`");
    expect(insertionText("Sales", "order")).toBe('"Sales"."order"');
    expect(insertionText("shop", "orders", "spark")).toBe("shop.orders");
  });
});

describe("Trino reads a nested namespace as one schema", () => {
  it("schemaIdent quotes the whole path on Trino / Athena, level by level elsewhere", () => {
    expect(schemaIdent(dotted, "analytics.daily", false, "trino")).toBe('"analytics.daily"');
    expect(schemaIdent(dotted, "analytics.daily", false, "athena")).toBe('"analytics.daily"');
    expect(schemaIdent(dotted, "analytics.daily", false, "spark")).toBe("analytics.daily");
    expect(schemaIdent(dotted, "analytics.daily", false, "datafusion")).toBe("analytics.daily");
    expect(schemaIdent(dotted, "shop", false, "trino")).toBe("shop");
  });
  it("completion inserts the namespace as one schema and offers it whole, never level by level", () => {
    expect(at("select * from |", "trino").filter((s) => s.label === "orders").map((s) => s.insertText)).toContain('"analytics.daily".orders');
    expect(at("select * from |", "trino").find((s) => s.kind === "schema" && s.label === "analytics.daily")?.insertText).toBe('"analytics.daily".');
    expect(at("select * from analytics.|", "trino").some((s) => s.label === "daily")).toBe(false);
    // The local engine keeps its level-by-level form.
    expect(at("select * from analytics.|", "datafusion").some((s) => s.label === "daily")).toBe(true);
  });
  it("the table helpers write the same form, time travel included", () => {
    expect(qualifiedName(["analytics", "daily"], "orders", "trino")).toBe('"analytics.daily".orders');
    expect(qualifiedName(["analytics", "daily"], "orders", "spark")).toBe("analytics.daily.orders");
    expect(qualifiedName(["analytics"], "orders", "trino")).toBe("analytics.orders");
    expect(namespaceSql(["analytics", "daily"], "athena")).toBe('"analytics.daily"');
    expect(selectAsOfSnapshotSql(["analytics", "daily"], "orders", "42", 10, "trino")).toBe('SELECT * FROM "analytics.daily".orders FOR VERSION AS OF 42 LIMIT 10;');
    expect(selectAsOfSnapshotSql(["analytics", "daily"], "orders", "42", 10, "spark")).toBe("SELECT * FROM analytics.daily.orders VERSION AS OF 42 LIMIT 10;");
  });
});
