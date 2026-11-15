// SQL text helpers for Iceberg tables. Identifiers follow the same rule as the Postgres tree: bare when
// they are plain lower-case words, double-quoted (with "" escaping) otherwise.
const BARE = /^[a-z_][a-z0-9_]*$/;
const RESERVED = new Set([
  "all", "and", "any", "as", "asc", "between", "by", "case", "cast", "create", "cross", "current", "delete", "desc", "distinct", "drop", "else", "end", "except", "exists",
  "false", "from", "full", "group", "having", "in", "inner", "insert", "intersect", "into", "is", "join", "left", "like", "limit", "not", "null", "offset", "on", "or", "order",
  "outer", "right", "select", "set", "table", "then", "to", "true", "union", "update", "user", "using", "values", "when", "where", "with",
]);

export const quoteAlways = (s: string): string => `"${s.replace(/"/g, '""')}"`;
export const quoteIfNeeded = (s: string): string => (BARE.test(s) && !RESERVED.has(s) ? s : quoteAlways(s));

/** `shop.orders`, `lake."Sales".orders` */
export const qualifiedName = (namespace: string[], name: string): string => [...namespace, name].map(quoteIfNeeded).join(".");

export const selectAllSql = (namespace: string[], name: string, limit = 100): string => `SELECT * FROM ${qualifiedName(namespace, name)} LIMIT ${limit};`;

/**
 * Time travel with the local engine's `table@snapshotId` syntax. Always quoted, because `@` is not an
 * identifier character. The id stays a string end to end: snapshot ids exceed 2^53.
 */
export function selectAsOfSnapshotSql(namespace: string[], name: string, snapshotId: string, limit = 100): string {
  const parts = [...namespace.map(quoteAlways), quoteAlways(`${name}@${snapshotId}`)];
  return `SELECT * FROM ${parts.join(".")} LIMIT ${limit};`;
}
