// Editor dialects of the new engines: Trino (same vocabulary as Athena), Spark SQL (its own small
// table) and Redshift (Postgres vocabulary, its own catalog source). Plus the Iceberg SQL text helpers
// that must follow the engine (Spark quotes with backticks; time travel per engine).
import { describe, expect, it } from "vitest";
import type { ColumnInfo, ConnectionProfile, IcebergQueryEngine, SchemaMetadata, TableDescription } from "../../../ipc/types";
import { buildAskPrompt } from "../aiPrompts";
import { indexCatalog } from "../catalog";
import { complete } from "../complete";
import { DIALECT_AI_NOTE, DIALECT_ENGINE, DIALECT_LABEL, afterRelationExtras, catalogEngineOf, dialectOf, functionsFor, isIcebergDialect, snippetsFor, statementStartFor, typeNamesFor, type SqlDialect } from "../dialect";
import { schemaMetadataKey } from "../../schemaCache";
import { newProfile } from "../../defaults";
import { planExplain } from "../../../components/editor/explainGuard";
import { qualifiedName, selectAllSql, selectAsOfSnapshotSql } from "../../../components/iceberg/iceSql";
import { cur, labels } from "./fixtures";

const withEngine = (queryEngine: IcebergQueryEngine): ConnectionProfile => {
  const p = newProfile("iceberg");
  return { ...p, iceberg: { ...p.iceberg!, queryEngine } };
};
const TRINO: IcebergQueryEngine = { kind: "trino", url: "https://t", catalog: "iceberg", user: "u", auth: { kind: "none" }, caCertPath: null };
const SPARK: IcebergQueryEngine = { kind: "spark", url: "https://l", catalog: "lake", auth: { kind: "none" }, caCertPath: null, proxyUser: null, sessionConf: {}, sessionStartTimeoutSecs: null };

let ord = 0;
const col = (name: string, dataType: string): ColumnInfo => ({ name, dataType, nullable: true, default: null, isPrimaryKey: false, ordinal: ++ord, comment: null });
const table = (schema: string, name: string, columns: ColumnInfo[]): TableDescription => ({ schema, name, kind: "table", comment: null, estimatedRows: null, columns, indexes: [], foreignKeys: [] });
const META: SchemaMetadata = { database: "x", serverVersion: null, truncated: false, tables: [table("shop", "orders", [col("id", "long"), col("total", "decimal(12, 2)")])] };
const dotted = indexCatalog(META, { dotted: true, defaultSchema: null });
const at = (src: string, dialect: SqlDialect) => {
  const { text, offset } = cur(src);
  return complete(dotted, text, offset, { dialect });
};

describe("dialect of the new engines", () => {
  it("is chosen from the profile", () => {
    expect(dialectOf(withEngine(TRINO))).toBe("trino");
    expect(dialectOf(withEngine(SPARK))).toBe("spark");
    expect(dialectOf(newProfile("redshift"))).toBe("redshift");
  });
  it("Trino and Spark read the Iceberg catalog; Redshift reads its own (never the Postgres one)", () => {
    expect(isIcebergDialect("trino")).toBe(true);
    expect(isIcebergDialect("spark")).toBe(true);
    expect(isIcebergDialect("redshift")).toBe(false);
    expect(catalogEngineOf("trino")).toBe("iceberg");
    expect(catalogEngineOf("spark")).toBe("iceberg");
    expect(catalogEngineOf("redshift")).toBe("redshift");
    expect(schemaMetadataKey("p1", "dev", catalogEngineOf("redshift"))).toEqual(["rs", "p1", "dev", "schema-metadata"]);
  });
  it("names each engine as the stats event does", () => {
    expect(DIALECT_ENGINE).toMatchObject({ trino: "trino", spark: "spark", redshift: "redshift", athena: "athena", datafusion: "datafusion" });
    for (const d of ["trino", "spark", "redshift"] as const) {
      expect(DIALECT_LABEL[d]).toBeTruthy();
      expect(DIALECT_AI_NOTE[d]).toBeTruthy();
    }
  });
});

describe("Trino completes like Athena", () => {
  it("has the Athena function, type and snippet vocabulary", () => {
    expect(functionsFor("trino")).toEqual(functionsFor("athena"));
    expect(typeNamesFor("trino")).toEqual(typeNamesFor("athena"));
    expect(snippetsFor("trino")).toEqual(snippetsFor("athena"));
    expect(afterRelationExtras("trino")).toEqual(["FOR VERSION AS OF", "FOR TIMESTAMP AS OF"]);
  });
  it("offers the $ metadata tables and FOR VERSION AS OF", () => {
    expect(labels(at('select * from shop."orders$|', "trino"))).toEqual(expect.arrayContaining(["orders$snapshots", "orders$history"]));
    expect(labels(at("select * from shop.orders fo|", "trino"))).toEqual(expect.arrayContaining(["FOR VERSION AS OF"]));
  });
  it("does not offer statements the Trino engine refuses as session state (USE is not a start keyword)", () => {
    expect(statementStartFor("trino")).toContain("MERGE INTO");
    expect(statementStartFor("trino")).not.toContain("BEGIN");
  });
});

