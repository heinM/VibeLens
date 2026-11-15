// Merges the rows loaded from the database with the unsaved edits, for display in the grid.
// New rows are pinned to the top of the grid so they are visible without scrolling.
import type { Cell } from "../../ipc/types";
import { rowKeyOf, type PendingState, type RowKey } from "./pendingChanges";

export type RowMeta =
  | { kind: "new"; id: string }
  | {
      kind: "row";
      /** Index into the loaded rows. */
      loaded: number;
      /** Primary-key identity; null when the table has none (read-only). */
      key: RowKey | null;
      pk: Record<string, Cell> | null;
      deleted: boolean;
      /** Names of columns with pending edits. */
      edited: ReadonlySet<string>;
    };

export interface ComposedRows {
  rows: Cell[][];
  meta: RowMeta[];
  /** Number of pinned new rows at the top. */
  newCount: number;
}

const NONE: ReadonlySet<string> = new Set();

export function composeRows(columnNames: readonly string[], pkNames: readonly string[], loaded: readonly Cell[][], pending: PendingState): ComposedRows {
  const rows: Cell[][] = [];
  const meta: RowMeta[] = [];
  for (const ins of pending.inserts) {
    rows.push(columnNames.map((n) => ins.values[n] ?? null));
    meta.push({ kind: "new", id: ins.id });
  }
  const pkIdx = pkNames.map((n) => columnNames.indexOf(n));
  const canKey = pkNames.length > 0 && pkIdx.every((i) => i >= 0);
  for (let i = 0; i < loaded.length; i++) {
    const row = loaded[i];
    if (!canKey) {
      rows.push(row as Cell[]);
      meta.push({ kind: "row", loaded: i, key: null, pk: null, deleted: false, edited: NONE });
      continue;
    }
    const pk: Record<string, Cell> = {};
    pkNames.forEach((n, k) => (pk[n] = row[pkIdx[k]] ?? null));
    const key = rowKeyOf(pkNames, pk);
    const upd = pending.updates[key];
    let shown = row as Cell[];
    let edited = NONE;
    if (upd) {
      shown = row.slice();
      const set = new Set<string>();
      for (const [name, v] of Object.entries(upd.set)) {
        const ci = columnNames.indexOf(name);
        if (ci >= 0) {
          shown[ci] = v;
          set.add(name);
        }
      }
      edited = set;
    }
    rows.push(shown);
    meta.push({ kind: "row", loaded: i, key, pk, deleted: !!pending.deletes[key], edited });
  }
  return { rows, meta, newCount: pending.inserts.length };
}
