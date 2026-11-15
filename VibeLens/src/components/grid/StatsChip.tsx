// Engine statistics for one statement, shown in the results status line: engine badge, data scanned,
// the Athena cost estimate and the Athena query id (with copy).
import { Check, Copy, Info } from "lucide-react";
import type { StatementStats } from "../../hooks/useQueryRunner";
import { cn } from "../../lib/cn";
import { useCopy } from "../ui/formKit";
import { Chip } from "../ui/wsKit";
import { statsView } from "./statsFormat";

export function StatsChip({ stats, className }: { stats: StatementStats | null | undefined; className?: string }) {
  if (!stats) return null;
  const v = statsView(stats);
  return (
    <div className={cn("flex min-w-0 items-center gap-2 text-xs", className)} data-testid="result-stats">
      <Chip tone={v.engine.tone} title={v.engine.title}>
        {v.engine.label}
      </Chip>
      {v.scanned && (
        <span className="shrink-0 whitespace-nowrap tabular-nums text-muted" title={v.scanned.title}>
          {v.scanned.text}
        </span>
      )}
      {v.cost && (
        <span className="inline-flex shrink-0 cursor-help items-center gap-1 whitespace-nowrap tabular-nums text-fg" title={v.cost.title} aria-label={`${v.cost.text}. ${v.cost.title}`}>
          {v.cost.text}
          <Info className="size-3 text-muted" aria-hidden />
        </span>
      )}
      {v.queryId && <QueryId short={v.queryId.short} full={v.queryId.full} />}
    </div>
  );
}

/** The engine's query id, shortened, with a copy button. Hidden when the results pane is narrow. */
function QueryId({ short, full }: { short: string; full: string }) {
  const { copied, copy } = useCopy();
  return (
    <button
      type="button"
      onClick={() => void copy(full)}
      title={copied ? "Copied" : `Copy query id\n${full}`}
      aria-label={copied ? "Query id copied" : `Copy query id ${full}`}
      className="hidden shrink-0 items-center gap-1 rounded px-1 font-mono text-[11px] text-muted outline-none hover:text-fg focus-visible:ring-2 focus-visible:ring-accent/60 @[780px]:inline-flex"
    >
      {short}
      {copied ? <Check className="size-3 text-ok" /> : <Copy className="size-3" />}
    </button>
  );
}
