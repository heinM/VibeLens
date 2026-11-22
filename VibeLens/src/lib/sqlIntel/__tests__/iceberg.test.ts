// Completion for Iceberg connections: the same engine as Postgres, fed by `ice_schema_metadata`
// (schema = dotted namespace) and a DataFusion / Athena vocabulary.
import { describe, expect, it } from "vitest";
import type { ColumnInfo, SchemaMetadata, TableDescription } from "../../../ipc/types";
import { buildAskPrompt, buildFixPrompt, schemaDigest } from "../aiPrompts";
import { childNamespaces, indexCatalog, lookupTable, resolveSchemaQualifier, schemaIdent } from "../catalog";
import { complete, type CompletionResult } from "../complete";
import { catalogEngineOf, dialectOf, functionsFor, snippetsFor, statementStartFor, tableSuffixTarget, type SqlDialect } from "../dialect";
import { schemaMetadataKey, isSchemaMetadataKeyOf } from "../../schemaCache";
import { newProfile } from "../../defaults";
import { cur, labels } from "./fixtures";

let ord = 0;
const col = (name: string, dataType: string, o: Partial<ColumnInfo> = {}): ColumnInfo => ({ name, dataType, nullable: true, default: null, isPrimaryKey: false, ordinal: ++ord, comment: null, ...o });
const table = (schema: string, name: string, columns: ColumnInfo[], o: Partial<TableDescription> = {}): TableDescription => ({
  schema,
  name,
  kind: "table",
  comment: null,
  estimatedRows: null,
  columns,
  indexes: [],
  foreignKeys: [],
  ...o,
});

const ICE: SchemaMetadata = {
  database: "vg_test",
  serverVersion: null,
  truncated: false,
  tables: [
    table("shop", "orders", [col("id", "long"), col("customer_id", "long"), col("status", "string"), col("total", "decimal(12, 2)"), col("placed_at", "timestamptz")]),
    table("shop", "customers", [col("id", "long"), col("email", "string"), col("country", "string")]),
    table("shop", "events", [col("event_id", "long"), col("ctx", "struct<device:string>")]),
    table("lake.sales", "daily_revenue", [col("day", "date"), col("revenue", "decimal(14, 2)")]),
    table("lake.sales", "Q4 forecast", [col("amount", "double")]),
    table("lake.marketing", "campaigns", [col("id", "long")]),
    table("shop", "v_recent", [col("id", "long")], { kind: "view" }),
  ],
};
const dotted = indexCatalog(ICE, { dotted: true, defaultSchema: null });
const withDefault = indexCatalog(ICE, { dotted: true, defaultSchema: "shop" });

const at = (src: string, dialect: SqlDialect, cat = dotted, trigger?: string): CompletionResult | null => {
  const { text, offset } = cur(src);
  return complete(cat, text, offset, { dialect, triggerCharacter: trigger });
};

describe("dialects", () => {
  it("is chosen from the profile's engine", () => {
    expect(dialectOf(newProfile("postgres"))).toBe("postgres");
    const local = newProfile("iceberg");
    expect(dialectOf(local)).toBe("datafusion");
    const athena = { ...local, iceberg: { ...local.iceberg!, queryEngine: { kind: "athena" as const, workgroup: "primary", outputLocation: null, aws: null } } };
    expect(dialectOf(athena)).toBe("athena");
    expect(dialectOf(newProfile("valkey"))).toBeNull();
    expect(dialectOf(undefined)).toBeNull();
  });

  it("maps to the catalog family", () => {
    expect(catalogEngineOf("postgres")).toBe("postgres");
    expect(catalogEngineOf(undefined)).toBe("postgres");
    expect(catalogEngineOf("datafusion")).toBe("iceberg");
    expect(catalogEngineOf("athena")).toBe("iceberg");
  });

  it("keeps Postgres and Iceberg catalog cache keys apart", () => {
    const pg = schemaMetadataKey("p1", "db");
    const ice = schemaMetadataKey("p1", "db", "iceberg");
    expect(pg).toEqual(["pg", "p1", "db", "schema-metadata"]);
    expect(ice).toEqual(["ice", "p1", "schema-metadata"]);
    expect(JSON.stringify(pg)).not.toBe(JSON.stringify(ice));
    // The Iceberg key does not depend on a database, so a stale tab.database cannot split the cache.
    expect(schemaMetadataKey("p1", "other", "iceberg")).toEqual(ice);
    expect(isSchemaMetadataKeyOf(ice, "p1")).toBe(true);
    expect(isSchemaMetadataKeyOf(pg, "p1")).toBe(true);
    expect(isSchemaMetadataKeyOf(ice, "p2")).toBe(false);
  });
});

