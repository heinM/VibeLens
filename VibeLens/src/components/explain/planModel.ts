// Pure model for Postgres `EXPLAIN (FORMAT JSON)` output: normalisation, exclusive time/cost,
// parallel-worker and loop handling, misestimates and hot spots. No React, no IPC.
//
// Conventions in Postgres JSON plans that matter here:
//  - "Actual Total Time" / "Actual Rows" are averages PER LOOP; multiply by "Actual Loops" for totals.
//  - Below a Gather, "Actual Loops" counts the processes that ran the node (workers + leader).
//    Those processes run concurrently, so wall-clock time is the per-loop time, not the sum.
//    A nested-loop inner side inside a worker has loops = total across all processes, so we divide
//    by the process count to get that worker's wall-clock.

export type RawPlan = Record<string, unknown>;

export interface Misestimate {
  /** Always >= 1. 10 means "off by 10x". */
  factor: number;
  /** "under": planner expected fewer rows than it got. "over": it expected more. */
  direction: "under" | "over";
  severity: "mild" | "serious" | "severe";
}

export interface PlanNode {
  /** Path in the tree: "0", "0.1", "0.1.0". Stable for a given plan. */
  id: string;
  parentId: string | null;
  depth: number;
  nodeType: string;
  /** "Outer", "Inner", "InitPlan", "SubPlan", "Member", ... */
  parentRel: string | null;
  schema: string | null;
  relation: string | null;
  alias: string | null;
  indexName: string | null;
  cteName: string | null;
  /** "Subplan Name" (e.g. "InitPlan 1", "CTE recent"). */
  subplanName: string | null;
  joinType: string | null;
  strategy: string | null;

  startupCost: number;
  totalCost: number;
  planRows: number;
  planWidth: number;

  /** True when this node carries ANALYZE timings. */
  analyzed: boolean;
  loops: number;
  /** Per-loop averages, as Postgres reports them. */
  actualRows: number | null;
  actualStartup: number | null;
  actualTotal: number | null;
  /** loops === 0 on an analyzed plan. */
  neverExecuted: boolean;
  workersPlanned: number | null;
  workersLaunched: number | null;

  children: PlanNode[];
  /** Every property of the raw node except "Plans". */
  props: Record<string, unknown>;

  // ---- computed ----
  /** Concurrent processes sharing this node's work (1 outside parallel regions). */
  parallelFactor: number;
  /** Inclusive wall-clock estimate in ms: actualTotal x loops / parallelFactor. Null if not analyzed. */
  inclusiveTime: number | null;
  /** Inclusive minus children, clamped at 0. */
  exclusiveTime: number | null;
  exclusiveCost: number;
  /** Share of the plan's total (exclusive time if analyzed, else exclusive cost), 0..1. */
  share: number;
  /** Rows this node produced in total (actualRows x loops), or planRows x 1 when not analyzed. */
  totalRows: number;
  misestimate: Misestimate | null;
  /** Number of nodes below this one. */
  descendants: number;
}

export interface Buffers {
  sharedHit: number;
  sharedRead: number;
  sharedDirtied: number;
  sharedWritten: number;
  tempRead: number;
  tempWritten: number;
}

export interface ParsedPlan {
  root: PlanNode;
  /** Pre-order. */
  nodes: PlanNode[];
  byId: Map<string, PlanNode>;
  analyzed: boolean;
  /** What the bars and hot spots are measured in. */
  metric: "time" | "cost";
  /** Denominator for shares: wall-clock ms or cost units. */
  metricTotal: number;
  totalCost: number;
  planningTime: number | null;
  executionTime: number | null;
  /** Rows out of the root: actual if analyzed, else estimated. */
  rows: number;
  buffers: Buffers | null;
  jitMs: number | null;
}

// ---------------------------------------------------------------------------------------------

const num = (v: unknown): number | null => {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && v.trim() !== "" && Number.isFinite(Number(v))) return Number(v);
  return null;
};
const str = (v: unknown): string | null => (typeof v === "string" && v !== "" ? v : null);

/** Accepts the raw cell: array-of-one, the object itself, or JSON text. Returns the wrapper object or null. */
export function unwrapPlan(raw: unknown): RawPlan | null {
  let v = raw;
  if (typeof v === "string") {
    const t = v.trim();
    if (!t.startsWith("[") && !t.startsWith("{")) return null;
    try {
      v = JSON.parse(t);
    } catch {
      return null;
    }
  }
  if (Array.isArray(v)) v = v[0];
  if (!v || typeof v !== "object") return null;
  const o = v as RawPlan;
  return o.Plan && typeof o.Plan === "object" ? o : null;
}

