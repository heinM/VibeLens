// SQL text helpers for Iceberg tables. Identifiers follow the same rule as the Postgres tree: bare when
// they are plain lower-case words, quoted otherwise: with double quotes ("" escaping) for DataFusion,
// Athena and Trino, with backticks (`` escaping) for Spark SQL, where "x" is a string literal.
// Trino and Athena read a nested namespace as one schema, so `["analytics","daily"]` is written
// `"analytics.daily"` there (level by level it would be catalog.schema).
import { nestedNamespaceIsOneIdent, quoteIdentFor, type SqlDialect } from "../../lib/sqlIntel/dialect";

const BARE = /^[a-z_][a-z0-9_]*$/;
const RESERVED = new Set([
  "all", "and", "any", "as", "asc", "between", "by", "case", "cast", "create", "cross", "current", "delete", "desc", "distinct", "drop", "else", "end", "except", "exists",
  "false", "from", "full", "group", "having", "in", "inner", "insert", "intersect", "into", "is", "join", "left", "like", "limit", "not", "null", "offset", "on", "or", "order",
  "outer", "right", "select", "set", "table", "then", "to", "true", "union", "update", "user", "using", "values", "when", "where", "with",
]);

export const quoteAlways = (s: string, dialect?: SqlDialect | null): string => quoteIdentFor(s, dialect);
export const quoteIfNeeded = (s: string, dialect?: SqlDialect | null): string => (BARE.test(s) && !RESERVED.has(s) ? s : quoteAlways(s, dialect));

/** A namespace as SQL: `lake.sales` level by level, or `"lake.sales"` as one schema on Trino / Athena. */
export function namespaceSql(namespace: string[], dialect?: SqlDialect | null): string {
  if (namespace.length > 1 && nestedNamespaceIsOneIdent(dialect)) return quoteAlways(namespace.join("."), dialect);
  return namespace.map((p) => quoteIfNeeded(p, dialect)).join(".");
}

/** `shop.orders`, `lake."Sales".orders` (Spark: ``lake.`Sales`.orders``; Trino: `"lake.sales".orders`) */
export const qualifiedName = (namespace: string[], name: string, dialect?: SqlDialect | null): string =>
  namespace.length ? `${namespaceSql(namespace, dialect)}.${quoteIfNeeded(name, dialect)}` : quoteIfNeeded(name, dialect);

export const selectAllSql = (namespace: string[], name: string, limit = 100, dialect?: SqlDialect | null): string => `SELECT * FROM ${qualifiedName(namespace, name, dialect)} LIMIT ${limit};`;

/**
 * Time travel to a snapshot, in the engine's own syntax. The local engine reads `"table@snapshotId"`
 * (always quoted, because `@` is not an identifier character); Athena and Trino write
 * `FOR VERSION AS OF id`, Spark `VERSION AS OF id`. The id stays a string end to end: snapshot ids
 * exceed 2^53.
 */
export function selectAsOfSnapshotSql(namespace: string[], name: string, snapshotId: string, limit = 100, dialect?: SqlDialect | null): string {
  const id = /^-?\d+$/.test(snapshotId) ? snapshotId : null;
  if (id && (dialect === "athena" || dialect === "trino")) return `SELECT * FROM ${qualifiedName(namespace, name, dialect)} FOR VERSION AS OF ${id} LIMIT ${limit};`;
  if (id && dialect === "spark") return `SELECT * FROM ${qualifiedName(namespace, name, dialect)} VERSION AS OF ${id} LIMIT ${limit};`;
  // The local engine's form (also the fallback for an id that is not a plain integer).
  const ns = nestedNamespaceIsOneIdent(dialect) && namespace.length > 1 ? [quoteAlways(namespace.join("."))] : namespace.map((p) => quoteAlways(p));
  return `SELECT * FROM ${[...ns, quoteAlways(`${name}@${snapshotId}`)].join(".")} LIMIT ${limit};`;
}
