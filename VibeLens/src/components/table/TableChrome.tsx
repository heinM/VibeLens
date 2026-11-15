// Presentational pieces of the table tab: header bar, edit bar and footer.
import type { ReactNode } from "react";
import { ChevronDown, ChevronLeft, ChevronRight, ChevronsLeft, Copy, FileCode2, Filter, Layers, Loader2, Lock, Plus, Redo2, RefreshCw, Table2, Trash2, Undo2, Eye } from "lucide-react";
import type { Environment, ObjectKind } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { Button, EnvBadge, IconButton } from "../ui";
import { Chip, Kbd, MiniSelect } from "../ui/wsKit";
import { MOD } from "../ui/platform";
import { formatCount, formatElapsed } from "../grid/cellFormat";
import type { PendingCounts } from "./pendingChanges";

export const PAGE_SIZES = [50, 100, 200, 500, 1000];

const KIND_LABEL: Record<ObjectKind, string> = {
  table: "table",
  partitionedTable: "partitioned table",
  view: "view",
  materializedView: "materialized view",
  foreignTable: "foreign table",
  sequence: "sequence",
  function: "function",
  procedure: "procedure",
};

/** 1,250,000 -> "1.25M" for the header badge. */
export function formatApprox(n: number): string {
  if (n >= 1e9) return `${+(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${+(n / 1e6).toFixed(2)}M`;
  if (n >= 1e4) return `${+(n / 1e3).toFixed(1)}k`;
  return formatCount(n);
}

export function TableHeaderBar({
  schema,
  name,
  kind,
  comment,
  estimatedRows,
  environment,
  loading,
  filterCount,
  filterOpen,
  onToggleFilter,
  onRefresh,
  onOpenSql,
  onCopyMenu,
}: {
  schema: string;
  name: string;
  kind: ObjectKind | null;
  comment: string | null;
  estimatedRows: number | null;
  environment: Environment | null;
  loading: boolean;
  filterCount: number;
  filterOpen: boolean;
  onToggleFilter: () => void;
  onRefresh: () => void;
  onOpenSql: () => void;
  onCopyMenu: (anchor: HTMLElement) => void;
}) {
  const Icon = kind === "view" || kind === "materializedView" ? Eye : kind === "partitionedTable" ? Layers : Table2;
  return (
    <div className="@container flex h-10 shrink-0 items-center gap-2 border-b border-border bg-panel px-3" title={comment ?? undefined}>
      <Icon className="size-4 shrink-0 text-muted" />
      <div className="flex min-w-0 items-baseline gap-0.5 font-mono text-[13px]">
        <span className="shrink-0 text-muted">{schema}.</span>
        <span className="truncate font-semibold text-fg">{name}</span>
      </div>
      {kind && <Chip>{KIND_LABEL[kind]}</Chip>}
      {estimatedRows !== null && (
        <span className="hidden shrink-0 text-xs tabular-nums text-muted @[520px]:inline" title="Planner estimate from pg_class.reltuples. Use Count in the footer for an exact figure.">
          ~{formatApprox(estimatedRows)} rows
        </span>
      )}
      {environment && environment !== "dev" && <EnvBadge env={environment} />}
      <div className="ml-auto flex shrink-0 items-center gap-1">
        <button
          type="button"
          onClick={onToggleFilter}
          aria-pressed={filterOpen}
          className={cn(
            "inline-flex h-7 items-center gap-1.5 rounded-md border px-2 text-xs font-medium",
            filterOpen || filterCount > 0 ? "border-accent/40 bg-accent/10 text-accent" : "border-border bg-panel-2 text-fg hover:border-muted/60",
          )}
        >
          <Filter className="size-3.5" />
          <span className="hidden @[420px]:inline">Filter</span>
          {filterCount > 0 && <span className="rounded bg-accent/20 px-1 text-[10px] font-semibold tabular-nums leading-4">{filterCount}</span>}
        </button>
        <button
          type="button"
          onClick={(e) => onCopyMenu(e.currentTarget)}
          aria-haspopup="menu"
          className="inline-flex h-7 items-center gap-1 rounded-md border border-border bg-panel-2 px-2 text-xs font-medium text-fg hover:border-muted/60"
        >
          <Copy className="size-3.5" /> <span className="hidden @[420px]:inline">Copy</span> <ChevronDown className="size-3 text-muted" />
        </button>
        <IconButton label="Open this query in a SQL tab" onClick={onOpenSql}>
          <FileCode2 className="size-4" />
        </IconButton>
        <IconButton label="Refresh" onClick={onRefresh}>
          {loading ? <Loader2 className="size-4 animate-spin text-accent" /> : <RefreshCw className="size-4" />}
        </IconButton>
      </div>
    </div>
  );
}

export function ReadOnlyBanner({ children }: { children: ReactNode }) {
  return (
    <div className="flex shrink-0 items-center gap-2 border-b border-warn/30 bg-warn/10 px-3 py-1.5 text-xs text-warn">
      <Lock className="size-3.5 shrink-0" />
      <span className="min-w-0">{children}</span>
    </div>
  );
}

