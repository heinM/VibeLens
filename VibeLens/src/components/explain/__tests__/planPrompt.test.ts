import { describe, expect, it } from "vitest";
import { buildInsights } from "../planInsights";
import { parsePlan } from "../planModel";
import { buildExplainPrompt, compactPlanJson, stripExplainPrefix } from "../planPrompt";
import { formatBlocks, formatMs, formatRows } from "../planFormat";
import { HASH_JOIN_MISESTIMATE, PARALLEL_SEQ_SCAN } from "./fixtures";

const wide = (n: number) => {
  const leaf = (i: number) => ({
    "Node Type": "Index Scan",
    "Parent Relationship": "Member",
    "Relation Name": `part_${i}`,
    Schema: "public",
    Alias: `part_${i}`,
    "Startup Cost": 0.4,
    "Total Cost": 8 + i,
    "Plan Rows": 5,
    "Plan Width": 8,
    "Actual Startup Time": 0.01,
    "Actual Total Time": 0.02 + (i === 7 ? 40 : 0),
    "Actual Rows": 5,
    "Actual Loops": 1,
    "Index Cond": `(tenant_id = ${i})`,
  });
  return [
    {
      Plan: {
        "Node Type": "Append",
        "Startup Cost": 0,
        "Total Cost": 5000,
        "Plan Rows": n * 5,
        "Actual Total Time": 41,
        "Actual Rows": n * 5,
        "Actual Loops": 1,
        Plans: Array.from({ length: n }, (_, i) => leaf(i)),
      },
    },
  ];
};

describe("compactPlanJson", () => {
  it("keeps small plans whole and parseable", () => {
    const p = parsePlan(PARALLEL_SEQ_SCAN);
    if (!p) throw new Error();
    const j = JSON.parse(compactPlanJson(p));
    expect(j.node).toBe("Aggregate");
    expect(j.children[0].node).toBe("Gather");
    expect(j.children[0]["Workers Launched"]).toBe(2);
  });

  it("truncates big plans to the budget, keeping the hot node and saying what was dropped", () => {
    const p = parsePlan(wide(300));
    if (!p) throw new Error();
    const full = JSON.stringify(compactPlanJson(p, 1e9)).length;
    const text = compactPlanJson(p, 6000);
    expect(full).toBeGreaterThan(6000);
    expect(text.length).toBeLessThanOrEqual(6000);
    const j = JSON.parse(text);
    expect(JSON.stringify(j)).toContain("part_7"); // the 40 ms leaf survives
    expect(JSON.stringify(j)).toMatch(/omitted/);
  });

  it("never exceeds the budget even for tiny limits", () => {
    const p = parsePlan(wide(50));
    if (!p) throw new Error();
    expect(compactPlanJson(p, 200).length).toBeLessThanOrEqual(200);
  });
});

describe("buildExplainPrompt", () => {
  it("includes SQL, findings, plan and the cost-of-fix requirement", () => {
    const p = parsePlan(HASH_JOIN_MISESTIMATE);
    if (!p) throw new Error();
    const prompt = buildExplainPrompt({ sql: "select 1", plan: p, insights: buildInsights(p) });
    expect(prompt).toContain("```sql\nselect 1\n```");
    expect(prompt).toContain("[high] node 0.0.0");
    expect(prompt).toContain("Execution 422.1 ms");
    expect(prompt).toMatch(/cost of every fix/);
    expect(prompt).toMatch(/Do not run EXPLAIN ANALYZE/);
  });
  it("copes with missing SQL and no findings", () => {
    const p = parsePlan(PARALLEL_SEQ_SCAN);
    if (!p) throw new Error();
    const prompt = buildExplainPrompt({ sql: undefined, plan: p, insights: [] });
    expect(prompt).toContain("SQL text was not available");
    expect(prompt).toContain("- none");
  });
});

describe("stripExplainPrefix", () => {
  it("removes EXPLAIN and its options", () => {
    expect(stripExplainPrefix("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) select 1")).toBe("select 1");
    expect(stripExplainPrefix("  explain analyze\nselect * from t")).toBe("select * from t");
    expect(stripExplainPrefix("explain (format json, settings on) with x as (select 1) select * from x")).toBe("with x as (select 1) select * from x");
    expect(stripExplainPrefix("select 'explain'")).toBe("select 'explain'");
  });
});

describe("formatters", () => {
  it("formats time with sensible precision", () => {
    expect(formatMs(0.058)).toBe("0.06 ms");
    expect(formatMs(42.37)).toBe("42.4 ms");
    expect(formatMs(421.3)).toBe("421 ms");
    expect(formatMs(5120.8)).toBe("5.12 s");
    expect(formatMs(125_000)).toBe("2m 05s");
    expect(formatMs(null)).toBe("–");
  });
  it("formats rows and blocks", () => {
    expect(formatRows(48210)).toBe("48,210");
    expect(formatRows(2_000_000)).toBe("2.00M");
    expect(formatBlocks(128)).toBe("1.0 MB");
  });
});
