// Pure SQL generation for the table browser: identifier / literal quoting, type-aware casts,
// filters, ORDER BY, keyset pagination and the UPDATE / INSERT / DELETE statements behind inline
// editing. No React, no IPC; everything here is covered by unit tests.
//
// Safety model
//  * Identifiers are always double-quoted with `"` doubled. NUL is rejected (Postgres cannot store it).
//  * Literals are always single-quoted with `'` doubled and cast to the column's type
//    (`'42'::bigint`), so the server parses the value, never the application. Strings that contain a
//    backslash are emitted as `E'...'` with the backslash doubled, which is correct whatever
//    `standard_conforming_strings` is set to.
//  * Numeric-typed values must look numeric before they are emitted (digits only inside the quotes).
//  * The only free-form SQL is the user's "raw WHERE" text, which `validateRawWhere` fences in (no `;`,
//    balanced quotes / parentheses / comments) and which is only ever used in read-only SELECTs.
import type { Cell } from "../../ipc/types";
import { findTruncated, formatBytes, hasInexactNumber, isBytesCell, isTruncatedCell, NUMERIC_STRING, type BytesCell } from "../grid/cellFormat";

export class SqlGenError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SqlGenError";
  }
}

// ---------------------------------------------------------------------------------------------
// Quoting
// ---------------------------------------------------------------------------------------------

/** `"name"` with embedded quotes doubled. */
export function quoteIdent(name: string): string {
  if (name.includes("\0")) throw new SqlGenError("Identifiers cannot contain NUL characters.");
  if (name.length === 0) throw new SqlGenError("Identifiers cannot be empty.");
  return `"${name.replace(/"/g, '""')}"`;
}

/** `"schema"."name"`. */
export const qualifiedName = (schema: string, name: string): string => `${quoteIdent(schema)}.${quoteIdent(name)}`;

/**
 * A string literal. Plain `'...'` (quotes doubled) unless the text contains a backslash, in which
 * case `E'...'` with `\\` so the result does not depend on `standard_conforming_strings`.
 */
export function quoteLiteral(s: string): string {
  if (s.includes("\0")) throw new SqlGenError("Text values cannot contain NUL (\\0) characters.");
  const q = s.replace(/'/g, "''");
  return s.includes("\\") ? `E'${q.replace(/\\/g, "\\\\")}'` : `'${q}'`;
}

// ---------------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------------

export type TypeCategory = "bool" | "int" | "float" | "numeric" | "money" | "text" | "uuid" | "datetime" | "json" | "jsonb" | "bytea" | "array" | "other";

const INT_TYPES = new Set(["smallint", "integer", "bigint", "int", "int2", "int4", "int8", "oid", "xid", "cid", "smallserial", "serial", "bigserial", "serial2", "serial4", "serial8"]);
const FLOAT_TYPES = new Set(["real", "double precision", "float", "float4", "float8"]);
const TEXT_TYPES = new Set(["text", "character varying", "varchar", "character", "char", "bpchar", "name", '"char"', "citext", "xml"]);

/** Classify a `format_type()` string ("character varying(20)") or a short result type name ("int8", "_text"). */
export function typeCategory(dataType: string): TypeCategory {
  const s = dataType.trim().toLowerCase();
  if (s.endsWith("]")) return "array";
  if (s.length > 1 && s.startsWith("_")) return "array";
  const base = s.replace(/\([^)]*\)/g, "").replace(/\s+/g, " ").trim();
  if (base === "boolean" || base === "bool") return "bool";
  if (INT_TYPES.has(base)) return "int";
  if (FLOAT_TYPES.has(base)) return "float";
  if (base === "numeric" || base === "decimal") return "numeric";
  if (base === "money") return "money";
  if (TEXT_TYPES.has(base)) return "text";
  if (base === "uuid") return "uuid";
  if (base === "json") return "json";
  if (base === "jsonb") return "jsonb";
  if (base === "bytea") return "bytea";
  if (base.startsWith("timestamp") || base.startsWith("time ") || base === "time" || base === "date" || base === "interval") return "datetime";
  return "other";
}

export const isNumericCategory = (c: TypeCategory): boolean => c === "int" || c === "float" || c === "numeric";

