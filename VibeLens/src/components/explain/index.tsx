// Public surface of the explain module. The grid renders <PlanView> for `EXPLAIN (FORMAT JSON)` results.
import type { Cell, ColumnMeta } from "../../ipc/types";
import { looksLikePlan } from "./planModel";

export { PlanView } from "./PlanView";
export type { PlanViewProps } from "./PlanView";

/** True when a result set is the output of `EXPLAIN (FORMAT JSON …)`. */
export function isExplainJson(columns: ColumnMeta[], rows: Cell[][]): boolean {
  return columns.length === 1 && columns[0].name === "QUERY PLAN" && rows.length === 1 && rows[0].length === 1 && looksLikePlan(rows[0][0]);
}
