// Rule-based findings. Each one names its evidence and its cost, and can jump to the node it is about.
import { CircleCheck, Crosshair, Info, OctagonAlert, TriangleAlert } from "lucide-react";
import { cn } from "../../lib/cn";
import { CodeBlock } from "../agent/CodeView.tsx";
import type { Insight, InsightSeverity } from "./planInsights";
import { nodeTitle, type ParsedPlan } from "./planModel";
import { formatPercent } from "./planFormat";

const SEV: Record<InsightSeverity, { icon: typeof Info; tone: string; label: string }> = {
  high: { icon: OctagonAlert, tone: "text-danger", label: "High impact" },
  warn: { icon: TriangleAlert, tone: "text-warn", label: "Worth fixing" },
  info: { icon: Info, tone: "text-muted", label: "Good to know" },
};

export function InsightsList({ plan, insights, selectedId, onSelectNode }: { plan: ParsedPlan; insights: Insight[]; selectedId: string | null; onSelectNode: (id: string) => void }) {
  if (insights.length === 0) {
    const top = [...plan.nodes].sort((a, b) => b.share - a.share)[0];
    return (
      <div className="flex flex-col items-center gap-2 px-6 py-10 text-center">
        <CircleCheck className="size-6 text-ok" />
        <p className="text-[13px] font-medium text-fg">Nothing here needs attention</p>
        <p className="max-w-xs text-[12px] leading-snug text-muted">
          None of the checks fired: no large filtered scans, row-count misses, disk spills or runaway loops.
          {top && top.share > 0.5 ? ` ${nodeTitle(top).type} accounts for ${formatPercent(top.share)} of the ${plan.analyzed ? "runtime" : "cost"}, which is the place to look if it is still too slow.` : ""}
        </p>
      </div>
    );
  }
  return (
    <ul className="divide-y divide-border">
      {insights.map((i) => (
        <InsightItem key={i.id} insight={i} plan={plan} active={selectedId === i.nodeId} onSelectNode={onSelectNode} />
      ))}
    </ul>
  );
}

function InsightItem({ insight: i, plan, active, onSelectNode }: { insight: Insight; plan: ParsedPlan; active: boolean; onSelectNode: (id: string) => void }) {
  const sev = SEV[i.severity];
  const Icon = sev.icon;
  const node = plan.byId.get(i.nodeId);
  const where = node ? nodeTitle(node) : null;
  return (
    <li className={cn("px-3 py-3", active && "bg-accent/5")}>
      <div className="flex items-start gap-2">
        <Icon className={cn("mt-0.5 size-4 shrink-0", sev.tone)} aria-label={sev.label} />
        <div className="min-w-0 flex-1">
          <p className="text-[13px] font-medium leading-snug text-fg">{i.title}</p>
          {where && (
            <button
              type="button"
              onClick={() => onSelectNode(i.nodeId)}
              className="mt-1 inline-flex max-w-full items-center gap-1 rounded border border-border bg-panel-2 px-1.5 py-0.5 text-[11px] text-muted outline-none hover:border-accent/50 hover:text-fg focus-visible:ring-2 focus-visible:ring-accent/60"
            >
              <Crosshair className="size-3 shrink-0" />
              <span className="truncate">{where.type}{where.target ? ` · ${where.target}` : ""}</span>
            </button>
          )}
          <p className="mt-1.5 text-[12px] leading-[1.55] text-fg/80">{i.detail}</p>
          {i.cost && (
            <p className="mt-1 text-[12px] leading-[1.55] text-muted">
              <span className="font-medium text-fg/80">Cost: </span>
              {i.cost}
            </p>
          )}
          {i.fix && <CodeBlock lang="sql" code={i.fix} open={false} wrap />}
        </div>
      </div>
    </li>
  );
}
