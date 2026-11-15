// Rule-based findings about a parsed plan. Pure and deterministic: same plan, same insights.
// Every rule names the evidence (numbers from the plan) and the cost of acting on it.
import { filterColumns, joinColumnsFor } from "./planConditions";
import { type ParsedPlan, type PlanNode, qualifiedName, quoteIdent, scanTargets } from "./planModel";

export type InsightRule = "seqScan" | "misestimate" | "sortSpill" | "hashSpill" | "nestedLoop" | "rowsRemoved" | "heapFetches" | "workers";
export type InsightSeverity = "info" | "warn" | "high";

export interface Insight {
  id: string;
  rule: InsightRule;
  severity: InsightSeverity;
  /** The node the finding is about (click to select). */
  nodeId: string;
  title: string;
  /** What was seen and what to do about it. */
  detail: string;
  /** What acting on it costs: write overhead, locks, memory. */
  cost?: string;
  /** SQL the user can run or review. */
  fix?: string;
}

export interface InsightOptions {
  /** Seq Scans below this many rows scanned are never flagged. */
  seqScanMinRows?: number;
  nestedLoopMinLoops?: number;
  removedMinRows?: number;
}

const DEFAULTS: Required<InsightOptions> = { seqScanMinRows: 10_000, nestedLoopMinLoops: 1000, removedMinRows: 1000 };

const fmt = (n: number) => {
  const r = Math.round(n);
  return Math.abs(r) >= 1e6 ? `${(r / 1e6).toFixed(r >= 1e7 ? 0 : 1)}M` : r.toLocaleString("en-US");
};
const pct = (x: number) => `${x >= 0.995 ? 100 : Math.round(x * 100)}%`;
const ms = (x: number) => (x >= 1000 ? `${(x / 1000).toFixed(2)} s` : x >= 10 ? `${Math.round(x)} ms` : `${x.toFixed(1)} ms`);
const numProp = (n: PlanNode, k: string): number | null => {
  const v = n.props[k];
  return typeof v === "number" && Number.isFinite(v) ? v : null;
};
const strProp = (n: PlanNode, k: string): string | null => (typeof n.props[k] === "string" ? (n.props[k] as string) : null);
const isSeqScan = (n: PlanNode) => n.nodeType === "Seq Scan" || n.nodeType === "Parallel Seq Scan";
const tableOf = (n: PlanNode) => (n.relation ? qualifiedName(n.schema, n.relation) : null);
const label = (n: PlanNode) => (n.relation ? `${n.nodeType} on ${n.relation}` : n.nodeType);

const SEVERITY_ORDER: Record<InsightSeverity, number> = { high: 0, warn: 1, info: 2 };

export function buildInsights(plan: ParsedPlan, options: InsightOptions = {}): Insight[] {
  const o = { ...DEFAULTS, ...options };
  const out: Insight[] = [];
  const covered = new Set<string>();

  // Nested loops first: they claim their inner Seq Scan so it isn't reported twice.
  for (const n of plan.nodes) nestedLoop(n, plan, o, out, covered);
  for (const n of plan.nodes) {
    seqScan(n, plan, o, out, covered);
    misestimate(n, out);
    sortSpill(n, out);
    hashSpill(n, out);
    rowsRemoved(n, plan, o, out, covered);
    heapFetches(n, out);
    workers(n, out);
  }
  const byNode = (i: Insight) => plan.byId.get(i.nodeId)?.share ?? 0;
  return out.sort((a, b) => SEVERITY_ORDER[a.severity] - SEVERITY_ORDER[b.severity] || byNode(b) - byNode(a));
}

// ---- Seq Scan on a large relation with a selective filter -------------------------------

