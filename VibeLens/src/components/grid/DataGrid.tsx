// Virtualised result grid. TanStack Table v9 owns the column model (header, sizing, drag-resize);
// the body is rendered straight from the runner's row buffer. Feeding 100k rows through the table's
// row model would allocate a Row object per row (and rebuild them on every streamed page), so the
// table is given an empty data array and rows/cells are virtualised by hand: only the visible
// rows x visible columns exist in the DOM.
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, type KeyboardEvent, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { columnResizingFeature, columnSizingFeature, tableFeatures, useTable, type ColumnDef } from "@tanstack/react-table";
import { useVirtualizer } from "@tanstack/react-virtual";
import { ArrowDown, ArrowUp, Braces, ChevronDown } from "lucide-react";
import type { Cell, ColumnMeta } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { cellKind, displayText, estimateColumnWidth, isNumericType, type BytesCell, type CellRange } from "./cellFormat";

export interface GridSelection {
  anchor: { r: number; c: number };
  focus: { r: number; c: number };
}

export const selectionRange = (s: GridSelection): CellRange => ({
  r0: Math.min(s.anchor.r, s.focus.r),
  r1: Math.max(s.anchor.r, s.focus.r),
  c0: Math.min(s.anchor.c, s.focus.c),
  c1: Math.max(s.anchor.c, s.focus.c),
});

const ROW_H = 24;
const HEADER_H = 30;
const features = tableFeatures({ columnSizingFeature, columnResizingFeature });
type GridRow = Cell[];
const NO_ROWS: GridRow[] = []; // stable identity: the table never sees real rows

/** Visual state of a row in an editable grid. */
export type RowState = "new" | "deleted" | undefined;

export interface GridSortKey {
  col: number;
  dir: "asc" | "desc";
}

export interface DataGridProps {
  columns: ColumnMeta[];
  /** Live, mutable row buffer (see useQueryRunner). */
  rows: Cell[][];
  /** Row count at render time; changes whenever `rows` grew. */
  rowCount: number;
  selection: GridSelection | null;
  onSelectionChange: (s: GridSelection | null) => void;
  /** Double-click / expand: open the full value. */
  onOpenCell: (row: number, col: number) => void;
  onCopy: () => void;

  // ---- optional: sorting (table tab) --------------------------------------------------------
  /** Sort indicators in the header, in priority order. */
  sort?: readonly GridSortKey[];
  /** When set, a header click calls this instead of selecting the column (shift = multi-sort). */
  onHeaderClick?: (col: number, e: ReactMouseEvent) => void;
  /** Column menu (the chevron on hover, or right-click on the header). */
  onHeaderMenu?: (col: number, e: ReactMouseEvent | { clientX: number; clientY: number; anchor: HTMLElement }) => void;
  /** Right-click on a body cell. */
  onCellContextMenu?: (e: ReactMouseEvent, row: number, col: number) => void;

  // ---- optional: inline editing (table tab) -------------------------------------------------
  /** Start editing a cell: double-click, Enter, F2, or typing (then `initial` is the typed character). */
  onEditCell?: (row: number, col: number, initial?: string) => void;
  /** The cell currently being edited; `renderEditor` draws its editor in place. */
  editing?: { r: number; c: number } | null;
  renderEditor?: (row: number, col: number) => ReactNode;
  rowState?: (row: number) => RowState;
  /** True for cells with an unsaved change. */
  cellDirty?: (row: number, col: number) => boolean;
  /** Replace a cell's rendering (e.g. "DEFAULT" placeholders in new rows). Return null/undefined for the normal look. */
  cellOverride?: (row: number, col: number, value: Cell) => ReactNode;
  /** Columns that cannot be edited get a subtle hint on hover. */
  readOnlyCol?: (col: number) => boolean;
  /** First look at every key press; return true to stop the grid handling it. */
  onKeyDownExtra?: (e: KeyboardEvent<HTMLDivElement>) => boolean;
  /** Text of the row-number gutter (default: 1-based row index, "+" for new rows). */
  rowLabel?: (row: number) => string | number;
  /** Widest row label in characters, so the gutter fits page-offset numbers. */
  gutterChars?: number;
  /** Bump to move keyboard focus into the grid (after a toolbar action such as "Add row"). */
  focusToken?: number;
}

