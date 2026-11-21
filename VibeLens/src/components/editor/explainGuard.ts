// Decides how the toolbar's Explain buttons may treat a statement. Pure, so it is tested.
import { isIcebergDialect, type SqlDialect } from "../../lib/sqlIntel/dialect";
import { tokenize } from "../../lib/sqlIntel/tokenizer";
import { splitStatements } from "./sqlSplit";

/** True when the first real token (after whitespace and any -- or block comments) is the word EXPLAIN. */
export function startsWithExplain(sql: string): boolean {
  const first = tokenize(sql).find((t) => t.kind !== "comment");
  return !!first && first.kind === "word" && first.text.toLowerCase() === "explain";
}

export const ALREADY_EXPLAIN = "This is already an EXPLAIN; run it normally.";

export const ICEBERG_NO_ANALYZE = "Explain Analyze isn't available for Iceberg connections yet. Use Explain for the plan, or run the query and read its data-scanned figure.";
export const REDSHIFT_NO_EXPLAIN = "Explain isn't available for Redshift connections. Run EXPLAIN <statement> to see Redshift's text plan.";
export const ICEBERG_ONE_STATEMENT = "Explain works on one statement at a time. Select a single statement, or put the cursor in it.";

export type ExplainPlan =
  /** `text: true` marks an Iceberg plan: plain text rows, not Postgres `FORMAT JSON`. */
  | { ok: true; sql: string; readOnly: true; text?: true }
  | { ok: false; message: string };

const stripSemicolon = (s: string) => s.trim().replace(/[;\s]+$/, "");

/**
 * The statement to send for the Explain / Explain Analyze buttons. A statement that already starts
 * with EXPLAIN is refused: wrapping it would nest EXPLAINs, and sending it as is would run whatever
 * ANALYZE it carries with write access. Both modes always run inside a READ ONLY transaction.
 */
export function planExplain(sql: string, mode: "plan" | "analyze", dialect?: SqlDialect | null): ExplainPlan {
  // The plan view reads Postgres JSON plans and Iceberg text plans; Redshift's plan is neither (and the
  // toolbar hides the buttons for it). Refuse rather than send Postgres-only EXPLAIN options to Redshift.
  if (dialect === "redshift") return { ok: false, message: REDSHIFT_NO_EXPLAIN };
  if (startsWithExplain(sql)) return { ok: false, message: ALREADY_EXPLAIN };
  const body = stripSemicolon(sql);
  if (isIcebergDialect(dialect)) {
    // DataFusion and Athena both answer a plain `EXPLAIN <statement>` with text. ANALYZE would execute
    // the statement (and, on Athena, bill for it), so only the estimated plan is offered. A script is
    // refused rather than half-explained: the statements after the first would run for real.
    if (mode === "analyze") return { ok: false, message: ICEBERG_NO_ANALYZE };
    if (splitStatements(sql).length > 1) return { ok: false, message: ICEBERG_ONE_STATEMENT };
    return { ok: true, readOnly: true, text: true, sql: `EXPLAIN ${body}` };
  }
  return {
    ok: true,
    readOnly: true,
    sql: mode === "plan" ? `EXPLAIN (FORMAT JSON, VERBOSE, COSTS) ${body}` : `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) ${body}`,
  };
}
