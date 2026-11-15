// The table data tab: browse any table or view with paging, filters and sorting, and edit rows in
// place when the table has a primary key. Pending edits are reviewed as SQL before they are applied.
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Loader2, Plug } from "lucide-react";
import { useWorkspaceStore, type WorkspaceTab } from "../../store/useWorkspaceStore";
import { useHistoryStore } from "../../store/useHistoryStore";
import { pgQueries, usePgDescribeTable } from "../../hooks/usePg";
import { useConnect, useConnectionStatus } from "../../hooks/useConnectionStatus";
import { useProfile } from "../../hooks/useProfiles";
import { useTableData } from "../../hooks/useTableData";
import { useTableEdits } from "../../hooks/useTableEdits";
import { useTabActions } from "../../hooks/useTabActions";
import { asError } from "../../ipc/commands";
import type { Cell, ColumnMeta, ErrorPayload } from "../../ipc/types";
import { uid } from "../../lib/id";
import { Button, ConfirmDialog, EmptyState, ErrorBox } from "../ui";
import { useMenu, type MenuEntry } from "../ui/Menu";
import { copyToClipboard } from "../ui/clipboard";
import { readStorage, writeStorage } from "../layout/storage";
import { DataGrid, selectionRange, type GridSelection, type GridSortKey, type RowState } from "../grid/DataGrid";
import { CellViewerDialog } from "../grid/CellViewerDialog";
import { copyText, formatCount } from "../grid/cellFormat";
import { useCopyExport } from "../grid/copyExport";
import { toJson, toSqlInserts } from "../grid/exportFormats";
import { showToast } from "../grid/toast";
import { InlineCellEditor, LongValueEditor, editorModeFor, isGeneratedColumn, toEditorColumn, type Move } from "./CellEditors";
import { FilterBar, newFilter, type FilterMode } from "./FilterBar";
import { ReviewDialog } from "./ReviewDialog";
import { EditBar, PAGE_SIZES, ReadOnlyBanner, TableFooter, TableHeaderBar } from "./TableChrome";
import { executeScript } from "./executeScript";
import {
  addInsert,
  buildApplyScript,
  describeTableEdit,
  hasPending,
  missingRequired,
  pendingCounts,
  removeInsert,
  revertRow,
  setCell,
  setInsertCell,
  toggleDelete,
  type ApplyScript,
  type PendingState,
} from "./pendingChanges";
import { SqlGenError, buildWhere, cellToEditText, shortTypeName, type FilterOp, type FilterRow, type SortKey, type TableTarget } from "./sqlGen";
import { composeRows } from "./tableRows";

function readPageSize(): number {
  const n = Number(readStorage("vg.table.pageSize"));
  return PAGE_SIZES.includes(n) ? n : 200;
}

export function TableTab({ tab }: { tab: WorkspaceTab; active: boolean }) {
  const ref = tab.objectRef;
  if (!ref || !tab.profileId) {
    return <EmptyState title="Nothing to show">This tab is not linked to a table. Open one from the navigator.</EmptyState>;
  }
  return <TableBrowser tab={tab} profileId={tab.profileId} schema={ref.schema} name={ref.name} />;
}

type EditingCell = { r: number; c: number; initial?: string };

