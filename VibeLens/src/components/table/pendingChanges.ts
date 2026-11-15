// Pure model of the unsaved edits in a table tab: cell updates to existing rows, new rows and
// deleted rows, all immutable so undo / redo is just a stack of snapshots. Rows are identified by
// their primary-key values, so pending changes survive paging, sorting and filtering.
import type { Cell } from "../../ipc/types";
import { buildDelete, buildInsert, buildUpdate, cellEquals, quoteLiteral, type TableTarget } from "./sqlGen";

export type RowKey = string;

export interface UpdateEntry {
  key: RowKey;
  /** Original primary-key values (the WHERE clause), column name -> value. */
  pk: Record<string, Cell>;
  /** Changed columns only. */
  set: Record<string, Cell>;
}

export interface InsertEntry {
  id: string;
  /** Only the columns the user filled in; everything else takes its DEFAULT. */
  values: Record<string, Cell>;
}

export interface DeleteEntry {
  key: RowKey;
  pk: Record<string, Cell>;
}

export interface PendingState {
  updates: Readonly<Record<RowKey, UpdateEntry>>;
  inserts: readonly InsertEntry[];
  deletes: Readonly<Record<RowKey, DeleteEntry>>;
}

export const EMPTY_PENDING: PendingState = { updates: {}, inserts: [], deletes: {} };

/** Stable identity of a row from its primary-key values, in primary-key column order. */
export function rowKeyOf(pkColumns: readonly string[], pk: Readonly<Record<string, Cell>>): RowKey {
  return JSON.stringify(pkColumns.map((c) => pk[c] ?? null));
}

export interface PendingCounts {
  updates: number;
  inserts: number;
  deletes: number;
  /** Statements that will run (one per changed row). */
  total: number;
  /** Individual changed cells across updates and inserts. */
  cells: number;
}

export function pendingCounts(p: PendingState): PendingCounts {
  const updates = Object.keys(p.updates).length;
  const deletes = Object.keys(p.deletes).length;
  const inserts = p.inserts.length;
  let cells = 0;
  for (const u of Object.values(p.updates)) cells += Object.keys(u.set).length;
  for (const i of p.inserts) cells += Object.keys(i.values).length;
  return { updates, inserts, deletes, total: updates + inserts + deletes, cells };
}

export const hasPending = (p: PendingState): boolean => pendingCounts(p).total > 0;

/**
 * Set a cell of an existing row. Setting it back to its original value removes the change (and the
 * whole update when nothing else changed). Edits to a row marked for deletion are ignored.
 */
export function setCell(
  p: PendingState,
  key: RowKey,
  pk: Record<string, Cell>,
  column: string,
  value: Cell,
  original: Cell,
): PendingState {
  if (p.deletes[key]) return p;
  const current = p.updates[key];
  const set = { ...(current?.set ?? {}) };
  if (cellEquals(value, original)) delete set[column];
  else set[column] = value;
  const updates = { ...p.updates };
  if (Object.keys(set).length === 0) delete updates[key];
  else updates[key] = { key, pk: current?.pk ?? pk, set };
  return { ...p, updates };
}

/** Add a blank row to be inserted. */
export function addInsert(p: PendingState, id: string, values: Record<string, Cell> = {}): PendingState {
  return { ...p, inserts: [...p.inserts, { id, values }] };
}

/** Set, or with `undefined` clear back to DEFAULT, one cell of a new row. */
export function setInsertCell(p: PendingState, id: string, column: string, value: Cell | undefined): PendingState {
  return {
    ...p,
    inserts: p.inserts.map((i) => {
      if (i.id !== id) return i;
      const values = { ...i.values };
      if (value === undefined) delete values[column];
      else values[column] = value;
      return { ...i, values };
    }),
  };
}

export const removeInsert = (p: PendingState, id: string): PendingState => ({ ...p, inserts: p.inserts.filter((i) => i.id !== id) });

/** Mark an existing row for deletion, or unmark it. Marking drops that row's pending cell edits. */
export function toggleDelete(p: PendingState, key: RowKey, pk: Record<string, Cell>): PendingState {
  if (p.deletes[key]) {
    const deletes = { ...p.deletes };
    delete deletes[key];
    return { ...p, deletes };
  }
  const updates = { ...p.updates };
  delete updates[key];
  return { ...p, updates, deletes: { ...p.deletes, [key]: { key, pk } } };
}

