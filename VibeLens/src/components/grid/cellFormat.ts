// Pure helpers for rendering / copying result cells.
import type { Cell, ColumnMeta, TruncatedCell } from "../../ipc/types";

export type CellKind = "null" | "bool" | "number" | "text" | "bytes" | "json" | "truncated";

export interface BytesCell {
  $bytes: string;
  len: number;
}

export function isBytesCell(c: unknown): c is BytesCell {
  return (
    typeof c === "object" &&
    c !== null &&
    !Array.isArray(c) &&
    typeof (c as Record<string, unknown>).$bytes === "string" &&
    typeof (c as Record<string, unknown>).len === "number"
  );
}

// ---------------------------------------------------------------------------------------------
// Values that were too large to load in full
// ---------------------------------------------------------------------------------------------
// The backend sends only the start of a value over 1 MiB, inside a marker (`TruncatedCell`). The
// start is for looking at. Everything that copies, exports or writes a cell goes through the
// helpers below so the start is never passed off as the value.

export function isTruncatedCell(c: unknown): c is TruncatedCell {
  if (typeof c !== "object" || c === null || Array.isArray(c)) return false;
  const keys = Object.keys(c);
  if (keys.length !== 1 || keys[0] !== "$truncated") return false;
  const t = (c as Record<string, unknown>).$truncated as Record<string, unknown> | null;
  return typeof t === "object" && t !== null && typeof t.kind === "string" && typeof t.bytes === "number" && typeof t.preview === "string";
}

/** The first marker in `v`, looking inside arrays and objects (an Iceberg struct can hold one). */
export function findTruncated(v: unknown, depth = 0): TruncatedCell | null {
  if (typeof v !== "object" || v === null || depth > 32) return null;
  if (isTruncatedCell(v)) return v;
  for (const x of Array.isArray(v) ? v : Object.values(v)) {
    const found = findTruncated(x, depth + 1);
    if (found) return found;
  }
  return null;
}

export const containsTruncated = (v: unknown): boolean => findTruncated(v) !== null;

/** "50.0 MB": binary units, one decimal. */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} bytes`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(1)} GB`;
}

const TRUNCATED_WHAT: Record<string, string> = { text: "text", json: "JSON", bytes: "binary data" };

/**
 * What stands in for a cut value wherever cell text leaves the grid (clipboard, CSV, Markdown, JSON):
 * a note with the size and no part of the value.
 */
export function truncatedNote(c: TruncatedCell): string {
  const what = TRUNCATED_WHAT[c.$truncated.kind];
  return `‹value not loaded in full: ${formatCount(c.$truncated.bytes)} bytes${what ? ` of ${what}` : ""}›`;
}

/** How many cells in `rows` (within `range`, when given) are or contain a cut value. */
export function countTruncated(rows: readonly (readonly Cell[])[], range?: CellRange): number {
  const r0 = range?.r0 ?? 0;
  const r1 = Math.min(range?.r1 ?? rows.length - 1, rows.length - 1);
  let n = 0;
  for (let r = r0; r <= r1; r++) {
    const row = rows[r];
    if (!row) continue;
    const c1 = Math.min(range?.c1 ?? row.length - 1, row.length - 1);
    for (let c = range?.c0 ?? 0; c <= c1; c++) {
      const v = row[c];
      if (typeof v === "object" && v !== null && containsTruncated(v)) n++;
    }
  }
  return n;
}

/** Adds to a "Copied …" / "Exported …" message that some values are placeholders. */
export function withTruncatedNote(message: string, truncated: number): string {
  if (truncated <= 0) return message;
  return `${message}. ${truncated === 1 ? "1 value was too large to load and is a placeholder" : `${formatCount(truncated)} values were too large to load and are placeholders`}, not data`;
}

/** A JSON value holding a number JS cannot have received exactly (a big integer or a 16+ digit decimal). */
export function hasInexactNumber(v: unknown): boolean {
  if (typeof v === "number") {
    if (Number.isInteger(v)) return !Number.isSafeInteger(v);
    return String(Math.abs(v)).replace(/e.*$/, "").replace(".", "").replace(/^0+/, "").length >= 16;
  }
  if (typeof v !== "object" || v === null) return false;
  return Object.values(v).some(hasInexactNumber);
}

