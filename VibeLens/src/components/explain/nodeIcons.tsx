import { ArrowDownUp, Boxes, Combine, Database, DatabaseZap, FunctionSquare, GitMerge, Hash, Layers, ListFilter, ListOrdered, Repeat, Rows3, ScanSearch, Sigma, Split, Table2, Workflow } from "lucide-react";
import type { ComponentType } from "react";
import { cn } from "../../lib/cn";

type Icon = ComponentType<{ className?: string }>;
export type NodeFamily = "scan" | "join" | "order" | "aggregate" | "parallel" | "other";

interface IconSpec {
  icon: Icon;
  family: NodeFamily;
}

const SPECS: [RegExp, IconSpec][] = [
  [/^(Parallel )?Seq Scan$/, { icon: Table2, family: "scan" }],
  [/^(Index Scan|Index Only Scan)$/, { icon: ScanSearch, family: "scan" }],
  [/^Bitmap Index Scan$/, { icon: ScanSearch, family: "scan" }],
  [/^Bitmap Heap Scan$/, { icon: Layers, family: "scan" }],
  [/^(Hash Join|Merge Join)$/, { icon: GitMerge, family: "join" }],
  [/^Nested Loop$/, { icon: Repeat, family: "join" }],
  [/^Hash$/, { icon: Hash, family: "join" }],
  [/^(Sort|Incremental Sort)$/, { icon: ArrowDownUp, family: "order" }],
  [/^Limit$/, { icon: ListOrdered, family: "order" }],
  [/^(Aggregate|GroupAggregate|HashAggregate|MixedAggregate)$/, { icon: Sigma, family: "aggregate" }],
  [/^(WindowAgg)$/, { icon: Rows3, family: "aggregate" }],
  [/^Unique$/, { icon: ListFilter, family: "aggregate" }],
  [/^Gather( Merge)?$/, { icon: Split, family: "parallel" }],
  [/^CTE Scan$/, { icon: Workflow, family: "scan" }],
  [/^(Append|Merge Append)$/, { icon: Combine, family: "other" }],
  [/^(Materialize|Memoize)$/, { icon: Boxes, family: "other" }],
  [/^(Subquery Scan|Function Scan|Values Scan|Table Function Scan|Named Tuplestore Scan|WorkTable Scan)$/, { icon: FunctionSquare, family: "scan" }],
  [/^(ModifyTable|Insert|Update|Delete)$/, { icon: DatabaseZap, family: "other" }],
];

export function specFor(nodeType: string): IconSpec {
  for (const [re, spec] of SPECS) if (re.test(nodeType)) return spec;
  return { icon: Database, family: "other" };
}

const FAMILY_TONE: Record<NodeFamily, string> = {
  scan: "text-accent bg-accent/10 border-accent/25",
  join: "text-fg bg-panel-2 border-border",
  order: "text-warn bg-warn/10 border-warn/25",
  aggregate: "text-fg bg-panel-2 border-border",
  parallel: "text-accent bg-accent/10 border-accent/25",
  other: "text-muted bg-panel-2 border-border",
};

export function NodeIcon({ nodeType, className }: { nodeType: string; className?: string }) {
  const { icon: I, family } = specFor(nodeType);
  return (
    <span className={cn("grid size-6 shrink-0 place-items-center rounded-md border", FAMILY_TONE[family], className)} aria-hidden>
      <I className="size-3.5" />
    </span>
  );
}
