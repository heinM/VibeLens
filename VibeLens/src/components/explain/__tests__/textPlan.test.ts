import { describe, expect, it } from "vitest";
import type { Cell, ColumnMeta } from "../../../ipc/types";
import { buildTextPlanPrompt, planAsText, redactPlanLine, redactTextPlan, textPlanSections } from "../textPlan";

const cols = (...names: string[]): ColumnMeta[] => names.map((name) => ({ name, typeName: "string", typeOid: 0 }));

// What DataFusion 53 prints for: select status, count(*) from shop.orders where status = 'paid' and total > 100 group by status
const DF_LOGICAL = [
  "Projection: orders.status, count(Int64(1))",
  "  Aggregate: groupBy=[[orders.status]], aggr=[[count(Int64(1))]]",
  '    Filter: orders.status = Utf8("paid") AND orders.total > Decimal128(Some(10000),12,2)',
  '      TableScan: shop.orders projection=[status, total], partial_filters=[orders.status = Utf8("paid")]',
].join("\n");
const DF_PHYSICAL = [
  "ProjectionExec: expr=[status@0 as status, count(Int64(1))@1 as count(*)]",
  "  AggregateExec: mode=FinalPartitioned, gby=[status@0 as status], aggr=[count(Int64(1))]",
  "    CoalesceBatchesExec: target_batch_size=8192",
  "      FilterExec: status@0 = paid AND total@1 > 10000",
  "        DataSourceExec: file_groups={2 groups: [[s3://acme-lake/warehouse/shop.db/orders/data/placed_day=2026-10-01/00000-1.parquet], [s3://acme-lake/warehouse/shop.db/orders/data/placed_day=2026-10-02/00001-1.parquet]]}, projection=[status, total], file_type=parquet, predicate=status@2 = paid AND total@3 > 10000, pruning_predicate=status_null_count@2 != row_count@3 AND status_min@0 <= paid AND paid <= status_max@1",
].join("\n");

describe("textPlanSections", () => {
  const rows: Cell[][] = [
    ["logical_plan", DF_LOGICAL],
    ["physical_plan", DF_PHYSICAL],
  ];
  it("splits DataFusion's (plan_type, plan) rows into titled sections", () => {
    const s = textPlanSections(cols("plan_type", "plan"), rows)!;
    expect(s.map((x) => x.title)).toEqual(["logical_plan", "physical_plan"]);
    expect(s[0].text).toBe(DF_LOGICAL);
  });
  it("joins Athena's one-column plan into a single section", () => {
    const s = textPlanSections(cols("Query Plan"), [["Fragment 0 [SINGLE]"], ["    Output layout: [status]"], ["    - TableScan[table = iceberg:shop.orders]"]])!;
    expect(s).toHaveLength(1);
    expect(s[0].title).toBeNull();
    expect(s[0].text.split("\n")).toHaveLength(3);
  });
  it("accepts the whole plan in one cell", () => {
    expect(textPlanSections(cols("Query Plan"), [["a\nb\n"]])).toEqual([{ title: null, text: "a\nb" }]);
  });
  it("is null for anything else, so the grid shows it", () => {
    expect(textPlanSections(cols("a", "b", "c"), [[1, 2, 3]])).toBeNull();
    expect(textPlanSections(cols("plan_type", "plan"), [])).toBeNull();
    expect(textPlanSections(cols("Query Plan"), [[null]])).toBeNull();
    expect(textPlanSections(cols("id", "name"), [[1, "x"]])).toBeNull();
  });
  it("copies as readable text", () => {
    expect(planAsText([{ title: "logical_plan", text: "A" }, { title: null, text: "B" }])).toBe("logical_plan\nA\n\nB");
  });
});