export function cellKind(c: Cell): CellKind {
  if (c === null || c === undefined) return "null";
  if (typeof c === "boolean") return "bool";
  if (typeof c === "number") return "number";
  if (typeof c === "string") return "text";
  if (isBytesCell(c)) return "bytes";
  if (isTruncatedCell(c)) return "truncated";
  return "json";
}

const NUMERIC_TYPE = /^(int\d?|integer|smallint|bigint|serial\d?|bigserial|smallserial|oid|float\d?|real|double|numeric|decimal|money|xid|cid)/i;
export const isNumericType = (typeName: string): boolean => NUMERIC_TYPE.test(typeName.trim());
export const NUMERIC_STRING = /^[-+]?(\d+\.?\d*|\.\d+)([eE][-+]?\d+)?$/;
export const isNumericString = (s: string): boolean => NUMERIC_STRING.test(s);

const jsonCache = new WeakMap<object, string>();
/** Compact single-line JSON, cached per object (rows are immutable once streamed in). */
export function jsonPreview(value: object, max = 240): string {
  let s = jsonCache.get(value);
  if (s === undefined) {
    try {
      s = JSON.stringify(value) ?? "";
    } catch {
      s = "[unserialisable]";
    }
    jsonCache.set(value, s);
  }
  return s.length > max ? s.slice(0, max) + "…" : s;
}

/** Text shown in a grid cell (and used for width estimation). */
export function displayText(c: Cell): string {
  switch (cellKind(c)) {
    case "null":
      return "NULL";
    case "bool":
    case "number":
      return String(c);
    case "text": {
      const s = c as string;
      return s.length > 400 ? s.slice(0, 400) + "…" : s;
    }
    case "bytes":
      return `‹bytes ${(c as BytesCell).len}›`;
    case "truncated": {
      const t = (c as TruncatedCell).$truncated;
      const what = TRUNCATED_WHAT[t.kind];
      const label = `‹cut: ${formatBytes(t.bytes)}${what ? ` of ${what}` : " value"}›`;
      const start = t.kind === "text" || t.kind === "json" ? t.preview.slice(0, 300) : "";
      return start ? `${label} ${start}…` : label;
    }
    case "json":
      return jsonPreview(c as object);
  }
}

/**
 * Full-fidelity text for the clipboard (NULL becomes empty, JSON stays JSON). A value that was cut
 * when it was read has no full text: it is copied as a note that says so (`truncatedNote`).
 */
export function copyText(c: Cell): string {
  switch (cellKind(c)) {
    case "null":
      return "";
    case "bool":
    case "number":
    case "text":
      return String(c);
    case "bytes":
      return `‹bytes ${(c as BytesCell).len}›`;
    case "truncated":
      return truncatedNote(c as TruncatedCell);
    case "json":
      try {
        return JSON.stringify(c);
      } catch {
        return "";
      }
  }
}

/** RFC-4180-ish field escaping, parameterised on the delimiter (`,` for CSV, `\t` for TSV). */
export function escapeField(s: string, delimiter: string): string {
  if (s.includes(delimiter) || s.includes('"') || s.includes("\n") || s.includes("\r")) {
    return `"${s.replace(/"/g, '""')}"`;
  }
  return s;
}

export interface CellRange {
  r0: number;
  r1: number;
  c0: number;
  c1: number;
}

/**
 * `defuse` rewrites text values (and header names) before escaping; pass `defuseFormula` from
 * exportFormats for anything a spreadsheet may open. Only string cells are rewritten: numbers,
 * booleans and JSON never start a formula.
 */