/** Revert every change made to one existing row (cell edits and deletion mark). */
export function revertRow(p: PendingState, key: RowKey): PendingState {
  const updates = { ...p.updates };
  const deletes = { ...p.deletes };
  delete updates[key];
  delete deletes[key];
  return { ...p, updates, deletes };
}

export interface ApplyScript {
  /** `BEGIN; ... COMMIT;`, one statement per line. */
  sql: string;
  /** Just the data-changing statements, in execution order. */
  statements: string[];
  counts: PendingCounts;
}

/**
 * Wraps a keyed UPDATE/DELETE so the whole transaction aborts if it matches no row (someone else
 * changed or deleted it since it was loaded). Without this, a stale edit would silently commit the
 * rest of the batch. Uses a PL/pgSQL DO block: `FOUND` is false when the statement touched 0 rows.
 */
export function guardKeyedStatement(stmt: string, changeNo: number, target: TableTarget): string {
  // RAISE's format string treats % as a placeholder (so % is doubled); the message itself is built
  // with quoteLiteral, which doubles quotes and switches to E'...' with doubled backslashes when needed.
  const where = `${target.schema}.${target.name}`.replace(/%/g, "%%");
  const msg = quoteLiteral(`VibeLens: change ${changeNo} matched no row in ${where} (it was changed or deleted elsewhere). Nothing was applied.`);
  // The dollar tag must not occur in the statement or in the message, whose text comes from names.
  let tag = "$vg$";
  for (let n = 1; stmt.includes(tag) || msg.includes(tag); n++) tag = `$vg${n}$`;
  // `use_column`: a primary-key column called `found` (or any other PL/pgSQL variable name) must
  // resolve to the column inside the statement, not raise "column reference is ambiguous".
  return `DO ${tag}\n#variable_conflict use_column\nBEGIN\n  ${stmt}\n  IF NOT FOUND THEN RAISE EXCEPTION ${msg}; END IF;\nEND ${tag};`;
}

/**
 * The exact script that applies the pending changes: UPDATEs, then INSERTs, then DELETEs, in one
 * transaction. Keyed UPDATEs/DELETEs are guarded so a row that vanished aborts everything.
 */
export function buildApplyScript(target: TableTarget, p: PendingState): ApplyScript {
  const statements: string[] = [];
  let n = 0;
  for (const u of Object.values(p.updates)) statements.push(guardKeyedStatement(buildUpdate(target, u.pk, u.set), ++n, target));
  for (const i of p.inserts) statements.push((++n, buildInsert(target, i.values)));
  for (const d of Object.values(p.deletes)) statements.push(guardKeyedStatement(buildDelete(target, d.pk), ++n, target));
  return { sql: ["BEGIN;", ...statements, "COMMIT;"].join("\n"), statements, counts: pendingCounts(p) };
}

/**
 * What the query history stores for a table-editor apply: counts only, never the script, whose
 * UPDATE / INSERT statements carry the edited cell values. `-- Table edit: 3 updates, 1 insert in shop.orders`.
 */
export function describeTableEdit(counts: Pick<PendingCounts, "updates" | "inserts" | "deletes">, schema: string, name: string): string {
  const part = (n: number, one: string, many: string) => (n > 0 ? [`${n} ${n === 1 ? one : many}`] : []);
  const parts = [...part(counts.updates, "update", "updates"), ...part(counts.inserts, "insert", "inserts"), ...part(counts.deletes, "delete", "deletes")];
  // Names are user data too (and can contain newlines): keep the summary on one comment line.
  const where = `${schema}.${name}`.replace(/[\r\n]+/g, " ");
  return `-- Table edit: ${parts.length > 0 ? parts.join(", ") : "no changes"} in ${where}`;
}

export interface MissingRequired {
  /** 1-based position among the new rows. */
  row: number;
  columns: string[];
}

/** New rows that leave a NOT NULL column without a default empty: the database would reject them. */
export function missingRequired(
  columns: readonly { name: string; nullable: boolean; default: string | null }[],
  inserts: readonly InsertEntry[],
  generated: (name: string) => boolean = () => false,
): MissingRequired[] {
  const required = columns.filter((c) => !c.nullable && !c.default && !generated(c.name)).map((c) => c.name);
  const out: MissingRequired[] = [];
  inserts.forEach((ins, i) => {
    const missing = required.filter((n) => !(n in ins.values));
    if (missing.length > 0) out.push({ row: i + 1, columns: missing });
  });
  return out;
}
