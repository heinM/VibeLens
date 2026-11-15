// The "last result" context chip: columns plus the first rows of the most recent query result,
// attached to a message only when the user chooses to, and only if the connection's AI access
// allows row data to leave the machine.
import { create } from "zustand";
import type { AiDataAccess, Cell, ColumnMeta } from "../../ipc/types";

export const LAST_RESULT_ROWS = 20;
const CELL_MAX = 80;
const BLOCK_MAX = 8000;

export interface LastResultSnapshot {
  columns: ColumnMeta[];
  /** At most LAST_RESULT_ROWS rows. */
  rows: Cell[][];
  /** Rows in the full result. */
  rowCount: number;
  sql: string | null;
  profileId: string | null;
  database: string | null;
  at: number;
}

interface LastResultState {
  snapshot: LastResultSnapshot | null;
  set: (s: LastResultSnapshot | null) => void;
}

export const useLastResultStore = create<LastResultState>((set) => ({ snapshot: null, set: (snapshot) => set({ snapshot }) }));

/**
 * Called by whatever shows query results. Keeps a copy of the first rows only, so the full
 * (possibly 100k-row) buffer is never retained.
 */
export function publishLastResult(r: { columns: ColumnMeta[]; rows: Cell[][]; rowCount: number; sql?: string | null; profileId?: string | null; database?: string | null }): void {
  if (r.columns.length === 0) return;
  useLastResultStore.getState().set({
    columns: r.columns,
    rows: r.rows.slice(0, LAST_RESULT_ROWS),
    rowCount: r.rowCount,
    sql: r.sql ?? null,
    profileId: r.profileId ?? null,
    database: r.database ?? null,
    at: Date.now(),
  });
}

/** Row data may only be attached when the connection lets results leave the machine. */
export function canAttachResults(access: AiDataAccess | null | undefined): boolean {
  return access === "results" || access === "resultsWithApproval";
}

export function attachBlockedReason(access: AiDataAccess | null | undefined): string | null {
  if (canAttachResults(access)) return null;
  if (access === "none") return "AI access is off for this connection, so result rows can't be sent. Change it in the connection's settings.";
  if (access === "schemaOnly") return "This connection is set to schema only, so result rows never leave your machine. Change AI access in the connection's settings to attach results.";
  return "Result rows can't be attached: this connection's AI access isn't known yet.";
}

function cellText(c: Cell): string {
  if (c === null) return "NULL";
  if (typeof c === "object" && !Array.isArray(c) && "$bytes" in c) return `<${(c as { len: number }).len} bytes>`;
  const s = typeof c === "string" ? c : typeof c === "object" ? JSON.stringify(c) : String(c);
  const flat = s.replace(/\s+/g, " ").replace(/\|/g, "\\|");
  return flat.length > CELL_MAX ? `${flat.slice(0, CELL_MAX - 1)}…` : flat;
}

/** A compact markdown table of the snapshot, for the model. */
export function formatLastResult(s: LastResultSnapshot): string {
  const head = `| ${s.columns.map((c) => c.name.replace(/\|/g, "\\|")).join(" | ")} |`;
  const rule = `| ${s.columns.map(() => "---").join(" | ")} |`;
  const lines = [head, rule];
  for (const row of s.rows.slice(0, LAST_RESULT_ROWS)) lines.push(`| ${s.columns.map((_, i) => cellText(row[i] ?? null)).join(" | ")} |`);
  let table = lines.join("\n");
  if (table.length > BLOCK_MAX) table = `${table.slice(0, BLOCK_MAX)}\n…(truncated)`;
  const shown = Math.min(s.rows.length, LAST_RESULT_ROWS);
  const total = s.rowCount.toLocaleString("en-US");
  const intro = `The result of my last query (${shown} of ${total} rows, ${s.columns.length} column${s.columns.length === 1 ? "" : "s"}):`;
  const sql = s.sql?.trim() ? `\nQuery:\n\`\`\`sql\n${s.sql.trim().slice(0, 1500)}\n\`\`\`\n` : "\n";
  return `${intro}${sql}${table}`;
}

/** The prompt actually sent: what the user typed, then the attached result. */
export function withLastResult(prompt: string, s: LastResultSnapshot | null): string {
  return s ? `${prompt.trim()}\n\n---\n${formatLastResult(s)}` : prompt.trim();
}
