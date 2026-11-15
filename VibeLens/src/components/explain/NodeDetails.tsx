// Everything known about one plan node: computed numbers, full conditions, and the raw property list.
import { useMemo } from "react";
import { NodeIcon } from "./nodeIcons";
import { CopyIconButton } from "../ui/formKit";
import { nodeConditions, nodeTitle, type ParsedPlan, type PlanNode } from "./planModel";
import type { Insight } from "./planInsights";
import { formatCost, formatMs, formatPercent, formatPropValue, formatRows } from "./planFormat";
import { MisestimateBadge } from "./PlanTree";
import { cn } from "../../lib/cn";

function Fact({ label, value, hint }: { label: string; value: React.ReactNode; hint?: string }) {
  return (
    <div className="min-w-0" title={hint}>
      <div className="text-[11px] text-muted">{label}</div>
      <div className="truncate text-[13px] font-medium tabular-nums text-fg">{value}</div>
    </div>
  );
}

export function NodeDetails({ plan, node, insights, onSelectInsight }: { plan: ParsedPlan; node: PlanNode; insights: Insight[]; onSelectInsight: (i: Insight) => void }) {
  const { type, target } = nodeTitle(node);
  const conds = nodeConditions(node);
  const analyzed = plan.analyzed;
  const mine = insights.filter((i) => i.nodeId === node.id);
  const json = useMemo(() => JSON.stringify(node.props, null, 2), [node]);
  const entries = Object.entries(node.props);

  return (
    <div className="space-y-4 p-3">
      <div className="flex items-start gap-2">
        <NodeIcon nodeType={node.nodeType} className="mt-0.5 size-7" />
        <div className="min-w-0 flex-1">
          <h3 className="text-[14px] font-semibold leading-5 text-fg">{type}</h3>
          {target && <p className="selectable break-all font-mono text-[12px] text-muted">{target}</p>}
        </div>
        <CopyIconButton text={json} label="Copy node JSON" />
      </div>

      <div className="grid grid-cols-2 gap-x-4 gap-y-3 rounded-lg border border-border bg-panel-2/40 p-3">
        <Fact label={analyzed ? "Share of runtime" : "Share of cost"} value={formatPercent(node.share)} />
        {analyzed ? (
          <>
            <Fact label="Self time" value={formatMs(node.exclusiveTime)} hint="This node's time without its children" />
            <Fact label="Time including children" value={formatMs(node.inclusiveTime)} hint="Per-loop time × loops, divided across parallel processes" />
            <Fact label="Loops" value={node.loops.toLocaleString("en-US")} hint={node.parallelFactor > 1 ? `Shared by ${node.parallelFactor} parallel processes` : undefined} />
          </>
        ) : (
          <>
            <Fact label="Self cost" value={formatCost(node.exclusiveCost)} />
            <Fact label="Total cost" value={formatCost(node.totalCost)} />
            <Fact label="Startup cost" value={formatCost(node.startupCost)} />
          </>
        )}
        <Fact label="Rows planned" value={formatRows(node.planRows)} hint="Per loop" />
        {analyzed && (
          <Fact
            label="Rows actual"
            value={
              <span className="flex items-center gap-1.5">
                {formatRows(node.actualRows)}
                <MisestimateBadge node={node} />
              </span>
            }
            hint="Per loop"
          />
        )}
        {node.parallelFactor > 1 && <Fact label="Parallel processes" value={node.parallelFactor} />}
      </div>

      {mine.length > 0 && (
        <section>
          <h4 className="mb-1 text-[12px] font-semibold text-fg">Findings for this node</h4>
          <ul className="space-y-1">
            {mine.map((i) => (
              <li key={i.id}>
                <button type="button" onClick={() => onSelectInsight(i)} className="w-full rounded-md border border-border bg-bg px-2 py-1.5 text-left text-[12px] leading-snug outline-none hover:border-accent/50 focus-visible:ring-2 focus-visible:ring-accent/60">
                  {i.title}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}

      {conds.length > 0 && (
        <section>
          <h4 className="mb-1 text-[12px] font-semibold text-fg">Conditions</h4>
          <dl className="space-y-1.5">
            {conds.map((c) => (
              <div key={c.key}>
                <dt className="text-[11px] text-muted">{c.key}</dt>
                <dd className="selectable whitespace-pre-wrap break-words rounded border border-border bg-bg px-2 py-1 font-mono text-[11.5px] leading-[1.5] text-fg">{c.text}</dd>
              </div>
            ))}
          </dl>
        </section>
      )}

      <section>
        <h4 className="mb-1 text-[12px] font-semibold text-fg">All properties</h4>
        <dl className="overflow-hidden rounded-lg border border-border">
          {entries.map(([k, v], i) => {
            const text = formatPropValue(v);
            const multiline = text.includes("\n") || text.length > 44;
            return (
              <div key={k} className={cn("px-2.5 py-1.5", multiline ? "block" : "grid grid-cols-[minmax(0,42%)_1fr] gap-3", i > 0 && "border-t border-border/60", i % 2 === 1 && "bg-panel-2/30")}>
                <dt className="break-words text-[11.5px] text-muted">{k}</dt>
                <dd className={cn("selectable break-words font-mono text-[11.5px] leading-[1.5] text-fg", multiline ? "mt-0.5 whitespace-pre-wrap" : "text-right")}>{text}</dd>
              </div>
            );
          })}
        </dl>
      </section>
    </div>
  );
}