/** True when the value looks like `EXPLAIN (FORMAT JSON)` output. */
export function looksLikePlan(raw: unknown): boolean {
  return unwrapPlan(raw) !== null;
}

const GATHER = /^Gather( Merge)?$/;
const BLOCKING = new Set(["Sort", "Hash", "Materialize", "Incremental Sort", "WindowAgg"]);

interface BuildCtx {
  nodes: PlanNode[];
  byId: Map<string, PlanNode>;
}

function build(raw: RawPlan, parent: PlanNode | null, index: number, ctx: BuildCtx): PlanNode {
  const id = parent ? `${parent.id}.${index}` : "0";
  const props: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(raw)) if (k !== "Plans") props[k] = v;

  const actualTotal = num(raw["Actual Total Time"]);
  const analyzed = actualTotal !== null;
  const loops = analyzed ? (num(raw["Actual Loops"]) ?? 1) : 1;
  const planRows = num(raw["Plan Rows"]) ?? 0;
  const actualRows = analyzed ? num(raw["Actual Rows"]) : null;

  const node: PlanNode = {
    id,
    parentId: parent?.id ?? null,
    depth: parent ? parent.depth + 1 : 0,
    nodeType: str(raw["Node Type"]) ?? "Unknown",
    parentRel: str(raw["Parent Relationship"]),
    schema: str(raw["Schema"]),
    relation: str(raw["Relation Name"]),
    alias: str(raw["Alias"]),
    indexName: str(raw["Index Name"]),
    cteName: str(raw["CTE Name"]),
    subplanName: str(raw["Subplan Name"]),
    joinType: str(raw["Join Type"]),
    strategy: str(raw["Strategy"]),
    startupCost: num(raw["Startup Cost"]) ?? 0,
    totalCost: num(raw["Total Cost"]) ?? 0,
    planRows,
    planWidth: num(raw["Plan Width"]) ?? 0,
    analyzed,
    loops,
    actualRows,
    actualStartup: analyzed ? num(raw["Actual Startup Time"]) : null,
    actualTotal,
    neverExecuted: analyzed && loops === 0,
    workersPlanned: num(raw["Workers Planned"]),
    workersLaunched: num(raw["Workers Launched"]),
    children: [],
    props,
    parallelFactor: 1,
    inclusiveTime: null,
    exclusiveTime: null,
    exclusiveCost: 0,
    share: 0,
    totalRows: analyzed ? (actualRows ?? 0) * loops : planRows,
    misestimate: null,
    descendants: 0,
  };
  ctx.nodes.push(node);
  ctx.byId.set(id, node);

  const kids = Array.isArray(raw.Plans) ? (raw.Plans as unknown[]) : [];
  let i = 0;
  for (const k of kids) {
    if (k && typeof k === "object") node.children.push(build(k as RawPlan, node, i++, ctx));
  }
  node.descendants = node.children.reduce((a, c) => a + 1 + c.descendants, 0);
  return node;
}

/** CTE definitions are InitPlans named "CTE <name>"; their cost is paid inside the CTE Scan that triggers them. */
const isCteDef = (n: PlanNode) => n.parentRel === "InitPlan" && /^CTE\s/i.test(n.subplanName ?? "");

export function misestimateOf(planRows: number, actualRows: number | null, loops: number): Misestimate | null {
  if (actualRows === null || loops <= 0) return null;
  const est = Math.max(planRows, 1);
  const act = Math.max(actualRows, 1);
  const factor = Math.max(est, act) / Math.min(est, act);
  if (factor < 10) return null;
  return {
    factor,
    direction: act > est ? "under" : "over",
    severity: factor >= 1000 ? "severe" : factor >= 100 ? "serious" : "mild",
  };
}