describe("function vocabulary", () => {
  const names = (d: SqlDialect) => new Set(functionsFor(d).map((f) => f.name));
  it("offers the Trino / DataFusion essentials", () => {
    for (const d of ["datafusion", "athena"] as const) {
      for (const f of ["date_trunc", "approx_distinct", "array_agg", "coalesce", "count"]) expect(names(d).has(f), `${d}:${f}`).toBe(true);
    }
    expect(names("athena").has("count_if")).toBe(true);
    expect(names("athena").has("date_diff")).toBe(true);
    expect(names("datafusion").has("date_bin")).toBe(true);
  });
  it("drops Postgres-only functions, and each engine's missing ones", () => {
    for (const d of ["datafusion", "athena"] as const) for (const f of ["jsonb_agg", "jsonb_build_object", "gen_random_uuid", "age"]) expect(names(d).has(f), `${d}:${f}`).toBe(false);
    expect(names("athena").has("to_char")).toBe(false);
    expect(names("athena").has("string_agg")).toBe(false);
    expect(names("datafusion").has("string_agg")).toBe(true);
    expect(names("postgres").has("jsonb_agg")).toBe(true);
  });
  it("has no duplicate names", () => {
    for (const d of ["datafusion", "athena", "postgres"] as const) {
      const all = functionsFor(d).map((f) => f.name);
      expect(new Set(all).size).toBe(all.length);
    }
  });
  it("shows up in expressions", () => {
    const l = labels(at("select | from shop.orders", "athena"));
    expect(l).toContain("approx_distinct");
    expect(l).toContain("total");
    expect(l).not.toContain("jsonb_agg");
  });
  it("types and statements follow the dialect", () => {
    expect(labels(at("select x::|", "athena"))).toEqual(expect.arrayContaining(["varchar", "bigint", "varbinary"]));
    expect(labels(at("select x::|", "athena"))).not.toContain("jsonb");
    expect(labels(at("select x::|", "datafusion"))).toContain("string");
    expect(labels(at("select x::|", "postgres"))).toContain("jsonb");
    expect(statementStartFor("datafusion")).not.toContain("VACUUM");
    expect(statementStartFor("athena")).toContain("MERGE INTO");
    expect(statementStartFor("datafusion")).not.toContain("MERGE INTO");
    expect(statementStartFor("postgres")).toContain("VACUUM");
  });
  it("hides UPDATE and DELETE snippets on the append-only local engine only", () => {
    const f = (d: SqlDialect) => snippetsFor(d).map((s) => s.filter);
    expect(f("datafusion")).not.toContain("upd");
    expect(f("datafusion")).not.toContain("del");
    expect(f("datafusion")).toContain("ins");
    expect(f("athena")).toContain("upd");
    expect(f("athena")).toContain("selver");
    expect(f("datafusion")).toContain("selsnap");
  });
});

describe("catalog with dotted namespaces", () => {
  it("resolves qualified names of any depth", () => {
    expect(lookupTable(dotted, ["shop", "orders"])?.name).toBe("orders");
    expect(lookupTable(dotted, ["lake", "sales", "daily_revenue"])?.schema).toBe("lake.sales");
    expect(lookupTable(dotted, ["daily_revenue"])?.schema).toBe("lake.sales");
    expect(lookupTable(dotted, ["lake", "daily_revenue"])).toBeUndefined();
  });
  it("sees through time travel and metadata-table suffixes", () => {
    expect(lookupTable(dotted, ["shop", "orders@123456789012"])?.name).toBe("orders");
    expect(lookupTable(dotted, ["shop", "orders$snapshots"])?.name).toBe("orders");
    // Postgres catalogs are untouched by the suffix rule.
    expect(lookupTable(indexCatalog(ICE), ["shop", "orders@1"])).toBeUndefined();
  });
  it("prefers the default namespace for bare names", () => {
    const meta: SchemaMetadata = { ...ICE, tables: [...ICE.tables, table("staging", "orders", [col("id", "long")])] };
    expect(lookupTable(indexCatalog(meta, { dotted: true, defaultSchema: "staging" }), ["orders"])?.schema).toBe("staging");
    expect(lookupTable(indexCatalog(meta, { dotted: true, defaultSchema: "shop" }), ["orders"])?.schema).toBe("shop");
  });
  it("names namespace levels", () => {
    expect(resolveSchemaQualifier(dotted, ["lake", "sales"])).toBe("lake.sales");
    expect(resolveSchemaQualifier(dotted, ["shop"])).toBe("shop");
    expect(resolveSchemaQualifier(dotted, ["lake"])).toBeNull();
    expect(childNamespaces(dotted, ["lake"])).toEqual(["marketing", "sales"]);
    expect(childNamespaces(dotted, ["shop"])).toEqual([]);
    expect(schemaIdent(dotted, "lake.sales")).toBe("lake.sales");
    expect(schemaIdent(dotted, "Lake.Sales")).toBe('"Lake"."Sales"');
    expect(schemaIdent(indexCatalog(ICE), "weird.name")).toBe('"weird.name"');
  });
  it("re-indexes when the options change", () => {
    expect(indexCatalog(ICE, { dotted: true, defaultSchema: null }).dotted).toBe(true);
    expect(indexCatalog(ICE).dotted).toBe(false);
    expect(indexCatalog(ICE, { dotted: true, defaultSchema: "shop" }).defaultSchema).toBe("shop");
  });
});

