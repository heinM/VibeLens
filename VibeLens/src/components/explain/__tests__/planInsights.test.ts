import { describe, expect, it } from "vitest";
import { buildInsights, type Insight } from "../planInsights";
import { parsePlan } from "../planModel";
import { filterColumns, joinColumnsFor, parseConditions } from "../planConditions";
import { ESTIMATE_ONLY, EXTERNAL_SORT, HASH_JOIN_MISESTIMATE, INDEX_SCAN, NESTED_LOOP_SEQ, PARALLEL_SEQ_SCAN } from "./fixtures";

const insightsFor = (raw: unknown, ...opts: Parameters<typeof buildInsights>[1][]): Insight[] => {
  const p = parsePlan(raw);
  if (!p) throw new Error("bad fixture");
  return buildInsights(p, opts[0]);
};
const rule = (xs: Insight[], r: Insight["rule"]) => xs.filter((i) => i.rule === r);

describe("condition parsing", () => {
  it("strips casts and parentheses, splits on top-level AND", () => {
    const atoms = parseConditions("(((status)::text = 'pending'::text) AND (created_at > (now() - '30 days'::interval)))");
    expect(atoms.map((a) => [a.left, a.op])).toEqual([
      ["status", "="],
      ["created_at", ">"],
    ]);
  });
  it("puts equality columns before the range column", () => {
    const f = filterColumns("((created_at > '2026-01-01'::date) AND (customer_id = 7) AND ((status)::text = 'x'::text))");
    expect(f.index).toEqual(["customer_id", "status", "created_at"]);
  });
  it("ignores OR groups, join conditions and quoted ANDs", () => {
    expect(filterColumns("((a = 1) OR (b = 2))").index).toEqual([]);
    expect(filterColumns("(o.customer_id = c.id)").index).toEqual([]);
    expect(filterColumns("(note = 'this AND that')").index).toEqual(["note"]);
  });
  it("reports expressions separately", () => {
    const f = filterColumns("(lower((email)::text) = 'a@b.c'::text)");
    expect(f.index).toEqual([]);
    expect(f.expressions).toEqual(["lower(email)"]);
  });
  it("handles ANY, IS NULL and keeps only the first range column", () => {
    const f = filterColumns("((tenant_id = ANY ('{1,2}'::bigint[])) AND (deleted_at IS NULL) AND (a > 1) AND (b < 9))");
    expect(f.equality).toEqual(["tenant_id", "deleted_at"]);
    expect(f.range).toEqual(["a"]);
  });
  it("finds the join columns of one alias", () => {
    expect(joinColumnsFor("((oi.order_id = o.id) AND (oi.sku = p.sku))", "oi")).toEqual(["order_id", "sku"]);
    expect(joinColumnsFor("(oi.order_id = o.id)", "o")).toEqual(["id"]);
  });
});

