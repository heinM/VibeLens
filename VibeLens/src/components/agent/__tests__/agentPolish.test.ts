import { describe, expect, it } from "vitest";
import { applyInsertion, computeInsertion } from "../editorInsert";
import { attachBlockedReason, canAttachResults, formatLastResult, LAST_RESULT_ROWS, withLastResult, type LastResultSnapshot } from "../lastResult";
import { suggestionsFor } from "../suggestions";

const ins = (text: string, start: number, end: number, sql: string) => {
  const i = computeInsertion(text, start, end, sql);
  return { i, out: applyInsertion(text, i) };
};

describe("computeInsertion", () => {
  it("replaces the selection exactly", () => {
    const text = "select 1;\nselect old;\nselect 3;";
    const start = text.indexOf("select old;");
    const { out, i } = ins(text, start, start + "select old;".length, "select new;\n");
    expect(out).toBe("select 1;\nselect new;\nselect 3;");
    expect(out.slice(i.selectFrom, i.selectTo)).toBe("select new;");
  });

  it("fills an empty editor without padding", () => {
    const { out, i } = ins("", 0, 0, "select 1;");
    expect(out).toBe("select 1;\n");
    expect(out.slice(i.selectFrom, i.selectTo)).toBe("select 1;");
  });

  it("starts a new block after existing statements", () => {
    const text = "select 1;";
    const { out } = ins(text, text.length, text.length, "select 2;");
    expect(out).toBe("select 1;\n\nselect 2;\n");
  });

  it("does not add extra blank lines when they are already there", () => {
    const text = "select 1;\n\n";
    expect(ins(text, text.length, text.length, "select 2;").out).toBe("select 1;\n\nselect 2;\n");
    const t2 = "select 1;\n";
    expect(ins(t2, t2.length, t2.length, "select 2;").out).toBe("select 1;\n\nselect 2;\n");
  });

  it("splits cleanly when the cursor is between statements", () => {
    const text = "select 1;\nselect 3;";
    const at = "select 1;\n".length;
    const { out } = ins(text, at, at, "select 2;");
    expect(out).toBe("select 1;\n\nselect 2;\n\nselect 3;");
  });

  it("accepts reversed offsets and clamps out-of-range ones", () => {
    expect(ins("abc", 3, 1, "X").out).toBe("aX");
    expect(ins("abc", 99, 99, "X").out).toBe("abc\n\nX\n");
  });
});

const snap = (rows: number, extra: Partial<LastResultSnapshot> = {}): LastResultSnapshot => ({
  columns: [
    { name: "id", typeName: "int8", typeOid: 20 },
    { name: "note|x", typeName: "text", typeOid: 25 },
  ],
  rows: Array.from({ length: rows }, (_, i) => [i, i % 2 ? null : `row ${i}\nwith | pipe`]),
  rowCount: 1500,
  sql: "select id, note from t",
  profileId: "p1",
  database: "db",
  at: 0,
  ...extra,
});

describe("last result chip", () => {
  it("is allowed only when results may leave the machine", () => {
    expect(canAttachResults("results")).toBe(true);
    expect(canAttachResults("resultsWithApproval")).toBe(true);
    expect(canAttachResults("schemaOnly")).toBe(false);
    expect(canAttachResults("none")).toBe(false);
    expect(canAttachResults(undefined)).toBe(false);
    expect(attachBlockedReason("schemaOnly")).toMatch(/schema only/);
    expect(attachBlockedReason("none")).toMatch(/off/);
    expect(attachBlockedReason("results")).toBeNull();
  });

  it("renders a compact table of the first 20 rows, escaping pipes and newlines", () => {
    const text = formatLastResult(snap(30));
    const lines = text.split("\n");
    expect(lines[0]).toBe("The result of my last query (20 of 1,500 rows, 2 columns):");
    const table = lines.filter((l) => l.startsWith("|"));
    expect(table).toHaveLength(2 + LAST_RESULT_ROWS);
    expect(table[0]).toBe("| id | note\\|x |");
    expect(table[2]).toBe("| 0 | row 0 with \\| pipe |");
    expect(table[3]).toBe("| 1 | NULL |");
    expect(text).toContain("select id, note from t");
  });

  it("describes bytes and long values without dumping them", () => {
    const s = snap(1, { rows: [[{ $bytes: "AAEC", len: 3 }, "x".repeat(500)]] });
    const text = formatLastResult(s);
    expect(text).toContain("<3 bytes>");
    expect(text).not.toContain("x".repeat(100));
  });

  it("leaves the prompt alone when nothing is attached", () => {
    expect(withLastResult("  why is this slow?  ", null)).toBe("why is this slow?");
    expect(withLastResult("why?", snap(2))).toMatch(/^why\?\n\n---\nThe result of my last query/);
  });
});

describe("suggestions", () => {
  const base = { connectionName: "Shop", database: "shop", access: "results" as const, hasEditorText: false };

  it("offers Postgres starters named for the database", () => {
    const s = suggestionsFor({ ...base, engine: "postgres" });
    expect(s.length).toBeGreaterThanOrEqual(4);
    expect(s.length).toBeLessThanOrEqual(6);
    expect(s[0].title).toBe("Map out the shop schema");
    expect(s.some((x) => /pg_stat_statements/.test(x.title))).toBe(true);
    expect(s.some((x) => x.title === "Write a migration…" && x.incomplete)).toBe(true);
  });

  it("swaps the query starter for a review when the editor has SQL", () => {
    expect(suggestionsFor({ ...base, engine: "postgres", hasEditorText: true }).some((x) => x.id === "pg-review")).toBe(true);
    expect(suggestionsFor({ ...base, engine: "postgres" }).some((x) => x.id === "pg-review")).toBe(false);
  });

  it("disables starters that need rows when AI access is schema-only", () => {
    const s = suggestionsFor({ ...base, engine: "postgres", access: "schemaOnly" });
    expect(s.find((x) => x.id === "pg-slow")?.disabledReason).toMatch(/results access/);
    expect(s.find((x) => x.id === "pg-schema")?.disabledReason).toBeNull();
  });

  it("offers keyspace starters for Valkey", () => {
    const s = suggestionsFor({ ...base, engine: "valkey", database: null });
    expect(s[0].title).toBe("Summarise the keyspace");
    expect(s.length).toBeGreaterThanOrEqual(4);
  });

  it("has a fallback with no connection", () => {
    expect(suggestionsFor({ ...base, engine: null }).length).toBe(1);
  });
});