function annotate(root: PlanNode, analyzed: boolean, ctx: BuildCtx) {
  // 1. parallel factors (top-down)
  const setFactor = (n: PlanNode, f: number) => {
    n.parallelFactor = f;
    for (const c of n.children) {
      if (GATHER.test(n.nodeType)) {
        const processes = c.analyzed && c.loops > 0 ? c.loops : (n.workersLaunched ?? n.workersPlanned ?? 0) + 1;
        setFactor(c, Math.max(1, processes));
      } else setFactor(c, f);
    }
  };
  setFactor(root, 1);

  // 2. inclusive and exclusive time (bottom-up via reverse pre-order)
  const nodes = ctx.nodes;
  if (analyzed) {
    for (const n of nodes) n.inclusiveTime = n.neverExecuted ? 0 : ((n.actualTotal ?? 0) * n.loops) / n.parallelFactor;
    for (let i = nodes.length - 1; i >= 0; i--) {
      const n = nodes[i];
      let sub = 0;
      for (const c of n.children) if (!isCteDef(c)) sub += c.inclusiveTime ?? 0;
      n.exclusiveTime = Math.max(0, (n.inclusiveTime ?? 0) - sub);
    }
    // A CTE's work is billed to the CTE Scan that ran it first (the one with the most time).
    const defs = new Map<string, PlanNode>();
    for (const n of nodes) if (isCteDef(n) && n.subplanName) defs.set(n.subplanName.replace(/^CTE\s+/i, ""), n);
    const scansByCte = new Map<string, PlanNode[]>();
    for (const n of nodes) if (n.nodeType === "CTE Scan" && n.cteName) scansByCte.set(n.cteName, [...(scansByCte.get(n.cteName) ?? []), n]);
    for (const [name, scans] of scansByCte) {
      const def = defs.get(name);
      if (!def) continue;
      const first = scans.reduce((a, b) => ((b.inclusiveTime ?? 0) > (a.inclusiveTime ?? 0) ? b : a));
      first.exclusiveTime = Math.max(0, (first.exclusiveTime ?? 0) - (def.inclusiveTime ?? 0));
    }
  }

  // 3. exclusive cost: estimates only; clamp because inner sides of nested loops are priced per loop
  for (let i = nodes.length - 1; i >= 0; i--) {
    const n = nodes[i];
    const sub = n.children.reduce((a, c) => a + (isCteDef(c) ? 0 : c.totalCost), 0);
    n.exclusiveCost = Math.max(0, n.totalCost - sub);
  }

  // 4. shares
  const sumExcl = analyzed ? nodes.reduce((a, n) => a + (n.exclusiveTime ?? 0), 0) : nodes.reduce((a, n) => a + n.exclusiveCost, 0);
  const denom = analyzed ? Math.max(root.inclusiveTime ?? 0, sumExcl) : Math.max(root.totalCost, sumExcl);
  for (const n of nodes) {
    const v = analyzed ? (n.exclusiveTime ?? 0) : n.exclusiveCost;
    n.share = denom > 0 ? Math.min(1, v / denom) : 0;
  }

  // 5. misestimates; a Limit stops its pipelined subtree early, which makes children look over-estimated
  const walk = (n: PlanNode, early: boolean) => {
    const m = misestimateOf(n.planRows, n.actualRows, n.loops);
    n.misestimate = m && early && m.direction === "over" ? null : m;
    const pipelined = n.nodeType === "Limit" || (early && !BLOCKING.has(n.nodeType) && !(n.nodeType === "Aggregate" && n.strategy !== "Sorted"));
    for (const c of n.children) walk(c, pipelined && !(n.nodeType === "Hash Join" && c.parentRel === "Inner"));
  };
  walk(root, false);
  return denom;
}

function readBuffers(p: RawPlan): Buffers | null {
  const keys = ["Shared Hit Blocks", "Shared Read Blocks", "Shared Dirtied Blocks", "Shared Written Blocks", "Temp Read Blocks", "Temp Written Blocks"];
  if (!keys.some((k) => num(p[k]) !== null)) return null;
  return {
    sharedHit: num(p["Shared Hit Blocks"]) ?? 0,
    sharedRead: num(p["Shared Read Blocks"]) ?? 0,
    sharedDirtied: num(p["Shared Dirtied Blocks"]) ?? 0,
    sharedWritten: num(p["Shared Written Blocks"]) ?? 0,
    tempRead: num(p["Temp Read Blocks"]) ?? 0,
    tempWritten: num(p["Temp Written Blocks"]) ?? 0,
  };
}

/** Normalises the raw `QUERY PLAN` cell. Returns null if it isn't a JSON plan. */
export function parsePlan(raw: unknown): ParsedPlan | null {
  const wrapper = unwrapPlan(raw);
  if (!wrapper) return null;
  const ctx: BuildCtx = { nodes: [], byId: new Map() };
  const root = build(wrapper.Plan as RawPlan, null, 0, ctx);
  const analyzed = root.analyzed;
  const metricTotal = annotate(root, analyzed, ctx);
  const jit = wrapper.JIT as { Timing?: { Total?: unknown } } | undefined;
  return {
    root,
    nodes: ctx.nodes,
    byId: ctx.byId,
    analyzed,
    metric: analyzed ? "time" : "cost",
    metricTotal,
    totalCost: root.totalCost,
    planningTime: num(wrapper["Planning Time"]),
    executionTime: num(wrapper["Execution Time"]) ?? (analyzed ? root.inclusiveTime : null),
    rows: analyzed ? root.totalRows : root.planRows,
    buffers: readBuffers(wrapper.Plan as RawPlan),
    jitMs: num(jit?.Timing?.Total),
  };
}