export function formatDelimited(
  columns: readonly ColumnMeta[],
  rows: readonly (readonly Cell[])[],
  range: CellRange,
  delimiter: "," | "\t",
  withHeader: boolean,
  defuse?: (s: string) => string,
): string {
  const lines: string[] = [];
  if (withHeader) {
    const h: string[] = [];
    for (let c = range.c0; c <= range.c1; c++) h.push(escapeField(defuse ? defuse(columns[c]?.name ?? "") : (columns[c]?.name ?? ""), delimiter));
    lines.push(h.join(delimiter));
  }
  for (let r = range.r0; r <= range.r1; r++) {
    const row = rows[r];
    if (!row) continue;
    const f: string[] = [];
    for (let c = range.c0; c <= range.c1; c++) {
      const cell = row[c] ?? null;
      const text = copyText(cell);
      f.push(escapeField(defuse && typeof cell === "string" ? defuse(text) : text, delimiter));
    }
    lines.push(f.join(delimiter));
  }
  return lines.join("\n");
}

/** Unique object keys for the JSON view when a result has duplicate column names. */
export function uniqueColumnKeys(columns: readonly ColumnMeta[]): string[] {
  const seen = new Map<string, number>();
  return columns.map((c) => {
    const n = (seen.get(c.name) ?? 0) + 1;
    seen.set(c.name, n);
    return n === 1 ? c.name : `${c.name}_${n}`;
  });
}

export function rowsToObjects(columns: readonly ColumnMeta[], rows: readonly (readonly Cell[])[], limit: number): Record<string, Cell>[] {
  const keys = uniqueColumnKeys(columns);
  const out: Record<string, Cell>[] = [];
  const n = Math.min(rows.length, limit);
  for (let r = 0; r < n; r++) {
    const o: Record<string, Cell> = {};
    for (let c = 0; c < keys.length; c++) {
      const v = rows[r][c] ?? null;
      o[keys[c]] = isTruncatedCell(v) ? truncatedNote(v) : v;
    }
    out.push(o);
  }
  return out;
}

/** psql-style aligned text table for the "Text" view. */
export function formatTextTable(columns: readonly ColumnMeta[], rows: readonly (readonly Cell[])[], limit: number, maxWidth = 80): string {
  const n = Math.min(rows.length, limit);
  const cut = (s: string) => {
    const one = s.replace(/\r?\n/g, "⏎");
    return one.length > maxWidth ? one.slice(0, maxWidth - 1) + "…" : one;
  };
  const numeric = columns.map((c) => isNumericType(c.typeName));
  const cells: string[][] = [];
  const widths = columns.map((c) => c.name.length);
  for (let r = 0; r < n; r++) {
    const line: string[] = [];
    for (let c = 0; c < columns.length; c++) {
      const v = rows[r][c] ?? null;
      const s = cut(v === null ? "NULL" : copyText(v));
      line.push(s);
      if (s.length > widths[c]) widths[c] = s.length;
    }
    cells.push(line);
  }
  const pad = (s: string, w: number, right: boolean) => (right ? s.padStart(w) : s.padEnd(w));
  const head = columns.map((c, i) => pad(c.name, widths[i], false)).join(" │ ");
  const sep = widths.map((w) => "─".repeat(w)).join("─┼─");
  const body = cells.map((l) => l.map((s, i) => pad(s, widths[i], numeric[i])).join(" │ "));
  const tail = rows.length > n ? [`… ${rows.length - n} more rows not shown in the text view`] : [];
  return [head, sep, ...body, ...tail].join("\n");
}

export function estimateColumnWidth(col: ColumnMeta, sample: readonly (readonly Cell[])[], index: number, headerExtra = 0): number {
  const header = col.name.length * 7.4 + col.typeName.length * 5.6 + 44 + headerExtra;
  let content = 0;
  const n = Math.min(sample.length, 100);
  for (let r = 0; r < n; r++) {
    const len = displayText(sample[r][index] ?? null).length;
    if (len > content) content = len;
  }
  const w = Math.max(header, content * 7.3 + 22);
  return Math.round(Math.min(420, Math.max(80, w)));
}

export function formatCount(n: number): string {
  return n.toLocaleString("en-US");
}

export function formatElapsed(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
  const m = Math.floor(ms / 60_000);
  return `${m}m ${Math.round((ms % 60_000) / 1000)}s`;
}
