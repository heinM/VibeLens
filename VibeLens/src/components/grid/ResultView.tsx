// One statement's result: toolbar (counts, truncation, view toggle, copy) + grid / text / JSON body.
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, Check, ChevronDown, Copy, Grid3x3, Braces, AlignLeft, Loader2, Zap } from "lucide-react";
import type { Cell } from "../../ipc/types";
import type { SqlDialect } from "../../lib/sqlIntel/dialect";
import { ErrorBox } from "../ui";
import { Chip, Segmented } from "../ui/wsKit";
import { useMenu } from "../ui/Menu";
import { JsonTree } from "../json/JsonTree";
import { isExplainJson, PlanView } from "../explain";
import { textPlanSections } from "../explain/textPlan";
import { TextPlanView } from "../explain/TextPlanView";
import type { ResultSetMeta } from "../../hooks/useQueryRunner";
import { DataGrid, selectionRange, type GridSelection } from "./DataGrid";
import { CellViewerDialog } from "./CellViewerDialog";
import { formatCount, formatElapsed, formatTextTable, rowsToObjects } from "./cellFormat";
import { useCopyExport } from "./copyExport";
import { StatsChip } from "./StatsChip";

type View = "plan" | "grid" | "text" | "json";

/** What produced a result set. SqlTabPane passes this so plan views know their statement and connection. */
export interface ResultContext {
  sql?: string | null;
  /** False while this worksheet is not the visible workspace tab (it then must not publish its result to the agent). */
  active?: boolean;
  profileId?: string | null;
  database?: string | null;
  /** The run was an Iceberg EXPLAIN: its rows are a text plan (shown verbatim), not Postgres plan JSON. */
  textPlan?: boolean;
  /** SQL dialect of the connection (Iceberg: DataFusion or Athena). Names the engine of a text plan. */
  dialect?: SqlDialect | null;
  /** File name stem for exports. */
  name?: string;
}
const TEXT_LIMIT = 2000;
const JSON_LIMIT = 1000;

