// SQL tokenizer for editor intelligence. Understands Postgres lexical structure well enough to
// drive scope and cursor-context analysis: '...' strings (with '' and E'\' escapes), "..."
// identifiers, $tag$...$tag$ dollar quotes, -- and nested /* */ comments, $1 parameters, `::` casts.
// It never throws and always makes progress; unterminated constructs run to the end of input and
// are flagged `closed: false` so callers can tell "cursor is inside an open string" from "after it".

export type TokenKind = "word" | "quoted" | "string" | "number" | "punct" | "comment" | "param";

export interface Token {
  kind: TokenKind;
  /** Source text exactly as written (quotes included). */
  text: string;
  start: number;
  /** Exclusive end offset. */
  end: number;
  /** False for an unterminated string, quoted identifier or block comment. */
  closed: boolean;
}

const WORD_START = /[A-Za-z_\u0080-￿]/;
const WORD_PART = /[A-Za-z0-9_$\u0080-￿]/;
const DIGIT = /[0-9]/;
const DOLLAR_TAG = /^\$([A-Za-z_\u0080-￿][A-Za-z0-9_\u0080-￿]*)?\$/;

export interface TokenizeOptions {
  /**
   * MySQL lexing: `#` starts a line comment and `` `quoted` `` is an identifier. Off for every
   * other dialect, which keeps its tokens exactly as before (in Postgres `#` is an operator).
   */
  mysql?: boolean;
}

/** Tokenizes `sql`. Whitespace is dropped; comments are kept (as `comment` tokens). */
export function tokenize(sql: string, opts: TokenizeOptions = {}): Token[] {
  const out: Token[] = [];
  const n = sql.length;
  let i = 0;
  const push = (kind: TokenKind, start: number, end: number, closed = true) => out.push({ kind, text: sql.slice(start, end), start, end, closed });

  while (i < n) {
    const ch = sql[i];
    const next = sql[i + 1];

    if (ch === " " || ch === "\t" || ch === "\n" || ch === "\r" || ch === "\f" || ch === " ") {
      i++;
      continue;
    }

    if (ch === "-" && next === "-") {
      let j = i + 2;
      while (j < n && sql[j] !== "\n" && sql[j] !== "\r") j++;
      push("comment", i, j);
      i = j;
      continue;
    }

    if (ch === "#" && opts.mysql) {
      let j = i + 1;
      while (j < n && sql[j] !== "\n") j++;
      push("comment", i, j);
      i = j;
      continue;
    }

    if (ch === "`" && opts.mysql) {
      let j = i + 1;
      let closed = false;
      while (j < n) {
        if (sql[j] === "`") {
          if (sql[j + 1] === "`") {
            j += 2;
            continue;
          }
          j++;
          closed = true;
          break;
        }
        j++;
      }
      push("quoted", i, Math.min(j, n), closed);
      i = Math.min(j, n);
      continue;
    }

    if (ch === "/" && next === "*") {
      let depth = 1;
      let j = i + 2;
      while (j < n && depth > 0) {
        if (sql[j] === "/" && sql[j + 1] === "*") {
          depth++;
          j += 2;
        } else if (sql[j] === "*" && sql[j + 1] === "/") {
          depth--;
          j += 2;
        } else j++;
      }
      push("comment", i, j, depth === 0);
      i = j;
      continue;
    }

    if (ch === "'") {
      // E'..' allows backslash escapes. A one-letter prefix (E, B, X, N, U&) is part of the literal.
      const prev = out[out.length - 1];
      const prefixed = prev && prev.kind === "word" && prev.end === i && /^[EeBbXxNn]$/.test(prev.text);
      const escapeStyle = prefixed && /^[Ee]$/.test(prev.text);
      let j = i + 1;
      let closed = false;
      while (j < n) {
        if (escapeStyle && sql[j] === "\\") {
          j += 2;
          continue;
        }
        if (sql[j] === "'") {
          if (sql[j + 1] === "'") {
            j += 2;
            continue;
          }
          j++;
          closed = true;
          break;
        }
        j++;
      }
      j = Math.min(j, n);
      if (prefixed) {
        const p = out.pop() as Token;
        push("string", p.start, j, closed);
      } else push("string", i, j, closed);
      i = j;
      continue;
    }

    if (ch === '"') {
      let j = i + 1;
      let closed = false;
      while (j < n) {
        if (sql[j] === '"') {
          if (sql[j + 1] === '"') {
            j += 2;
            continue;
          }
          j++;
          closed = true;
          break;
        }
        j++;
      }
      push("quoted", i, Math.min(j, n), closed);
      i = Math.min(j, n);
      continue;
    }

    if (ch === "$") {
      if (next !== undefined && DIGIT.test(next)) {
        let j = i + 1;
        while (j < n && DIGIT.test(sql[j])) j++;
        push("param", i, j);
        i = j;
        continue;
      }
      const m = DOLLAR_TAG.exec(sql.slice(i, i + 80));
      if (m) {
        const tag = m[0];
        const close = sql.indexOf(tag, i + tag.length);
        const end = close < 0 ? n : close + tag.length;
        push("string", i, end, close >= 0);
        i = end;
        continue;
      }
    }

    if (WORD_START.test(ch)) {
      let j = i + 1;
      while (j < n && WORD_PART.test(sql[j])) j++;
      push("word", i, j);
      i = j;
      continue;
    }

    if (DIGIT.test(ch)) {
      let j = i + 1;
      while (j < n && /[0-9_]/.test(sql[j])) j++;
      if (sql[j] === "." && DIGIT.test(sql[j + 1] ?? "")) {
        j++;
        while (j < n && DIGIT.test(sql[j])) j++;
      }
      if ((sql[j] === "e" || sql[j] === "E") && /[0-9+-]/.test(sql[j + 1] ?? "")) {
        let k = j + 1;
        if (sql[k] === "+" || sql[k] === "-") k++;
        if (DIGIT.test(sql[k] ?? "")) {
          while (k < n && DIGIT.test(sql[k])) k++;
          j = k;
        }
      }
      push("number", i, j);
      i = j;
      continue;
    }

    if (ch === ":" && next === ":") {
      push("punct", i, i + 2);
      i += 2;
      continue;
    }

    push("punct", i, i + 1);
    i++;
  }
  return out;
}