describe("insights", () => {
  it("is quiet for a healthy index scan", () => {
    expect(insightsFor(INDEX_SCAN)).toEqual([]);
  });

  it("suggests an index for a selective parallel seq scan, with evidence", () => {
    const xs = insightsFor(PARALLEL_SEQ_SCAN);
    const seq = rule(xs, "seqScan");
    expect(seq).toHaveLength(1);
    expect(seq[0].severity).toBe("high");
    expect(seq[0].fix).toBe("CREATE INDEX CONCURRENTLY ON big_table (tenant_id);");
    expect(seq[0].detail).toMatch(/discarded/);
    expect(seq[0].title).toMatch(/consider an index on \(tenant_id\)/);
    // the same node must not also appear under rowsRemoved
    expect(rule(xs, "rowsRemoved")).toHaveLength(0);
  });

  it("reports a misestimate at its origin only, and suggests ANALYZE", () => {
    const xs = insightsFor(HASH_JOIN_MISESTIMATE);
    const m = rule(xs, "misestimate");
    expect(m).toHaveLength(1);
    expect(m[0].nodeId).toBe("0.0.0"); // the Seq Scan on orders, not the join or aggregate above it
    expect(m[0].fix).toContain("ANALYZE orders;");
    expect(m[0].fix).toContain("CREATE STATISTICS"); // two correlated filter columns
    expect(m[0].detail).toMatch(/Stale statistics\? ANALYZE orders/);
    expect(m[0].title).toMatch(/expected 120 rows, got 48,210/);
    // the same Seq Scan also qualifies for an index suggestion: 1.2M rows discarded
    const idx = rule(xs, "seqScan")[0];
    expect(idx.fix).toBe("CREATE INDEX CONCURRENTLY ON orders (status, created_at);");
  });

  it("detects a sort that spilled to disk and proposes a work_mem", () => {
    const xs = insightsFor(EXTERNAL_SORT);
    const s = rule(xs, "sortSpill");
    expect(s).toHaveLength(1);
    expect(s[0].severity).toBe("high");
    expect(s[0].fix).toBe("SET LOCAL work_mem = '368MB';");
    expect(s[0].cost).toMatch(/per sort or hash node per backend/);
    expect(s[0].detail).toMatch(/created_at/);
  });

  it("does not flag an in-memory sort", () => {
    const raw = JSON.parse(JSON.stringify(EXTERNAL_SORT));
    raw[0].Plan["Sort Method"] = "quicksort";
    raw[0].Plan["Sort Space Type"] = "Memory";
    expect(rule(insightsFor(raw), "sortSpill")).toHaveLength(0);
  });

  it("flags a nested loop that re-scans the inner relation and proposes the join index", () => {
    const xs = insightsFor(NESTED_LOOP_SEQ);
    const nl = rule(xs, "nestedLoop");
    expect(nl).toHaveLength(1);
    expect(nl[0].severity).toBe("high");
    expect(nl[0].title).toMatch(/12,000 times/);
    expect(nl[0].fix).toBe("CREATE INDEX CONCURRENTLY ON order_items (order_id);");
    expect(nl[0].detail).toMatch(/discards 144M pairs/);
    // claimed by the nested-loop rule, so no duplicate
    expect(rule(xs, "rowsRemoved")).toHaveLength(0);
    expect(rule(xs, "seqScan")).toHaveLength(0);
  });

  it("respects the nested-loop threshold", () => {
    const xs = insightsFor(NESTED_LOOP_SEQ, { nestedLoopMinLoops: 20_000 });
    expect(rule(xs, "nestedLoop")).toHaveLength(0);
  });

  it("flags Rows Removed by Filter far above rows returned on an index scan", () => {
    const raw = JSON.parse(JSON.stringify(INDEX_SCAN));
    Object.assign(raw[0].Plan, {
      "Index Name": "orders_customer_idx",
      "Index Cond": "(customer_id = 7)",
      Filter: "(status = 'void'::text)",
      "Rows Removed by Filter": 52000,
      "Actual Rows": 3,
      "Plan Rows": 3,
    });
    const xs = insightsFor(raw);
    const r = rule(xs, "rowsRemoved");
    expect(r).toHaveLength(1);
    expect(r[0].title).toMatch(/52,000 rows removed to return 3/);
    expect(r[0].fix).toBe("CREATE INDEX CONCURRENTLY ON orders (status);");
  });

  it("uses cost heuristics when the plan was not analysed", () => {
    const xs = insightsFor(ESTIMATE_ONLY);
    const seq = rule(xs, "seqScan");
    expect(seq).toHaveLength(1);
    expect(seq[0].severity).toBe("high");
    expect(seq[0].fix).toBe("CREATE INDEX CONCURRENTLY ON orders (customer_id, status);");
    expect(seq[0].detail).toMatch(/EXPLAIN ANALYZE/);
    expect(rule(xs, "misestimate")).toHaveLength(0);
  });

  it("flags a hash that spilled into batches", () => {
    const raw = JSON.parse(JSON.stringify(HASH_JOIN_MISESTIMATE));
    const hash = raw[0].Plan.Plans[0].Plans[1];
    hash["Hash Batches"] = 8;
    hash["Original Hash Batches"] = 1;
    hash["Peak Memory Usage"] = 4096;
    const h = rule(insightsFor(raw), "hashSpill");
    expect(h).toHaveLength(1);
    expect(h[0].title).toMatch(/8 batches/);
    expect(h[0].fix).toMatch(/^SET LOCAL work_mem/);
  });

  it("orders by severity, then by share", () => {
    const xs = insightsFor(HASH_JOIN_MISESTIMATE);
    const sev = xs.map((i) => i.severity);
    expect(sev).toEqual([...sev].sort((a, b) => ["high", "warn", "info"].indexOf(a) - ["high", "warn", "info"].indexOf(b)));
  });

  it("notes parallel workers that did not start", () => {
    const raw = JSON.parse(JSON.stringify(PARALLEL_SEQ_SCAN));
    raw[0].Plan.Plans[0]["Workers Launched"] = 0;
    expect(rule(insightsFor(raw), "workers")).toHaveLength(1);
  });
});
