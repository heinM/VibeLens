import { describe, expect, it } from "vitest";
import { buildAskPrompt, buildFixPrompt, partialSqlBlock, schemaDigest, toolProgress } from "../aiPrompts";
import { diffLines, hunksOf } from "../diff";
import { formatSql, mapOffsetAcrossFormat } from "../format";
import { META } from "./fixtures";

describe("formatSql", () => {
  it("uppercases keywords and breaks clauses onto lines", () => {
    const out = formatSql("select a,b from t where x=1 order by a");
    expect(out).toBe("SELECT\n  a,\n  b\nFROM\n  t\nWHERE\n  x = 1\nORDER BY\n  a");
  });
  it("keeps a trailing newline and is idempotent", () => {
    const once = formatSql("select 1\n");
    expect(once.endsWith("\n")).toBe(true);
    expect(formatSql(once)).toBe(once);
  });
  it("handles postgres syntax: casts, jsonb operators, params", () => {
    const out = formatSql("select data->>'a'::text, $1 from t where j @> '{}'::jsonb");
    expect(out).toContain("->>");
    expect(out).toContain("$1");
    expect(out).toContain("::jsonb");
  });
  it("formats multiple statements separated by a blank line", () => {
    expect(formatSql("select 1; select 2;")).toContain(";\n\nSELECT");
  });
  it("returns whitespace-only input unchanged", () => {
    expect(formatSql("  \n ")).toBe("  \n ");
  });
});

describe("mapOffsetAcrossFormat", () => {
  it("keeps the cursor on the same token", () => {
    const before = "select a,b from t";
    const after = formatSql(before);
    const off = before.indexOf("b");
    const mapped = mapOffsetAcrossFormat(before, off + 1, after);
    expect(after.slice(0, mapped).endsWith("b")).toBe(true);
    expect(mapOffsetAcrossFormat(before, 0, after)).toBe(0);
    expect(mapOffsetAcrossFormat(before, before.length, after)).toBe(after.length);
  });
});

describe("diffLines / hunksOf", () => {
  it("marks an inserted WHERE as pure additions", () => {
    const ops = diffLines(["SELECT *", "FROM orders;"], ["SELECT *", "FROM orders", "WHERE status = 'paid';"]);
    const h = hunksOf(ops);
    expect(h.removed).toEqual([1]);
    expect(h.added).toEqual([{ afterOld: 1, lines: ["FROM orders", "WHERE status = 'paid';"] }]);
  });
  it("identical text has no hunks", () => {
    const h = hunksOf(diffLines(["a", "b"], ["a", "b"]));
    expect(h.removed).toEqual([]);
    expect(h.added).toEqual([]);
  });
  it("replacement of everything", () => {
    const h = hunksOf(diffLines(["x"], ["y", "z"]));
    expect(h.removed).toEqual([0]);
    expect(h.added).toEqual([{ afterOld: 0, lines: ["y", "z"] }]);
  });
  it("addition at the top", () => {
    const h = hunksOf(diffLines(["b"], ["a", "b"]));
    expect(h.added).toEqual([{ afterOld: -1, lines: ["a"] }]);
    expect(h.removed).toEqual([]);
  });
  it("middle edits keep surrounding lines", () => {
    const h = hunksOf(diffLines(["a", "b", "c"], ["a", "B", "c"]));
    expect(h.removed).toEqual([1]);
    expect(h.added).toEqual([{ afterOld: 1, lines: ["B"] }]);
  });
  it("empty old text", () => {
    const h = hunksOf(diffLines([], ["a"]));
    expect(h.added).toEqual([{ afterOld: -1, lines: ["a"] }]);
  });
});

describe("AI prompts", () => {
  it("ask (replace) includes the instruction, the selection, and the single-block rule", () => {
    const p = buildAskPrompt({ instruction: "add a WHERE for paid orders", target: "SELECT * FROM orders", buffer: "SELECT * FROM orders;\nSELECT 2;" });
    expect(p).toContain("add a WHERE for paid orders");
    expect(p).toContain("SELECT * FROM orders");
    expect(p).toContain("exactly ONE ```sql");
    expect(p).toContain("worksheet currently contains");
  });
  it("ask (insert) asks for new SQL and omits an empty buffer", () => {
    const p = buildAskPrompt({ instruction: "orders per customer last 30 days", target: null, buffer: "  " });
    expect(p).toContain("Write a PostgreSQL query");
    expect(p).not.toContain("worksheet currently contains");
    expect(p).toContain("Look up the schema");
  });
  it("truncates a huge buffer", () => {
    const p = buildAskPrompt({ instruction: "x", target: null, buffer: "select 1;\n".repeat(5000) });
    expect(p.length).toBeLessThan(9000);
    expect(p).toContain("truncated");
  });
  it("fix includes statement and error", () => {
    const p = buildFixPrompt({ sql: "select cstomer_id from orders", error: 'column "cstomer_id" does not exist' });
    expect(p).toContain("cstomer_id from orders");
    expect(p).toContain('column "cstomer_id" does not exist');
  });
  it("schema digest covers mentioned tables only", () => {
    const d = schemaDigest(META, "select * from shop.orders o join customers c on 1=1");
    expect(d).toContain("shop.orders(");
    expect(d).toContain("customer_id int8");
    expect(d).toContain("-> customers(id)");
    expect(d).toContain("shop.customers(");
    expect(d).not.toContain("products");
    expect(schemaDigest(undefined, "select 1")).toBe("");
  });
  it("schema digest honours schema qualification", () => {
    const d = schemaDigest(META, "select * from shop.orders");
    expect(d).toContain("shop.orders(");
    expect(d).not.toContain("public.orders");
  });
  it("extracts a streaming sql block", () => {
    expect(partialSqlBlock("Here you go\n```sql\nSELECT 1")).toBe("SELECT 1");
    expect(partialSqlBlock("```sql\nSELECT 1;\n```\nDone")).toBe("SELECT 1;");
    expect(partialSqlBlock("no block yet")).toBeNull();
    expect(partialSqlBlock("```sql\n")).toBeNull();
  });
  it("describes tool calls", () => {
    expect(toolProgress("get_schema", {})).toBe("Reading the schema");
    expect(toolProgress("describe_table", { table: "orders" })).toBe("Looking at orders");
    expect(toolProgress("odd_tool", null)).toBe("Using odd tool");
  });
});
