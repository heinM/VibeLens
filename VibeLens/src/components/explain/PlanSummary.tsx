// Top of the plan view: headline numbers, a ribbon of where the time went, and the hot spots.
import type { ReactNode } from "react";
import { Flame } from "lucide-react";
import { cn } from "../../lib/cn";
import { hotSpots, nodeTitle, type ParsedPlan, type PlanNode } from "./planModel";
import { formatBlocks, formatCost, formatMs, formatPercent, formatRows, heatBg, heatText } from "./planFormat";
import { NodeIcon } from "./nodeIcons";

function Stat({ label, value, sub, title, strong }: { label: string; value: ReactNode; sub?: ReactNode; title?: string; strong?: boolean }) {
  return (
    <div className="min-w-0 pr-5" title={title}>
      <div className="text-[11px] leading-4 text-muted">{label}</div>
      <div className={cn("whitespace-nowrap tabular-nums leading-6 text-fg", strong ? "text-[18px] font-semibold" : "text-[15px] font-medium")}>{value}</div>
      {sub && <div className="whitespace-nowrap text-[10.5px] leading-4 text-muted">{sub}</div>}
    </div>
  );
}

/** One segment per node holding at least 1% of the total, in execution-tree order. */
function Ribbon({ plan, selectedId, onSelect }: { plan: ParsedPlan; selectedId: string | null; onSelect: (id: string) => void }) {
  const segs = plan.nodes.filter((n) => n.share >= 0.01);
  const used = segs.reduce((a, n) => a + n.share, 0);
  return (
    <div className="flex h-2.5 w-full gap-px overflow-hidden rounded-full bg-panel-2" role="group" aria-label={`${plan.analyzed ? "Runtime" : "Cost"} by node`}>
      {segs.map((n) => {
        const { type, target } = nodeTitle(n);
        return (
          <button
            key={n.id}
            type="button"
            tabIndex={-1}
            onClick={() => onSelect(n.id)}
            title={`${type}${target ? ` on ${target}` : ""}: ${formatPercent(n.share)}`}
            style={{ flexGrow: n.share, flexBasis: 0 }}
            className={cn("min-w-[3px] outline-none transition-[filter] hover:brightness-125", heatBg(n.share), selectedId === n.id && "ring-2 ring-inset ring-fg")}
          />
        );
      })}
      {used < 0.999 && <span style={{ flexGrow: Math.max(0, 1 - used), flexBasis: 0 }} />}
    </div>
  );
}

function HotChip({ node, rank, selected, onSelect }: { node: PlanNode; rank: number; selected: boolean; onSelect: (id: string) => void }) {
  const { type, target } = nodeTitle(node);
  return (
    <button
      type="button"
      onClick={() => onSelect(node.id)}
      aria-pressed={selected}
      title={`Hot spot ${rank}: ${type}${target ? ` on ${target}` : ""}, ${formatPercent(node.share)} of ${node.analyzed ? "runtime" : "cost"}`}
      className={cn(
        "inline-flex h-7 max-w-[300px] items-center gap-1.5 rounded-md border bg-bg pl-1 pr-2 text-[12px] outline-none transition-colors hover:border-accent/60 focus-visible:ring-2 focus-visible:ring-accent/60",
        selected ? "border-accent/70 bg-accent/10" : "border-border",
      )}
    >
      <NodeIcon nodeType={node.nodeType} className="size-5 [&>svg]:size-3" />
      <span className="truncate font-medium text-fg">{type}</span>
      {(node.relation ?? node.indexName ?? node.cteName) && <span className="truncate font-mono text-[11px] text-muted">{node.relation ?? node.indexName ?? node.cteName}</span>}
      <span className={cn("ml-0.5 shrink-0 font-semibold tabular-nums", heatText(node.share))}>{formatPercent(node.share)}</span>
    </button>
  );
}

export function PlanSummary({ plan, selectedId, onSelect, actions }: { plan: ParsedPlan; selectedId: string | null; onSelect: (id: string) => void; actions?: ReactNode }) {
  const hs = hotSpots(plan, 3);
  const b = plan.buffers;
  return (
    <div className="shrink-0 border-b border-border bg-panel px-3 pb-2.5 pt-2">
      <div className="flex flex-wrap items-start gap-y-2">
        {plan.analyzed ? (
          <Stat label="Execution time" value={formatMs(plan.executionTime)} strong />
        ) : (
          <Stat label="Estimated only" value="No timings" strong sub="Plain EXPLAIN: nothing was run" title="Run EXPLAIN (ANALYZE) to see actual time and row counts" />
        )}
        <Stat label="Total cost" value={formatCost(plan.totalCost)} title="Planner's cost units for the whole plan" />
        {plan.analyzed && plan.planningTime !== null && <Stat label="Planning" value={formatMs(plan.planningTime)} />}
        <Stat label={plan.analyzed ? "Rows returned" : "Rows (planned)"} value={formatRows(plan.rows)} />
        {b && (
          <Stat
            label="Buffers"
            value={
              <>
                {formatRows(b.sharedHit)} <span className="text-[11px] font-normal text-muted">hit</span> · {formatRows(b.sharedRead)} <span className="text-[11px] font-normal text-muted">read</span>
              </>
            }
            sub={`${formatBlocks(b.sharedHit)} cached, ${formatBlocks(b.sharedRead)} from disk${b.tempWritten > 0 ? `, ${formatBlocks(b.tempWritten)} temp written` : ""}`}
            title="Shared buffer blocks (8 kB each) touched by the whole plan"
          />
        )}
        {plan.jitMs !== null && <Stat label="JIT" value={formatMs(plan.jitMs)} />}
        <Stat label="Nodes" value={plan.nodes.length} />
        {actions && <div className="ml-auto flex items-center gap-2 self-center pl-3">{actions}</div>}
      </div>

      <div className="mt-2.5 flex items-center gap-3">
        <span className="w-[88px] shrink-0 text-[11px] text-muted">{plan.analyzed ? "Time by node" : "Cost by node"}</span>
        <Ribbon plan={plan} selectedId={selectedId} onSelect={onSelect} />
      </div>

      {hs.length > 0 && (
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          <span className="flex w-[88px] shrink-0 items-center gap-1 text-[11px] text-muted">
            <Flame className="size-3 text-warn" /> Hot spots
          </span>
          {hs.map((n, i) => (
            <HotChip key={n.id} node={n} rank={i + 1} selected={selectedId === n.id} onSelect={onSelect} />
          ))}
        </div>
      )}
    </div>
  );
}
