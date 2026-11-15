// Builds the "Explain with AI" prompt: SQL, a size-bounded view of the plan and the rule-based
// findings, so the model starts from facts instead of re-deriving them.
import type { Insight } from "./planInsights";
import type { ParsedPlan, PlanNode } from "./planModel";
import { nodeTitle } from "./planModel";

/** Properties worth a model's attention. Everything else (widths, async flags, ...) is noise. */
const KEEP_PROPS = [
  "Join Type",
  "Strategy",
  "Index Cond",
  "Recheck Cond",
  "Hash Cond",
  "Merge Cond",
  "Join Filter",
  "Filter",
  "Sort Key",
  "Group Key",
  "Sort Method",
  "Sort Space Used",
  "Sort Space Type",
  "Hash Batches",
  "Peak Memory Usage",
  "Disk Usage",
  "Heap Fetches",
  "Rows Removed by Filter",
  "Rows Removed by Join Filter",
  "Rows Removed by Index Recheck",
  "Shared Hit Blocks",
  "Shared Read Blocks",
  "Temp Written Blocks",
  "Workers Planned",
  "Workers Launched",
  "Scan Direction",
] as const;

const clip = (s: string, n = 300) => (s.length > n ? `${s.slice(0, n)}…` : s);

type Compact = Record<string, unknown> & { children?: Compact[] };

function compactNode(n: PlanNode, analyzed: boolean): Compact {
  const { type, target } = nodeTitle(n);
  const o: Compact = { id: n.id, node: type };
  if (target) o.on = target;
  o.cost = [Number(n.startupCost.toFixed(2)), Number(n.totalCost.toFixed(2))];
  o.rows = analyzed ? { est: n.planRows, actual: n.actualRows, loops: n.loops } : { est: n.planRows };
  if (analyzed) {
    o.timeMs = { exclusive: Number((n.exclusiveTime ?? 0).toFixed(2)), inclusive: Number((n.inclusiveTime ?? 0).toFixed(2)) };
    o.share = Number(n.share.toFixed(3));
  }
  if (n.misestimate) o.misestimate = `${Math.round(n.misestimate.factor)}x ${n.misestimate.direction}`;
  for (const k of KEEP_PROPS) {
    const v = n.props[k];
    if (v === undefined || v === null || v === false) continue;
    if (typeof v === "string") o[k] = clip(v);
    else if (Array.isArray(v)) o[k] = v.map((x) => clip(String(x), 120));
    else o[k] = v;
  }
  return o;
}

/** Weight used to decide which subtrees survive truncation: the busiest ones. */
function weight(n: PlanNode): number {
  let w = n.share;
  for (const c of n.children) w += weight(c);
  return w;
}

/**
 * Compact JSON of the plan, at most `maxChars` long. When the plan is too big, the least
 * interesting subtrees (lowest combined share, then smallest) are replaced by a stub that says
 * how many nodes were dropped.
 */
