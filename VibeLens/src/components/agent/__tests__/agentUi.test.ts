import { describe, expect, it } from "vitest";
import { parseBlocks, parseInline } from "../markdownParser";
import { tokenizeSql } from "../sqlHighlight";
import { describeTarget, mentionsProd, previewKind, summarizeToolArgs, formatElapsed } from "../toolSummary";
import type { ApprovalTarget } from "../../../ipc/types";
import { buildAgentContext, resolveTarget, NO_EXCLUSIONS } from "../../../hooks/useAgentContext";

describe("markdown tables", () => {
  it("parses a table with alignment and inline formatting", () => {
    const b = parseBlocks("Intro\n\n| Fix | Cost |\n|:--|--:|\n| `idx` | **small** |\n| x \\| y | none |\n\nAfter");
    expect(b.map((x) => x.t)).toEqual(["p", "table", "p"]);
    const t = b[1];
    if (t.t !== "table") throw new Error("not a table");
    expect(t.head).toHaveLength(2);
    expect(t.align).toEqual(["left", "right"]);
    expect(t.rows).toHaveLength(2);
    expect(t.rows[0][0][0]).toEqual({ t: "code", v: "idx" });
    expect(t.rows[1][0]).toEqual([{ t: "text", v: "x | y" }]);
  });
  it("leaves a lone pipe line as a paragraph", () => {
    expect(parseBlocks("a | b").map((x) => x.t)).toEqual(["p"]);
  });
  it("pads short rows and survives a missing trailing pipe", () => {
    const b = parseBlocks("| a | b |\n| - | - |\n| 1 |");
    const t = b[0];
    expect(t.t === "table" && t.rows[0]).toHaveLength(2);
  });
});

describe("markdown", () => {
  it("parses paragraphs, lists and headings", () => {
    const b = parseBlocks("# Title\n\nSome **bold** and `code`.\n\n- one\n- two\n\n1. a\n2. b");
    expect(b.map((x) => x.t)).toEqual(["h", "p", "ul", "ol"]);
    const ul = b[2];
    expect(ul.t === "ul" && ul.items.length).toBe(2);
  });

  it("parses fenced code with a language and keeps content verbatim", () => {
    const b = parseBlocks("Here:\n\n```sql\nselect *\n  from t;\n```\nAfter");
    expect(b[1]).toEqual({ t: "code", lang: "sql", code: "select *\n  from t;", open: false });
    expect(b[2].t).toBe("p");
  });

  it("treats an unterminated fence (mid-stream) as an open code block", () => {
    const b = parseBlocks("```sql\nselect 1");
    expect(b).toEqual([{ t: "code", lang: "sql", code: "select 1", open: true }]);
  });

  it("does not interpret markdown inside code blocks", () => {
    const b = parseBlocks("```\n**not bold**\n- not a list\n```");
    expect(b).toHaveLength(1);
    expect(b[0]).toMatchObject({ t: "code", code: "**not bold**\n- not a list" });
  });

  it("parses inline styles and only allows http(s) links", () => {
    expect(parseInline("a `b` **c** *d*").map((n) => n.t)).toEqual(["text", "code", "text", "bold", "text", "italic"]);
    expect(parseInline("[ok](https://example.com)")[0]).toMatchObject({ t: "link", href: "https://example.com" });
    expect(parseInline("[bad](javascript:alert(1))").some((n) => n.t === "link")).toBe(false);
  });

  it("keeps snake_case identifiers intact", () => {
    const n = parseInline("use user_id and order_items table");
    expect(n).toEqual([{ t: "text", v: "use user_id and order_items table" }]);
  });
});

describe("sql tokenizer", () => {
  it("round-trips the input exactly", () => {
    const sql = "SELECT id, 'it''s' AS s, 42 -- note\nFROM \"Odd Table\" /* c */ WHERE x = $1;";
    expect(tokenizeSql(sql).map((t) => t.v).join("")).toBe(sql);
  });
  it("classifies keywords, strings, numbers and comments", () => {
    const t = tokenizeSql("select 'a', 1 -- hi");
    expect(t.find((x) => x.v === "select")?.k).toBe("kw");
    expect(t.find((x) => x.v === "'a'")?.k).toBe("str");
    expect(t.find((x) => x.v === "1")?.k).toBe("num");
    expect(t.find((x) => x.v.startsWith("--"))?.k).toBe("com");
  });
});