function seqScan(n: PlanNode, plan: ParsedPlan, o: Required<InsightOptions>, out: Insight[], covered: Set<string>) {
  if (!isSeqScan(n) || !n.relation || covered.has(n.id)) return;
  const filter = strProp(n, "Filter");
  if (!filter) return;
  const removed = numProp(n, "Rows Removed by Filter");

  let evidence: string;
  let severity: InsightSeverity = "warn";
  if (plan.analyzed && !n.neverExecuted) {
    if (removed === null) return;
    const kept = n.totalRows;
    const dropped = removed * n.loops;
    const scanned = kept + dropped;
    if (scanned < o.seqScanMinRows || dropped / scanned < 0.9) return;
    evidence = `It read ${fmt(scanned)} rows and kept ${fmt(kept)} (${pct(dropped / scanned)} discarded)${n.exclusiveTime ? `, taking ${ms(n.exclusiveTime)} (${pct(n.share)} of runtime)` : ""}.`;
    if (n.share >= 0.2 || scanned >= 1_000_000) severity = "high";
  } else {
    // Estimates only: a pricey scan expected to return little.
    if (n.totalCost < 1000 || n.planRows > 1000) return;
    evidence = `The planner prices it at ${fmt(n.totalCost)} cost units for about ${fmt(n.planRows)} rows out. Run EXPLAIN ANALYZE to confirm how many rows it actually discards.`;
    if (n.totalCost >= 50_000) severity = "high";
  }

  const cols = filterColumns(filter, n.alias);
  const tbl = tableOf(n) as string;
  let title: string;
  let fix: string | undefined;
  if (cols.index.length > 0) {
    const list = cols.index.map(quoteIdent).join(", ");
    title = `Seq Scan on ${n.relation} with a selective filter: consider an index on (${cols.index.join(", ")})`;
    fix = `CREATE INDEX CONCURRENTLY ON ${tbl} (${list});`;
  } else if (cols.expressions.length > 0) {
    title = `Seq Scan on ${n.relation} filters on an expression: consider an expression index on (${cols.expressions[0]})`;
    fix = `CREATE INDEX CONCURRENTLY ON ${tbl} (${cols.expressions[0]});`;
  } else {
    title = `Seq Scan on ${n.relation} discards most rows it reads: an index on the filtered columns may help`;
  }
  const order = cols.equality.length > 0 && cols.range.length > 0 ? " Equality columns come first, then the range column, so the index can narrow on both." : "";
  out.push({
    id: `seqScan:${n.id}`,
    rule: "seqScan",
    severity,
    nodeId: n.id,
    title,
    detail: `${evidence}${order}`,
    cost: `One more index to maintain on every write to ${n.relation}. CONCURRENTLY avoids blocking writes but builds slower and cannot run inside a transaction. If an index on these columns already exists, check that the filter does not wrap the column in a cast or function.`,
    fix,
  });
}

// ---- Row-count misestimates --------------------------------------------------------------

function misestimate(n: PlanNode, out: Insight[]) {
  const m = n.misestimate;
  if (!m || n.neverExecuted) return;
  // Report where the error starts, not every ancestor that inherits it.
  if (n.children.some((c) => c.misestimate && c.misestimate.direction === m.direction)) return;
  if (Math.max(n.planRows, n.actualRows ?? 0) < 100) return;

  const targets = scanTargets(n, 3);
  const tables = targets.map((t) => qualifiedName(t.schema, t.relation));
  const act = n.actualRows ?? 0;
  const dir = m.direction === "under" ? "fewer" : "more";
  const loopNote = n.loops > 1 ? " per loop" : "";
  const severity: InsightSeverity = m.severity === "mild" ? "warn" : "high";

  const stmts: string[] = tables.map((t) => `ANALYZE ${t};`);
  let extra = "";
  const filter = strProp(n, "Filter") ?? strProp(n, "Index Cond");
  const cols = filterColumns(filter, n.alias).index;
  if (n.relation && cols.length >= 2) {
    const stat = `${n.relation}_${cols.slice(0, 3).join("_")}_stx`.slice(0, 60);
    stmts.unshift(`CREATE STATISTICS ${quoteIdent(stat)} (dependencies, ndistinct) ON ${cols.map(quoteIdent).join(", ")} FROM ${tableOf(n)};`);
    extra = ` The filter combines ${cols.join(" and ")}: if they are correlated, the planner multiplies their selectivities and under-counts. CREATE STATISTICS teaches it the dependency.`;
  } else if (n.children.length > 0 && /Join|Nested Loop/.test(n.nodeType)) {
    extra = " For joins this is often correlated columns or skewed keys; extended statistics on the joined tables can help.";
  }
  const target = tables.length > 0 ? ` Stale statistics? ANALYZE ${tables[0]}${tables.length > 1 ? " (and the other tables below it)" : ""}.` : " Stale statistics?";
  out.push({
    id: `misestimate:${n.id}`,
    rule: "misestimate",
    severity,
    nodeId: n.id,
    title: `${label(n)}: expected ${fmt(n.planRows)} rows, got ${fmt(act)} (${fmt(m.factor)}× ${m.direction === "under" ? "more" : "fewer"} than planned)`,
    detail: `The planner expected ${dir} rows than it found${loopNote}, so join order and method above this node were chosen on wrong numbers.${target}${extra}`,
    cost: "ANALYZE samples the table: a short burst of I/O and a brief lock. On very large tables, raise default_statistics_target for the column first.",
    fix: stmts.length ? stmts.join("\n") : undefined,
  });
}

