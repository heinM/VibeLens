// Text query plans from the Iceberg engines. DataFusion answers `EXPLAIN <sql>` with rows of
// (plan_type, plan); Athena (Trino) answers with one text column, one line per row. These are shown
// verbatim in a monospaced view and never go to the Postgres plan visualiser.
//
// For "Explain with AI" the same rules as the Postgres plans apply (see planRedact.ts): unless the
// connection lets result rows leave the machine, constants and filter expressions are removed
// first. Text plans have no structure to walk, so the redaction here is deliberately blunt and
// errs towards removing too much: predicates and expressions go, operator names and columns stay.
import type { Cell, ColumnMeta } from "../../ipc/types";
import { redactSqlLiterals } from "../../lib/sqlIntel/redact";
import { stripExplainPrefix } from "./planPrompt";
import { redactConditionText } from "./planRedact";

export interface TextPlanSection {
  /** `logical_plan`, `physical_plan`, ... (DataFusion). Null for a single unnamed plan. */
  title: string | null;
  text: string;
}

const cellText = (c: Cell): string => (c === null || c === undefined ? "" : typeof c === "string" ? c : typeof c === "object" ? JSON.stringify(c) : String(c));

/**
 * The plan sections of an Iceberg `EXPLAIN` result, or null when the result does not have the shape
 * of one (so the caller can fall back to the plain grid).
 */
export function textPlanSections(columns: ColumnMeta[], rows: readonly Cell[][]): TextPlanSection[] | null {
  if (rows.length === 0) return null;
  if (columns.length === 2 && columns[0].name === "plan_type" && columns[1].name === "plan") {
    return rows.map((r) => ({ title: cellText(r[0]) || null, text: cellText(r[1]).replace(/\s+$/, "") }));
  }
  if (columns.length === 1) {
    // Athena: the plan is one text column, a line per row (or the whole plan in one cell).
    const text = rows.map((r) => cellText(r[0])).join("\n").replace(/\s+$/, "");
    return text ? [{ title: null, text }] : null;
  }
  return null;
}

/** The plan as one block of text, for copying. */
export function planAsText(sections: readonly TextPlanSection[]): string {
  return sections.map((s) => (s.title ? `${s.title}\n${s.text}` : s.text)).join("\n\n");
}

// -- redaction ------------------------------------------------------------------------------------

/** DataFusion prints constants as a typed wrapper: Utf8("paid"), Int64(42), Date32("19000"), Decimal128(Some(1999),10,2). */
const DF_LITERAL = /\b(?:Utf8View|LargeUtf8|Utf8|Boolean|UInt(?:8|16|32|64)|Int(?:8|16|32|64)|Float(?:16|32|64)|Decimal(?:128|256)|Date(?:32|64)|BinaryView|LargeBinary|Binary|FixedSizeBinary|Time(?:32|64)\w*|Timestamp\w*|Interval\w*|Duration\w*|LargeList|List|Struct|Map|Dictionary)\(/g;

/** Index just past the `)` matching the `(` at `open`, skipping quoted text; the end of the line if unbalanced. */
function closeOf(s: string, open: number): number {
  let depth = 0;
  for (let i = open; i < s.length; i++) {
    const ch = s[i];
    if (ch === '"' || ch === "'") {
      for (i++; i < s.length && s[i] !== ch; i++) if (s[i] === "\\") i++;
    } else if (ch === "(" || ch === "[" || ch === "{") depth++;
    else if (ch === ")" || ch === "]" || ch === "}") {
      depth--;
      if (depth === 0) return i + 1;
    }
  }
  return s.length;
}

function stripTypedLiterals(line: string): string {
  let out = "";
  let at = 0;
  DF_LITERAL.lastIndex = 0;
  for (let m = DF_LITERAL.exec(line); m; m = DF_LITERAL.exec(line)) {
    if (m.index < at) continue;
    const open = m.index + m[0].length - 1;
    out += line.slice(at, m.index) + "?";
    at = closeOf(line, open);
    DF_LITERAL.lastIndex = at;
  }
  return out + line.slice(at);
}

/** Attributes whose value is an expression or a file location and so may carry constants. */
const CUT_ATTR = /\b(\w*(?:[Pp]redicate|[Ff]ilters?|guarantees|bounds?|file_groups|expr|aggr|filterPredicate|constraint)\w*|lower_bound|upper_bound)\s*=\s*/g;

function cutAttributes(line: string): string {
  let out = "";
  let at = 0;
  CUT_ATTR.lastIndex = 0;
  for (let m = CUT_ATTR.exec(line); m; m = CUT_ATTR.exec(line)) {
    if (m.index < at) continue;
    const valueStart = m.index + m[0].length;
    const first = line[valueStart];
    let end: number;
    if (first === "(" || first === "[" || first === "{") end = closeOf(line, valueStart);
    else {
      // Bare value: runs to the next `, name=` or the end of the line.
      const next = /,\s*\w+\s*=/.exec(line.slice(valueStart));
      end = next ? valueStart + next.index : line.length;
    }
    const open = first === "[" || first === "{" || first === "(" ? first : "";
    const close = open === "[" ? "]" : open === "{" ? "}" : open === "(" ? ")" : "";
    out += line.slice(at, valueStart) + `${open}…${close}`;
    at = end;
    CUT_ATTR.lastIndex = end;
  }
  return out + line.slice(at);
}

const FILTER_LABEL = /^(\s*(?:\d+\.\s*|[-|+\s]*)?(?:Filter|FilterExec|Selection)):\s.*$/;
/** A storage URL, up to the next delimiter (plans print them inside `{[[...]]}` lists). */
const LOCATION = /\b(?:s3a?|gs|abfss?|file|hdfs):\/\/[^\s,\]\)\}"']*/g;

