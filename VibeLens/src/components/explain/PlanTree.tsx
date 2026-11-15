// The plan as an indented, collapsible tree. One row per node: what it is, where its time went,
// and whether the planner was right about row counts.
import { memo, useCallback, useEffect, useMemo, useRef } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { cn } from "../../lib/cn";
import { nodeConditions, nodeTitle, shorten, type ParsedPlan, type PlanNode } from "./planModel";
import { formatCost, formatFactor, formatMs, formatPercent, formatRows, heatBg, heatText } from "./planFormat";
import { NodeIcon } from "./nodeIcons";

const GRID = "grid grid-cols-[minmax(260px,1fr)_148px_84px_172px] items-stretch";
const INDENT = 16;

/** Pre-order list of the nodes that are visible given the collapsed set. */
export function visibleNodes(plan: ParsedPlan, collapsed: ReadonlySet<string>): PlanNode[] {
  const out: PlanNode[] = [];
  const walk = (n: PlanNode) => {
    out.push(n);
    if (!collapsed.has(n.id)) n.children.forEach(walk);
  };
  walk(plan.root);
  return out;
}

const BADGE: Record<"mild" | "serious" | "severe", string> = {
  mild: "border border-warn/50 bg-warn/10 text-warn",
  serious: "bg-warn text-bg",
  severe: "bg-danger text-white",
};

export function MisestimateBadge({ node }: { node: PlanNode }) {
  const m = node.misestimate;
  if (!m) return null;
  const word = m.direction === "under" ? "under" : "over";
  return (
    <span
      className={cn("inline-flex h-4 items-center rounded px-1 text-[10px] font-semibold leading-none tabular-nums", BADGE[m.severity])}
      title={`The planner estimated ${formatRows(node.planRows)} rows per loop and found ${formatRows(node.actualRows)}: ${word}estimated by ${Math.round(m.factor)}×`}
    >
      {formatFactor(m.factor)} {word}
    </span>
  );
}

export function PlanTree({
  plan,
  selectedId,
  collapsed,
  expandedConds,
  onSelect,
  onToggleCollapse,
  onToggleConds,
  onCollapseAll,
  onExpandAll,
  scrollNonce,
}: {
  plan: ParsedPlan;
  selectedId: string | null;
  collapsed: ReadonlySet<string>;
  expandedConds: ReadonlySet<string>;
  onSelect: (id: string) => void;
  onToggleCollapse: (id: string) => void;
  onToggleConds: (id: string) => void;
  onCollapseAll: () => void;
  onExpandAll: () => void;
  /** Bump to scroll the selected row into view (set by hot-spot and insight clicks). */
  scrollNonce: number;
}) {
  const rows = useMemo(() => visibleNodes(plan, collapsed), [plan, collapsed]);
  const scroller = useRef<HTMLDivElement>(null);
  const timeLabel = plan.analyzed ? "Self time" : "Self cost";

  useEffect(() => {
    if (!selectedId || scrollNonce === 0) return;
    const el = scroller.current?.querySelector<HTMLElement>(`[data-node-id="${selectedId}"]`);
    el?.scrollIntoView({ block: "nearest" });
  }, [scrollNonce, selectedId]);

  const focusRow = useCallback((id: string) => {
    requestAnimationFrame(() => {
      scroller.current?.querySelector<HTMLElement>(`[data-node-id="${id}"]`)?.focus({ preventScroll: false });
    });
  }, []);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const idx = rows.findIndex((r) => r.id === selectedId);
    const cur = idx >= 0 ? rows[idx] : null;
    const go = (n: PlanNode | undefined) => {
      if (!n) return;
      e.preventDefault();
      onSelect(n.id);
      focusRow(n.id);
    };
    switch (e.key) {
      case "ArrowDown":
        go(rows[Math.min(rows.length - 1, idx + 1)]);
        break;
      case "ArrowUp":
        go(rows[Math.max(0, idx - 1)]);
        break;
      case "Home":
        go(rows[0]);
        break;
      case "End":
        go(rows[rows.length - 1]);
        break;
      case "ArrowRight":
        if (!cur) break;
        if (cur.children.length && collapsed.has(cur.id)) {
          e.preventDefault();
          onToggleCollapse(cur.id);
        } else go(cur.children[0]);
        break;
      case "ArrowLeft":
        if (!cur) break;
        if (cur.children.length && !collapsed.has(cur.id)) {
          e.preventDefault();
          onToggleCollapse(cur.id);
        } else go(cur.parentId ? plan.byId.get(cur.parentId) : undefined);
        break;
      case "*":
        e.preventDefault();
        onExpandAll();
        break;
    }
  };

  const anyParents = plan.nodes.some((n) => n.children.length > 0);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div ref={scroller} className="min-h-0 flex-1 overflow-auto" onKeyDown={onKeyDown}>
        <div className="min-w-[700px]" role="tree" aria-label="Query plan">
          <div className={cn(GRID, "sticky top-0 z-10 border-b border-border bg-panel/95 text-[11px] text-muted backdrop-blur")} role="presentation">
            <div className="flex items-center gap-1 px-3 py-1.5">
              <span>Node</span>
              {anyParents && (
                <span className="ml-1 flex items-center gap-0.5">
                  <button type="button" onClick={onExpandAll} className="rounded px-1.5 py-0.5 outline-none hover:bg-panel-2 hover:text-fg focus-visible:ring-2 focus-visible:ring-accent/60">
                    Expand all
                  </button>
                  <button type="button" onClick={onCollapseAll} className="rounded px-1.5 py-0.5 outline-none hover:bg-panel-2 hover:text-fg focus-visible:ring-2 focus-visible:ring-accent/60">
                    Collapse all
                  </button>
                </span>
              )}
            </div>
            <div className="px-2 py-1.5">Share of {plan.analyzed ? "runtime" : "cost"}</div>
            <div className="px-2 py-1.5 text-right">{timeLabel}</div>
            <div className="px-3 py-1.5 text-right">{plan.analyzed ? "Rows: planned → actual" : "Rows (planned)"}</div>
          </div>

          {rows.map((n) => (
            <Row
              key={n.id}
              plan={plan}
              node={n}
              selected={n.id === selectedId}
              collapsed={collapsed.has(n.id)}
              condsOpen={expandedConds.has(n.id)}
              onSelect={onSelect}
              onToggleCollapse={onToggleCollapse}
              onToggleConds={onToggleConds}
            />
          ))}
        </div>
      </div>
    </div>
  );
}