// ---- Spills to disk -----------------------------------------------------------------------

function mbFor(kb: number): number {
  // Disk usage understates the in-memory need (tuples are packed tighter on disk); leave headroom.
  const mb = Math.ceil((kb * 2) / 1024);
  return Math.max(8, Math.ceil(mb / 8) * 8);
}

function sortSpill(n: PlanNode, out: Insight[]) {
  if (n.nodeType !== "Sort" && n.nodeType !== "Incremental Sort") return;
  const method = strProp(n, "Sort Method") ?? "";
  const spaceType = strProp(n, "Sort Space Type");
  if (!/external/i.test(method) && spaceType !== "Disk") return;
  const kb = numProp(n, "Sort Space Used") ?? 0;
  const mb = mbFor(kb);
  const keys = Array.isArray(n.props["Sort Key"]) ? (n.props["Sort Key"] as string[]).join(", ") : null;
  out.push({
    id: `sortSpill:${n.id}`,
    rule: "sortSpill",
    severity: n.share >= 0.2 ? "high" : "warn",
    nodeId: n.id,
    title: `Sort spilled to disk (${method || "external"}, ${fmt(kb / 1024)} MB): work_mem is too small for this sort`,
    detail: `The sort did not fit in work_mem and wrote ${fmt(kb / 1024)} MB of temporary files${n.exclusiveTime ? `, ${pct(n.share)} of runtime` : ""}. Try work_mem around ${mb}MB for this query only. ${keys ? `An index matching the sort key (${keys}) would remove the sort. ` : ""}A LIMIT lets Postgres use a top-N heapsort instead.`,
    cost: `work_mem applies per sort or hash node per backend, so raising it globally multiplies memory across concurrent queries; SET LOCAL inside a transaction confines it to this one.${keys ? " An ordering index adds write overhead." : ""}`,
    fix: `SET LOCAL work_mem = '${mb}MB';`,
  });
}

function hashSpill(n: PlanNode, out: Insight[]) {
  if (n.nodeType === "Hash") {
    const batches = numProp(n, "Hash Batches");
    if (batches === null || batches <= 1) return;
    const orig = numProp(n, "Original Hash Batches");
    const peakKb = numProp(n, "Peak Memory Usage");
    out.push({
      id: `hashSpill:${n.id}`,
      rule: "hashSpill",
      severity: n.share >= 0.2 ? "high" : "warn",
      nodeId: n.id,
      title: `Hash split into ${fmt(batches)} batches: the build side did not fit in memory`,
      detail: `Postgres planned ${fmt(orig ?? 1)} batch${(orig ?? 1) === 1 ? "" : "es"} and needed ${fmt(batches)}, writing the rest to temporary files${peakKb ? ` (peak memory ${fmt(peakKb)} kB)` : ""}. Raise work_mem (or hash_mem_multiplier) for this query, or shrink the build side with a more selective filter.`,
      cost: "Memory is per hash node per backend, so prefer SET LOCAL over a global change.",
      fix: `SET LOCAL work_mem = '${peakKb ? mbFor(peakKb * batches) : 64}MB';`,
    });
  } else if (n.nodeType === "Aggregate" && n.strategy === "Hashed") {
    const disk = numProp(n, "Disk Usage");
    if (!disk || disk <= 0) return;
    out.push({
      id: `hashSpill:${n.id}`,
      rule: "hashSpill",
      severity: "warn",
      nodeId: n.id,
      title: `HashAggregate spilled ${fmt(disk / 1024)} MB to disk`,
      detail: `The grouping did not fit in memory (${fmt(numProp(n, "HashAgg Batches") ?? 0)} batches). Raise work_mem or hash_mem_multiplier for this query, or group on fewer or narrower columns.`,
      cost: "Memory is per node per backend, so prefer SET LOCAL over a global change.",
      fix: `SET LOCAL work_mem = '${mbFor(disk)}MB';`,
    });
  }
}