/** `col@3` keeps its column index through the numeric scrub (digits become letters, then back). */
const protectIndexes = (s: string): string => s.replace(/@(\d+)/g, (_m, d: string) => "@\u0001" + [...d].map((c) => String.fromCharCode(97 + Number(c))).join("") + "\u0002");
const restoreIndexes = (s: string): string => s.replace(/@\u0001([a-j]+)\u0002/g, (_m, l: string) => "@" + [...l].map((c) => c.charCodeAt(0) - 97).join(""));

/** One line of a text plan with constants, predicates, expressions and file locations removed. */
export function redactPlanLine(line: string): string {
  let s = line.replace(LOCATION, "<location>");
  const filter = FILTER_LABEL.exec(s);
  if (filter) return `${filter[1]}: …`;
  s = stripTypedLiterals(s);
  s = cutAttributes(s);
  return restoreIndexes(redactConditionText(protectIndexes(s)));
}

/**
 * The plan text with constants removed. Never returns the input unchanged by accident: every line
 * goes through the same scrub. Returns "" when nothing is left.
 */
export function redactTextPlan(text: string): string {
  return text
    .split(/\r?\n/)
    .map(redactPlanLine)
    .join("\n")
    .trim();
}

// -- prompt ---------------------------------------------------------------------------------------

export interface TextPlanPromptInput {
  sql: string | undefined;
  sections: readonly TextPlanSection[];
  /** `DataFusion` (local) or `Athena`. */
  engineLabel: string;
  /** Replace constants before sending (see `explainNeedsRedaction`). */
  redact: boolean;
  maxPlanChars?: number;
}

/**
 * The "Explain with AI" prompt for a text plan, or null when redaction was required and left nothing
 * safe to send (fail closed, as for the Postgres plans).
 */
export function buildTextPlanPrompt({ sql, sections, engineLabel, redact, maxPlanChars = 12_000 }: TextPlanPromptInput): string | null {
  const bare = sql ? stripExplainPrefix(sql).trim() : "";
  const sqlText = redact ? redactSqlLiterals(bare) : bare;
  if (redact && bare && !sqlText) return null;
  const planParts = sections.map((s) => {
    const body = redact ? redactTextPlan(s.text) : s.text;
    return s.title ? `${s.title}:\n${body}` : body;
  });
  let plan = planParts.join("\n\n");
  if (redact && !plan.trim()) return null;
  if (plan.length > maxPlanChars) plan = `${plan.slice(0, maxPlanChars)}\n… (plan truncated)`;
  const athena = /athena/i.test(engineLabel);
  return [
    `Review this ${engineLabel} query plan for an Apache Iceberg table. Give a concise diagnosis of what will make it slow${athena ? " or expensive (Athena bills by data scanned)" : ""}, or confirm that it is fine, then concrete fixes ranked by expected impact.`,
    "",
    "Requirements:",
    "- Start with a 2-3 sentence diagnosis that points at the specific plan operators.",
    "- Look for: full scans where a partition or filter could prune files, columns read but not needed, joins that move the large side, sorts or aggregates over big inputs, small-file overhead.",
    "- For each fix give runnable SQL in a ```sql block where possible (a rewritten query, a partition-aware filter, a table maintenance statement).",
    "- State the cost of every fix as specifically as its benefit: rewrite effort, extra storage, write amplification, maintenance windows.",
    "- This is an estimated plan: nothing was executed. If the evidence is weak, say so instead of guessing.",
    "- Do not run any statement that changes data. You may look up the schema if it helps.",
    "- Keep it short. No preamble.",
    ...(redact ? ["- Constants, predicates and file locations were removed from the plan and the query before sending, so do not ask for them."] : []),
    "",
    "Query:",
    "```sql",
    sqlText || "(the SQL text was not available)",
    "```",
    "",
    "Plan:",
    "```text",
    plan,
    "```",
  ].join("\n");
}
