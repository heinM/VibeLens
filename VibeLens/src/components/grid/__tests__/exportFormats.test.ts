import { describe, expect, it } from "vitest";
import { UTF8_BOM, suggestFileName, toCsv, toJson, toMarkdownTable, toSqlInserts } from "../exportFormats";
import type { Cell } from "../../../ipc/types";

const cols = [
  { name: "id", typeName: "int8" },
  { name: "name", typeName: "text" },
  { name: "total", typeName: "numeric" },
  { name: "meta", typeName: "jsonb" },
  { name: "blob", typeName: "bytea" },
];
const rows: Cell[][] = [
  ["9007199254740993", "Zoë, \"the\" boss", "12.50", { a: [1, 2] }, { $bytes: "3q2+7w==", len: 4 }],
  ["2", null, "0.10", null, null],
  ["3", "", "-5", { s: "it's" }, { $bytes: "", len: 0 }],
  ["4", "line1\nline2", "1e3", "x", null],
];

describe("toCsv", () => {
  it("quotes per RFC 4180 and uses CRLF", () => {
    const csv = toCsv(cols, rows);
    expect(csv.split("\r\n")).toEqual([
      "id,name,total,meta,blob",
      '9007199254740993,"Zoë, ""the"" boss",12.50,"{""a"":[1,2]}",\\xdeadbeef',
      "2,,0.10,,",
      '3,"",-5,"{""s"":""it\'s""}",\\x',
      '4,"line1\nline2",1e3,x,',
      "",
    ]);
  });
  it("tells NULL from an empty string", () => {
    const csv = toCsv([{ name: "a", typeName: "text" }], [[null], [""]], { header: false });
    expect(csv).toBe('\r\n""\r\n');
  });
  it("adds a BOM on request", () => {
    expect(toCsv(cols, [], { bom: true }).startsWith(UTF8_BOM + "id,")).toBe(true);
    expect(toCsv(cols, []).startsWith(UTF8_BOM)).toBe(false);
  });
  it("supports other delimiters", () => {
    expect(toCsv([{ name: "a;b", typeName: "text" }], [["x;y"]], { delimiter: ";" })).toBe('"a;b"\r\n"x;y"\r\n');
    expect(toCsv([{ name: "a", typeName: "text" }, { name: "b", typeName: "text" }], [["x,y", "z"]], { delimiter: "\t" })).toBe("a\tb\r\nx,y\tz\r\n");
  });
});

describe("toJson", () => {
  it("is an array of objects, keeping int8 / numeric as strings", () => {
    const parsed = JSON.parse(toJson(cols, rows));
    expect(parsed).toHaveLength(4);
    expect(parsed[0]).toEqual({ id: "9007199254740993", name: 'Zoë, "the" boss', total: "12.50", meta: { a: [1, 2] }, blob: "\\xdeadbeef" });
    expect(parsed[1].name).toBeNull();
  });
  it("de-duplicates repeated column names", () => {
    const parsed = JSON.parse(toJson([{ name: "x", typeName: "int4" }, { name: "x", typeName: "int4" }], [[1, 2]]));
    expect(parsed).toEqual([{ x: 1, x_2: 2 }]);
  });
  it("emits an empty array for no rows", () => {
    expect(JSON.parse(toJson(cols, []))).toEqual([]);
  });
});

describe("toSqlInserts", () => {
  it("writes typed literals without casts", () => {
    const sql = toSqlInserts(cols, rows.slice(0, 3), { schema: "public", name: "orders" });
    expect(sql).toBe(
      [
        `INSERT INTO "public"."orders" ("id", "name", "total", "meta", "blob") VALUES`,
        `  (9007199254740993, 'Zoë, "the" boss', 12.50, '{"a":[1,2]}', decode('deadbeef', 'hex')),`,
        `  (2, NULL, 0.10, NULL, NULL),`,
        `  (3, '', -5, '{"s":"it''s"}', decode('', 'hex'));`,
        "",
      ].join("\n"),
    );
  });
  it("escapes backslashes and quotes", () => {
    const sql = toSqlInserts([{ name: "p", typeName: "text" }], [["C:\\x'y"]], { name: "t" });
    expect(sql).toContain(`VALUES\n  (E'C:\\\\x''y');`);
  });
  it("batches rows", () => {
    const many: Cell[][] = Array.from({ length: 5 }, (_, i) => [String(i)]);
    const sql = toSqlInserts([{ name: "id", typeName: "int8" }], many, { name: "t" }, 2);
    expect(sql.match(/INSERT INTO/g)).toHaveLength(3);
  });
  it("renders arrays, NaN and booleans", () => {
    const sql = toSqlInserts(
      [{ name: "a", typeName: "_text" }, { name: "f", typeName: "float8" }, { name: "b", typeName: "bool" }],
      [[["x", "y z"], "NaN", true]],
      { name: "t" },
    );
    expect(sql).toContain(`('{"x","y z"}', 'NaN', TRUE)`);
  });
  it("uses a placeholder table with a comment when the source is unknown", () => {
    const sql = toSqlInserts([{ name: "a", typeName: "int4" }], [[1]], null);
    expect(sql.startsWith('-- Replace "table_name"')).toBe(true);
    expect(sql).toContain('INSERT INTO "table_name" ("a") VALUES\n  (1);');
  });
  it("handles zero rows", () => {
    expect(toSqlInserts(cols, [], { name: "t" })).toContain("-- No rows.");
  });
});

describe("toMarkdownTable", () => {
  it("escapes pipes and newlines and right-aligns numbers", () => {
    const md = toMarkdownTable(
      [{ name: "id", typeName: "int8" }, { name: "t|x", typeName: "text" }],
      [["1", "a|b\nc"], ["2", null]],
    );
    expect(md).toBe("| id | t\\|x |\n| ---: | --- |\n| 1 | a\\|b<br>c |\n| 2 | NULL |\n");
  });
});

describe("suggestFileName", () => {
  it("is safe and dated", () => {
    const d = new Date(2026, 9, 31);
    expect(suggestFileName("public.orders", "csv", d)).toBe("public.orders_2026-10-31.csv");
    expect(suggestFileName('we/ird:"na me"', "json", d)).toBe("we_ird_na_me_2026-10-31.json");
    expect(suggestFileName("", "sql", d)).toBe("results_2026-10-31.sql");
  });
});

describe("CSV formula injection", () => {
  it("defuses text that spreadsheets would evaluate, leaves numbers alone", async () => {
    const { defuseFormula, toCsv } = await import("../exportFormats");
    expect(defuseFormula("=HYPERLINK(\"http://x\")")).toBe("'=HYPERLINK(\"http://x\")");
    expect(defuseFormula("@SUM(A1)")).toBe("'@SUM(A1)");
    expect(defuseFormula("+cmd")).toBe("'+cmd");
    expect(defuseFormula("-5")).toBe("-5");
    expect(defuseFormula("+3.25e2")).toBe("+3.25e2");
    expect(defuseFormula("hello")).toBe("hello");
    const csv = toCsv([{ name: "a", typeName: "text" }, { name: "n", typeName: "int4" }], [["=1+1", -5]]);
    expect(csv).toBe("a,n\r\n'=1+1,-5\r\n");
    expect(toCsv([{ name: "a", typeName: "text" }], [["=1+1"]], { formulaSafe: false })).toBe("a\r\n=1+1\r\n");
  });
});
