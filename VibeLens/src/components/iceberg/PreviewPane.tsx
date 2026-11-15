import { useEffect, useRef, useState } from "react";
import { Loader2, RefreshCw, TerminalSquare, Copy, ChevronDown, Lock } from "lucide-react";
import type { IcebergTableInfo } from "../../ipc/types";
import { Button, EmptyState, ErrorBox } from "../ui";
import { Chip } from "../ui/wsKit";
import { useMenu } from "../ui/Menu";
import { DataGrid, selectionRange, type GridSelection } from "../grid/DataGrid";
import { CellViewerDialog } from "../grid/CellViewerDialog";
import { formatCount, formatElapsed } from "../grid/cellFormat";
import { useCopyExport } from "../grid/copyExport";
import { humanBytes } from "./iceFormat";
import { PREVIEW_ROWS, type PreviewState } from "./useIcePreview";

export function PreviewPane({
  info,
  state,
  onRun,
  onOpenSql,
}: {
  info: IcebergTableInfo;
  state: PreviewState;
  onRun: () => void;
  onOpenSql: () => void;
}) {
  const [selection, setSelection] = useState<GridSelection | null>(null);
  const [viewer, setViewer] = useState<{ r: number; c: number } | null>(null);
  const [flash, setFlash] = useState<string | null>(null);
  const flashTimer = useRef<number | undefined>(undefined);
  const { menuNode, openFrom } = useMenu();
  useEffect(() => () => window.clearTimeout(flashTimer.current), []);
  useEffect(() => setSelection(null), [state.runId]);

  const selectionRef = useRef<GridSelection | null>(null);
  selectionRef.current = selection;
  const { buildEntries, dialog } = useCopyExport({
    columns: state.columns,
    rows: state.rows,
    getRange: () => (selectionRef.current ? selectionRange(selectionRef.current) : null),
    baseName: info.name,
    notify: (msg) => {
      setFlash(msg);
      window.clearTimeout(flashTimer.current);
      flashTimer.current = window.setTimeout(() => setFlash(null), 1400);
    },
  });

  const running = state.status === "running";
  const empty = state.status === "done" && state.rows.length === 0;
  const hasData = state.columns.length > 0 && state.rows.length > 0;
  const current = info.snapshots.find((s) => s.snapshotId === info.currentSnapshotId);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex min-h-10 shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-border bg-panel px-4 py-1.5">
        <div className="min-w-0">
          <h2 className="text-[13px] font-semibold text-fg">Data preview</h2>
        </div>
        <span className="flex min-w-0 items-center gap-2 text-xs text-muted">
          {running && <Loader2 className="size-3.5 animate-spin text-accent" />}
          {state.status === "done" && (
            <span className="tabular-nums text-fg">
              {state.rows.length >= PREVIEW_ROWS ? `First ${formatCount(PREVIEW_ROWS)} rows` : `${formatCount(state.rows.length)} ${state.rows.length === 1 ? "row" : "rows"}`}
            </span>
          )}
          {state.elapsedMs !== null && <span className="tabular-nums">{formatElapsed(state.elapsedMs)}</span>}
          {state.bytesScanned !== null && <span className="tabular-nums">{humanBytes(state.bytesScanned)} scanned</span>}
          {!running && current && state.status !== "idle" && <span className="hidden truncate @[720px]:inline">current snapshot {current.snapshotId.slice(0, 6)}…{current.snapshotId.slice(-4)}</span>}
          {flash && <span role="status" className="text-ok">{flash}</span>}
        </span>
        <span className="ml-auto flex items-center gap-1.5">
          <Chip icon={<Lock className="size-3" />} title="The preview runs in read-only mode and cannot change the table">read-only</Chip>
          <button
            type="button"
            onClick={(e) => openFrom(e.currentTarget, buildEntries(), "right")}
            aria-haspopup="menu"
            disabled={!hasData}
            className="inline-flex h-7 items-center gap-1 rounded-md border border-border bg-panel-2 px-2 text-xs font-medium text-fg hover:border-muted/60 disabled:opacity-40"
          >
            <Copy className="size-3.5" /> Copy <ChevronDown className="size-3 text-muted" />
          </button>
          <Button size="sm" onClick={onOpenSql}>
            <TerminalSquare className="size-3.5" /> Open in SQL tab
          </Button>
          <Button size="sm" onClick={onRun} loading={running}>
            {!running && <RefreshCw className="size-3.5" />} Run again
          </Button>
        </span>
      </div>

      <div className="min-h-0 flex-1 bg-bg">
        {state.status === "error" ? (
          <div className="p-4">
            <ErrorBox error={state.error} />
            <p className="mt-3 text-[12px] text-muted">
              The preview reads real data files, so a storage or permission problem shows up here even when the catalog works. Check the Storage section of this connection.
            </p>
          </div>
        ) : state.status === "idle" || (running && state.columns.length === 0) ? (
          <div className="flex h-full items-center justify-center gap-2 text-xs text-muted">
            <Loader2 className="size-4 animate-spin" /> Reading the first {PREVIEW_ROWS} rows…
          </div>
        ) : empty ? (
          <EmptyState title="No rows in the current snapshot">The table exists but holds no data right now. Earlier snapshots may still have rows: try Query as of this snapshot.</EmptyState>
        ) : (
          <DataGrid
            columns={state.columns}
            rows={state.rows}
            rowCount={state.rows.length}
            selection={selection}
            onSelectionChange={setSelection}
            onOpenCell={(r, c) => setViewer({ r, c })}
            onCopy={() => {
              const e = buildEntries()[0];
              if (e !== "separator" && e && !e.disabled) e.onSelect();
            }}
          />
        )}
      </div>
      <CellViewerDialog open={viewer !== null} onClose={() => setViewer(null)} column={viewer ? (state.columns[viewer.c] ?? null) : null} rowIndex={viewer?.r ?? 0} cell={viewer ? (state.rows[viewer.r]?.[viewer.c] ?? null) : null} />
      {menuNode}
      {dialog}
    </div>
  );
}
