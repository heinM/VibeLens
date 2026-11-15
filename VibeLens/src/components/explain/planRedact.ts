// Removes constants from a query plan before it is sent to a model that must not see row data.
// Postgres prints the constants of a query inside plan conditions (`Filter: ((status)::text =
// 'pending'::text)`, `Index Cond: (tenant_id = 42)`), so a plan can carry the very values the
// connection's AI policy keeps away from the model. Column names, operators, casts and
// structure stay, which is all a plan review needs.
import type { AiDataAccess, Environment } from "../../ipc/types";

/** Property names whose string values are expressions: every `* Cond`, `* Filter`, `Output`, `* Key`, `Order By`. */
const EXPRESSION_KEY = /(?:Cond|Filter|Output|Key|Order By|Function Call)$/;

const WORD_START = /[A-Za-z_\u0080-￿]/;
const WORD_PART = /[A-Za-z0-9_$\u0080-￿]/;
const DIGIT = /[0-9]/;

/**
 * Replaces string literals (`'…'`, `E'…'`, `B'…'`, `X'…'`, with `''` escapes) and numeric constants
 * in a Postgres expression with `?`. Quoted identifiers, parameters (`$1`), names with digits
 * (`col1`, `t2.id`) and casts are left alone.
 */
export function redactConditionText(expr: string): string {
  let out = "";
  const n = expr.length;
  let i = 0;
  while (i < n) {
    const ch = expr[i];
    if (ch === '"') {
      let j = i + 1;
      while (j < n) {
        if (expr[j] === '"') {
          if (expr[j + 1] === '"') {
            j += 2;
            continue;
          }
          break;
        }
        j++;
      }
      out += expr.slice(i, Math.min(j + 1, n));
      i = j + 1;
    } else if (ch === "'") {
      i = skipString(expr, i, false);
      out += "?";
    } else if (ch === "$" && DIGIT.test(expr[i + 1] ?? "")) {
      let j = i + 1;
      while (j < n && DIGIT.test(expr[j])) j++;
      out += expr.slice(i, j);
      i = j;
    } else if (WORD_START.test(ch)) {
      let j = i + 1;
      while (j < n && WORD_PART.test(expr[j])) j++;
      const word = expr.slice(i, j);
      // A one-letter prefix glued to a quote belongs to the literal (E'..' allows backslash escapes).
      if (expr[j] === "'" && /^[EeBbXxNn]$/.test(word)) {
        i = skipString(expr, j, /^[Ee]$/.test(word));
        out += "?";
      } else {
        out += word;
        i = j;
      }
    } else if (DIGIT.test(ch) || (ch === "." && DIGIT.test(expr[i + 1] ?? "") && !/[A-Za-z0-9_"')\]]/.test(out[out.length - 1] ?? ""))) {
      let j = i;
      while (j < n && DIGIT.test(expr[j])) j++;
      if (expr[j] === "." && DIGIT.test(expr[j + 1] ?? "")) {
        j++;
        while (j < n && DIGIT.test(expr[j])) j++;
      } else if (expr[j] === "." && ch !== ".") j++; // `5.`
      if ((expr[j] === "e" || expr[j] === "E") && /[0-9+-]/.test(expr[j + 1] ?? "")) {
        let k = j + 1;
        if (expr[k] === "+" || expr[k] === "-") k++;
        if (DIGIT.test(expr[k] ?? "")) {
          while (k < n && DIGIT.test(expr[k])) k++;
          j = k;
        }
      }
      out += "?";
      i = j;
    } else {
      out += ch;
      i++;
    }
  }
  return out;
}

/** Index just past the string literal that opens at `start` (an unterminated one runs to the end). */
function skipString(s: string, start: number, escapes: boolean): number {
  let j = start + 1;
  while (j < s.length) {
    if (escapes && s[j] === "\\") {
      j += 2;
      continue;
    }
    if (s[j] === "'") {
      if (s[j + 1] === "'") {
        j += 2;
        continue;
      }
      return j + 1;
    }
    j++;
  }
  return s.length;
}

function redactValue(v: unknown): unknown {
  if (typeof v === "string") return redactConditionText(v);
  if (Array.isArray(v)) return v.map(redactValue);
  return v;
}

function redactNode(node: unknown): unknown {
  if (Array.isArray(node)) return node.map(redactNode);
  if (!node || typeof node !== "object") return node;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(node as Record<string, unknown>)) {
    if (k === "Plans" || k === "Plan") out[k] = redactNode(v);
    else if (EXPRESSION_KEY.test(k)) out[k] = redactValue(v);
    else out[k] = v;
  }
  return out;
}

/**
 * A copy of an `EXPLAIN (FORMAT JSON)` document (the raw cell: JSON text, an array of one object, or
 * the object) with the constants removed from every condition, filter, output list and sort / group
 * key. Always returns the unwrapped array-of-one form, or null when the input is not a plan.
 */
export function redactPlanLiterals(raw: unknown): unknown[] | null {
  let v = raw;
  if (typeof v === "string") {
    try {
      v = JSON.parse(v);
    } catch {
      return null;
    }
  }
  const arr = Array.isArray(v) ? v : [v];
  if (arr.length === 0 || !arr[0] || typeof arr[0] !== "object" || !("Plan" in (arr[0] as object))) return null;
  return arr.map(redactNode);
}

/**
 * Whether literal values must be stripped before an explain is sent to the assistant: always unless
 * the connection lets result rows leave the machine (`results`), and also for `resultsWithApproval`
 * on production. An unknown policy is treated as the strictest. (`none` never sends anything.)
 */
export function explainNeedsRedaction(access: AiDataAccess | null | undefined, environment?: Environment | null): boolean {
  if (access === "results") return false;
  if (access === "resultsWithApproval") return environment === "prod";
  return true;
}