/**
 * True when `t` only consists of identifiers, quoted identifiers, numbers, brackets, parentheses,
 * commas, dots and spaces, so it can be spliced after `::` without any chance of ending the
 * expression. Type names come from `format_type()`, which already quotes odd names.
 */
export function isSafeTypeName(t: string): boolean {
  const s = t.trim();
  if (s.length === 0 || s.length > 200) return false;
  const re = /\s+|[A-Za-z_][A-Za-z0-9_$]*|"(?:[^"\0]|"")+"|\d+|[[\](),.]/y;
  let i = 0;
  while (i < s.length) {
    re.lastIndex = i;
    const m = re.exec(s);
    if (!m) return false;
    i = re.lastIndex;
  }
  return true;
}

const SHORT_TYPES: [RegExp, string][] = [
  [/^timestamp(\(\d+\))? with time zone/, "timestamptz"],
  [/^timestamp(\(\d+\))? without time zone/, "timestamp"],
  [/^time(\(\d+\))? with time zone/, "timetz"],
  [/^time(\(\d+\))? without time zone/, "time"],
  [/^character varying\b/, "varchar"],
  [/^character\b(?!\s*varying)/, "bpchar"],
  [/^double precision\b/, "float8"],
  [/^bit varying\b/, "varbit"],
  [/^integer\b/, "int4"],
  [/^bigint\b/, "int8"],
  [/^smallint\b/, "int2"],
  [/^boolean\b/, "bool"],
  [/^real\b/, "float4"],
];

