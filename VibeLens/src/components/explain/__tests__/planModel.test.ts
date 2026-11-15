import { describe, expect, it } from "vitest";
import { hotSpots, misestimateOf, nodeTitle, parsePlan, shorten, unwrapPlan, type ParsedPlan } from "../planModel";
import { CTE_PLAN, ESTIMATE_ONLY, EXTERNAL_SORT, HASH_JOIN_MISESTIMATE, INDEX_SCAN, NESTED_LOOP_SEQ, PARALLEL_SEQ_SCAN } from "./fixtures";

const must = (raw: unknown): ParsedPlan => {
  const p = parsePlan(raw);
  if (!p) throw new Error("fixture did not parse");
  return p;
};
const byType = (p: ParsedPlan, type: string, rel?: string) => {
  const n = p.nodes.find((x) => x.nodeType === type && (rel === undefined || x.relation === rel));
  if (!n) throw new Error(`no ${type}`);
  return n;
};
const close = (a: number | null, b: number, digits = 3) => expect(a).toBeCloseTo(b, digits);

describe("parsePlan", () => {
  it("accepts the array wrapper, the bare object and JSON text", () => {
    expect(parsePlan(INDEX_SCAN)).not.toBeNull();
    expect(parsePlan(INDEX_SCAN[0])).not.toBeNull();
    expect(parsePlan(JSON.stringify(INDEX_SCAN))).not.toBeNull();
    expect(parsePlan("not json")).toBeNull();
    expect(parsePlan([{ nothing: 1 }])).toBeNull();
    expect(parsePlan(null)).toBeNull();
    expect(unwrapPlan([])).toBeNull();
  });

  it("reads a simple analysed index scan", () => {
    const p = must(INDEX_SCAN);
    expect(p.analyzed).toBe(true);
    expect(p.metric).toBe("time");
    expect(p.nodes).toHaveLength(1);
    expect(p.totalCost).toBe(8.45);
    expect(p.planningTime).toBe(0.114);
    expect(p.executionTime).toBe(0.058);
    expect(p.rows).toBe(1);
    expect(p.buffers).toMatchObject({ sharedHit: 4, sharedRead: 0 });
    expect(p.root.misestimate).toBeNull();
    expect(p.root.share).toBeCloseTo(1, 5);
    expect(nodeTitle(p.root)).toEqual({ type: "Index Scan", target: "orders_pkey on orders" });
  });

  it("assigns stable path ids and depths", () => {
    const p = must(HASH_JOIN_MISESTIMATE);
    expect(p.nodes.map((n) => n.id)).toEqual(["0", "0.0", "0.0.0", "0.0.1", "0.0.1.0"]);
    expect(p.byId.get("0.0.1")?.nodeType).toBe("Hash");
    expect(p.byId.get("0.0.1.0")?.depth).toBe(3);
    expect(p.root.descendants).toBe(4);
  });
});

describe("exclusive time", () => {
  it("subtracts children from the parent (hash join)", () => {
    const p = must(HASH_JOIN_MISESTIMATE);
    close(byType(p, "Seq Scan", "customers").exclusiveTime, 5.6);
    close(byType(p, "Hash").exclusiveTime, 5.3);
    close(byType(p, "Seq Scan", "orders").exclusiveTime, 352.8);
    close(byType(p, "Hash Join").exclusiveTime, 396.2 - 352.8 - 10.9);
    close(byType(p, "Aggregate").exclusiveTime, 421.3 - 396.2);
    const total = p.nodes.reduce((a, n) => a + (n.exclusiveTime ?? 0), 0);
    close(total, 421.3);
    expect(byType(p, "Seq Scan", "orders").share).toBeCloseTo(352.8 / 421.3, 4);
    expect(p.nodes.reduce((a, n) => a + n.share, 0)).toBeCloseTo(1, 4);
  });

  it("does not multiply by loops for workers: nodes under Gather run concurrently", () => {
    const p = must(PARALLEL_SEQ_SCAN);
    const seq = byType(p, "Seq Scan");
    expect(seq.loops).toBe(3);
    expect(seq.parallelFactor).toBe(3);
    close(seq.inclusiveTime, 487.2); // 487.2 x 3 loops / 3 processes
    close(byType(p, "Aggregate", undefined).exclusiveTime, 0.6);
    const partial = p.nodes.find((n) => n.props["Partial Mode"] === "Partial");
    close(partial?.exclusiveTime ?? null, 495 - 487.2);
    close(byType(p, "Gather").exclusiveTime, 501.9 - 495);
    close(
      p.nodes.reduce((a, n) => a + (n.exclusiveTime ?? 0), 0),
      502.5,
    );
    expect(seq.share).toBeGreaterThan(0.95);
  });

  it("multiplies by loops for repeated inner sides (nested loop + materialize)", () => {
    const p = must(NESTED_LOOP_SEQ);
    const mat = byType(p, "Materialize");
    expect(mat.parallelFactor).toBe(1);
    close(mat.inclusiveTime, 0.45 * 12000);
    close(mat.exclusiveTime, 0.45 * 12000 - 2.9);
    close(byType(p, "Nested Loop").exclusiveTime, 9210.4 - 8.1 - 5400);
  });

  it("bills a CTE to the CTE Scan that ran it, not to its owner twice", () => {
    const p = must(CTE_PLAN);
    const scans = p.nodes.filter((n) => n.nodeType === "CTE Scan");
    close(scans[0].exclusiveTime, 110 - 95); // first scan: 15 ms of its own
    close(scans[1].exclusiveTime, 0.01 * 500);
    close(byType(p, "Nested Loop").exclusiveTime, 130 - 110 - 5);
    close(byType(p, "Seq Scan", "events").exclusiveTime, 95);
  });

  it("treats loops=0 as never executed", () => {
    const raw = [
      {
        Plan: {
          "Node Type": "Nested Loop",
          "Startup Cost": 0,
          "Total Cost": 10,
          "Plan Rows": 1,
          "Actual Total Time": 1,
          "Actual Rows": 0,
          "Actual Loops": 1,
          Plans: [
            { "Node Type": "Seq Scan", "Parent Relationship": "Outer", "Plan Rows": 1, "Total Cost": 1, "Actual Total Time": 0.5, "Actual Rows": 0, "Actual Loops": 1 },
            { "Node Type": "Index Scan", "Parent Relationship": "Inner", "Plan Rows": 1, "Total Cost": 1, "Actual Total Time": 0, "Actual Rows": 0, "Actual Loops": 0 },
          ],
        },
      },
    ];
    const p = must(raw);
    expect(p.nodes[2].neverExecuted).toBe(true);
    expect(p.nodes[2].inclusiveTime).toBe(0);
    expect(p.nodes[2].misestimate).toBeNull();
  });
});