/** True for tokens that can name something: unquoted words and "quoted" identifiers. */
export const isNameToken = (t: Token | undefined): boolean => !!t && (t.kind === "word" || t.kind === "quoted");

/** Lower-cased text of a word token, "" for anything else. Used for keyword tests. */
export const kw = (t: Token | undefined): string => (t && t.kind === "word" ? t.text.toLowerCase() : "");

/** The identifier an unquoted/quoted name token denotes (quotes removed, "" unescaped). */
export function identValue(t: Token): string {
  if (t.kind !== "quoted") return t.text;
  const inner = t.closed ? t.text.slice(1, -1) : t.text.slice(1);
  return t.text.startsWith("`") ? inner.replace(/``/g, "`") : inner.replace(/""/g, '"');
}

/**
 * Where the cursor sits relative to non-code text: inside a string, comment, number or parameter
 * (no completions wanted) or in code. Offsets at a token's very end count as inside only when the
 * token is unterminated (or a line comment, which runs to the end of the line).
 */
export function insideNonCode(tokens: Token[], offset: number): Token | null {
  for (const t of tokens) {
    if (t.start >= offset) break;
    if (t.kind === "string" || t.kind === "comment") {
      const lineComment = t.kind === "comment" && (t.text.startsWith("--") || t.text.startsWith("#"));
      if (offset < t.end || (offset === t.end && (!t.closed || lineComment))) return t;
    } else if ((t.kind === "number" || t.kind === "param") && offset <= t.end) {
      return t;
    }
  }
  return null;
}
