// The MySQL editor dialect: its own vocabulary and catalog source (my_schema_metadata), backtick
// identifiers and `#` comments in the tokenizer, and backtick quoting of completed names.
import { describe, expect, it } from "vitest";
import type { ColumnInfo, SchemaMetadata, TableDescription } from "../../../ipc/types";
import { indexCatalog } from "../catalog";
import { complete } from "../complete";
import { DIALECT_AI_NOTE, DIALECT_ENGINE, DIALECT_LABEL, catalogEngineOf, dialectOf, functionsFor, hashComments, quoteIdentFor, statementStartFor, typeNamesFor } from "../dialect";
import { identValue, insideNonCode, tokenize } from "../tokenizer";
import { schemaMetadataKey } from "../../schemaCache";
import { newProfile } from "../../defaults";
import { planExplain } from "../../../components/editor/explainGuard";
import { cur, labels } from "./fixtures";

let ord = 0;
const col = (name: string, dataType: string): ColumnInfo => ({ name, dataType, nullable: true, default: null, isPrimaryKey: false, ordinal: ++ord, comment: null });
const table = (schema: string, name: string, columns: ColumnInfo[]): TableDescription => ({ schema, name, kind: "table", comment: null, estimatedRows: null, columns, indexes: [], foreignKeys: [] });
const META: SchemaMetadata = {
  database: "shop",
  serverVersion: "8.0.40",
  truncated: false,
  tables: [table("shop", "orders", [col("id", "int"), col("order total", "decimal(12,2)")]), table("shop", "select", [col("id", "int")])],
};
const CAT = indexCatalog(META, { defaultSchema: "shop" });
const at = (src: string) => {
  const { text, offset } = cur(src);
  return complete(CAT, text, offset, { dialect: "mysql" });
};

describe("MySQL dialect", () => {
  it("is chosen from the profile and reads its own catalog", () => {
    expect(dialectOf(newProfile("mysql"))).toBe("mysql");
    expect(catalogEngineOf("mysql")).toBe("mysql");
    expect(schemaMetadataKey("p1", "shop", catalogEngineOf("mysql"))).toEqual(["my", "p1", "shop", "schema-metadata"]);
    expect(DIALECT_ENGINE.mysql).toBe("mysql");
    expect(DIALECT_LABEL.mysql).toMatch(/MySQL/);
    expect(DIALECT_AI_NOTE.mysql).toMatch(/backticks/);
    expect(hashComments("mysql")).toBe(true);
    expect(hashComments("postgres")).toBe(false);
  });
  it("offers MySQL functions and types, not Postgres-only ones", () => {
    const fns = functionsFor("mysql").map((f) => f.name);
    expect(fns).toEqual(expect.arrayContaining(["group_concat", "ifnull", "date_format", "json_extract", "now"]));
    expect(fns).not.toEqual(expect.arrayContaining(["string_agg"]));
    expect(fns).not.toContain("generate_series");
    expect(new Set(fns).size).toBe(fns.length);
    expect(typeNamesFor("mysql")).toEqual(expect.arrayContaining(["varchar(255)", "json", "datetime(6)"]));
    const starts = statementStartFor("mysql");
    expect(starts).toEqual(expect.arrayContaining(["SHOW TABLES", "REPLACE INTO", "START TRANSACTION"]));
    expect(starts).not.toContain("VACUUM");
    expect(starts).not.toContain("COPY");
  });
  it("quotes with backticks, doubling a backtick", () => {
    expect(quoteIdentFor("order total", "mysql")).toBe("`order total`");
    expect(quoteIdentFor("a`b", "mysql")).toBe("`a``b`");
    expect(quoteIdentFor("x", "postgres")).toBe('"x"');
  });
  it("completes tables and columns with backticks where a name needs quoting", () => {
    const t = at("SELECT * FROM |");
    expect(labels(t)).toEqual(expect.arrayContaining(["orders", "select"]));
    const sel = t?.suggestions.find((s) => s.label === "select");
    expect(sel?.insertText).toBe("`select`");
    const c = at("SELECT o.| FROM orders o");
    const tot = c?.suggestions.find((s) => s.label === "order total");
    expect(tot?.insertText).toBe("`order total`");
  });
  it("offers no completion inside a # comment", () => {
    expect(at("SELECT * FROM orders # where |")).toBeNull();
  });
  it("has no plan view: Explain is refused with advice", () => {
    expect(planExplain("SELECT 1", "plan", "mysql")).toMatchObject({ ok: false });
    expect(planExplain("SELECT 1", "analyze", "mysql")).toMatchObject({ ok: false });
  });
});

describe("tokenizer: backticks and # comments", () => {
  it("reads `quoted` identifiers in MySQL, with doubled backticks", () => {
    const t = tokenize("SELECT `a``b`, `x y` FROM t", { mysql: true });
    const q = t.filter((x) => x.kind === "quoted");
    expect(q.map(identValue)).toEqual(["a`b", "x y"]);
    expect(tokenize("`open", { mysql: true }).at(-1)).toMatchObject({ kind: "quoted", closed: false });
    // Other dialects tokenize exactly as before.
    expect(tokenize("`a`").some((x) => x.kind === "quoted")).toBe(false);
  });
  it("treats # as a comment only when asked (it is an operator in Postgres)", () => {
    expect(tokenize("a # b").map((x) => x.kind)).toEqual(["word", "punct", "word"]);
    const my = tokenize("a # b\nc", { mysql: true });
    expect(my.map((x) => x.kind)).toEqual(["word", "comment", "word"]);
    expect(insideNonCode(my, 5)).not.toBeNull();
  });
});
