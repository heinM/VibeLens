// SQL formatting (sql-formatter, PostgreSQL dialect) plus cursor mapping across a reformat.
import { format } from "sql-formatter";

/** Formats `sql`. Throws an Error with a readable message when the formatter cannot parse it. */
export function formatSql(sql: string): string {
  const trail = /\s*$/.exec(sql)?.[0] ?? "";
  if (!sql.trim()) return sql;
  const out = format(sql, {
    language: "postgresql",
    keywordCase: "upper",
    dataTypeCase: "lower",
    functionCase: "lower",
    identifierCase: "preserve",
    tabWidth: 2,
    useTabs: false,
    linesBetweenQueries: 1,
    denseOperators: false,
  });
  // Keep the caller's surrounding blank space (a trailing newline matters in a worksheet).
  return out + (trail.includes("\n") ? "\n" : "");
}

/**
 * Where a cursor lands after a reformat that only changes whitespace and keyword case: count the
 * non-whitespace characters before the old cursor and find the same count in the new text.
 */
export function mapOffsetAcrossFormat(before: string, offset: number, after: string): number {
  let nonWs = 0;
  for (let i = 0; i < Math.min(offset, before.length); i++) if (!/\s/.test(before[i])) nonWs++;
  if (nonWs === 0) return 0;
  let seen = 0;
  for (let j = 0; j < after.length; j++) {
    if (!/\s/.test(after[j])) {
      seen++;
      if (seen === nonWs) return j + 1;
    }
  }
  return after.length;
}