/** "character varying(20)[]" -> "varchar(20)[]" so headers match the names the result grid shows. */
export function shortTypeName(dataType: string): string {
  const s = dataType.trim();
  const lower = s.toLowerCase();
  for (const [re, rep] of SHORT_TYPES) {
    const m = re.exec(lower);
    if (m) {
      const rest = s.slice(m[0].length);
      // keep a "(n)" modifier that was part of "character varying(n)" / "character(n)"
      return rep + rest.replace(/^\s+/, " ").replace(/^ (?=[([])/, "");
    }
  }
  return s;
}

// ---------------------------------------------------------------------------------------------
// Literals
// ---------------------------------------------------------------------------------------------

export function bytesToHex(c: BytesCell): string {
  const bin = atob(c.$bytes);
  let out = "";
  for (let i = 0; i < bin.length; i++) out += bin.charCodeAt(i).toString(16).padStart(2, "0");
  return out;
}

function arrayElement(v: unknown): string {
  if (v === null || v === undefined) return "NULL";
  if (Array.isArray(v)) return pgArrayLiteral(v);
  if (typeof v === "boolean" || typeof v === "number") return String(v);
  const s = isBytesCell(v) ? `\\x${bytesToHex(v)}` : typeof v === "string" ? v : JSON.stringify(v);
  return `"${s.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"`;
}

/** `{1,NULL,"a b"}`: a Postgres array literal. Every non-numeric element is double-quoted. */
export function pgArrayLiteral(arr: readonly unknown[]): string {
  return `{${arr.map(arrayElement).join(",")}}`;
}

const INT_STRING = /^[-+]?\d+$/;
const SPECIAL_FLOAT = /^[-+]?(nan|infinity|inf)$/i;

function numericText(cell: Cell, cat: TypeCategory, dataType: string): string {
  const raw = typeof cell === "number" ? String(cell) : typeof cell === "string" ? cell.trim() : null;
  const bad = () => new SqlGenError(`"${typeof cell === "string" ? cell.slice(0, 40) : String(cell)}" is not a valid ${shortTypeName(dataType)} value.`);
  if (raw === null) throw bad();
  if (cat === "int") {
    if (!INT_STRING.test(raw)) throw bad();
    return raw;
  }
  if (NUMERIC_STRING.test(raw) || SPECIAL_FLOAT.test(raw)) return raw;
  throw bad();
}

export interface LiteralOptions {
  /** Append `::type` (default true). Exports turn this off for readable INSERT scripts. */
  cast?: boolean;
}

/**
 * The type to cast a literal to. An explicit cast to `varchar(5)`, `bit(5)` or `numeric(6,2)` silently
 * truncates or rounds, so drop the modifier and let the column's own check reject a bad value.
 * Bare `char` and `bit` mean length 1, hence `bpchar` and `varbit`.
 */
function castType(type: string): string {
  if (type.includes('"')) return type;
  const t = type.replace(/\s*\([^)]*\)/g, "");
  return t.replace(/^(character|char)(?=$|\[)/i, "bpchar").replace(/^bit(?=$|\[)/i, "varbit");
}

/**
 * Why `cell` cannot be edited, filtered on or written into SQL: it is (or contains) a value that was
 * too large to load in full, so only its start is known here. `null` for every ordinary value.
 */
export function uneditableReason(cell: Cell | undefined): string | null {
  const cut = findTruncated(cell);
  if (!cut) return null;
  return `This value was too large to load in full (${formatBytes(cut.$truncated.bytes)}): only its start is known here, so it cannot be edited, filtered on or written into SQL. Use a SQL statement of your own for it.`;
}

/** Throws when `cell` is a cut value. `column` makes the message say where. */
function refuseTruncated(cell: Cell | undefined, column?: string): void {
  const reason = uneditableReason(cell);
  if (reason) throw new SqlGenError(column ? `Column ${column}: ${reason}` : reason);
}

/**
 * SQL for one cell value, typed by the column's `dataType`. Throws `SqlGenError` for values that
 * cannot be that type, and for a value that was cut when it was read: writing its start (or its
 * marker) would replace the real value with something else.
 */
export function sqlLiteral(cell: Cell | undefined, dataType: string, opts: LiteralOptions = {}): string {
  if (cell === null || cell === undefined) return "NULL";
  refuseTruncated(cell);
  const cat = typeCategory(dataType);
  const type = dataType.trim();
  const cast = opts.cast !== false && isSafeTypeName(type) ? `::${castType(type)}` : "";
  switch (cat) {
    case "bool":
      if (typeof cell === "boolean") return cell ? "TRUE" : "FALSE";
      return `${quoteLiteral(String(cell))}${cast}`;
    case "int":
    case "float":
    case "numeric": {
      const t = numericText(cell, cat, type);
      // After validation the text is digits / sign / dot / exponent, or NaN / Infinity (which must be quoted).
      if (!cast && NUMERIC_STRING.test(t)) return t;
      return `'${t}'${cast}`;
    }
    case "json":
    case "jsonb":
      return `${quoteLiteral(JSON.stringify(cell))}${cast}`;
    case "bytea":
      if (isBytesCell(cell)) return `decode('${bytesToHex(cell)}', 'hex')`;
      if (typeof cell === "string") return `${quoteLiteral(cell)}${cast}`;
      throw new SqlGenError("Binary values must be entered as hex text (\\xDEADBEEF).");
    case "array":
      if (Array.isArray(cell)) return `${quoteLiteral(pgArrayLiteral(cell))}${cast}`;
      if (typeof cell === "string") return `${quoteLiteral(cell)}${cast}`;
      return `${quoteLiteral(JSON.stringify(cell))}${cast}`;
    default: {
      let s: string;
      if (typeof cell === "string") s = cell;
      else if (typeof cell === "number" || typeof cell === "boolean") s = String(cell);
      else if (isBytesCell(cell)) s = `\\x${bytesToHex(cell)}`;
      else s = JSON.stringify(cell);
      return `${quoteLiteral(s)}${cast}`;
    }
  }
}

// ---------------------------------------------------------------------------------------------
// Editing helpers shared by the grid editors and the filter builder
// ---------------------------------------------------------------------------------------------

/** The text shown in an editor / filter box for a cell value. */
export function cellToEditText(cell: Cell | undefined): string {
  if (cell === null || cell === undefined) return "";
  // Never the start of a cut value: text that can be edited and saved must be the whole value.
  if (isTruncatedCell(cell)) return "";
  if (typeof cell === "string") return cell;
  if (typeof cell === "number" || typeof cell === "boolean") return String(cell);
  if (isBytesCell(cell)) return `\\x${bytesToHex(cell)}`;
  try {
    return JSON.stringify(cell, null, 2);
  } catch {
    return "";
  }
}

export type ParsedEdit = { ok: true; cell: Cell } | { ok: false; error: string };

/**
 * Turn text typed into an editor into a cell value for a column. Numbers are validated and kept as
 * strings (so `9007199254740993` is never rounded); booleans accept the spellings Postgres accepts;
 * JSON columns must contain valid JSON.
 */
export function parseEditedText(text: string, dataType: string): ParsedEdit {
  const cat = typeCategory(dataType);
  switch (cat) {
    case "int":
    case "float":
    case "numeric": {
      const t = text.trim();
      if (t === "") return { ok: false, error: "Enter a number, or set the value to NULL." };
      try {
        numericText(t, cat, dataType);
      } catch (e) {
        return { ok: false, error: (e as Error).message };
      }
      return { ok: true, cell: t };
    }
    case "bool": {
      const t = text.trim().toLowerCase();
      if (["true", "t", "yes", "y", "on", "1"].includes(t)) return { ok: true, cell: true };
      if (["false", "f", "no", "n", "off", "0"].includes(t)) return { ok: true, cell: false };
      return { ok: false, error: "Use true or false." };
    }
    case "json":
    case "jsonb":
      try {
        const v = JSON.parse(text) as Cell;
        if (hasInexactNumber(v)) return { ok: false, error: "This JSON has numbers too large or precise to keep exact here. Update it with SQL." };
        return { ok: true, cell: v };
      } catch (e) {
        return { ok: false, error: `Invalid JSON: ${(e as Error).message}` };
      }
    case "array": {
      const t = text.trim();
      if (t.startsWith("[")) {
        try {
          const v: unknown = JSON.parse(t);
          if (Array.isArray(v) && !hasInexactNumber(v)) return { ok: true, cell: v };
          if (Array.isArray(v)) return { ok: false, error: "These numbers would be rounded. Use a Postgres literal like {1,2,3}." };
        } catch {
          return { ok: false, error: 'Enter a JSON array like [1, 2, 3] or a Postgres literal like {1,2,3}.' };
        }
      }
      return { ok: true, cell: text };
    }
    default:
      return { ok: true, cell: text };
  }
}

/** Equality for "did the user really change this cell": 5 equals "5", objects compare structurally. */
export function cellEquals(a: Cell | undefined, b: Cell | undefined): boolean {
  const x = a ?? null;
  const y = b ?? null;
  if (x === null || y === null) return x === y;
  if (typeof x === typeof y && typeof x !== "object") return x === y;
  if ((typeof x === "number" && typeof y === "string") || (typeof x === "string" && typeof y === "number")) return String(x) === String(y);
  if (typeof x === "object" && typeof y === "object") {
    try {
      return JSON.stringify(x) === JSON.stringify(y);
    } catch {
      return false;
    }
  }
  return false;
}

// ---------------------------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------------------------

export type FilterOp = "=" | "<>" | "<" | ">" | "<=" | ">=" | "LIKE" | "ILIKE" | "IN" | "IS NULL" | "IS NOT NULL";

export const FILTER_OPS: { op: FilterOp; label: string }[] = [
  { op: "=", label: "=" },
  { op: "<>", label: "≠" },
  { op: "<", label: "<" },
  { op: ">", label: ">" },
  { op: "<=", label: "≤" },
  { op: ">=", label: "≥" },
  { op: "LIKE", label: "LIKE" },
  { op: "ILIKE", label: "ILIKE" },
  { op: "IN", label: "IN" },
  { op: "IS NULL", label: "IS NULL" },
  { op: "IS NOT NULL", label: "IS NOT NULL" },
];

export const opNeedsValue = (op: FilterOp): boolean => op !== "IS NULL" && op !== "IS NOT NULL";

export interface FilterRow {
  id: string;
  column: string;
  op: FilterOp;
  value: string;
}

export interface ColumnRef {
  name: string;
  /** `format_type()` output, e.g. "character varying(20)". */
  dataType: string;
}

/** Split `a, 'b,c', "d"` on top-level commas; quotes group, doubled quotes escape. */
export function splitInList(text: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quote: "'" | '"' | null = null;
  let quoted = false;
  const push = () => {
    const v = quoted ? cur : cur.trim();
    if (quoted || v !== "") out.push(v);
    cur = "";
    quoted = false;
  };
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (quote) {
      if (ch === quote) {
        if (text[i + 1] === quote) {
          cur += quote;
          i++;
        } else quote = null;
      } else cur += ch;
    } else if ((ch === "'" || ch === '"') && cur.trim() === "") {
      quote = ch;
      quoted = true;
      cur = "";
    } else if (ch === ",") push();
    else cur += ch;
  }
  push();
  return out;
}

/** `asText`: the column is compared through `::text` (json has no operators of its own), so no cast on the literal. */
function filterOperand(value: string, col: ColumnRef, asText: boolean): string {
  const cat = typeCategory(col.dataType);
  if (cat === "json" || cat === "jsonb") {
    try {
      JSON.parse(value);
    } catch (e) {
      throw new SqlGenError(`Invalid JSON: ${(e as Error).message}`);
    }
    return asText ? quoteLiteral(value) : `${quoteLiteral(value)}::${isSafeTypeName(col.dataType) ? col.dataType.trim() : "jsonb"}`;
  }
  const parsed = parseEditedText(value, col.dataType);
  if (!parsed.ok) throw new SqlGenError(parsed.error);
  return sqlLiteral(parsed.cell, col.dataType);
}

/** SQL for one filter row (no WHERE). Throws `SqlGenError` with a user-facing message. */
export function filterToSql(f: FilterRow, columns: readonly ColumnRef[]): string {
  // The operator is spliced into the SQL, so it must be one of ours.
  if (!FILTER_OPS.some((o) => o.op === f.op)) throw new SqlGenError("Choose an operator.");
  const col = columns.find((c) => c.name === f.column);
  if (!col) throw new SqlGenError(f.column ? `Column "${f.column}" does not exist.` : "Choose a column.");
  const id = quoteIdent(col.name);
  const cat = typeCategory(col.dataType);
  switch (f.op) {
    case "IS NULL":
    case "IS NOT NULL":
      return `${id} ${f.op}`;
    case "LIKE":
    case "ILIKE":
      return `${id}::text ${f.op} ${quoteLiteral(f.value)}`;
    case "IN": {
      const items = splitInList(f.value);
      if (items.length === 0) throw new SqlGenError("Enter one or more values separated by commas.");
      if (items.length > 1000) throw new SqlGenError("IN lists are limited to 1,000 values.");
      const lits = items.map((v) => filterOperand(v, col, cat === "json"));
      return `${cat === "json" ? `${id}::text` : id} IN (${lits.join(", ")})`;
    }
    default: {
      // `json` has no equality / ordering operators; compare its text form instead.
      const lit = filterOperand(f.value, col, cat === "json");
      return `${cat === "json" ? `${id}::text` : id} ${f.op} ${lit}`;
    }
  }
}

export interface WhereResult {
  /** Condition text without the WHERE keyword; null when there is nothing to filter. */
  sql: string | null;
  /** Per-filter-id error messages (also keyed "raw" for the raw WHERE text). */
  errors: Record<string, string>;
}

/**
 * True for a character that can be part of an identifier. Postgres treats every non-ASCII character
 * (and every UTF-16 surrogate half) as an identifier character, as well as ASCII letters, digits,
 * `_` and `$`. A `'` right after an identifier is never the start of an `E'...'` escape string.
 */
const isIdentUnit = (c: string | undefined): boolean => c !== undefined && c !== "" && (/[A-Za-z0-9_$]/.test(c) || c.charCodeAt(0) >= 0x80);

/** Scans SQL text just enough to reject statement breaks and unbalanced quoting / parentheses. */
export function validateRawWhere(text: string): { ok: true } | { ok: false; error: string } {
  let depth = 0;
  let i = 0;
  const n = text.length;
  while (i < n) {
    const ch = text[i];
    const next = text[i + 1];
    if (ch === "-" && next === "-") {
      while (i < n && text[i] !== "\n" && text[i] !== "\r") i++;
      continue;
    }
    if (ch === "/" && next === "*") {
      let d = 1;
      i += 2;
      while (i < n && d > 0) {
        if (text[i] === "/" && text[i + 1] === "*") {
          d++;
          i += 2;
        } else if (text[i] === "*" && text[i + 1] === "/") {
          d--;
          i += 2;
        } else i++;
      }
      if (d > 0) return { ok: false, error: "Unterminated /* comment." };
      continue;
    }
    if (ch === "'") {
      // `E'..'` is an escape string only when the E is a token of its own: any identifier character
      // before it (including non-ASCII letters) makes it part of a longer name, and the quote that
      // follows then opens an ordinary string where a backslash is just a backslash.
      const prev = i > 0 ? text[i - 1] : "";
      const prevPrev = i > 1 ? text[i - 2] : "";
      const escapeString = (prev === "e" || prev === "E") && !isIdentUnit(prevPrev);
      i++;
      let closed = false;
      while (i < n) {
        if (escapeString && text[i] === "\\") {
          i += 2;
          continue;
        }
        if (text[i] === "'") {
          if (text[i + 1] === "'") {
            i += 2;
            continue;
          }
          closed = true;
          i++;
          break;
        }
        i++;
      }
      if (!closed) return { ok: false, error: "Unterminated string literal." };
      continue;
    }
    if (ch === '"') {
      i++;
      let closed = false;
      while (i < n) {
        if (text[i] === '"') {
          if (text[i + 1] === '"') {
            i += 2;
            continue;
          }
          closed = true;
          i++;
          break;
        }
        i++;
      }
      if (!closed) return { ok: false, error: "Unterminated quoted identifier." };
      continue;
    }
    // A `$` that continues an identifier (`x$$`) is part of that name, not a dollar quote.
    if (ch === "$" && !isIdentUnit(i > 0 ? text[i - 1] : undefined)) {
      const m = /^\$([A-Za-z_\u0080-\uFFFF][A-Za-z0-9_\u0080-\uFFFF]*)?\$/.exec(text.slice(i, i + 80));
      if (m) {
        const tag = m[0];
        const end = text.indexOf(tag, i + tag.length);
        if (end < 0) return { ok: false, error: "Unterminated $$ string." };
        i = end + tag.length;
        continue;
      }
    }
    if (ch === ";") return { ok: false, error: "A WHERE clause cannot contain ';'." };
    if (ch === "(") depth++;
    else if (ch === ")") {
      depth--;
      if (depth < 0) return { ok: false, error: "Unbalanced ')'." };
    }
    i++;
  }
  if (depth !== 0) return { ok: false, error: "Unbalanced '('." };
  return { ok: true };
}

/** True when the text holds nothing but whitespace and comments. */
const isBlankSql = (s: string): boolean => s.replace(/--[^\n\r]*/g, "").replace(/\/\*[\s\S]*?\*\//g, "").trim() === "";

/** Combine the filter rows (AND) and an optional raw WHERE into one condition. */
export function buildWhere(filters: readonly FilterRow[], columns: readonly ColumnRef[], rawWhere?: string | null): WhereResult {
  const parts: string[] = [];
  const errors: Record<string, string> = {};
  for (const f of filters) {
    try {
      parts.push(filterToSql(f, columns));
    } catch (e) {
      errors[f.id] = e instanceof SqlGenError ? e.message : "Invalid filter.";
    }
  }
  if (rawWhere !== undefined && rawWhere !== null && !isBlankSql(rawWhere)) {
    const v = validateRawWhere(rawWhere);
    if (v.ok) parts.push(`(\n${rawWhere.trim()}\n)`);
    else errors.raw = v.error;
  }
  return { sql: parts.length > 0 ? parts.join("\n  AND ") : null, errors };
}

// ---------------------------------------------------------------------------------------------
// SELECT / COUNT / keyset pagination
// ---------------------------------------------------------------------------------------------

export interface SortKey {
  column: string;
  dir: "asc" | "desc";
}

export const buildOrderBy = (keys: readonly SortKey[]): string =>
  keys.length === 0 ? "" : `ORDER BY ${keys.map((k) => `${quoteIdent(k.column)} ${k.dir === "desc" ? "DESC" : "ASC"}`).join(", ")}`;

export interface KeysetKey {
  column: string;
  dataType: string;
  dir: "asc" | "desc";
}

/**
 * "Rows strictly after `values` in the order given by `keys`", in expanded form so mixed ASC / DESC
 * keys work: `(a > x) OR (a = x AND b < y) ...`. Keys must be NOT NULL (the caller checks).
 */
export function buildKeysetPredicate(keys: readonly KeysetKey[], values: readonly Cell[]): string {
  if (keys.length === 0 || keys.length !== values.length) throw new SqlGenError("Keyset needs one value per key column.");
  const branches: string[] = [];
  for (let i = 0; i < keys.length; i++) {
    const terms: string[] = [];
    for (let j = 0; j < i; j++) terms.push(`${quoteIdent(keys[j].column)} = ${sqlLiteral(values[j], keys[j].dataType)}`);
    terms.push(`${quoteIdent(keys[i].column)} ${keys[i].dir === "desc" ? "<" : ">"} ${sqlLiteral(values[i], keys[i].dataType)}`);
    branches.push(terms.length === 1 ? terms[0] : `(${terms.join(" AND ")})`);
  }
  return branches.length === 1 ? branches[0] : `(${branches.join("\n    OR ")})`;
}

export interface SelectPageOptions {
  schema: string;
  table: string;
  columns: readonly string[];
  where?: string | null;
  orderBy?: readonly SortKey[];
  limit: number;
  offset?: number;
  /** Keyset continuation: rows after these key values. */
  after?: { keys: readonly KeysetKey[]; values: readonly Cell[] } | null;
}

export function buildSelectPage(o: SelectPageOptions): string {
  if (o.columns.length === 0) throw new SqlGenError("The table has no columns.");
  const conds: string[] = [];
  if (o.where) conds.push(`(${o.where})`);
  if (o.after) conds.push(buildKeysetPredicate(o.after.keys, o.after.values));
  const lines = [`SELECT ${o.columns.map(quoteIdent).join(", ")}`, `FROM ${qualifiedName(o.schema, o.table)}`];
  if (conds.length > 0) lines.push(`WHERE ${conds.join("\n  AND ")}`);
  const order = buildOrderBy(o.orderBy ?? []);
  if (order) lines.push(order);
  lines.push(`LIMIT ${Math.max(1, Math.floor(o.limit))}`);
  if (o.offset && o.offset > 0) lines.push(`OFFSET ${Math.floor(o.offset)}`);
  return lines.join("\n");
}

export function buildCount(schema: string, table: string, where?: string | null): string {
  return `SELECT count(*) FROM ${qualifiedName(schema, table)}${where ? `\nWHERE ${where}` : ""}`;
}

// ---------------------------------------------------------------------------------------------
// Row changes
// ---------------------------------------------------------------------------------------------

export interface TableTarget {
  schema: string;
  name: string;
  columns: readonly ColumnRef[];
}

const colType = (t: TableTarget, name: string): string => {
  const c = t.columns.find((x) => x.name === name);
  if (!c) throw new SqlGenError(`Column "${name}" does not exist in ${t.schema}.${t.name}.`);
  return c.dataType;
};

function whereByKey(t: TableTarget, pk: Readonly<Record<string, Cell>>): string {
  const names = Object.keys(pk);
  if (names.length === 0) throw new SqlGenError("A primary key is required to identify the row.");
  return names
    .map((n) => {
      const v = pk[n];
      refuseTruncated(v, n);
      return v === null || v === undefined ? `${quoteIdent(n)} IS NULL` : `${quoteIdent(n)} = ${sqlLiteral(v, colType(t, n))}`;
    })
    .join(" AND ");
}

export function buildUpdate(t: TableTarget, pk: Readonly<Record<string, Cell>>, set: Readonly<Record<string, Cell>>): string {
  const names = Object.keys(set);
  if (names.length === 0) throw new SqlGenError("Nothing to update.");
  const assigns = names.map((n) => (refuseTruncated(set[n], n), `${quoteIdent(n)} = ${sqlLiteral(set[n], colType(t, n))}`));
  return `UPDATE ${qualifiedName(t.schema, t.name)} SET ${assigns.join(", ")} WHERE ${whereByKey(t, pk)};`;
}

export function buildInsert(t: TableTarget, values: Readonly<Record<string, Cell>>): string {
  const names = Object.keys(values);
  if (names.length === 0) return `INSERT INTO ${qualifiedName(t.schema, t.name)} DEFAULT VALUES;`;
  for (const n of names) refuseTruncated(values[n], n);
  return `INSERT INTO ${qualifiedName(t.schema, t.name)} (${names.map(quoteIdent).join(", ")}) VALUES (${names.map((n) => sqlLiteral(values[n], colType(t, n))).join(", ")});`;
}

export function buildDelete(t: TableTarget, pk: Readonly<Record<string, Cell>>): string {
  return `DELETE FROM ${qualifiedName(t.schema, t.name)} WHERE ${whereByKey(t, pk)};`;
}