export const ResultView = memo(function ResultView({
  meta,
  rows,
  version,
  totalElapsedMs,
  context,
}: {
  meta: ResultSetMeta;
  rows: Cell[][];
  version: number;
  totalElapsedMs: number | null;
  context?: ResultContext;
}) {
  const isJsonPlan = meta.status === "done" && isExplainJson(meta.columns, rows);
  // An Iceberg EXPLAIN is plain text. Never fed to the Postgres plan visualiser.
  const textSections = useMemo(
    () => (meta.status === "done" && context?.textPlan && !isJsonPlan ? textPlanSections(meta.columns, rows) : null),
    // rows is a mutable buffer: it is complete once the statement is done
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [meta.status, meta.columns, context?.textPlan, isJsonPlan],
  );
  const isPlan = isJsonPlan || textSections !== null;
  const [view, setViewState] = useState<View>(isPlan ? "plan" : "grid");
  const pickedView = useRef(false);
  const setView = useCallback((v: View) => {
    pickedView.current = true;
    setViewState(v);
  }, []);
  // A plan arrives after the (still empty) result mounted: show it by default unless the user chose a view.
  useEffect(() => {
    if (isPlan && !pickedView.current) setViewState("plan");
  }, [isPlan]);
  const [selection, setSelection] = useState<GridSelection | null>(null);
  const [viewer, setViewer] = useState<{ r: number; c: number } | null>(null);
  const [flash, setFlash] = useState<string | null>(null);
  const flashTimer = useRef<number | undefined>(undefined);
  const { menuNode, openFrom } = useMenu();

  useEffect(() => () => window.clearTimeout(flashTimer.current), []);

  const columns = meta.columns;
  const rowCount = meta.rowCount;
  const isRunning = meta.status === "running";

  const notify = useCallback((msg: string) => {
    setFlash(msg);
    window.clearTimeout(flashTimer.current);
    flashTimer.current = window.setTimeout(() => setFlash(null), 1400);
  }, []);

  const selectionRef = useRef<GridSelection | null>(null);
  selectionRef.current = selection;
  const { buildEntries, dialog: exportDialog } = useCopyExport({
    columns,
    rows,
    getRange: () => (selectionRef.current ? selectionRange(selectionRef.current) : null),
    baseName: context?.name ?? "results",
    notify,
  });

  const copySelection = useCallback(() => {
    const entry = buildEntries()[0];
    if (entry !== "separator" && entry && !entry.disabled) entry.onSelect();
  }, [buildEntries]);

  const textView = useMemo(
    () => (view === "text" ? formatTextTable(columns, rows, TEXT_LIMIT) : ""),
    // rows is a mutable buffer: `version` and rowCount signal growth
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [view, columns, rowCount, version],
  );
  const jsonView = useMemo(
    () => (view === "json" ? rowsToObjects(columns, rows, JSON_LIMIT) : null),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [view, columns, rowCount, version],
  );

  if (meta.status === "error") {
    return (
      <div className="h-full overflow-auto p-3">
        <ErrorBox error={meta.error} />
      </div>
    );
  }

  // Command-only statement: "UPDATE 3 · 12 ms"
  if (columns.length === 0) {
    return (
      <div className="flex h-full items-start p-4">
        {isRunning ? (
          <div className="flex items-center gap-2 text-muted">
            <Loader2 className="size-4 animate-spin" /> Running…
          </div>
        ) : (
          <div className="flex items-center gap-2.5 rounded-md border border-ok/30 bg-ok/10 px-3 py-2 text-[13px]">
            <Check className="size-4 text-ok" />
            <span className="font-mono font-medium text-fg">{meta.commandTag ?? (meta.rowsAffected !== null ? `${meta.rowsAffected} rows affected` : "OK")}</span>
            <span className="text-muted">·</span>
            <span className="tabular-nums text-muted">{formatElapsed(meta.elapsedMs ?? totalElapsedMs ?? 0)}</span>
            {meta.stats && <span className="text-muted">·</span>}
            <StatsChip stats={meta.stats} />
          </div>
        )}
      </div>
    );
  }

  const selCells = selection ? (() => {
    const r = selectionRange(selection);
    return (r.r1 - r.r0 + 1) * (r.c1 - r.c0 + 1);
  })() : 0;

  const copyMenu = (anchor: HTMLElement) => openFrom(anchor, buildEntries(), "right");

  const viewerCell = viewer ? (rows[viewer.r]?.[viewer.c] ?? null) : null;

  return (
    <div className="@container flex h-full min-h-0 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border bg-panel px-2">
        <div className="flex min-w-0 items-center gap-2 overflow-hidden text-xs">
          {isRunning && <Loader2 className="size-3.5 shrink-0 animate-spin text-accent" />}
          <span className="shrink-0 whitespace-nowrap font-medium tabular-nums text-fg">
            {formatCount(rowCount)} {rowCount === 1 ? "row" : "rows"}
            {isRunning && <span className="font-normal text-muted"> so far</span>}
          </span>
          {meta.elapsedMs !== null && <span className="shrink-0 tabular-nums text-muted">{formatElapsed(meta.elapsedMs)}</span>}
          <StatsChip stats={meta.stats} />
          {meta.truncated && (
            <span className="shrink-0 whitespace-nowrap">
              <Chip tone="warn" icon={<AlertTriangle className="size-3" />} title="The result was cut at the row limit; the remaining rows were discarded by the server-side cap.">
                <span className="@[900px]:hidden">first {formatCount(rowCount)}</span>
                <span className="hidden @[900px]:inline">showing first {formatCount(rowCount)} rows</span>
              </Chip>
            </span>
          )}
          {selCells > 1 && <span className="truncate text-muted">{formatCount(selCells)} cells selected</span>}
          {flash && (
            <span role="status" className="flex items-center gap-1 text-ok">
              <Check className="size-3.5" /> {flash}
            </span>
          )}
        </div>
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <Segmented<View>
            label="Result view"
            value={view}
            onChange={setView}
            options={[
              ...(isPlan ? [{ value: "plan" as const, label: "Plan", icon: <Zap className="size-3" /> }] : []),
              { value: "grid", label: "Grid", icon: <Grid3x3 className="size-3" /> },
              { value: "text", label: "Text", icon: <AlignLeft className="size-3" /> },
              { value: "json", label: "JSON", icon: <Braces className="size-3" /> },
            ]}
          />
          <button
            type="button"
            onClick={(e) => copyMenu(e.currentTarget)}
            aria-haspopup="menu"
            className="inline-flex h-7 items-center gap-1 rounded-md border border-border bg-panel-2 px-2 text-xs font-medium text-fg hover:border-muted/60"
          >
            <Copy className="size-3.5" /> Copy <ChevronDown className="size-3 text-muted" />
          </button>
        </div>
      </div>

      <div className="min-h-0 flex-1">
        {view === "grid" && (
          <DataGrid
            columns={columns}
            rows={rows}
            rowCount={rowCount}
            selection={selection}
            onSelectionChange={setSelection}
            onOpenCell={(r, c) => setViewer({ r, c })}
            onCopy={copySelection}
          />
        )}
        {view === "plan" && isJsonPlan && (
          <div className="h-full overflow-auto bg-bg">
            <PlanView plan={rows[0][0]} sql={context?.sql ?? undefined} profileId={context?.profileId} database={context?.database} />
          </div>
        )}
        {view === "plan" && textSections && (
          <div className="h-full overflow-auto bg-bg">
            <TextPlanView sections={textSections} engine={meta.stats?.engine ?? (context?.dialect === "athena" ? "athena" : "datafusion")} sql={context?.sql ?? undefined} profileId={context?.profileId} />
          </div>
        )}
        {view === "text" && <pre className="selectable h-full overflow-auto bg-bg p-3 font-mono text-[12px] leading-5 text-fg">{textView}</pre>}
        {view === "json" && (
          <div className="flex h-full flex-col">
            {rowCount > JSON_LIMIT && (
              <div className="border-b border-border bg-panel px-3 py-1 text-[11px] text-muted">
                Showing the first {formatCount(JSON_LIMIT)} of {formatCount(rowCount)} rows. Use Copy all rows as JSON for everything.
              </div>
            )}
            <div className="min-h-0 flex-1 bg-bg">{jsonView && <JsonTree value={jsonView} defaultExpandDepth={1} />}</div>
          </div>
        )}
      </div>

      <CellViewerDialog open={viewer !== null} onClose={() => setViewer(null)} column={viewer ? (columns[viewer.c] ?? null) : null} rowIndex={viewer?.r ?? 0} cell={viewerCell} />
      {menuNode}
      {exportDialog}
    </div>
  );
});