function TableBrowser({ tab, profileId, schema, name }: { tab: WorkspaceTab; profileId: string; schema: string; name: string }) {
  const qc = useQueryClient();
  const profile = useProfile(profileId);
  const status = useConnectionStatus(profileId);
  const connect = useConnect();
  const connected = status?.state === "connected";
  const database = tab.database;
  const updateTab = useWorkspaceStore((s) => s.updateTab);
  const { newSqlTab } = useTabActions();
  const { menuNode, openAt, openFrom } = useMenu();

  const descQ = usePgDescribeTable(profileId, database, schema, name);
  const desc = descQ.data ?? null;
  const columns = useMemo(() => (desc ? [...desc.columns].sort((a, b) => a.ordinal - b.ordinal) : null), [desc]);

  const pkNames = useMemo(() => (columns ?? []).filter((c) => c.isPrimaryKey).map((c) => c.name), [columns]);
  const isTableKind = desc?.kind === "table" || desc?.kind === "partitionedTable";
  const editable = !!desc && isTableKind && pkNames.length > 0;
  const readOnlyReason = !desc
    ? null
    : desc.kind === "view"
      ? "This is a view. Views are read-only; edit the tables behind it, or use a SQL tab."
      : desc.kind === "materializedView"
        ? "This is a materialized view. It is read-only; refresh it with REFRESH MATERIALIZED VIEW."
        : desc.kind === "foreignTable"
          ? "This is a foreign table. It is read-only here."
          : !isTableKind
            ? "This object cannot be edited."
            : pkNames.length === 0
              ? "This table has no primary key, so rows cannot be told apart safely and editing is turned off. Add a primary key, or change data from a SQL tab."
              : null;

  // ---- filters, sort, paging ------------------------------------------------------------------
  const [filterOpen, setFilterOpen] = useState(false);
  const [mode, setMode] = useState<FilterMode>("builder");
  const [draftRows, setDraftRows] = useState<FilterRow[]>([]);
  const [draftRaw, setDraftRaw] = useState("");
  const [applied, setApplied] = useState<{ rows: FilterRow[]; raw: string }>({ rows: [], raw: "" });
  const [filterErrors, setFilterErrors] = useState<Record<string, string>>({});
  const [sort, setSort] = useState<SortKey[]>([]);
  const [pageSize, setPageSizeState] = useState(readPageSize);
  const setPageSize = (n: number) => {
    writeStorage("vg.table.pageSize", String(n));
    setPageSizeState(n);
  };

  const colRefs = useMemo(() => (columns ?? []).map((c) => ({ name: c.name, dataType: c.dataType })), [columns]);

  const data = useTableData({
    profileId,
    database,
    schema,
    name,
    columns,
    filters: applied.rows,
    rawWhere: applied.raw,
    sort,
    pageSize,
    enabled: connected && !!columns,
  });

  const dirtyFilters = JSON.stringify(mode === "builder" ? { rows: draftRows, raw: "" } : { rows: [], raw: draftRaw }) !== JSON.stringify(applied);
  const filterCount = applied.rows.length + (applied.raw.trim() ? 1 : 0);

  const applyFilters = useCallback(() => {
    const rows = mode === "builder" ? draftRows : [];
    const raw = mode === "raw" ? draftRaw : "";
    const w = buildWhere(rows, colRefs, raw);
    if (Object.keys(w.errors).length > 0) {
      setFilterErrors(w.errors);
      return;
    }
    setFilterErrors({});
    setApplied({ rows, raw });
  }, [mode, draftRows, draftRaw, colRefs]);

  const clearFilters = () => {
    setDraftRows([]);
    setDraftRaw("");
    setFilterErrors({});
    setApplied({ rows: [], raw: "" });
  };

  const changeMode = (m: FilterMode) => {
    if (m === "raw" && draftRaw.trim() === "") {
      // Start the raw box from what the builder would run: a free lesson in the SQL it generates.
      const w = buildWhere(draftRows, colRefs);
      if (w.sql && Object.keys(w.errors).length === 0) setDraftRaw(w.sql);
    }
    setFilterErrors({});
    setMode(m);
  };

  /** "Filter by this value" and friends. */
  const quickFilter = (column: string, op: FilterOp, value: string) => {
    const next = [...(applied.raw ? [] : applied.rows), newFilter(column, op, value)];
    setMode("builder");
    setDraftRows(next);
    setDraftRaw("");
    setApplied({ rows: next, raw: "" });
    setFilterErrors({});
    setFilterOpen(true);
  };

  const addColumnFilter = (column: string) => {
    setMode("builder");
    setDraftRows((rows) => [...rows, newFilter(column)]);
    setFilterOpen(true);
  };

  const onHeaderClick = useCallback(
    (col: number, e: ReactMouseEvent) => {
      const column = columns?.[col]?.name;
      if (!column) return;
      setSort((prev) => {
        const i = prev.findIndex((k) => k.column === column);
        if (e.shiftKey) {
          if (i < 0) return [...prev, { column, dir: "asc" }];
          if (prev[i].dir === "asc") return prev.map((k, j) => (j === i ? { ...k, dir: "desc" } : k));
          return prev.filter((_, j) => j !== i);
        }
        if (i < 0 || prev.length > 1) return [{ column, dir: "asc" }];
        return prev[i].dir === "asc" ? [{ column, dir: "desc" }] : [];
      });
    },
    [columns],
  );

  // ---- grid model -----------------------------------------------------------------------------
  const edits = useTableEdits();
  const pending = edits.pending;
  const counts = useMemo(() => pendingCounts(pending), [pending]);

  const columnNames = useMemo(() => (columns ?? []).map((c) => c.name), [columns]);
  const gridColumns = useMemo<ColumnMeta[]>(() => (columns ?? []).map((c) => ({ name: c.name, typeName: shortTypeName(c.dataType), typeOid: 0 })), [columns]);
  const composed = useMemo(() => composeRows(columnNames, editable ? pkNames : [], data.rows, pending), [columnNames, editable, pkNames, data.rows, pending]);

  const sortKeys = useMemo<GridSortKey[]>(
    () => sort.map((k) => ({ col: columnNames.indexOf(k.column), dir: k.dir })).filter((k) => k.col >= 0),
    [sort, columnNames],
  );

  const [selection, setSelection] = useState<GridSelection | null>(null);
  const [viewer, setViewer] = useState<{ r: number; c: number } | null>(null);
  const [editing, setEditing] = useState<EditingCell | null>(null);
  const [dialogEdit, setDialogEdit] = useState<{ r: number; c: number } | null>(null);
  const [focusToken, setFocusToken] = useState(0);
  const focusGrid = () => setFocusToken((n) => n + 1);
  const [flash, setFlash] = useState<string | null>(null);
  const flashTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(flashTimer.current), []);
  const notify = useCallback((msg: string) => {
    setFlash(msg);
    window.clearTimeout(flashTimer.current);
    flashTimer.current = window.setTimeout(() => setFlash(null), 1600);
  }, []);

  // A new page or query invalidates row positions.
  useEffect(() => {
    setSelection(null);
    setEditing(null);
  }, [data.rows]);

  // Unsaved edits make the tab "dirty": the tab strip asks before closing it.
  const dirty = counts.total > 0;
  useEffect(() => {
    updateTab(tab.id, { dirty });
  }, [dirty, tab.id, updateTab]);

  const selectionRef = useRef<GridSelection | null>(null);
  selectionRef.current = selection;
  const { buildEntries, dialog: exportDialog } = useCopyExport({
    columns: gridColumns,
    rows: composed.rows,
    sourceRows: data.rows,
    getRange: () => (selectionRef.current ? selectionRange(selectionRef.current) : null),
    table: { schema, name },
    baseName: name,
    notify,
  });

  // ---- editing --------------------------------------------------------------------------------
  const valueAt = (r: number, c: number): Cell => composed.rows[r]?.[c] ?? null;

  const writeCell = useCallback(
    (r: number, c: number, value: Cell | undefined) => {
      const col = columns?.[c];
      const m = composed.meta[r];
      if (!col || !m) return;
      if (m.kind === "new") edits.update((p) => setInsertCell(p, m.id, col.name, value));
      else if (m.key !== null && m.pk) {
        const original = data.rows[m.loaded]?.[c] ?? null;
        edits.update((p) => setCell(p, m.key as string, m.pk as Record<string, Cell>, col.name, value ?? null, original));
      }
    },
    [columns, composed.meta, data.rows, edits],
  );

  const startEdit = useCallback(
    (r: number, c: number, initial?: string) => {
      const col = columns?.[c];
      const m = composed.meta[r];
      if (!editable || !col || !m) return;
      if (m.kind === "row" && m.deleted) {
        showToast({ title: "This row is marked for deletion", detail: "Restore it before editing.", tone: "info", durationMs: 3500 });
        return;
      }
      const mode = editorModeFor(col, valueAt(r, c));
      if (mode.mode === "blocked") {
        showToast({ title: "Cannot edit this cell", detail: mode.reason, tone: "info", durationMs: 4000 });
        return;
      }
      if (mode.mode === "dialog") setDialogEdit({ r, c });
      else setEditing({ r, c, initial });
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [columns, composed, editable],
  );

  const commitEdit = (r: number, c: number, cell: Cell, move: Move) => {
    writeCell(r, c, cell);
    setEditing(null);
    const lastCol = columnNames.length - 1;
    const nc = move === "right" ? Math.min(lastCol, c + 1) : move === "left" ? Math.max(0, c - 1) : c;
    setSelection({ anchor: { r, c: nc }, focus: { r, c: nc } });
  };

  const addRow = () => {
    const at = pending.inserts.length;
    edits.update((p) => addInsert(p, uid()));
    const first = (columns ?? []).findIndex((c) => !isGeneratedColumn(c));
    setSelection({ anchor: { r: at, c: Math.max(0, first) }, focus: { r: at, c: Math.max(0, first) } });
    focusGrid();
  };

  /** Rows touched by the selection, as grid row indexes. */
  const selectedRows = (): number[] => {
    if (!selection) return [];
    const { r0, r1 } = selectionRange(selection);
    const out: number[] = [];
    for (let r = r0; r <= r1; r++) if (composed.meta[r]) out.push(r);
    return out;
  };

  const deleteRows = (rowsIdx: number[]) => {
    if (rowsIdx.length === 0) return;
    const metas = rowsIdx.map((r) => composed.meta[r]).filter(Boolean);
    const existing = metas.filter((m) => m.kind === "row" && m.key !== null);
    // Mixed selections: if every selected existing row is already marked, this restores them; otherwise it marks them all.
    const allMarked = existing.length > 0 && existing.every((m) => m.kind === "row" && m.deleted);
    edits.update((p) => {
      let next: PendingState = p;
      for (const m of metas) {
        if (m.kind === "new") next = removeInsert(next, m.id);
        else if (m.key !== null && m.pk && m.deleted === allMarked) next = toggleDelete(next, m.key, m.pk);
      }
      return next;
    });
    setSelection(null);
  };

  const duplicateRow = (r: number) => {
    const m = composed.meta[r];
    if (!m || !columns) return;
    const values: Record<string, Cell> = {};
    columns.forEach((col, i) => {
      if (col.isPrimaryKey || isGeneratedColumn(col) || (col.default && /nextval|GENERATED/i.test(col.default))) return;
      values[col.name] = composed.rows[r][i] ?? null;
    });
    const at = pending.inserts.length;
    edits.update((p) => addInsert(p, uid(), values));
    setSelection({ anchor: { r: at, c: 0 }, focus: { r: at, c: 0 } });
    focusGrid();
  };

  const rowState = useCallback(
    (r: number): RowState => {
      const m = composed.meta[r];
      return m?.kind === "new" ? "new" : m?.kind === "row" && m.deleted ? "deleted" : undefined;
    },
    [composed.meta],
  );
  const cellDirty = useCallback(
    (r: number, c: number): boolean => {
      const m = composed.meta[r];
      return m?.kind === "row" && m.edited.size > 0 && m.edited.has(columnNames[c]);
    },
    [composed.meta, columnNames],
  );
  const cellOverride = useCallback(
    (r: number, c: number, value: Cell) => {
      const m = composed.meta[r];
      if (m?.kind !== "new" || value !== null) return null;
      const col = columns?.[c];
      if (!col) return null;
      const explicit = pending.inserts[r]?.values && col.name in pending.inserts[r].values;
      if (explicit) return null; // the user chose NULL: normal NULL rendering
      return col.default ? (
        <span className="italic text-muted/70">DEFAULT</span>
      ) : col.nullable ? (
        <span className="italic text-muted/50">NULL</span>
      ) : (
        <span className="italic text-warn">required</span>
      );
    },
    [composed.meta, columns, pending.inserts],
  );
  const rowLabel = useCallback(
    (r: number): string | number => (composed.meta[r]?.kind === "new" ? "+" : data.pageIndex * pageSize + (r - composed.newCount) + 1),
    [composed.meta, composed.newCount, data.pageIndex, pageSize],
  );
  const readOnlyCol = useCallback((c: number) => !!columns?.[c] && isGeneratedColumn(columns[c]), [columns]);

  const renderEditor = useCallback(
    (r: number, c: number) => {
      const col = columns?.[c];
      if (!col || !editing) return null;
      return (
        <InlineCellEditor
          column={toEditorColumn(col)}
          value={composed.rows[r]?.[c] ?? null}
          initialText={editing.initial}
          onCommit={(cell, move) => commitEdit(r, c, cell, move)}
          onCancel={() => setEditing(null)}
        />
      );
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [columns, editing, composed],
  );

  const onKeyDownExtra = useCallback(
    (e: ReactKeyboardEvent<HTMLDivElement>): boolean => {
      const mod = e.metaKey || e.ctrlKey;
      const key = e.key.toLowerCase();
      if (editable && mod && key === "z") {
        e.preventDefault();
        if (e.shiftKey) edits.redo();
        else edits.undo();
        return true;
      }
      if (editable && mod && key === "y") {
        e.preventDefault();
        edits.redo();
        return true;
      }
      if (editable && mod && e.key === "Enter" && hasPending(pending)) {
        e.preventDefault();
        openReview();
        return true;
      }
      if (editable && (e.key === "Delete" || e.key === "Backspace") && selection) {
        const r = selectionRange(selection);
        if (r.c0 === 0 && r.c1 === columnNames.length - 1) {
          e.preventDefault();
          deleteRows(selectedRows());
          return true;
        }
      }
      return false;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [editable, edits, pending, selection, columnNames, composed],
  );

  // ---- context menus --------------------------------------------------------------------------
  const onCellContextMenu = (e: ReactMouseEvent, r: number, c: number) => {
    const col = columns?.[c];
    const m = composed.meta[r];
    if (!col || !m) return;
    const value = valueAt(r, c);
    const isNull = value === null;
    const canEditHere = editable && !(m.kind === "row" && m.deleted) && !isGeneratedColumn(col);
    const items: MenuEntry[] = [];
    if (editable) {
      items.push(
        { label: "Edit cell", hint: "↵", disabled: !canEditHere, onSelect: () => startEdit(r, c) },
        { label: "Set to NULL", disabled: !canEditHere || !col.nullable || isNull, onSelect: () => writeCell(r, c, null) },
      );
      if (m.kind === "new") items.push({ label: "Set to DEFAULT", disabled: !(col.name in pending.inserts[r].values), onSelect: () => writeCell(r, c, undefined) });
      items.push("separator");
    }
    const text = cellToEditText(value);
    items.push(
      { label: isNull ? "Filter: IS NULL" : "Filter by this value", onSelect: () => quickFilter(col.name, isNull ? "IS NULL" : "=", text) },
      { label: isNull ? "Filter: IS NOT NULL" : "Exclude this value", onSelect: () => quickFilter(col.name, isNull ? "IS NOT NULL" : "<>", text) },
      "separator",
      { label: "Copy cell", onSelect: () => void copyToClipboard(copyText(value)).then((ok) => ok && notify("Copied cell")) },
      {
        label: "Copy row as JSON",
        onSelect: () => void copyToClipboard(toJson(gridColumns, [composed.rows[r]]).trim()).then((ok) => ok && notify("Copied row as JSON")),
      },
      {
        label: "Copy row as INSERT",
        onSelect: () => void copyToClipboard(toSqlInserts(gridColumns, [composed.rows[r]], { schema, name })).then((ok) => ok && notify("Copied row as INSERT")),
      },
      { label: "View value…", onSelect: () => setViewer({ r, c }) },
    );
    if (editable) {
      items.push("separator");
      items.push({ label: "Duplicate row", onSelect: () => duplicateRow(r) });
      if (m.kind === "new") items.push({ label: "Remove new row", danger: true, onSelect: () => deleteRows([r]) });
      else
        items.push({ label: m.deleted ? "Restore row" : "Delete row", danger: !m.deleted, onSelect: () => deleteRows([r]) });
      if (m.kind === "row" && m.key !== null && (m.edited.size > 0 || m.deleted))
        items.push({ label: "Revert changes in this row", onSelect: () => edits.update((p) => revertRow(p, m.key as string)) });
    }
    openAt(e, items);
  };

  const onHeaderMenu = (col: number, ev: ReactMouseEvent | { clientX: number; clientY: number; anchor: HTMLElement }) => {
    const column = columns?.[col];
    if (!column) return;
    const sorted = sort.find((k) => k.column === column.name);
    const items: MenuEntry[] = [
      { label: "Sort ascending", disabled: sorted?.dir === "asc" && sort.length === 1, onSelect: () => setSort([{ column: column.name, dir: "asc" }]) },
      { label: "Sort descending", disabled: sorted?.dir === "desc" && sort.length === 1, onSelect: () => setSort([{ column: column.name, dir: "desc" }]) },
      { label: "Remove from sort", disabled: !sorted, onSelect: () => setSort((s) => s.filter((k) => k.column !== column.name)) },
      "separator",
      { label: `Filter on ${column.name}…`, onSelect: () => addColumnFilter(column.name) },
      { label: "Copy column name", onSelect: () => void copyToClipboard(column.name).then((ok) => ok && notify("Copied column name")) },
    ];
    if ("anchor" in ev) openFrom(ev.anchor, items);
    else openAt(ev.nativeEvent, items);
  };

  // ---- review & apply ---------------------------------------------------------------------------
  const [review, setReview] = useState<{ script: ApplyScript | null; error: string | null } | null>(null);
  const [applying, setApplying] = useState(false);
  const [applyError, setApplyError] = useState<ErrorPayload | null>(null);
  const [confirm, setConfirm] = useState<{ message: string } | null>(null);

  const target = useMemo<TableTarget>(() => ({ schema, name, columns: colRefs }), [schema, name, colRefs]);

  const problems = useMemo(
    () =>
      missingRequired(columns ?? [], pending.inserts, (n) => !!columns?.find((c) => c.name === n && isGeneratedColumn(c))).map(
        (m) => `New row ${m.row} needs a value for ${m.columns.join(", ")}.`,
      ),
    [columns, pending.inserts],
  );

  const openReview = useCallback(() => {
    if (!hasPending(pending)) return;
    setApplyError(null);
    try {
      setReview({ script: buildApplyScript(target, pending), error: null });
    } catch (e) {
      setReview({ script: null, error: e instanceof SqlGenError ? e.message : asError(e).message });
    }
  }, [pending, target]);

  // History keeps a one-line summary of an apply, never the script: its statements hold the edited values.
  const record = (script: ApplyScript, ok: boolean, elapsedMs: number | null, error: string | null) => {
    try {
      useHistoryStore.getState().record({
        sql: describeTableEdit(script.counts, schema, name),
        profileId,
        profileName: profile?.name ?? null,
        database,
        environment: profile?.environment ?? null,
        ok,
        elapsedMs,
        rowCount: null,
        error,
      });
    } catch {
      /* history must never get in the way of saving */
    }
  };

  const applyLock = useRef(false);
  const doApply = async (confirmed: boolean) => {
    if (!review?.script || applyLock.current) return;
    const script = review.script;
    applyLock.current = true;
    setApplying(true);
    setApplyError(null);
    const outcome = await executeScript({ profileId, database, sql: script.sql, confirmed }).finally(() => {
      applyLock.current = false;
    });
    setApplying(false);
    if (!outcome.ok) {
      if (outcome.error.code === "confirm_required") {
        setConfirm({ message: outcome.error.message });
        return;
      }
      record(script, false, null, outcome.error.message);
      setApplyError(outcome.error);
      return;
    }
    record(script, true, outcome.elapsedMs, null);
    // statements: BEGIN, <data statements...>, COMMIT. Warn if a keyed UPDATE / DELETE matched no row.
    const data0 = outcome.statements.filter((s) => /^(UPDATE|DELETE)/i.test(s.commandTag ?? ""));
    const missed = data0.filter((s) => s.rowsAffected !== null && s.rowsAffected !== 1).length;
    edits.reset();
    setReview(null);
    setConfirm(null);
    showToast({
      title: `Applied ${script.counts.total} ${script.counts.total === 1 ? "change" : "changes"}`,
      detail: missed > 0 ? `${missed} ${missed === 1 ? "statement" : "statements"} did not match exactly one row: the data may have changed elsewhere.` : `${schema}.${name} · ${Math.round(outcome.elapsedMs)} ms`,
      tone: missed > 0 ? "info" : "ok",
    });
    refresh();
  };

  const refresh = () => {
    data.reload();
    void qc.invalidateQueries({ queryKey: pgQueries.describe(profileId, database, schema, name).queryKey });
  };

  const openInSql = (sql: string) => {
    newSqlTab({ profileId, database, title: `${name}.sql`, contents: sql });
  };

  // ---- render -----------------------------------------------------------------------------------
  if (!profile) return <EmptyState title="Connection not found">The connection this tab used no longer exists.</EmptyState>;
  if (!connected) {
    return (
      <EmptyState icon={<Plug className="size-5" />} title={status?.state === "connecting" ? "Connecting…" : `Not connected to ${profile.name}`}>
        <Button variant="primary" loading={connect.isPending || status?.state === "connecting"} onClick={() => connect.mutate(profileId)}>
          Connect
        </Button>
        {connect.error && <div className="mt-2 text-danger">{asError(connect.error).message}</div>}
      </EmptyState>
    );
  }
  if (descQ.isError) {
    return (
      <div className="p-3">
        <ErrorBox error={asError(descQ.error)} />
        <Button className="mt-3" onClick={() => void descQ.refetch()}>
          Try again
        </Button>
      </div>
    );
  }

  const loading = data.status === "loading" || descQ.isFetching;
  const rowCount = composed.rows.length;
  const isProd = profile.environment === "prod";
  const canDeleteRows = editable && selectedRows().length > 0;

  return (
    <div className="relative flex h-full min-h-0 flex-col bg-bg">
      {isProd && <div className="h-0.5 shrink-0 bg-env-prod" aria-hidden />}
      <TableHeaderBar
        schema={schema}
        name={name}
        kind={desc?.kind ?? null}
        comment={desc?.comment ?? null}
        estimatedRows={desc?.estimatedRows ?? null}
        environment={profile.environment}
        loading={loading}
        filterCount={filterCount}
        filterOpen={filterOpen}
        onToggleFilter={() => setFilterOpen((v) => !v)}
        onRefresh={refresh}
        onOpenSql={() => data.sql && openInSql(`${data.sql};`)}
        onCopyMenu={(anchor) => openFrom(anchor, buildEntries(), "right")}
      />
      {desc && editable && (
        <EditBar
          counts={counts}
          canDeleteRows={canDeleteRows}
          canUndo={edits.canUndo}
          canRedo={edits.canRedo}
          onAddRow={addRow}
          onDeleteRows={() => deleteRows(selectedRows())}
          onUndo={edits.undo}
          onRedo={edits.redo}
          onDiscard={() => edits.update(() => ({ updates: {}, inserts: [], deletes: {} }))}
          onReview={openReview}
        />
      )}
      {desc && readOnlyReason && <ReadOnlyBanner>{readOnlyReason}</ReadOnlyBanner>}
      {filterOpen && columns && (
        <FilterBar
          columns={colRefs}
          mode={mode}
          onModeChange={changeMode}
          rows={draftRows}
          onRowsChange={(r) => {
            setDraftRows(r);
            setFilterErrors({});
          }}
          raw={draftRaw}
          onRawChange={(v) => {
            setDraftRaw(v);
            setFilterErrors({});
          }}
          errors={filterErrors}
          dirty={dirtyFilters}
          active={filterCount > 0}
          onApply={applyFilters}
          onClear={clearFilters}
        />
      )}

      <div className="relative min-h-0 flex-1">
        {loading && <div className="absolute inset-x-0 top-0 z-30 h-0.5 animate-pulse bg-accent" aria-hidden />}
        {data.error && (
          <div className="absolute inset-x-0 top-0 z-30 border-b border-border bg-bg p-2">
            <ErrorBox error={data.error} />
          </div>
        )}
        {!columns ? (
          <div className="flex h-full items-center justify-center gap-2 text-muted">
            <Loader2 className="size-4 animate-spin" /> Loading columns…
          </div>
        ) : (
          <DataGrid
            columns={gridColumns}
            rows={composed.rows}
            rowCount={rowCount}
            selection={selection}
            onSelectionChange={setSelection}
            onOpenCell={(r, c) => setViewer({ r, c })}
            onCopy={() => {
              const first = buildEntries()[0];
              if (first !== "separator" && first && !first.disabled) first.onSelect();
            }}
            sort={sortKeys}
            onHeaderClick={onHeaderClick}
            onHeaderMenu={onHeaderMenu}
            onCellContextMenu={onCellContextMenu}
            onEditCell={editable ? startEdit : undefined}
            editing={editing}
            renderEditor={renderEditor}
            rowState={editable ? rowState : undefined}
            cellDirty={editable ? cellDirty : undefined}
            cellOverride={editable ? cellOverride : undefined}
            readOnlyCol={editable ? readOnlyCol : undefined}
            onKeyDownExtra={onKeyDownExtra}
            rowLabel={rowLabel}
            focusToken={focusToken}
            gutterChars={String(data.pageIndex * pageSize + data.rows.length + composed.newCount).length}
          />
        )}
        {columns && rowCount === 0 && data.status === "ready" && (
          <div className="pointer-events-none absolute inset-x-0 top-[30px] flex flex-col items-center gap-1 p-10 text-center text-xs text-muted">
            <div className="text-sm font-medium text-fg">{data.whereActive ? "No rows match these filters" : "This table is empty"}</div>
            {data.whereActive ? "Loosen or clear the filters to see more." : editable ? "Use Add row to insert the first one." : null}
          </div>
        )}
        {flash && (
          <div role="status" className="absolute bottom-2 left-1/2 z-30 -translate-x-1/2 rounded-md border border-ok/40 bg-panel px-2.5 py-1 text-xs text-ok shadow-lg">
            {flash}
          </div>
        )}
      </div>

      <TableFooter
        pageIndex={data.pageIndex}
        pageSize={pageSize}
        rowsOnPage={data.rows.length}
        hasNext={data.hasNext}
        loading={data.status === "loading"}
        exactTotal={data.exactTotal}
        estimatedRows={desc?.estimatedRows ?? null}
        filtered={data.whereActive}
        countStatus={data.countStatus}
        keyset={data.keyset}
        elapsedMs={data.elapsedMs}
        onFirst={data.first}
        onPrev={data.prev}
        onNext={data.next}
        onPageSize={setPageSize}
        onCount={data.loadCount}
      />

      {menuNode}
      {exportDialog}
      <CellViewerDialog
        open={viewer !== null}
        onClose={() => setViewer(null)}
        column={viewer ? (gridColumns[viewer.c] ?? null) : null}
        rowIndex={viewer?.r ?? 0}
        cell={viewer ? valueAt(viewer.r, viewer.c) : null}
      />
      {dialogEdit && columns?.[dialogEdit.c] && (
        <LongValueEditor
          key={`${dialogEdit.r}:${dialogEdit.c}`}
          column={toEditorColumn(columns[dialogEdit.c])}
          value={valueAt(dialogEdit.r, dialogEdit.c)}
          title={`${columns[dialogEdit.c].name}  ·  ${shortTypeName(columns[dialogEdit.c].dataType)}`}
          onClose={() => setDialogEdit(null)}
          onCommit={(cell) => {
            writeCell(dialogEdit.r, dialogEdit.c, cell);
            setDialogEdit(null);
          }}
        />
      )}
      <ReviewDialog
        open={review !== null}
        tableLabel={`${schema}.${name}`}
        script={review?.script ?? null}
        scriptError={review?.error ?? null}
        problems={problems}
        applying={applying}
        error={applyError}
        environment={profile.environment}
        connectionName={profile.name}
        onApply={() => void doApply(false)}
        onClose={() => {
          setReview(null);
          setApplyError(null);
        }}
        onOpenInSql={openInSql}
      />
      <ConfirmDialog
        open={confirm !== null}
        title={isProd ? "Confirm change on a production database" : "Confirm change"}
        danger
        confirmText="Apply to database"
        requireTyping={isProd ? profile.name : undefined}
        onCancel={() => setConfirm(null)}
        onConfirm={() => void doApply(true)}
        body={
          <>
            <p>{confirm?.message}</p>
            <p className="text-muted">
              {formatCount(counts.total)} {counts.total === 1 ? "change" : "changes"} to <span className="font-mono text-fg">{schema}.{name}</span> on {profile.name}. They run in one transaction.
            </p>
          </>
        }
      />
    </div>
  );
}