// ---------------------------------------------------------------------------------------------
// Helpers for display and analysis

/** The nodes ranked by share of the plan's metric, descending, ignoring the negligible. */
export function hotSpots(plan: ParsedPlan, limit = 3, minShare = 0.02): PlanNode[] {
  return [...plan.nodes]
    .filter((n) => n.share >= minShare)
    .sort((a, b) => b.share - a.share || b.totalCost - a.totalCost)
    .slice(0, limit);
}

/** "Seq Scan on orders o", "Index Scan using orders_pkey on orders", "Hash Join". */
export function nodeTitle(n: PlanNode): { type: string; target: string | null } {
  const type = n.strategy && n.nodeType === "Aggregate" ? (n.strategy === "Hashed" ? "HashAggregate" : n.strategy === "Sorted" ? "GroupAggregate" : "Aggregate") : n.nodeType;
  const rel = n.relation ? (n.schema && n.schema !== "public" ? `${n.schema}.${n.relation}` : n.relation) : null;
  const aliasPart = n.alias && n.alias !== n.relation ? ` ${n.alias}` : "";
  let target: string | null = null;
  if (rel && n.indexName && /Index/.test(n.nodeType)) target = `${n.indexName} on ${rel}${aliasPart}`;
  else if (rel) target = `${rel}${aliasPart}`;
  else if (n.indexName) target = n.indexName;
  else if (n.cteName) target = n.cteName + (n.alias && n.alias !== n.cteName ? ` ${n.alias}` : "");
  else if (n.alias && /Scan$/.test(n.nodeType)) target = n.alias;
  return { type, target };
}

/** Condition properties in the order a reader wants them. */
const CONDITION_KEYS = [
  "Index Cond",
  "Recheck Cond",
  "Hash Cond",
  "Merge Cond",
  "Join Filter",
  "Filter",
  "TID Cond",
  "One-Time Filter",
  "Sort Key",
  "Group Key",
  "Presorted Key",
] as const;

export function nodeConditions(n: PlanNode): { key: string; text: string }[] {
  const out: { key: string; text: string }[] = [];
  for (const k of CONDITION_KEYS) {
    const v = n.props[k];
    if (typeof v === "string" && v) out.push({ key: k, text: v });
    else if (Array.isArray(v) && v.length > 0) out.push({ key: k, text: v.join(", ") });
  }
  return out;
}

/** Collapses whitespace and elides the middle of long expressions. */
export function shorten(text: string, max = 96): string {
  const t = text.replace(/\s+/g, " ").trim();
  if (t.length <= max) return t;
  const head = Math.ceil((max - 1) * 0.65);
  return `${t.slice(0, head)}…${t.slice(t.length - (max - 1 - head))}`;
}

/** Ancestors from the root down to (excluding) the node. */
export function ancestorsOf(plan: ParsedPlan, id: string): PlanNode[] {
  const out: PlanNode[] = [];
  let cur = plan.byId.get(id)?.parentId ?? null;
  while (cur) {
    const n = plan.byId.get(cur);
    if (!n) break;
    out.unshift(n);
    cur = n.parentId;
  }
  return out;
}

/** The scan relations at or below a node, nearest first. */
export function scanTargets(n: PlanNode, limit = 3): { schema: string | null; relation: string }[] {
  const out: { schema: string | null; relation: string }[] = [];
  const seen = new Set<string>();
  const queue: PlanNode[] = [n];
  while (queue.length && out.length < limit) {
    const cur = queue.shift() as PlanNode;
    if (cur.relation) {
      const key = `${cur.schema ?? ""}.${cur.relation}`;
      if (!seen.has(key)) {
        seen.add(key);
        out.push({ schema: cur.schema, relation: cur.relation });
      }
    }
    queue.push(...cur.children);
  }
  return out;
}

/** `schema.table`, omitting `public`, quoting identifiers that need it. */
export function qualifiedName(schema: string | null, relation: string): string {
  const q = (s: string) => (/^[a-z_][a-z0-9_$]*$/.test(s) ? s : `"${s.replace(/"/g, '""')}"`);
  return schema && schema !== "public" ? `${q(schema)}.${q(relation)}` : q(relation);
}

export function quoteIdent(s: string): string {
  return /^[a-z_][a-z0-9_$]*$/.test(s) ? s : `"${s.replace(/"/g, '""')}"`;
}