function renderCell(cell: Cell, numeric: boolean, onExpand: (() => void) | null) {
  switch (cellKind(cell)) {
    case "null":
      return <span className="italic text-muted/70">NULL</span>;
    case "bool":
      return <span className="text-violet-700 dark:text-violet-300">{String(cell)}</span>;
    case "number":
      return <span className="tabular-nums text-amber-700 dark:text-amber-300">{String(cell)}</span>;
    case "bytes":
      return <span className="italic text-muted">‹bytes {(cell as BytesCell).len}›</span>;
    case "json":
      return (
        <span className="flex items-center gap-1 text-sky-700 dark:text-sky-300">
          <button
            type="button"
            tabIndex={-1}
            aria-label="Expand JSON value"
            title="Expand"
            onMouseDown={(e) => e.stopPropagation()}
            onClick={(e) => {
              e.stopPropagation();
              onExpand?.();
            }}
            className="inline-flex size-4 shrink-0 items-center justify-center rounded text-muted hover:bg-border hover:text-fg"
          >
            <Braces className="size-3" />
          </button>
          <span className="truncate">{displayText(cell)}</span>
        </span>
      );
    default:
      return <span className={cn(numeric && "tabular-nums")}>{displayText(cell)}</span>;
  }
}

export const DataGrid = memo(function DataGrid({
  columns,
  rows,
  rowCount,
  selection,
  onSelectionChange,
  onOpenCell,
  onCopy,
  sort,
  onHeaderClick,
  onHeaderMenu,
  onCellContextMenu,
  onEditCell,
  editing,
  renderEditor,
  rowState,
  cellDirty,
  cellOverride,
  readOnlyCol,
  onKeyDownExtra,
  rowLabel,
  gutterChars,
  focusToken,
}: DataGridProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const autoSizedRef = useRef(false);

  const numeric = useMemo(() => columns.map((c) => isNumericType(c.typeName)), [columns]);
  // Room for the sort arrow and the column menu chevron in the header.
  const headerExtra = onHeaderClick || onHeaderMenu ? 34 : 0;

  const columnDefs = useMemo<ColumnDef<typeof features, GridRow>[]>(
    () =>
      columns.map((c, i) => ({
        id: `c${i}`,
        header: c.name,
        accessorFn: (row: GridRow) => row[i],
        size: Math.round(Math.min(420, Math.max(80, c.name.length * 7.4 + c.typeName.length * 5.6 + 44))),
        minSize: 48,
        maxSize: 1600,
      })),
    [columns],
  );

  const table = useTable({ features, columns: columnDefs, data: NO_ROWS, columnResizeMode: "onChange" });
  const leafCols = table.getAllLeafColumns();
  const sizes = leafCols.map((c) => c.getSize());
  const sizesKey = sizes.join(",");
  const sizesRef = useRef(sizes);
  sizesRef.current = sizes;

  // Fit columns to their content once the first rows arrive (user resizing afterwards is respected).
  useEffect(() => {
    autoSizedRef.current = false;
  }, [columns]);
  useEffect(() => {
    if (autoSizedRef.current || rowCount === 0 || columns.length === 0) return;
    autoSizedRef.current = true;
    const next: Record<string, number> = {};
    columns.forEach((c, i) => {
      next[`c${i}`] = estimateColumnWidth(c, rows, i, headerExtra);
    });
    table.setColumnSizing(next);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rowCount, columns, rows, table]);

  const gutterW = Math.max(44, (gutterChars ?? String(Math.max(rowCount, 1)).length) * 8 + 22);

  useEffect(() => {
    if (focusToken) scrollRef.current?.focus({ preventScroll: true });
  }, [focusToken]);

  // Give the keyboard back to the grid when an editor closes.
  const wasEditing = useRef(false);
  useEffect(() => {
    if (wasEditing.current && !editing) scrollRef.current?.focus({ preventScroll: true });
    wasEditing.current = !!editing;
  }, [editing]);

  const rowVirt = useVirtualizer({
    count: rowCount,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    overscan: 12,
    scrollPaddingStart: HEADER_H,
  });
  const colVirt = useVirtualizer({
    horizontal: true,
    count: columns.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => sizesRef.current[i] ?? 120,
    overscan: 3,
    paddingStart: gutterW,
    scrollPaddingStart: gutterW,
  });
  useLayoutEffect(() => {
    colVirt.measure();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sizesKey, gutterW, columns]);

  const range = selection ? selectionRange(selection) : null;
  const lastRow = rowCount - 1;
  const lastCol = columns.length - 1;

  const setSel = useCallback(
    (r: number, c: number, extend: boolean) => {
      onSelectionChange(extend && selection ? { anchor: selection.anchor, focus: { r, c } } : { anchor: { r, c }, focus: { r, c } });
    },
    [onSelectionChange, selection],
  );

  const onCellMouseDown = (e: ReactMouseEvent, r: number, c: number) => {
    if (e.button !== 0) return;
    if (e.detail > 1) e.preventDefault(); // a double-click opens the cell, it should not also select a word
    scrollRef.current?.focus({ preventScroll: true });
    setSel(r, c, e.shiftKey);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (editing) return; // the cell editor owns the keyboard
    if (onKeyDownExtra?.(e)) return;
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === "c") {
      e.preventDefault();
      onCopy();
      return;
    }
    if (mod && e.key.toLowerCase() === "a") {
      e.preventDefault();
      if (rowCount > 0 && columns.length > 0) onSelectionChange({ anchor: { r: 0, c: 0 }, focus: { r: lastRow, c: lastCol } });
      return;
    }
    if (rowCount === 0 || columns.length === 0) return;
    const cur = selection?.focus ?? { r: 0, c: 0 };
    let { r, c } = cur;
    switch (e.key) {
      case "ArrowDown":
        r = Math.min(lastRow, r + 1);
        break;
      case "ArrowUp":
        r = Math.max(0, r - 1);
        break;
      case "ArrowRight":
        c = Math.min(lastCol, c + 1);
        break;
      case "ArrowLeft":
        c = Math.max(0, c - 1);
        break;
      case "PageDown":
        r = Math.min(lastRow, r + Math.max(1, Math.floor((scrollRef.current?.clientHeight ?? 300) / ROW_H) - 2));
        break;
      case "PageUp":
        r = Math.max(0, r - Math.max(1, Math.floor((scrollRef.current?.clientHeight ?? 300) / ROW_H) - 2));
        break;
      case "Home":
        if (mod) r = 0;
        c = 0;
        break;
      case "End":
        if (mod) r = lastRow;
        c = lastCol;
        break;
      case "Enter":
      case "F2":
        if (selection) {
          e.preventDefault();
          if (onEditCell) onEditCell(cur.r, cur.c);
          else if (e.key === "Enter") onOpenCell(cur.r, cur.c);
        }
        return;
      case "Escape":
        onSelectionChange(null);
        return;
      default:
        // Typing on a selected cell starts editing it, spreadsheet style.
        if (onEditCell && selection && e.key.length === 1 && !mod && !e.altKey) {
          e.preventDefault();
          onEditCell(cur.r, cur.c, e.key);
        }
        return;
    }
    e.preventDefault();
    setSel(r, c, e.shiftKey);
    rowVirt.scrollToIndex(r);
    colVirt.scrollToIndex(c);
  };

  const rowItems = rowVirt.getVirtualItems();
  const colItems = colVirt.getVirtualItems();
  const totalW = Math.max(colVirt.getTotalSize(), gutterW);
  const headerGroup = table.getHeaderGroups()[0];
  const headers = headerGroup?.headers ?? [];

  return (
    <div
      ref={scrollRef}
      tabIndex={0}
      role="grid"
      aria-rowcount={rowCount}
      aria-colcount={columns.length}
      onKeyDown={onKeyDown}
      className="selectable relative h-full overflow-auto bg-bg font-mono text-[12px] outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent/50"
    >
      <div style={{ width: totalW, minWidth: "100%" }}>
        {/* header */}
        <div role="row" className="sticky top-0 z-20 flex border-b border-border bg-panel" style={{ width: totalW, height: HEADER_H }}>
          <div
            className="sticky left-0 z-30 shrink-0 border-r border-border bg-panel"
            style={{ width: gutterW }}
            onClick={() => rowCount > 0 && onSelectionChange({ anchor: { r: 0, c: 0 }, focus: { r: lastRow, c: lastCol } })}
            title="Select all"
          />
          {colItems.map((item) => {
            const header = headers[item.index];
            const meta = columns[item.index];
            if (!header || !meta) return null;
            const selectedCol = range && range.c0 <= item.index && item.index <= range.c1 && range.r0 === 0 && range.r1 === lastRow;
            const sortPos = sort ? sort.findIndex((k) => k.col === item.index) : -1;
            const sortKey = sortPos >= 0 && sort ? sort[sortPos] : null;
            return (
              <div
                key={item.key}
                role="columnheader"
                aria-sort={sortKey ? (sortKey.dir === "asc" ? "ascending" : "descending") : undefined}
                onClick={(e) =>
                  onHeaderClick
                    ? onHeaderClick(item.index, e)
                    : rowCount > 0 && onSelectionChange({ anchor: { r: 0, c: item.index }, focus: { r: lastRow, c: item.index } })
                }
                onContextMenu={
                  onHeaderMenu
                    ? (e) => {
                        e.preventDefault();
                        onHeaderMenu(item.index, e);
                      }
                    : undefined
                }
                className={cn(
                  "group absolute top-0 flex h-full items-center gap-1.5 border-r border-border px-2 font-sans",
                  onHeaderClick && "cursor-pointer",
                  selectedCol ? "bg-accent/10" : "hover:bg-panel-2",
                )}
                style={{ left: item.start, width: item.size }}
              >
                <span className="min-w-0 truncate text-[12px] font-semibold text-fg" title={meta.name}>
                  <table.FlexRender header={header} />
                </span>
                <span className="shrink-0 truncate font-mono text-[10px] text-muted" title={meta.typeName}>
                  {meta.typeName}
                </span>
                {sortKey && (
                  <span className="ml-auto flex shrink-0 items-center gap-0.5 text-accent" title={`Sorted ${sortKey.dir === "asc" ? "ascending" : "descending"}`}>
                    {sortKey.dir === "asc" ? <ArrowUp className="size-3" /> : <ArrowDown className="size-3" />}
                    {sort && sort.length > 1 && <span className="text-[10px] font-semibold tabular-nums">{sortPos + 1}</span>}
                  </span>
                )}
                {onHeaderMenu && (
                  <button
                    type="button"
                    tabIndex={-1}
                    aria-label={`Column menu for ${meta.name}`}
                    onMouseDown={(e) => e.stopPropagation()}
                    onClick={(e) => {
                      e.stopPropagation();
                      onHeaderMenu(item.index, { clientX: 0, clientY: 0, anchor: e.currentTarget });
                    }}
                    className={cn(
                      "flex size-4 shrink-0 items-center justify-center rounded text-muted opacity-0 hover:bg-border hover:text-fg group-hover:opacity-100",
                      !sortKey && "ml-auto",
                    )}
                  >
                    <ChevronDown className="size-3" />
                  </button>
                )}
                <div
                  role="separator"
                  aria-orientation="vertical"
                  aria-label={`Resize column ${meta.name}`}
                  onMouseDown={(e) => {
                    e.stopPropagation();
                    header.getResizeHandler()(e);
                  }}
                  onTouchStart={(e) => {
                    e.stopPropagation();
                    header.getResizeHandler()(e);
                  }}
                  onClick={(e) => e.stopPropagation()}
                  onDoubleClick={(e) => {
                    e.stopPropagation();
                    table.setColumnSizing((old) => ({ ...old, [header.column.id]: estimateColumnWidth(meta, rows, item.index, headerExtra) }));
                  }}
                  className={cn(
                    "absolute right-0 top-0 h-full w-1.5 cursor-col-resize touch-none select-none hover:bg-accent/60",
                    header.column.getIsResizing() && "bg-accent",
                  )}
                />
              </div>
            );
          })}
        </div>

        {/* body */}
        <div role="rowgroup" style={{ height: rowVirt.getTotalSize(), position: "relative", width: totalW }}>
          {rowItems.map((ri) => {
            const row = rows[ri.index];
            if (!row) return null;
            const inRow = range && ri.index >= range.r0 && ri.index <= range.r1;
            const state = rowState?.(ri.index);
            return (
              <div
                key={ri.key}
                role="row"
                aria-rowindex={ri.index + 1}
                className={cn(
                  "vg-row absolute left-0 flex border-b border-border/40",
                  state === "new" ? "bg-ok/10" : state === "deleted" ? "bg-danger/10 text-muted line-through decoration-danger/70" : ri.index % 2 === 1 && "bg-panel/40",
                )}
                style={{ top: 0, transform: `translateY(${ri.start}px)`, height: ROW_H, width: totalW, zIndex: editing?.r === ri.index ? 15 : undefined }}
              >
                <div
                  className={cn(
                    "sticky left-0 z-10 shrink-0 cursor-default border-r border-border bg-panel pr-2 text-right text-[11px] leading-6 tabular-nums",
                    inRow && range && range.c0 === 0 && range.c1 === lastCol ? "bg-accent/15 text-fg" : "text-muted",
                    state === "new" && "font-semibold text-ok shadow-[inset_3px_0_0_var(--vg-ok)]",
                    state === "deleted" && "text-danger shadow-[inset_3px_0_0_var(--vg-danger)]",
                  )}
                  style={{ width: gutterW }}
                  onMouseDown={(e) => {
                    if (e.button !== 0) return;
                    scrollRef.current?.focus({ preventScroll: true });
                    onSelectionChange(
                      e.shiftKey && selection
                        ? { anchor: selection.anchor, focus: { r: ri.index, c: lastCol } }
                        : { anchor: { r: ri.index, c: 0 }, focus: { r: ri.index, c: lastCol } },
                    );
                  }}
                >
                  {rowLabel ? rowLabel(ri.index) : state === "new" ? "+" : ri.index + 1}
                </div>
                {colItems.map((ci) => {
                  const value = row[ci.index] ?? null;
                  const isSel = !!range && inRow && ci.index >= range.c0 && ci.index <= range.c1;
                  const isFocus = !!selection && selection.focus.r === ri.index && selection.focus.c === ci.index;
                  const kind = cellKind(value);
                  const alignRight = kind === "number" || (numeric[ci.index] && kind === "text");
                  const isEditing = !!editing && editing.r === ri.index && editing.c === ci.index;
                  const dirty = !isSel && !!cellDirty?.(ri.index, ci.index);
                  const custom = cellOverride?.(ri.index, ci.index, value);
                  return (
                    <div
                      key={ci.key}
                      role="gridcell"
                      aria-selected={isSel || undefined}
                      onMouseDown={(e) => !isEditing && onCellMouseDown(e, ri.index, ci.index)}
                      onDoubleClick={() => (onEditCell ? onEditCell(ri.index, ci.index) : onOpenCell(ri.index, ci.index))}
                      onContextMenu={
                        onCellContextMenu
                          ? (e) => {
                              if (!isSel) setSel(ri.index, ci.index, false);
                              onCellContextMenu(e, ri.index, ci.index);
                            }
                          : undefined
                      }
                      title={onEditCell && readOnlyCol?.(ci.index) ? "Generated column: read-only" : undefined}
                      className={cn(
                        "absolute top-0 h-full whitespace-nowrap border-r border-border/30 leading-6",
                        isEditing ? "overflow-visible" : "overflow-hidden text-ellipsis px-2",
                        alignRight && "text-right",
                        dirty && "bg-warn/20 shadow-[inset_2px_0_0_var(--vg-warn)]",
                        isSel && "bg-accent/15",
                        isFocus && "outline outline-1 -outline-offset-1 outline-accent",
                        isEditing && "z-10 bg-bg outline outline-2 -outline-offset-2 outline-accent",
                      )}
                      style={{ left: ci.start, width: ci.size }}
                    >
                      {isEditing && renderEditor
                        ? renderEditor(ri.index, ci.index)
                        : (custom ?? renderCell(value, numeric[ci.index], kind === "json" ? () => onOpenCell(ri.index, ci.index) : null))}
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
});