describe("Spark SQL", () => {
  it("has its own functions and types, without Postgres-only names", () => {
    const names = functionsFor("spark").map((f) => f.name);
    expect(names).toEqual(expect.arrayContaining(["collect_list", "approx_count_distinct", "date_format", "get_json_object"]));
    expect(names).not.toContain("string_agg");
    expect(names).not.toContain("jsonb_agg");
    expect(typeNamesFor("spark")).toContain("string");
  });
  it("time-travels with VERSION AS OF and has no quoted $ metadata tables", () => {
    expect(afterRelationExtras("spark")).toEqual(["VERSION AS OF", "TIMESTAMP AS OF"]);
    expect(labels(at('select * from shop."orders$|', "spark"))).not.toContain("orders$snapshots");
    expect(snippetsFor("spark").map((s) => s.label)).toEqual(expect.arrayContaining(["selver", "selmeta"]));
  });
});

describe("Redshift completes with the Postgres vocabulary", () => {
  it("uses the Postgres functions, types, statements and snippets", () => {
    expect(functionsFor("redshift")).toEqual(functionsFor("postgres"));
    expect(typeNamesFor("redshift")).toEqual(typeNamesFor("postgres"));
    expect(statementStartFor("redshift")).toEqual(statementStartFor("postgres"));
    expect(snippetsFor("redshift")).toEqual(snippetsFor("postgres"));
    expect(afterRelationExtras("redshift")).toEqual([]);
  });
  it("tells the assistant it writes Redshift SQL", () => {
    const p = buildAskPrompt({ instruction: "count orders", target: null, buffer: "", digest: "", dialect: "redshift" });
    expect(p).toContain("Amazon Redshift SQL");
    expect(p).toMatch(/commits on its own/);
  });
});

describe("Explain per engine", () => {
  it("sends a plain EXPLAIN for Trino and Spark (text plan), refuses Analyze", () => {
    expect(planExplain("select 1", "plan", "trino")).toMatchObject({ ok: true, text: true, sql: "EXPLAIN select 1" });
    expect(planExplain("select 1", "plan", "spark")).toMatchObject({ ok: true, text: true });
    expect(planExplain("select 1", "analyze", "trino").ok).toBe(false);
  });
  it("is not offered on Redshift (no Postgres JSON options are sent there)", () => {
    expect(planExplain("select 1", "plan", "redshift")).toMatchObject({ ok: false, message: expect.stringMatching(/Redshift/) });
    expect(planExplain("select 1", "analyze", "redshift").ok).toBe(false);
  });
});

describe("Iceberg SQL text per engine", () => {
  it("quotes with backticks for Spark and double quotes elsewhere", () => {
    expect(qualifiedName(["lake", "Sales"], "orders", "spark")).toBe("lake.`Sales`.orders");
    expect(qualifiedName(["lake"], "we`ird", "spark")).toBe("lake.`we``ird`");
    expect(qualifiedName(["lake", "Sales"], "orders", "trino")).toBe('lake."Sales".orders');
    expect(qualifiedName(["lake", "Sales"], "orders")).toBe('lake."Sales".orders');
    expect(selectAllSql(["shop"], "Order", 10, "spark")).toBe("SELECT * FROM shop.`Order` LIMIT 10;");
  });
  it("writes time travel in each engine's syntax", () => {
    expect(selectAsOfSnapshotSql(["shop"], "orders", "123", 100)).toBe('SELECT * FROM "shop"."orders@123" LIMIT 100;');
    expect(selectAsOfSnapshotSql(["shop"], "orders", "123", 100, "athena")).toBe("SELECT * FROM shop.orders FOR VERSION AS OF 123 LIMIT 100;");
    expect(selectAsOfSnapshotSql(["shop"], "orders", "123", 100, "trino")).toBe("SELECT * FROM shop.orders FOR VERSION AS OF 123 LIMIT 100;");
    expect(selectAsOfSnapshotSql(["shop"], "orders", "123", 100, "spark")).toBe("SELECT * FROM shop.orders VERSION AS OF 123 LIMIT 100;");
  });
  it("never splices a non-numeric snapshot id into VERSION AS OF", () => {
    expect(selectAsOfSnapshotSql(["shop"], "orders", "1; drop table x", 100, "trino")).toBe('SELECT * FROM "shop"."orders@1; drop table x" LIMIT 100;');
  });
});