describe("completion over Iceberg metadata", () => {
  it("lists namespaces and tables after FROM", () => {
    const l = labels(at("select * from |", "datafusion"));
    expect(l).toEqual(expect.arrayContaining(["orders", "customers", "daily_revenue", "shop", "lake"]));
    // Only top-level namespaces are offered as schemas; the deeper ones come after the dot.
    expect(l).not.toContain("lake.sales");
    const r = at("select * from |", "datafusion");
    const orders = r!.suggestions.find((s) => s.label === "orders")!;
    expect(orders.insertText).toBe("shop.orders");
    expect(r!.suggestions.find((s) => s.label === "daily_revenue")!.insertText).toBe("lake.sales.daily_revenue");
    expect(r!.suggestions.find((s) => s.label === "Q4 forecast")!.insertText).toBe('lake.sales."Q4 forecast"');
    expect(r!.suggestions.find((s) => s.label === "lake")!.insertText).toBe("lake.");
    expect(r!.suggestions.find((s) => s.label === "v_recent")!.kind).toBe("view");
  });
  it("leaves tables in the default namespace unqualified", () => {
    const r = at("select * from |", "datafusion", withDefault);
    expect(r!.suggestions.find((s) => s.label === "orders")!.insertText).toBe("orders");
    expect(r!.suggestions.find((s) => s.label === "daily_revenue")!.insertText).toBe("lake.sales.daily_revenue");
  });
  it("completes tables after a namespace path, one level at a time", () => {
    expect(labels(at("select * from shop.|", "datafusion"))).toEqual(expect.arrayContaining(["orders", "customers", "events"]));
    expect(labels(at("select * from lake.|", "datafusion"))).toEqual(["marketing", "sales"]);
    expect(labels(at("select * from lake.sales.|", "datafusion"))).toEqual(expect.arrayContaining(["daily_revenue", "Q4 forecast"]));
    expect(at("select * from lake.|", "datafusion")!.suggestions.every((s) => s.kind === "schema" && s.retrigger)).toBe(true);
  });
  it("completes columns through aliases, qualified names and nested namespaces", () => {
    expect(labels(at("select o.| from shop.orders o", "athena"))).toEqual(expect.arrayContaining(["id", "customer_id", "total", "placed_at"]));
    expect(labels(at("select lake.sales.daily_revenue.| from lake.sales.daily_revenue", "athena"))).toEqual(["day", "revenue"]);
    expect(labels(at("select | from lake.sales.daily_revenue d", "athena"))).toEqual(expect.arrayContaining(["day", "revenue"]));
    const col = at("select | from shop.orders", "athena")!.suggestions.find((s) => s.label === "total")!;
    expect(col.detail).toBe("decimal(12, 2)");
  });
  it("completes columns of a table queried with time travel", () => {
    expect(labels(at('select | from shop."orders@4812730049218318"', "datafusion"))).toEqual(expect.arrayContaining(["id", "status"]));
  });
  it("does not leak Postgres schemas: there is no 'public' special case", () => {
    const meta: SchemaMetadata = { ...ICE, tables: [table("public", "pub_t", [col("id", "long")]), ...ICE.tables] };
    const r = complete(indexCatalog(meta, { dotted: true, defaultSchema: null }), "select * from ", 14, { dialect: "datafusion" });
    expect(r!.suggestions.find((s) => s.label === "pub_t")!.insertText).toBe("public.pub_t");
  });
});

