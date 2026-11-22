// Conservative client-side SQL statement splitter used to find "the statement under the cursor".
// It understands: '...' strings (with '' and E'\' escapes), "..." identifiers, $tag$...$tag$ dollar
// quotes, -- line comments and nested /* */ block comments. The backend does the authoritative
// parse when a script runs; this only decides *what text to send* for a cursor-run.

export interface Statement {
  /** Offset of the first significant character (a leading comment counts). */
  start: number;
  /** Exclusive end offset; includes the terminating semicolon when present. */
  end: number;
  text: string;
}

const isIdentChar = (ch: string | undefined): boolean => ch !== undefined && /[A-Za-z0-9_\u0080-￿]/.test(ch);
const DOLLAR_TAG = /^\$([A-Za-z_\u0080-￿][A-Za-z0-9_\u0080-￿]*)?\$/;

export interface SplitOptions {
  /**
   * MySQL lexing: `#` line comments, `` `backtick` `` identifiers, backslash escapes in '…' and "…",
   * no dollar quotes, non-nesting block comments, and the BEGIN … END body of a CREATE PROCEDURE,
   * FUNCTION, TRIGGER or EVENT kept in one statement (the Rust splitter does the same).
   */
  mysql?: boolean;
}

const MYSQL_ROUTINE = /^\s*(?:(?:--[ \t][^\n]*|#[^\n]*)\n\s*|\/\*[\s\S]*?\*\/\s*)*(?:create\b[^;(]*?\b(?:procedure|function|trigger|event)\b|alter\s+event\b)/i;

export function splitStatements(sql: string, opts: SplitOptions = {}): Statement[] {
  const my = !!opts.mysql;
  /** BEGIN / CASE nesting inside a MySQL routine body (only counted when the statement is one). */
  let depth = 0;
  const out: Statement[] = [];
  const n = sql.length;
  let i = 0;
  let start = -1; // first non-whitespace index of the current chunk
  let hasContent = false; // saw something other than whitespace/comments
  let lastSignificant = -1; // end (exclusive) of the last non-whitespace char

  const touch = (from: number, to: number, content: boolean) => {
    if (start < 0) start = from;
    if (content) hasContent = true;
    lastSignificant = to;
  };

  while (i < n) {
    const ch = sql[i];
    const next = sql[i + 1];

    if (ch === " " || ch === "\t" || ch === "\n" || ch === "\r" || ch === "\f") {
      i++;
      continue;
    }

    // # line comment (MySQL)
    if (my && ch === "#") {
      let j = i + 1;
      while (j < n && sql[j] !== "\n") j++;
      touch(i, j, false);
      i = j;
      continue;
    }

    // `identifier` (MySQL)
    if (my && ch === "`") {
      let j = i + 1;
      while (j < n) {
        if (sql[j] === "`") {
          if (sql[j + 1] === "`") {
            j += 2;
            continue;
          }
          j++;
          break;
        }
        j++;
      }
      touch(i, Math.min(j, n), true);
      i = Math.min(j, n);
      continue;
    }

    // MySQL 'string' / "string" with backslash escapes
    if (my && (ch === "'" || ch === '"')) {
      let j = i + 1;
      while (j < n) {
        if (sql[j] === "\\") {
          j += 2;
          continue;
        }
        if (sql[j] === ch) {
          if (sql[j + 1] === ch) {
            j += 2;
            continue;
          }
          j++;
          break;
        }
        j++;
      }
      touch(i, Math.min(j, n), true);
      i = Math.min(j, n);
      continue;
    }

    // MySQL BEGIN … END inside a routine body
    if (my && /[A-Za-z]/.test(ch) && !isIdentChar(sql[i - 1])) {
      let j = i + 1;
      while (j < n && isIdentChar(sql[j])) j++;
      const w = sql.slice(i, j).toUpperCase();
      if ((w === "BEGIN" || w === "CASE" || w === "END") && start >= 0 && MYSQL_ROUTINE.test(sql.slice(start, i))) {
        if (w === "END") {
          const after = /^\s*([A-Za-z]+)/.exec(sql.slice(j))?.[1]?.toUpperCase();
          if (!(after === "IF" || after === "LOOP" || after === "WHILE" || after === "REPEAT")) depth = Math.max(0, depth - 1);
        } else depth++;
      }
      touch(i, j, true);
      i = j;
      continue;
    }

    // -- line comment
    if (ch === "-" && next === "-") {
      let j = i + 2;
      while (j < n && sql[j] !== "\n") j++;
      touch(i, j, false);
      i = j;
      continue;
    }

    // /* block comment */ (Postgres nests these; MySQL does not)
    if (ch === "/" && next === "*") {
      let depth = 1;
      let j = i + 2;
      while (j < n && depth > 0) {
        if (!my && sql[j] === "/" && sql[j + 1] === "*") {
          depth++;
          j += 2;
        } else if (sql[j] === "*" && sql[j + 1] === "/") {
          depth--;
          j += 2;
        } else j++;
      }
      touch(i, j, false);
      i = j;
      continue;
    }

    // 'string' (E'..' allows backslash escapes)
    if (ch === "'") {
      const prev = sql[i - 1];
      const escapeStyle = (prev === "E" || prev === "e") && !isIdentChar(sql[i - 2]);
      let j = i + 1;
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
          break;
        }
        j++;
      }
      touch(i, Math.min(j, n), true);
      i = Math.min(j, n);
      continue;
    }

    // "identifier"
    if (ch === '"') {
      let j = i + 1;
      while (j < n) {
        if (sql[j] === '"') {
          if (sql[j + 1] === '"') {
            j += 2;
            continue;
          }
          j++;
          break;
        }
        j++;
      }
      touch(i, Math.min(j, n), true);
      i = Math.min(j, n);
      continue;
    }

    // $tag$ ... $tag$
    if (!my && ch === "$" && !isIdentChar(sql[i - 1])) {
      const m = DOLLAR_TAG.exec(sql.slice(i, i + 80));
      if (m) {
        const tag = m[0];
        const close = sql.indexOf(tag, i + tag.length);
        const end = close < 0 ? n : close + tag.length;
        touch(i, end, true);
        i = end;
        continue;
      }
    }

    if (ch === ";" && depth > 0) {
      touch(i, i + 1, true);
      i++;
      continue;
    }

    if (ch === ";") {
      if (hasContent) {
        out.push({ start, end: i + 1, text: sql.slice(start, i + 1) });
      }
      start = -1;
      hasContent = false;
      lastSignificant = -1;
      i++;
      continue;
    }

    touch(i, i + 1, true);
    i++;
  }

  if (hasContent && start >= 0) {
    out.push({ start, end: lastSignificant, text: sql.slice(start, lastSignificant) });
  }
  return out;
}

/**
 * The statement the cursor belongs to. A cursor sitting right after a `;`, or in the blank space
 * between two statements, resolves to the nearest preceding statement (then the first following).
 */
export function statementAtOffset(sql: string, offset: number, opts: SplitOptions = {}): Statement | null {
  const stmts = splitStatements(sql, opts);
  if (stmts.length === 0) return null;
  const hit = stmts.find((s) => offset >= s.start && offset <= s.end);
  if (hit) return hit;
  let prev: Statement | null = null;
  for (const s of stmts) {
    if (s.end < offset) prev = s;
    else break;
  }
  return prev ?? stmts[0];
}