export function EditBar({
  counts,
  canDeleteRows,
  canUndo,
  canRedo,
  onAddRow,
  onDeleteRows,
  onUndo,
  onRedo,
  onDiscard,
  onReview,
}: {
  counts: PendingCounts;
  canDeleteRows: boolean;
  canUndo: boolean;
  canRedo: boolean;
  onAddRow: () => void;
  onDeleteRows: () => void;
  onUndo: () => void;
  onRedo: () => void;
  onDiscard: () => void;
  onReview: () => void;
}) {
  return (
    <div className="@container flex h-9 shrink-0 items-center gap-1 border-b border-border bg-bg px-2">
      <Button size="sm" variant="ghost" onClick={onAddRow} title="Add a row">
        <Plus className="size-3.5" /> Add row
      </Button>
      <Button size="sm" variant="ghost" onClick={onDeleteRows} disabled={!canDeleteRows} title="Mark the selected rows for deletion (Delete key when whole rows are selected)">
        <Trash2 className="size-3.5" /> Delete row
      </Button>
      <div className="mx-1 h-4 w-px bg-border" aria-hidden />
      <IconButton label={`Undo (${MOD}+Z)`} onClick={onUndo} disabled={!canUndo}>
        <Undo2 className="size-3.5" />
      </IconButton>
      <IconButton label={`Redo (${MOD}+Shift+Z)`} onClick={onRedo} disabled={!canRedo}>
        <Redo2 className="size-3.5" />
      </IconButton>
      <div className="ml-auto flex items-center gap-2">
        {counts.total > 0 ? (
          <div className="hidden items-center gap-1.5 @[560px]:flex" aria-live="polite">
            {counts.updates > 0 && <Chip tone="warn">{counts.updates} edited</Chip>}
            {counts.inserts > 0 && <Chip tone="ok">{counts.inserts} new</Chip>}
            {counts.deletes > 0 && <Chip tone="danger">{counts.deletes} deleted</Chip>}
          </div>
        ) : (
          <span className="hidden text-xs text-muted @[560px]:inline">
            Double-click a cell or press <Kbd>↵</Kbd> to edit
          </span>
        )}
        <Button size="sm" variant="ghost" onClick={onDiscard} disabled={counts.total === 0}>
          Discard
        </Button>
        <Button size="sm" variant="primary" onClick={onReview} disabled={counts.total === 0} title={`Review the SQL, then apply (${MOD}+Enter)`}>
          Review &amp; apply{counts.total > 0 ? ` (${counts.total})` : ""}
        </Button>
      </div>
    </div>
  );
}

export function TableFooter({
  pageIndex,
  pageSize,
  rowsOnPage,
  hasNext,
  loading,
  exactTotal,
  estimatedRows,
  filtered,
  countStatus,
  keyset,
  elapsedMs,
  onFirst,
  onPrev,
  onNext,
  onPageSize,
  onCount,
}: {
  pageIndex: number;
  pageSize: number;
  rowsOnPage: number;
  hasNext: boolean;
  loading: boolean;
  exactTotal: number | null;
  estimatedRows: number | null;
  filtered: boolean;
  countStatus: "idle" | "loading" | "error";
  keyset: boolean;
  elapsedMs: number | null;
  onFirst: () => void;
  onPrev: () => void;
  onNext: () => void;
  onPageSize: (n: number) => void;
  onCount: () => void;
}) {
  const start = rowsOnPage === 0 ? 0 : pageIndex * pageSize + 1;
  const end = pageIndex * pageSize + rowsOnPage;
  let total: ReactNode;
  if (exactTotal !== null) total = <span>of {formatCount(exactTotal)}</span>;
  else if (!filtered && estimatedRows !== null) total = <span title="Estimated total">of ~{formatCount(Math.max(estimatedRows, end))}</span>;
  else total = <span>of {formatCount(end)}+</span>;

  return (
    <div className="flex h-8 shrink-0 items-center gap-2 border-t border-border bg-panel px-2 text-xs">
      <div className="flex items-center">
        <IconButton label="First page" onClick={onFirst} disabled={pageIndex === 0 || loading}>
          <ChevronsLeft className="size-4" />
        </IconButton>
        <IconButton label="Previous page" onClick={onPrev} disabled={pageIndex === 0 || loading}>
          <ChevronLeft className="size-4" />
        </IconButton>
        <IconButton label="Next page" onClick={onNext} disabled={!hasNext || loading}>
          <ChevronRight className="size-4" />
        </IconButton>
      </div>
      <span className="tabular-nums text-fg" title={keyset ? "Paged by primary key (keyset), so deep pages stay fast." : "Paged with OFFSET."}>
        {rowsOnPage === 0 ? "0 rows" : `${formatCount(start)}–${formatCount(end)}`} <span className="text-muted">{total}</span>
      </span>
      {exactTotal === null && (
        <button
          type="button"
          onClick={onCount}
          disabled={countStatus === "loading"}
          className="rounded px-1.5 py-0.5 text-accent hover:bg-accent/10 disabled:opacity-60"
          title="Run SELECT count(*) with the current filters"
        >
          {countStatus === "loading" ? "Counting…" : countStatus === "error" ? "Count failed, retry" : "Count rows"}
        </button>
      )}
      <div className="ml-auto flex items-center gap-3 text-muted">
        {elapsedMs !== null && <span className="tabular-nums">{formatElapsed(elapsedMs)}</span>}
        <label className="flex items-center gap-1.5">
          <span>Rows per page</span>
          <MiniSelect label="Rows per page" value={String(pageSize)} onChange={(v) => onPageSize(Number(v))} className="w-[4.5rem]">
            {PAGE_SIZES.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </MiniSelect>
        </label>
      </div>
    </div>
  );
}
