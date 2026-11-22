// Pure serialisers for result sets: CSV (RFC 4180), JSON, SQL INSERTs and Markdown. Used by the
// Copy / Export menu of every result and table tab. No React, no IPC.
import type { Cell, ColumnMeta } from "../../ipc/types";
import { bytesToHex, qualifiedName, quoteIdent, quoteLiteral, sqlLiteral, SqlGenError, typeCategory } from "../table/sqlGen";
import { copyText, isBytesCell, isNumericType, notingTruncated, uniqueColumnKeys } from "./cellFormat";
import { uneditableReason } from "../table/sqlGen";

type Rows = readonly (readonly Cell[])[];
type Cols = readonly Pick<ColumnMeta, "name" | "typeName">[];

export const UTF8_BOM = "\uFEFF";

// ---------------------------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------------------------

export interface CsvOptions {
  /** Prefix a UTF-8 byte-order mark so Excel opens the file as UTF-8. */
  bom?: boolean;
  delimiter?: "," | ";" | "\t";
  header?: boolean;
  /**
   * Defuse spreadsheet formula injection (OWASP "CSV injection"): text cells starting with
   * = + - @ TAB or CR get a leading apostrophe so Excel/Sheets/Numbers show them as text instead of
   * evaluating them. Plain numbers like -5 or +3.2 are left alone. Default: on.
   */
  formulaSafe?: boolean;
}