describe("cost mode (no ANALYZE)", () => {
  it("measures shares in exclusive cost", () => {
    const p = must(ESTIMATE_ONLY);
    expect(p.analyzed).toBe(false);
    expect(p.metric).toBe("cost");
    expect(p.executionTime).toBeNull();
    const seq = byType(p, "Seq Scan");
    close(seq.exclusiveCost, 98150, 1);
    close(byType(p, "Sort").exclusiveCost, 98213.9 - 98150, 1);
    expect(byType(p, "Limit").exclusiveCost).toBe(0);
    expect(seq.share).toBeGreaterThan(0.99);
    expect(seq.misestimate).toBeNull();
    expect(p.rows).toBe(10);
  });
});

describe("misestimates", () => {
  it("flags >=10x with direction and severity", () => {
    expect(misestimateOf(120, 48210, 1)).toMatchObject({ direction: "under", severity: "serious" });
    expect(misestimateOf(1000000, 3, 1)).toMatchObject({ direction: "over", severity: "severe" });
    expect(misestimateOf(10, 99, 1)).toBeNull();
    expect(misestimateOf(10, 100, 1)?.severity).toBe("mild");
    expect(misestimateOf(1, 0, 1)).toBeNull(); // est 1, got none: not a misestimate worth flagging
    expect(misestimateOf(100, 5, 0)).toBeNull();
  });

  it("marks the analysed fixture", () => {
    const p = must(HASH_JOIN_MISESTIMATE);
    const seq = byType(p, "Seq Scan", "orders");
    expect(seq.misestimate?.factor).toBeCloseTo(401.75, 1);
    expect(seq.misestimate?.direction).toBe("under");
    expect(byType(p, "Seq Scan", "customers").misestimate).toBeNull();
  });

  it("ignores over-estimates caused by an early-stopping Limit", () => {
    const raw = [
      {
        Plan: {
          "Node Type": "Limit",
          "Plan Rows": 10,
          "Total Cost": 5,
          "Actual Total Time": 0.1,
          "Actual Rows": 10,
          "Actual Loops": 1,
          Plans: [{ "Node Type": "Index Scan", "Parent Relationship": "Outer", "Plan Rows": 500000, "Total Cost": 5, "Actual Total Time": 0.09, "Actual Rows": 10, "Actual Loops": 1 }],
        },
      },
    ];
    expect(must(raw).nodes[1].misestimate).toBeNull();
  });
});

describe("hotSpots", () => {
  it("returns the top nodes by share", () => {
    const p = must(HASH_JOIN_MISESTIMATE);
    const hs = hotSpots(p, 3);
    expect(hs.map((n) => n.nodeType)).toEqual(["Seq Scan", "Hash Join", "Aggregate"]);
    expect(hs[0].relation).toBe("orders");
  });
  it("skips the negligible", () => {
    expect(hotSpots(must(EXTERNAL_SORT), 3).map((n) => n.nodeType)).toEqual(["Sort", "Seq Scan"]);
  });
});

describe("helpers", () => {
  it("shortens long conditions around the middle", () => {
    const s = shorten("a".repeat(200), 40);
    expect(s.length).toBe(40);
    expect(s).toContain("…");
    expect(shorten("x   y\n z")).toBe("x y z");
  });
  it("labels aggregates by strategy", () => {
    const p = must(HASH_JOIN_MISESTIMATE);
    expect(nodeTitle(p.root).type).toBe("HashAggregate");
  });
});