/** Looks through nodes that merely cache or reorder their single child. */
function throughWrappers(n: PlanNode): PlanNode {
  let cur = n;
  while (["Materialize", "Memoize", "Sort", "Hash"].includes(cur.nodeType) && cur.children.length === 1) cur = cur.children[0];
  return cur;
}

// ---- Nested loops with many iterations -----------------------------------------------------

function nestedLoop(n: PlanNode, plan: ParsedPlan, o: Required<InsightOptions>, out: Insight[], covered: Set<string>) {
  if (n.nodeType !== "Nested Loop" || n.children.length < 2) return;
  const inner = n.children.find((c) => c.parentRel === "Inner") ?? n.children[1];
  const outer = n.children.find((c) => c !== inner) ?? n.children[0];
  const iterations = plan.analyzed ? inner.loops : outer.planRows;
  if (iterations < o.nestedLoopMinLoops) return;

  const innerShare = plan.analyzed && plan.metricTotal > 0 ? (inner.inclusiveTime ?? 0) / plan.metricTotal : 0;
  const scan = throughWrappers(inner);
  const innerIsSeq = isSeqScan(scan);
  // Many iterations of a cheap index probe are fine; flag them only when they dominate or are very many.
  if (!innerIsSeq && plan.analyzed && innerShare < 0.15 && iterations < 100_000) return;
  if (!innerIsSeq && !plan.analyzed && iterations < 10_000) return;

  covered.add(inner.id);
  covered.add(scan.id);
  covered.add(n.id);
  const joinCols = innerIsSeq ? joinColumnsFor(strProp(n, "Join Filter") ?? strProp(scan, "Filter"), scan.alias ?? scan.relation) : [];
  const joinRemoved = numProp(n, "Rows Removed by Join Filter");
  const evidence = plan.analyzed
    ? `The inner side runs ${fmt(inner.loops)} times${innerShare > 0 ? ` and accounts for ${pct(innerShare)} of runtime` : ""}${joinRemoved ? `; the join filter then discards ${fmt(joinRemoved * n.loops)} pairs` : ""}.`
    : `The planner expects the outer side to return about ${fmt(outer.planRows)} rows, so the inner side would run that many times.`;

  let title = `Nested Loop runs its inner side ${fmt(iterations)} times`;
  let fix: string | undefined;
  let advice: string;
  let cost: string | undefined;
  if (innerIsSeq && scan.relation) {
    const cols = joinCols.length ? joinCols : filterColumns(strProp(scan, "Filter"), scan.alias).index;
    title = `Nested Loop re-reads ${scan.relation} ${fmt(iterations)} times: add an index on the join column`;
    if (cols.length) fix = `CREATE INDEX CONCURRENTLY ON ${tableOf(scan)} (${cols.map(quoteIdent).join(", ")});`;
    advice = `Each iteration scans ${scan.relation} again${scan === inner ? "" : ` (${inner.nodeType} only caches the rows)`}. ${cols.length ? `An index on (${cols.join(", ")}) turns each pass into a probe.` : "An index on the join column turns each pass into a probe."}`;
    cost = `Extra write overhead and storage on ${scan.relation}.`;
  } else {
    advice = `Check the join order and row estimates: a hash or merge join often wins when the outer side is this large. If the outer estimate is wrong, ANALYZE the tables under it.`;
    cost = "To compare plans, SET LOCAL enable_nestloop = off is a diagnostic only: it affects every join in the query, so don't ship it.";
  }
  out.push({
    id: `nestedLoop:${n.id}`,
    rule: "nestedLoop",
    severity: innerIsSeq || innerShare >= 0.3 ? "high" : "warn",
    nodeId: n.id,
    title,
    detail: `${evidence} ${advice}`,
    cost,
    fix,
  });
}