const FORMULA_START = /^[=+\-@\t\r]/;
const PLAIN_NUMBER = /^[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?$/;

/** Exported for tests. */
export function defuseFormula(s: string): string {
  return FORMULA_START.test(s) && !PLAIN_NUMBER.test(s) ? `'${s}` : s;
}

function csvField(cell: Cell, delimiter: string, formulaSafe = true): string {
  if (cell === null || cell === undefined) return ""; // NULL = empty, unquoted
  const raw = isBytesCell(cell) ? `\\x${bytesToHex(cell)}` : copyText(cell);
  const s = formulaSafe && typeof cell === "string" ? defuseFormula(raw) : raw;
  // An empty string is quoted so it can be told apart from NULL.
  if (s === "") return '""';
  return s.includes(delimiter) || s.includes('"') || s.includes("\n") || s.includes("\r") ? `"${s.replace(/"/g, '""')}"` : s;
}

/** RFC 4180: CRLF line ends, fields with delimiter / quote / newline quoted, quotes doubled. */
export function toCsv(columns: Cols, rows: Rows, opts: CsvOptions = {}): string {
  const d = opts.delimiter ?? ",";
  const lines: string[] = [];
  if (opts.header !== false) lines.push(columns.map((c) => csvField(c.name === "" ? "" : c.name, d)).join(d));
  for (const row of rows) {
    const f: string[] = [];
    for (let c = 0; c < columns.length; c++) f.push(csvField(row[c] ?? null, d, opts.formulaSafe !== false));
    lines.push(f.join(d));
  }
  return (opts.bom ? UTF8_BOM : "") + lines.join("\r\n") + "\r\n";
}

// ---------------------------------------------------------------------------------------------
// JSON
// ---------------------------------------------------------------------------------------------

/** An array of objects keyed by column name. int8 / numeric stay strings; bytea becomes `\x...` hex text. */
export function toJson(columns: Cols, rows: Rows, pretty = true): string {
  const keys = uniqueColumnKeys(columns as ColumnMeta[]);
  const out = rows.map((row) => {
    const o: Record<string, unknown> = {};
    for (let c = 0; c < keys.length; c++) {
      const v = row[c] ?? null;
      // A value that was cut when it was read is exported as a note that says so, never as its start.
      o[keys[c]] = isBytesCell(v) ? `\\x${bytesToHex(v)}` : notingTruncated(v);
    }
    return o;
  });
  return JSON.stringify(out, null, pretty ? 2 : undefined) + "\n";
}

// ---------------------------------------------------------------------------------------------
// SQL INSERTs
// ---------------------------------------------------------------------------------------------

export interface InsertTarget {
  schema?: string | null;
  name: string;
}

export const PLACEHOLDER_TABLE = "table_name";

function exportLiteral(cell: Cell, typeName: string, column: string): string {
  // Not caught below: an INSERT script holding a placeholder would load wrong data.
  const cut = uneditableReason(cell);
  if (cut) throw new SqlGenError(`Column ${column}: ${cut}`);
  try {
    return sqlLiteral(cell, typeName, { cast: false });
  } catch (e) {
    if (!(e instanceof SqlGenError)) throw e;
    // A value that does not fit its declared type (should not happen for data read from the database):
    // keep it as a plain string literal rather than dropping the row.
    return quoteLiteral(copyText(cell).replace(/\0/g, ""));
  }
}

/**
 * Multi-row INSERT statements, `batchSize` rows each. Values are typed by the result column type.
 * Throws `SqlGenError` when a row holds a value that was cut when it was read.
 */
export function toSqlInserts(columns: Cols, rows: Rows, table: InsertTarget | null, batchSize = 500): string {
  const target = table ? (table.schema ? qualifiedName(table.schema, table.name) : quoteIdent(table.name)) : quoteIdent(PLACEHOLDER_TABLE);
  const head = `INSERT INTO ${target} (${columns.map((c) => quoteIdent(c.name)).join(", ")}) VALUES`;
  const out: string[] = [];
  if (!table) out.push(`-- Replace ${quoteIdent(PLACEHOLDER_TABLE)} with the table to load these rows into.`);
  if (rows.length === 0) {
    out.push(`-- No rows.`);
    return out.join("\n") + "\n";
  }
  for (let i = 0; i < rows.length; i += batchSize) {
    const chunk = rows.slice(i, i + batchSize);
    const values = chunk.map((row) => `  (${columns.map((c, ci) => exportLiteral(row[ci] ?? null, c.typeName, c.name)).join(", ")})`);
    out.push(`${head}\n${values.join(",\n")};`);
  }
  return out.join("\n") + "\n";
}

// ---------------------------------------------------------------------------------------------
// Markdown
// ---------------------------------------------------------------------------------------------

function mdCell(cell: Cell): string {
  if (cell === null || cell === undefined) return "NULL";
  const s = isBytesCell(cell) ? `\\x${bytesToHex(cell)}` : copyText(cell);
  return s.replace(/\\/g, "\\\\").replace(/\|/g, "\\|").replace(/\r?\n/g, "<br>");
}

/** A GitHub-flavoured Markdown table; numeric columns are right-aligned. */
export function toMarkdownTable(columns: Cols, rows: Rows): string {
  const esc = (s: string) => s.replace(/\|/g, "\\|");
  const head = `| ${columns.map((c) => esc(c.name)).join(" | ")} |`;
  const sep = `| ${columns.map((c) => (isNumericType(c.typeName) || typeCategory(c.typeName) === "int" ? "---:" : "---")).join(" | ")} |`;
  const body = rows.map((r) => `| ${columns.map((_, i) => mdCell(r[i] ?? null)).join(" | ")} |`);
  return [head, sep, ...body].join("\n") + "\n";
}

// ---------------------------------------------------------------------------------------------
// File names
// ---------------------------------------------------------------------------------------------

/** `orders_2026-10-31.csv`: a safe, readable default name. */
export function suggestFileName(base: string, ext: string, now: Date = new Date()): string {
  const safe = base
    .replace(/[\\/:*?"<>|\0-\x1f]+/g, "_")
    .replace(/\s+/g, "_")
    .replace(/^[._]+|[._]+$/g, "")
    .slice(0, 60);
  const day = `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, "0")}-${String(now.getDate()).padStart(2, "0")}`;
  return `${safe || "results"}_${day}.${ext}`;
}
