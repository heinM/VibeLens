// Best-effort reading of the condition strings Postgres prints in plans:
//   ((status)::text = 'pending'::text) AND (created_at > '2026-01-01 00:00:00+00'::timestamp with time zone)
// Used only to suggest index columns. It never needs to be complete; it must never invent columns.

export interface Atom {
  /** Left operand, normalised: casts and redundant parentheses removed. */
  left: string;
  op: string;
  /** Right operand, trimmed, may be empty for IS [NOT] NULL. */
  right: string;
}

/** Splits on a top-level keyword (AND/OR), ignoring quotes and nested parentheses. */
export function splitTop(expr: string, kw: "AND" | "OR"): string[] {
  const out: string[] = [];
  let depth = 0;
  let quote: string | null = null;
  let start = 0;
  const re = kw === "AND" ? /^\s+AND\s+/i : /^\s+OR\s+/i;
  for (let i = 0; i < expr.length; i++) {
    const ch = expr[i];
    if (quote) {
      if (ch === quote) {
        if (expr[i + 1] === quote) i++;
        else quote = null;
      }
      continue;
    }
    if (ch === "'" || ch === '"') quote = ch;
    else if (ch === "(" || ch === "[") depth++;
    else if (ch === ")" || ch === "]") depth--;
    else if (depth === 0 && /\s/.test(ch)) {
      const m = re.exec(expr.slice(i));
      if (m) {
        out.push(expr.slice(start, i));
        i += m[0].length - 1;
        start = i + 1;
      }
    }
  }
  out.push(expr.slice(start));
  return out.map((s) => s.trim()).filter(Boolean);
}

/** Removes parentheses that wrap the whole string. */
export function stripOuterParens(s: string): string {
  let t = s.trim();
  while (t.startsWith("(") && t.endsWith(")")) {
    let depth = 0;
    let wraps = true;
    let quote: string | null = null;
    for (let i = 0; i < t.length; i++) {
      const ch = t[i];
      if (quote) {
        if (ch === quote) quote = null;
        continue;
      }
      if (ch === "'" || ch === '"') quote = ch;
      else if (ch === "(") depth++;
      else if (ch === ")") {
        depth--;
        if (depth === 0 && i < t.length - 1) {
          wraps = false;
          break;
        }
      }
    }
    if (!wraps) break;
    t = t.slice(1, -1).trim();
  }
  return t;
}

/** Drops `::type` casts (including `timestamp with time zone`, `character varying(20)`, `bigint[]`). */
function stripCasts(s: string): string {
  return s.replace(/::\s*(?:"[^"]+"|[a-z_][a-z0-9_]*)(?: (?:varying|precision|with(?:out)? time zone))?(?:\(\d+(?:,\s*\d+)?\))?(?:\[\])*/gi, "");
}

const OPS = /^(.*?)\s+(IS NOT NULL|IS NULL|IS NOT DISTINCT FROM|IS DISTINCT FROM|<>|!=|<=|>=|=|<|>|!~~\*|!~~|~~\*|~~|!~\*|!~|~\*|~|@>|<@|&&)(?:\s+(.*))?$/is;

/** Parses one conjunct into left/op/right. Returns null for anything it doesn't understand. */
export function parseAtom(raw: string): Atom | null {
  const t = stripOuterParens(raw);
  if (!t) return null;
  if (splitTop(t, "OR").length > 1) return null;
  const m = OPS.exec(t);
  if (!m) return null;
  const left = stripOuterParens(stripCasts(m[1]).trim()).replace(/\(\s*([a-z_][\w."$]*)\s*\)/gi, "$1");
  return { left, op: m[2].toUpperCase().replace(/\s+/g, " "), right: (m[3] ?? "").trim() };
}

/** AND-separated atoms; OR groups are dropped because they can't drive a single index scan. */
export function parseConditions(expr: string | null | undefined): Atom[] {
  if (!expr) return [];
  const out: Atom[] = [];
  for (const piece of splitTop(stripOuterParens(expr), "AND")) {
    const a = parseAtom(piece);
    if (a) out.push(a);
  }
  return out;
}

const IDENT = /^(?:"[^"]+"|[a-z_][a-z0-9_$]*)(?:\.(?:"[^"]+"|[a-z_][a-z0-9_$]*))?$/i;
const EQUALITY = new Set(["=", "IS NULL"]);
const RANGE = new Set(["<", ">", "<=", ">="]);

const unq = (s: string) => s.replace(/^"|"$/g, "");

export interface FilterColumns {
  /** Equality columns first, in order of appearance (alias prefix removed). */
  equality: string[];
  /** At most one range column; btree can only use the first. */
  range: string[];
  /** Function/expression operands, e.g. `lower(email)`: candidates for expression indexes. */
  expressions: string[];
  /** equality + range, the order to use in CREATE INDEX. */
  index: string[];
}

/** Which columns a Filter / Index Cond constrains, in the order a btree index would want them. */
export function filterColumns(expr: string | null | undefined, alias?: string | null): FilterColumns {
  const equality: string[] = [];
  const range: string[] = [];
  const expressions: string[] = [];
  for (const a of parseConditions(expr)) {
    const isEq = EQUALITY.has(a.op);
    const isRange = RANGE.has(a.op);
    if (!isEq && !isRange) continue;
    // Skip column-to-column comparisons (join conditions): `a.x = b.y`
    if (a.right && IDENT.test(a.right) && !/^\d/.test(a.right) && a.right.toLowerCase() !== "null") continue;
    if (IDENT.test(a.left)) {
      const parts = a.left.split(".");
      if (parts.length === 2 && alias && unq(parts[0]) !== alias) continue;
      const col = unq(parts[parts.length - 1]);
      if (isEq) {
        if (!equality.includes(col)) equality.push(col);
      } else if (!range.includes(col)) range.push(col);
    } else if (/^[a-z_][a-z0-9_]*\(.*\)$/i.test(a.left) && isEq) {
      if (!expressions.includes(a.left)) expressions.push(a.left);
    }
  }
  const rangeOnly = range.filter((c) => !equality.includes(c)).slice(0, 1);
  return { equality, range: rangeOnly, expressions, index: [...equality, ...rangeOnly] };
}

/** For a join condition `a.x = b.y`, returns the columns that belong to the given alias. */
export function joinColumnsFor(expr: string | null | undefined, alias: string | null | undefined): string[] {
  if (!alias) return [];
  const out: string[] = [];
  for (const a of parseConditions(expr)) {
    if (a.op !== "=") continue;
    for (const side of [a.left, a.right]) {
      const m = /^"?([^".\s]+)"?\.("?[^".\s]+"?)$/.exec(side.trim());
      if (m && m[1] === alias) {
        const c = unq(m[2]);
        if (!out.includes(c)) out.push(c);
      }
    }
  }
  return out;
}