const Row = memo(function Row({
  plan,
  node: n,
  selected,
  collapsed,
  condsOpen,
  onSelect,
  onToggleCollapse,
  onToggleConds,
}: {
  plan: ParsedPlan;
  node: PlanNode;
  selected: boolean;
  collapsed: boolean;
  condsOpen: boolean;
  onSelect: (id: string) => void;
  onToggleCollapse: (id: string) => void;
  onToggleConds: (id: string) => void;
}) {
  const { type, target } = nodeTitle(n);
  const conds = nodeConditions(n);
  const hasKids = n.children.length > 0;
  const analyzed = plan.analyzed;
  const selfValue = analyzed ? n.exclusiveTime : n.exclusiveCost;

  return (
    <div
      role="treeitem"
      aria-level={n.depth + 1}
      aria-selected={selected}
      aria-expanded={hasKids ? !collapsed : undefined}
      tabIndex={selected ? 0 : -1}
      data-node-id={n.id}
      onClick={() => onSelect(n.id)}
      className={cn(
        GRID,
        "cursor-default border-b border-border/40 outline-none transition-colors",
        selected ? "bg-accent/10 shadow-[inset_2px_0_0_var(--vg-accent)]" : "hover:bg-panel-2/60",
        "focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent/60",
        n.neverExecuted && "opacity-60",
      )}
    >
      {/* Node */}
      <div className="flex min-w-0 items-stretch pl-2 pr-2">
        {Array.from({ length: n.depth }, (_, i) => (
          <span key={i} aria-hidden className="shrink-0 border-l border-border/60" style={{ width: INDENT }} />
        ))}
        <div className="flex min-w-0 flex-1 items-start gap-1.5 py-1.5">
          {hasKids ? (
            <button
              type="button"
              tabIndex={-1}
              aria-label={collapsed ? `Expand ${type}` : `Collapse ${type}`}
              onClick={(e) => {
                e.stopPropagation();
                onToggleCollapse(n.id);
              }}
              className="mt-1 grid size-4 shrink-0 place-items-center rounded text-muted outline-none hover:bg-panel-2 hover:text-fg"
            >
              {collapsed ? <ChevronRight className="size-3.5" /> : <ChevronDown className="size-3.5" />}
            </button>
          ) : (
            <span className="size-4 shrink-0" />
          )}
          <NodeIcon nodeType={n.nodeType} className="mt-px" />
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 items-baseline gap-1.5 leading-5">
              <span className="shrink-0 text-[13px] font-semibold text-fg">{type}</span>
              {n.parentRel && n.parentRel !== "Outer" && n.parentRel !== "Inner" && <span className="shrink-0 text-[10.5px] text-muted">{n.parentRel}</span>}
              {n.joinType && n.joinType !== "Inner" && <span className="shrink-0 text-[10.5px] text-muted">{n.joinType.toLowerCase()}</span>}
              {target && <span className="truncate font-mono text-[12px] text-fg/80" title={target}>{target}</span>}
              {collapsed && n.descendants > 0 && (
                <span className="shrink-0 rounded border border-border bg-panel-2 px-1 text-[10px] leading-4 text-muted" title={`${n.descendants} nodes hidden`}>
                  +{n.descendants}
                </span>
              )}
            </div>
            {conds.length > 0 &&
              (condsOpen ? (
                <div className="mt-0.5 space-y-0.5">
                  {conds.map((c) => (
                    <div key={c.key} className="selectable break-words font-mono text-[11.5px] leading-[1.45] text-muted">
                      <span className="text-fg/60">{c.key}: </span>
                      {c.text}
                    </div>
                  ))}
                  <CondToggle open onClick={() => onToggleConds(n.id)} />
                </div>
              ) : (
                <div className="flex min-w-0 items-baseline gap-1.5 leading-[1.4]">
                  <span className="truncate font-mono text-[11.5px] text-muted" title={conds[0].text}>
                    <span className="text-fg/60">{conds[0].key}: </span>
                    {shorten(conds[0].text, 88)}
                  </span>
                  {(conds.length > 1 || conds[0].text.length > 52) && <CondToggle open={false} extra={conds.length - 1} onClick={() => onToggleConds(n.id)} />}
                </div>
              ))}
          </div>
        </div>
      </div>

      {/* Share bar */}
      <div className="flex items-center gap-2 px-2">
        {n.neverExecuted ? (
          <span className="text-[11px] italic text-muted">never run</span>
        ) : (
          <>
            <div className="h-2 min-w-0 flex-1 overflow-hidden rounded-full bg-panel-2" role="img" aria-label={`${formatPercent(n.share)} of ${analyzed ? "runtime" : "cost"}`}>
              <div className={cn("h-full rounded-full", heatBg(n.share))} style={{ width: `${Math.max(n.share > 0 ? 3 : 0, n.share * 100)}%` }} />
            </div>
            <span className={cn("w-8 shrink-0 text-right text-[11.5px] tabular-nums", heatText(n.share), n.share >= 0.1 && "font-semibold")}>{formatPercent(n.share)}</span>
          </>
        )}
      </div>

      {/* Self time / cost */}
      <div className="flex flex-col items-end justify-center px-2 text-right tabular-nums leading-[1.3]">
        <span className="text-[12px] text-fg">{analyzed ? formatMs(selfValue) : formatCost(selfValue)}</span>
        <span className="text-[10.5px] text-muted" title={analyzed ? "Including everything below this node" : "Planner's total cost including children"}>
          {analyzed ? `of ${formatMs(n.inclusiveTime)}` : `of ${formatCost(n.totalCost)}`}
        </span>
      </div>

      {/* Rows */}
      <div className="flex flex-col items-end justify-center gap-0.5 px-3 text-right leading-[1.3]">
        <span className="font-mono text-[12px] tabular-nums text-fg">
          {analyzed && !n.neverExecuted ? (
            <>
              <span className="text-muted">{formatRows(n.planRows)}</span>
              <span className="mx-1 text-muted/70">→</span>
              {formatRows(n.actualRows)}
            </>
          ) : (
            <span className="text-muted">{n.neverExecuted ? "–" : `~${formatRows(n.planRows)}`}</span>
          )}
        </span>
        {(n.misestimate || n.loops > 1) && (
          <span className="flex items-center gap-1.5">
            {n.loops > 1 && <span className="text-[10.5px] tabular-nums text-muted">×{formatRows(n.loops)} loops</span>}
            <MisestimateBadge node={n} />
          </span>
        )}
      </div>
    </div>
  );
});

function CondToggle({ open, extra = 0, onClick }: { open: boolean; extra?: number; onClick: () => void }) {
  return (
    <button
      type="button"
      tabIndex={-1}
      aria-expanded={open}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className="shrink-0 rounded px-1 text-[10.5px] text-accent outline-none hover:underline focus-visible:ring-2 focus-visible:ring-accent/60"
    >
      {open ? "less" : extra > 0 ? `+${extra} more` : "full"}
    </button>
  );
}