describe("time travel and metadata hints", () => {
  it("recognises the marker after a table name", () => {
    expect(tableSuffixTarget('"orders@')).toEqual({ table: "orders", kind: "snapshot", typed: "" });
    expect(tableSuffixTarget('"orders@4812')).toEqual({ table: "orders", kind: "snapshot", typed: "4812" });
    expect(tableSuffixTarget('"orders$sn')).toEqual({ table: "orders", kind: "metadata", typed: "sn" });
    expect(tableSuffixTarget("orders")).toBeNull();
    expect(tableSuffixTarget('"@x')).toBeNull();
  });
  it("offers a snapshot snippet for the local engine", () => {
    const r = at('select * from shop."orders@|', "datafusion", dotted, "@");
    expect(r!.suggestions).toHaveLength(1);
    const s = r!.suggestions[0];
    expect(s.label).toBe("orders@<snapshot_id>");
    expect(s.isSnippet).toBe(true);
    expect(s.insertText).toBe('"orders@${1:snapshot_id}"');
    // The filter text is what the user typed, so the editor never filters the hint away.
    expect(s.filterText).toBe('"orders@');
  });
  it("keeps what was already typed after the @", () => {
    const s = at('select * from shop."orders@48|', "datafusion")!.suggestions[0];
    expect(s.insertText).toBe('"orders@${1:48}"');
  });
  it("works without a namespace prefix and for the default namespace", () => {
    expect(at('select * from "orders@|', "datafusion", withDefault)!.suggestions[0].label).toBe("orders@<snapshot_id>");
  });
  it("has no @ hint for unknown tables, for Athena or for Postgres", () => {
    expect(labels(at('select * from shop."nope@|', "datafusion"))).not.toContain("nope@<snapshot_id>");
    expect(labels(at('select * from shop."orders@|', "athena")).includes("orders@<snapshot_id>")).toBe(false);
    expect(labels(at('select * from shop."orders@|', "postgres", dotted)).includes("orders@<snapshot_id>")).toBe(false);
  });
  it("offers the metadata tables of each engine", () => {
    const df = labels(at('select * from shop."orders$|', "datafusion"));
    expect(df).toEqual(["orders$snapshots", "orders$manifests", "orders$history"]);
    const ath = at('select * from shop."orders$|', "athena")!.suggestions;
    expect(ath.map((s) => s.label)).toEqual(expect.arrayContaining(["orders$snapshots", "orders$history", "orders$files", "orders$partitions", "orders$refs"]));
    expect(ath[0].insertText).toBe('"orders$snapshots"');
  });
  it("offers FOR VERSION AS OF after a table on Athena", () => {
    const l = labels(at("select * from shop.orders fo|", "athena"));
    expect(l).toEqual(expect.arrayContaining(["FOR VERSION AS OF", "FOR TIMESTAMP AS OF"]));
    expect(labels(at("select * from shop.orders fo|", "datafusion"))).not.toContain("FOR VERSION AS OF");
  });
  it("documents time travel on the table's completion card", () => {
    const doc = (d: SqlDialect) => at("select * from |", d)!.suggestions.find((s) => s.label === "orders")!.documentation!;
    expect(doc("datafusion")).toContain('"orders@<snapshot_id>"');
    expect(doc("athena")).toContain("FOR VERSION AS OF");
    expect(doc("postgres")).not.toContain("Time travel");
  });
});

describe("AI prompts name the engine", () => {
  it("defaults to PostgreSQL", () => {
    expect(buildAskPrompt({ instruction: "count orders", target: null, buffer: "" })).toContain("Write a PostgreSQL query");
    expect(buildFixPrompt({ sql: "select 1", error: "x" })).toContain("This PostgreSQL statement failed");
  });
  it("targets DataFusion with its limits", () => {
    const p = buildAskPrompt({ instruction: "count orders", target: null, buffer: "", dialect: "datafusion" });
    expect(p).toContain("Apache DataFusion SQL");
    expect(p).toContain("UPDATE, DELETE and MERGE are not supported");
    expect(p).not.toContain("PostgreSQL");
  });
  it("targets Athena and warns about scan cost", () => {
    const p = buildFixPrompt({ sql: "select 1", error: "x", dialect: "athena" });
    expect(p).toContain("Amazon Athena (Trino) SQL");
    expect(p).toContain("FOR VERSION AS OF");
    expect(p).not.toContain("PostgreSQL");
  });
  it("digests nested namespaces", () => {
    const d = schemaDigest(ICE, "select * from lake.sales.daily_revenue r join shop.orders o on 1=1", 12, { dotted: true, defaultSchema: null });
    expect(d).toContain("lake.sales.daily_revenue(day date, revenue decimal(14, 2))");
    expect(d).toContain("shop.orders(");
  });
});
