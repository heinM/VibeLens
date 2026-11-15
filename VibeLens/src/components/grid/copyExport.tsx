// The Copy / Export menu shared by SQL result sets and table tabs.
import { useCallback, useRef, useState, type ReactNode } from "react";
import type { Cell, ColumnMeta } from "../../ipc/types";
import type { MenuEntry } from "../ui/Menu";
import { copyToClipboard } from "../ui/clipboard";
import type { InsertTarget } from "./exportFormats";
import { defuseFormula, toJson, toMarkdownTable, toSqlInserts } from "./exportFormats";
import { copyText, formatCount, formatDelimited, type CellRange } from "./cellFormat";
import { ExportDialog, type ExportFormat } from "./ExportDialog";

export interface CopyExportArgs {
  columns: ColumnMeta[];
  /** The rows as displayed (selection ranges index into these); read when a menu item runs, never captured. */
  rows: Cell[][];
  /** Rows for "all rows" copies and exports when they differ from what is displayed (table tabs: loaded rows without unsaved edits). */
  sourceRows?: Cell[][];
  /** Current grid selection as a cell range, or null. */
  getRange: () => CellRange | null;
  /** Source table when known: used for INSERT export and the file name. */
  table?: InsertTarget | null;
  /** File name stem for exports ("orders", "query-1"). */
  baseName: string;
  /** Small confirmation near the toolbar ("Copied 12 rows"). */
  notify: (message: string) => void;
}

function sliceRange(columns: ColumnMeta[], rows: Cell[][], r: CellRange): { cols: ColumnMeta[]; rows: Cell[][] } {
  return { cols: columns.slice(r.c0, r.c1 + 1), rows: rows.slice(r.r0, r.r1 + 1).map((row) => row.slice(r.c0, r.c1 + 1)) };
}

export function useCopyExport(args: CopyExportArgs): { buildEntries: () => MenuEntry[]; dialog: ReactNode } {
  const ref = useRef(args);
  ref.current = args;
  const [format, setFormat] = useState<ExportFormat | null>(null);

  const copy = useCallback(async (text: string, label: string) => {
    if (await copyToClipboard(text)) ref.current.notify(label);
  }, []);

  const buildEntries = useCallback((): MenuEntry[] => {
    const { columns, getRange } = ref.current;
    const rows = ref.current.sourceRows ?? ref.current.rows;
    const sel = getRange();
    const selCells = sel ? (sel.r1 - sel.r0 + 1) * (sel.c1 - sel.c0 + 1) : 0;
    const none = rows.length === 0 || columns.length === 0;
    const all = (): CellRange => ({ r0: 0, r1: rows.length - 1, c0: 0, c1: columns.length - 1 });

    // Clipboard CSV/TSV is pasted into spreadsheets, so text starting with = + - @ is defused like a
    // file export. A plain single-cell copy (no header) stays byte-for-byte what the cell holds.
    const copySel = (delimiter: "," | "\t", header: boolean) => {
      const s = getRange();
      if (!s) return;
      const cells = (s.r1 - s.r0 + 1) * (s.c1 - s.c0 + 1);
      const text = cells === 1 && !header ? copyText(ref.current.rows[s.r0]?.[s.c0] ?? null) : formatDelimited(ref.current.columns, ref.current.rows, s, delimiter, header, defuseFormula);
      void copy(text, cells === 1 ? "Copied cell" : `Copied ${formatCount(cells)} cells`);
    };
    const copyAll = (delimiter: "," | "\t") => void copy(formatDelimited(ref.current.columns, rows, all(), delimiter, true, defuseFormula), `Copied ${formatCount(rows.length)} rows`);

    return [
      { label: "Copy selection", hint: "TSV", disabled: !sel, onSelect: () => copySel("\t", false) },
      { label: "Copy selection with header", hint: "TSV", disabled: !sel, onSelect: () => copySel("\t", true) },
      { label: "Copy selection as CSV", disabled: !sel, onSelect: () => copySel(",", true) },
      {
        label: "Copy selection as Markdown",
        disabled: !sel,
        onSelect: () => {
          const s = getRange();
          if (!s) return;
          const part = sliceRange(ref.current.columns, ref.current.rows, s);
          void copy(toMarkdownTable(part.cols, part.rows), `Copied ${formatCount(selCells)} cells as Markdown`);
        },
      },
      {
        label: "Copy selection as INSERT",
        disabled: !sel,
        onSelect: () => {
          const s = getRange();
          if (!s) return;
          const part = sliceRange(ref.current.columns, ref.current.rows, s);
          void copy(toSqlInserts(part.cols, part.rows, ref.current.table ?? null), `Copied ${formatCount(part.rows.length)} rows as INSERT`);
        },
      },
      "separator",
      { label: `Copy all ${formatCount(rows.length)} rows`, hint: "TSV", disabled: none, onSelect: () => copyAll("\t") },
      { label: "Copy all rows as CSV", disabled: none, onSelect: () => copyAll(",") },
      { label: "Copy all rows as JSON", disabled: none, onSelect: () => void copy(toJson(ref.current.columns, rows), `Copied ${formatCount(rows.length)} rows as JSON`) },
      { label: "Copy all rows as Markdown table", disabled: none, onSelect: () => void copy(toMarkdownTable(ref.current.columns, rows), `Copied ${formatCount(rows.length)} rows as Markdown`) },
      "separator",
      { label: "Export CSV…", hint: "RFC 4180", disabled: columns.length === 0, onSelect: () => setFormat("csv") },
      { label: "Export JSON…", disabled: columns.length === 0, onSelect: () => setFormat("json") },
      { label: "Export as SQL INSERTs…", disabled: columns.length === 0, onSelect: () => setFormat("sql") },
    ];
  }, [copy]);

  const dialog = (
    <ExportDialog
      format={format}
      onClose={() => setFormat(null)}
      columns={args.columns}
      getRows={() => ref.current.sourceRows ?? ref.current.rows}
      baseName={args.baseName}
      table={args.table ?? null}
    />
  );
  return { buildEntries, dialog };
}