export function compactPlanJson(plan: ParsedPlan, maxChars = 12_000): string {
  const analyzed = plan.analyzed;
  const render = (n: PlanNode, omitted: Set<string>): Compact => {
    const c = compactNode(n, analyzed);
    if (n.children.length === 0) return c;
    const kept: Compact[] = [];
    let dropped = 0;
    let droppedNodes = 0;
    let droppedShare = 0;
    for (const k of n.children) {
      if (omitted.has(k.id)) {
        dropped++;
        droppedNodes += k.descendants + 1;
        droppedShare += weight(k);
      } else kept.push(render(k, omitted));
    }
    if (dropped > 0) kept.push({ omitted: `${droppedNodes} nodes in ${dropped} subtree${dropped > 1 ? "s" : ""}, ${Math.round(droppedShare * 100)}% of ${plan.metric}` });
    c.children = kept;
    return c;
  };
  const sizeWith = (omitted: Set<string>) => JSON.stringify(render(plan.root, omitted)).length;

  const none = new Set<string>();
  if (sizeWith(none) <= maxChars) return JSON.stringify(render(plan.root, none));

  // Drop the least interesting subtrees first (lowest combined share, then biggest), found by bisection.
  const cands = plan.nodes
    .filter((n) => n.parentId !== null)
    .map((n) => ({ id: n.id, w: weight(n), size: n.descendants + 1 }))
    .sort((a, b) => a.w - b.w || b.size - a.size);
  const setOf = (k: number) => new Set(cands.slice(0, k).map((c) => c.id));
  let lo = 0;
  let hi = cands.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (sizeWith(setOf(mid)) <= maxChars) hi = mid;
    else lo = mid + 1;
  }
  const out = JSON.stringify(render(plan.root, setOf(lo)));
  if (out.length <= maxChars) return out;
  const stub = JSON.stringify({ id: plan.root.id, node: plan.root.nodeType, omitted: `${plan.nodes.length - 1} nodes` });
  return stub.length <= maxChars ? stub : stub.slice(0, maxChars);
}

/** `EXPLAIN (ANALYZE, FORMAT JSON) select …` → `select …`. The grid hands us the statement it ran. */
export function stripExplainPrefix(sql: string): string {
  const m = /^\s*explain(?:\s*\((?:[^()]|\([^()]*\))*\)|\s+(?:analy[sz]e|verbose))*\s+/i.exec(sql);
  return m ? sql.slice(m[0].length) : sql;
}

export interface PromptInput {
  sql: string | undefined;
  plan: ParsedPlan;
  insights: Insight[];
  /** Max characters of plan JSON. */
  maxPlanChars?: number;
}

export function buildExplainPrompt({ sql, plan, insights, maxPlanChars = 12_000 }: PromptInput): string {
  const head = [
    plan.analyzed
      ? `Execution ${plan.executionTime?.toFixed(1) ?? "?"} ms, planning ${plan.planningTime?.toFixed(1) ?? "?"} ms (EXPLAIN ANALYZE).`
      : "Estimates only (plain EXPLAIN, nothing was executed).",
    `Total cost ${plan.totalCost.toFixed(1)}. ${plan.nodes.length} plan nodes.`,
  ].join(" ");

  const bare = sql ? stripExplainPrefix(sql).trim() : "";
  const sqlText = bare ? (bare.length > 6000 ? `${bare.slice(0, 6000)}\n-- …truncated` : bare) : "(the SQL text was not available)";
  const findings = insights.length
    ? insights.map((i) => `- [${i.severity}] node ${i.nodeId}: ${i.title}`).join("\n")
    : "- none";

  return [
    "Review this PostgreSQL query plan. Give a concise diagnosis of what makes it slow (or confirm that it is fine), then concrete fixes ranked by expected impact.",
    "",
    "Requirements:",
    "- Start with a 2-3 sentence diagnosis that names the slowest nodes by their id and share of time (or cost, when not analyzed).",
    "- For each fix give runnable SQL (CREATE INDEX with exact columns, a rewritten query, or a SET LOCAL setting) in a ```sql block.",
    "- State the cost of every fix as specifically as its benefit: write overhead, index size, lock level, memory per backend.",
    "- If the evidence is weak (for example the plan is estimates only), say so instead of guessing.",
    "- Do not run EXPLAIN ANALYZE or any statement that changes data. You may look up the schema (columns, existing indexes) if it helps.",
    "- Keep it short. No preamble.",
    "",
    "Query:",
    "```sql",
    sqlText,
    "```",
    "",
    `Plan summary: ${head}`,
    "",
    "Findings from the rule-based checker (verify them, do not just repeat them):",
    findings,
    "",
    "Plan (JSON, one object per node; `share` is the node's exclusive share of total time or cost; `children` nest):",
    "```json",
    compactPlanJson(plan, maxPlanChars),
    "```",
  ].join("\n");
}