// ---- Rows Removed by Filter >> rows returned ------------------------------------------------

function rowsRemoved(n: PlanNode, plan: ParsedPlan, o: Required<InsightOptions>, out: Insight[], covered: Set<string>) {
  if (!plan.analyzed || n.neverExecuted || covered.has(n.id)) return;
  if (out.some((i) => i.nodeId === n.id && i.rule === "seqScan")) return;
  const kinds: [string, string][] = [
    ["Rows Removed by Filter", "Filter"],
    ["Rows Removed by Join Filter", "Join Filter"],
    ["Rows Removed by Index Recheck", "Recheck Cond"],
  ];
  for (const [prop, condKey] of kinds) {
    const per = numProp(n, prop);
    if (per === null) continue;
    const removed = per * n.loops;
    const kept = n.totalRows;
    if (removed < o.removedMinRows || removed < 10 * Math.max(kept, 1)) continue;
    const cond = strProp(n, condKey);
    const cols = filterColumns(cond, n.alias).index;
    const isIndexScan = /Index/.test(n.nodeType);
    let advice: string;
    let cost: string | undefined;
    let fix: string | undefined;
    if (prop === "Rows Removed by Index Recheck") {
      advice = "A lossy bitmap scan rechecks every row on the matching pages. That usually means the bitmap exceeded work_mem; raise work_mem for this query or make the condition more selective.";
    } else if (prop === "Rows Removed by Join Filter") {
      advice = "The join produced many pairs that a post-join condition then threw away. Put an equality condition into the join (so a hash or merge join applies) or index the joined columns.";
    } else if (isIndexScan && cols.length && n.relation) {
      advice = `The index finds candidates but the filter discards most of them after fetching the row. Add ${cols.join(", ")} to the index, or create a partial index with the filter as its WHERE clause.`;
      cost = "A wider or extra index to maintain on every write.";
      fix = `CREATE INDEX CONCURRENTLY ON ${tableOf(n)} (${cols.map(quoteIdent).join(", ")});`;
    } else {
      advice = "Filtering happens after the rows are read. Move the condition into an index, or filter earlier in the query.";
    }
    out.push({
      id: `rowsRemoved:${n.id}:${prop}`,
      rule: "rowsRemoved",
      severity: n.share >= 0.2 ? "high" : "warn",
      nodeId: n.id,
      title: `${label(n)}: ${fmt(removed)} rows removed to return ${fmt(kept)}`,
      detail: `${prop} is ${fmt(removed)}${n.loops > 1 ? ` across ${fmt(n.loops)} loops` : ""} against ${fmt(kept)} rows returned. ${advice}`,
      cost,
      fix,
    });
    return;
  }
}

// ---- Smaller findings ----------------------------------------------------------------------

function heapFetches(n: PlanNode, out: Insight[]) {
  if (n.nodeType !== "Index Only Scan" || !n.relation) return;
  const fetches = numProp(n, "Heap Fetches");
  if (fetches === null || fetches < 1000 || fetches < 0.5 * Math.max(n.totalRows, 1)) return;
  out.push({
    id: `heapFetches:${n.id}`,
    rule: "heapFetches",
    severity: "info",
    nodeId: n.id,
    title: `Index Only Scan on ${n.relation} still visited the heap ${fmt(fetches)} times`,
    detail: `The visibility map is out of date, so most rows needed a table lookup anyway. VACUUM updates it.`,
    cost: "VACUUM reads the table and competes for I/O while it runs; autovacuum settings for this table may need tightening.",
    fix: `VACUUM ${tableOf(n)};`,
  });
}

function workers(n: PlanNode, out: Insight[]) {
  if (!/^Gather/.test(n.nodeType)) return;
  const planned = n.workersPlanned;
  const launched = n.workersLaunched;
  if (planned === null || launched === null || launched >= planned) return;
  out.push({
    id: `workers:${n.id}`,
    rule: "workers",
    severity: "info",
    nodeId: n.id,
    title: `Only ${launched} of ${planned} parallel workers started`,
    detail: `The server ran out of background worker slots (max_parallel_workers or max_worker_processes) while this query ran, so it got less parallelism than planned. Timings here are slower than the plan intended.`,
  });
}
