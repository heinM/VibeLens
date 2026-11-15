// What the agent's "last result" chip is told about the query behind a result set. Pure, so the
// rule (the statement's own SQL, else the script, shortened) is tested.

export const PUBLISH_SQL_MAX = 2000;
const MARK = "\n-- …truncated";

/**
 * The statement that produced a result set when the runner could tell (its splitter agreed with the
 * server on statement boundaries), otherwise the script as a whole. Either is cut at 2,000 characters.
 */
export function sqlForPublish(statementSql: string | null | undefined, scriptSql: string | null | undefined): string | null {
  const pick = statementSql && statementSql.trim() ? statementSql.trim() : scriptSql && scriptSql.trim() ? scriptSql.trim() : null;
  if (pick === null) return null;
  return pick.length > PUBLISH_SQL_MAX ? pick.slice(0, PUBLISH_SQL_MAX) + MARK : pick;
}

/** Statement N of a run is publishable once it finished: statements finish in order, so this walks a moving pointer. */
export function nextToPublish<T extends { status: string }>(results: readonly T[], from: number): { items: T[]; next: number } {
  const items: T[] = [];
  let i = from;
  while (i < results.length && results[i].status !== "running") {
    items.push(results[i]);
    i++;
  }
  return { items, next: i };
}

/** True when a run with this many statements gets one "Statement N of M" picker instead of a tab per statement. */
export const COLLAPSE_TABS_ABOVE = 50;
export const collapseTabs = (statementCount: number): boolean => statementCount > COLLAPSE_TABS_ABOVE;