describe("redaction", () => {
  it("removes DataFusion's typed constants and filter expressions", () => {
    const out = redactTextPlan(DF_LOGICAL);
    expect(out).not.toMatch(/paid|10000|Utf8|Decimal128/);
    // structure survives
    expect(out).toContain("Projection: orders.status");
    expect(out).toContain("Aggregate: groupBy=[[orders.status]]");
    expect(out).toContain("TableScan: shop.orders projection=[status, total]");
    expect(out).toContain("Filter: …");
    expect(out).toContain("partial_filters=[…]");
  });
  it("removes bare constants, predicates, expressions and partition paths from a physical plan", () => {
    const out = redactTextPlan(DF_PHYSICAL);
    expect(out).not.toMatch(/paid|10000|2026|acme-lake|s3:\/\//);
    expect(out).toContain("AggregateExec: mode=FinalPartitioned, gby=[status@0 as status]");
    expect(out).toContain("FilterExec: …");
    expect(out).toContain("file_type=parquet");
    expect(out).toContain("projection=[status, total]");
    expect(out).toContain("predicate=…");
    expect(out).toContain("file_groups={…}");
    expect(out).toContain("expr=[…]");
  });
  it("keeps column indexes (status@0) readable", () => {
    expect(redactPlanLine("  SortExec: TopK(fetch=10), expr=[total@3 DESC]")).toContain("TopK(fetch=?)");
    expect(redactPlanLine("  RepartitionExec: partitioning=Hash([customer_id@1], 16), input_partitions=2")).toContain("customer_id@1");
  });
  it("removes Trino / Athena predicates and string literals", () => {
    const line = "    - ScanFilterProject[table = iceberg:shop.orders$data@4812730049218318, filterPredicate = (status = 'paid' AND total > 100), projectLocality = LOCAL]";
    const out = redactPlanLine(line);
    expect(out).not.toMatch(/paid|100\b/);
    expect(out).toContain("ScanFilterProject[table = iceberg:shop.orders");
    expect(redactPlanLine("Fragment 0 [SINGLE]  Estimates: {rows: 1200 (35.2kB)}")).not.toContain("1200");
    expect(redactPlanLine("  - Filter[filterPredicate = (email = 'ada@example.com')]")).not.toContain("ada");
  });
  it("hides storage locations", () => {
    expect(redactPlanLine("Scan: s3://bucket/ns/orders/data/day=2026-10-01/f.parquet and file:///tmp/x")).toBe("Scan: <location> and <location>");
  });
  it("hides nested typed constants and constants in strings with quotes", () => {
    expect(redactPlanLine('Filter: x = Utf8("it\'s (\\"quoted\\")") AND y = Int32(7)')).toBe("Filter: …");
    expect(redactPlanLine('Projection: Utf8("a)b") AS c, t.id')).toBe("Projection: ? AS c, t.id");
  });
  it("is stable: redacting twice changes nothing more", () => {
    const once = redactTextPlan(DF_PHYSICAL);
    expect(redactTextPlan(once)).toBe(once);
  });
});

describe("buildTextPlanPrompt", () => {
  const sections = [
    { title: "logical_plan", text: DF_LOGICAL },
    { title: "physical_plan", text: DF_PHYSICAL },
  ];
  const sql = "EXPLAIN select status, count(*) from shop.orders where status = 'paid' and total > 100 group by status";

  it("sends everything when result rows may leave the machine", () => {
    const p = buildTextPlanPrompt({ sql, sections, engineLabel: "Apache DataFusion", redact: false })!;
    expect(p).toContain("status = 'paid'");
    expect(p).not.toMatch(/^EXPLAIN/m);
    expect(p).toContain('Utf8("paid")');
    expect(p).toContain("logical_plan:");
    expect(p).toContain("Apache DataFusion query plan");
    expect(p).not.toContain("were removed");
  });
  it("removes constants from both the SQL and the plan otherwise", () => {
    const p = buildTextPlanPrompt({ sql, sections, engineLabel: "Apache DataFusion", redact: true })!;
    expect(p).not.toMatch(/paid|10000|\b100\b|s3:\/\//);
    expect(p).toContain("status = ?");
    expect(p).toContain("were removed");
  });
  it("mentions scan cost for Athena only", () => {
    expect(buildTextPlanPrompt({ sql, sections, engineLabel: "Amazon Athena", redact: true })).toContain("bills by data scanned");
    expect(buildTextPlanPrompt({ sql, sections, engineLabel: "Apache DataFusion", redact: true })).not.toContain("bills by data scanned");
  });
  it("tells the model nothing ran and nothing may change", () => {
    const p = buildTextPlanPrompt({ sql, sections, engineLabel: "Amazon Athena", redact: false })!;
    expect(p).toContain("nothing was executed");
    expect(p).toContain("Do not run any statement that changes data");
  });
  it("fails closed when nothing safe is left", () => {
    expect(buildTextPlanPrompt({ sql, sections: [{ title: null, text: "   " }], engineLabel: "Amazon Athena", redact: true })).toBeNull();
  });
  it("truncates huge plans", () => {
    const big = [{ title: null, text: "Scan\n".repeat(10_000) }];
    const p = buildTextPlanPrompt({ sql: undefined, sections: big, engineLabel: "Amazon Athena", redact: false, maxPlanChars: 500 })!;
    expect(p.length).toBeLessThan(2500);
    expect(p).toContain("plan truncated");
    expect(p).toContain("the SQL text was not available");
  });
});