describe("tool summaries", () => {
  it("summarises known tools and falls back to JSON", () => {
    expect(summarizeToolArgs("run_sql", { sql: "select  1\nfrom t" })).toBe("select 1 from t");
    expect(summarizeToolArgs("describe_table", { schema: "public", table: "orders" })).toBe("public.orders");
    expect(summarizeToolArgs("valkey_command", { argv: ["GET", "k"] })).toBe("GET k");
    expect(summarizeToolArgs("mystery", { a: 1 })).toBe('{"a":1}');
    expect(summarizeToolArgs("run_sql", null)).toBe("null");
  });
  it("detects production and preview kinds", () => {
    expect(mentionsProd("Runs against PRODUCTION")).toBe(true);
    expect(mentionsProd("a product table")).toBe(false);
    expect(previewKind("run_sql", "select 1")).toBe("sql");
    expect(previewKind("write_file", "--- a\n+++ b")).toBe("diff");
    expect(previewKind("other", "plain")).toBe("text");
    expect(previewKind("run_sql", null)).toBe("text");
  });
  it("formats elapsed time", () => {
    expect(formatElapsed(450)).toBe("450 ms");
    expect(formatElapsed(2300)).toBe("2.3 s");
    expect(formatElapsed(null)).toBe("");
  });
});

describe("approval target (review finding M3)", () => {
  const pg = (database: string, defaultDatabase: string): ApprovalTarget => ({ connection: "orders-db", engine: "postgres", environment: "dev", database, defaultDatabase, valkeyDb: null });

  it("names the connection, environment and database a statement runs against", () => {
    expect(describeTarget(pg("shop", "shop"))).toEqual({ connection: "orders-db", environment: "dev", where: "database shop", warning: null });
  });
  it("says so in plain words when the database is not the connection's default", () => {
    const t = describeTarget(pg("payments_prod", "shop"));
    expect(t?.where).toBe("database payments_prod");
    expect(t?.warning).toBe("Not this connection's default database (shop).");
  });
  it("shows the Valkey logical database number", () => {
    expect(describeTarget({ connection: "cache", engine: "valkey", environment: "prod", database: null, defaultDatabase: null, valkeyDb: 3 })).toEqual({
      connection: "cache",
      environment: "prod",
      where: "db 3",
      warning: null,
    });
  });
  it("has nothing to add for Iceberg and nothing at all without a target", () => {
    expect(describeTarget({ connection: "lake", engine: "iceberg", environment: "test", database: null, defaultDatabase: null, valkeyDb: null })?.where).toBeNull();
    expect(describeTarget(null)).toBeNull();
    expect(describeTarget(undefined)).toBeNull();
  });
});

describe("agent context", () => {
  const tab = { path: null, title: "scratch.sql", contents: "select 1", selectedText: "1" };
  it("prefers the active tab's connection over the sidebar selection", () => {
    expect(resolveTarget({ profileId: "a", database: "d", kind: "sql" }, "b", "e")).toEqual({ profileId: "a", database: "d" });
    expect(resolveTarget({ profileId: null, database: null, kind: "sql" }, "b", "e")).toEqual({ profileId: "b", database: "e" });
    expect(resolveTarget(null, "b", null)).toEqual({ profileId: "b", database: null });
  });
  it("includes everything by default", () => {
    expect(buildAgentContext({ profileId: "a", database: "d" }, tab, NO_EXCLUSIONS)).toEqual({
      profileId: "a",
      database: "d",
      openFile: { path: "scratch.sql", contents: "select 1" },
      selectedText: "1",
    });
  });
  it("honours removed chips", () => {
    expect(buildAgentContext({ profileId: "a", database: "d" }, tab, { connection: true, file: true, selection: true })).toEqual({
      profileId: null,
      database: null,
      openFile: null,
      selectedText: null,
    });
    expect(buildAgentContext({ profileId: "a", database: "d" }, { ...tab, selectedText: null }, NO_EXCLUSIONS).selectedText).toBeNull();
  });
});
