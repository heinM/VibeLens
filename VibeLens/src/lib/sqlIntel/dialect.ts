// SQL dialects the editor can complete for. Postgres is the default; the Iceberg query engines
// have their own function vocabulary and a few dialect-only constructs (time travel, metadata
// tables). Pure data plus small lookups, so it is unit-tested without Monaco.
//
//  * trino    = Iceberg through the user's Trino. Same vocabulary as Athena (Athena is Trino).
//  * spark    = Iceberg through Spark SQL on Livy. Its own small table: Spark spells time travel
//               `VERSION AS OF` and reaches metadata tables as `table.snapshots`, not `"table$snapshots"`.
//  * mysql    = MySQL 8 (and Aurora / RDS MySQL). Backtick identifiers, `#` comments, its own function
//               vocabulary; the catalog comes from my_schema_metadata (schema = database).
//  * redshift = Amazon Redshift. Completes like Postgres (Redshift's SQL is Postgres-derived), but its
//               catalog comes from the Redshift Data API, not from pg_catalog.
import type { ConnectionProfile } from "../../ipc/types";
import { FUNCTIONS, SNIPPETS, STATEMENT_START_KEYWORDS, TYPE_NAMES, type FunctionDef, type SnippetDef } from "./keywords";

/** `datafusion` = Iceberg through the embedded local engine; `athena` = Iceberg through Amazon Athena (Trino). */
export type SqlDialect = "postgres" | "datafusion" | "athena" | "trino" | "spark" | "redshift" | "mysql";

export const isIcebergDialect = (d: SqlDialect | null | undefined): boolean => d === "datafusion" || d === "athena" || d === "trino" || d === "spark";

/** Athena and Trino share a vocabulary (functions, types, time travel, `"table$snapshots"`). */
export const isTrinoFamily = (d: SqlDialect | null | undefined): boolean => d === "athena" || d === "trino";

/**
 * Quotes an identifier the way the dialect reads one: backticks (a backtick doubled) for Spark SQL,
 * where `"x"` is a string literal (`SELECT "user" FROM t` would return the text on every row); double
 * quotes (a double quote doubled) everywhere else. Shared by completion, the palette and the Iceberg
 * SQL helpers so they can never disagree.
 */
export function quoteIdentFor(name: string, dialect?: SqlDialect | null): string {
  return dialect === "spark" || dialect === "mysql" ? `\`${name.replace(/`/g, "``")}\`` : `"${name.replace(/"/g, '""')}"`;
}

/**
 * Trino (and Athena) address a nested Iceberg namespace as ONE schema: `"analytics.daily".orders`.
 * Written level by level (`analytics.daily.orders`) Trino reads catalog.schema.table.
 */
export const nestedNamespaceIsOneIdent = (d: SqlDialect | null | undefined): boolean => isTrinoFamily(d);

/** The catalog family a dialect reads its schema from. */
export const catalogEngineOf = (d: SqlDialect | null | undefined): "postgres" | "iceberg" | "redshift" | "mysql" =>
  isIcebergDialect(d) ? "iceberg" : d === "redshift" ? "redshift" : d === "mysql" ? "mysql" : "postgres";

/** The dialect reads `#` as the start of a line comment (MySQL). */
export const hashComments = (d: SqlDialect | null | undefined): boolean => d === "mysql";

/** Which dialect SQL tabs on this profile are written in. Valkey has none (null). */
export function dialectOf(profile: Pick<ConnectionProfile, "engine" | "iceberg"> | null | undefined): SqlDialect | null {
  if (!profile) return null;
  switch (profile.engine) {
    case "postgres":
      return "postgres";
    case "redshift":
      return "redshift";
    case "mysql":
      return "mysql";
    case "iceberg": {
      const k = profile.iceberg?.queryEngine.kind;
      return k === "athena" ? "athena" : k === "trino" ? "trino" : k === "spark" ? "spark" : "datafusion";
    }
    case "valkey":
      return null;
  }
}

/** The engine name a dialect runs on, as the `stats` event spells it (`engine` field). */
export const DIALECT_ENGINE: Record<SqlDialect, string> = {
  postgres: "postgres",
  datafusion: "datafusion",
  athena: "athena",
  trino: "trino",
  spark: "spark",
  redshift: "redshift",
  mysql: "mysql",
};

/** How the dialect is named to people and to the AI assistant. */
export const DIALECT_LABEL: Record<SqlDialect, string> = {
  postgres: "PostgreSQL",
  datafusion: "Apache DataFusion SQL",
  athena: "Amazon Athena (Trino) SQL",
  trino: "Trino SQL",
  spark: "Apache Spark SQL",
  redshift: "Amazon Redshift SQL",
  mysql: "MySQL 8 SQL",
};

/** One sentence for the AI prompts: what the engine can and cannot do. */
export const DIALECT_AI_NOTE: Record<SqlDialect, string> = {
  postgres: "",
  datafusion:
    "The tables are Apache Iceberg tables queried through the embedded DataFusion engine. Namespaces play the role of schemas (a.b.table). INSERT INTO works; UPDATE, DELETE and MERGE are not supported. Time travel is written as \"table@<snapshot_id>\" (quoted).",
  athena:
    "The tables are Apache Iceberg tables queried through Amazon Athena (Trino dialect). Namespaces play the role of schemas. Time travel is written as FOR VERSION AS OF <snapshot_id> or FOR TIMESTAMP AS OF TIMESTAMP '...'. Athena bills by data scanned, so keep scans narrow.",
  trino:
    "The tables are Apache Iceberg tables queried through Trino. Namespaces play the role of schemas. Time travel is written as FOR VERSION AS OF <snapshot_id> or FOR TIMESTAMP AS OF TIMESTAMP '...'. Each statement runs on its own: USE, SET SESSION and transactions are not available.",
  spark:
    "The tables are Apache Iceberg tables queried through Spark SQL (Apache Livy). Namespaces play the role of schemas; quote identifiers with backticks. Time travel is written as VERSION AS OF <snapshot_id> or TIMESTAMP AS OF '...'; metadata tables are table.snapshots, table.history, table.files. Read-only runs refuse reflect/java_method and direct file reads such as parquet.`path`.",
  redshift:
    "The database is Amazon Redshift, queried through the Redshift Data API. The SQL is Redshift's Postgres-derived dialect: no RETURNING, no arrays or JSONB (use SUPER and PartiQL), no CREATE INDEX (use sort and distribution keys). Each statement commits on its own: BEGIN, COMMIT and ROLLBACK are refused. System views are SVV_*, SYS_* and STL_*.",
  mysql:
    "The database is MySQL 8. Quote identifiers with backticks, use LIMIT n, and remember a schema is a database (db.table). There is no RETURNING, no FULL OUTER JOIN, no ILIKE and no :: casts (use CAST). Each statement runs on its own; DELIMITER is not needed.",
};

// -- functions ------------------------------------------------------------------------------------

const fn = (name: string, signature: string, doc: string, agg = false): FunctionDef => ({ name, signature, doc, ...(agg ? { agg: true } : {}) });

/** Postgres-only entries of the base list that DataFusion and Trino do not have (or spell differently). */
const NOT_ICEBERG = new Set(["json_agg", "jsonb_agg", "jsonb_build_object", "jsonb_array_elements", "gen_random_uuid", "age"]);
/** Also missing from Trino: Athena has `listagg`, `date_format`, `sequence`, and `md5` only takes varbinary. */
const NOT_TRINO = new Set(["string_agg", "bool_and", "bool_or", "to_char", "to_date", "to_timestamp", "generate_series", "md5"]);

const DATAFUSION_EXTRA: readonly FunctionDef[] = [
  fn("approx_distinct", "(expr)", "Approximate number of distinct values (HyperLogLog).", true),
  fn("approx_percentile_cont", "(expr, percentile)", "Approximate percentile of a continuous distribution.", true),
  fn("approx_median", "(expr)", "Approximate median.", true),
  fn("median", "(expr)", "Median of the values.", true),
  fn("stddev", "(expr)", "Sample standard deviation.", true),
  fn("corr", "(y, x)", "Correlation coefficient.", true),
  fn("first_value", "(expr)", "Window or aggregate: first value.", true),
  fn("last_value", "(expr)", "Window or aggregate: last value.", true),
  fn("ntile", "(n)", "Window: bucket number when rows are split into n groups.", true),
  fn("date_bin", "(interval, source, origin)", "Bins a timestamp into fixed-size intervals aligned to an origin."),
  fn("date_part", "(field, source)", "Extracts a date/time field, e.g. 'year'."),
  fn("make_date", "(year, month, day)", "Builds a date."),
  fn("to_unixtime", "(timestamp)", "Seconds since the Unix epoch."),
  fn("from_unixtime", "(seconds)", "Timestamp from Unix seconds."),
  fn("array_length", "(array)", "Number of elements in an array."),
  fn("array_has", "(array, element)", "True if the array contains the element."),
  fn("array_element", "(array, index)", "Element at a 1-based index."),
  fn("get_field", "(struct, name)", "Field of a struct by name."),
  fn("named_struct", "(name, value)", "Builds a struct from name/value pairs."),
  fn("map_keys", "(map)", "Keys of a map."),
  fn("map_values", "(map)", "Values of a map."),
  fn("starts_with", "(text, prefix)", "True if text starts with the prefix."),
  fn("ends_with", "(text, suffix)", "True if text ends with the suffix."),
  fn("regexp_like", "(text, pattern)", "True if the regular expression matches."),
  fn("strpos", "(text, substring)", "1-based position of a substring (0 if absent)."),
  fn("lpad", "(text, length, fill)", "Left-pads to a length."),
  fn("rpad", "(text, length, fill)", "Right-pads to a length."),
  fn("initcap", "(text)", "Capitalises each word."),
  fn("uuid", "()", "Random v4 UUID."),
  fn("arrow_typeof", "(expr)", "Arrow data type of an expression."),
];

const TRINO_EXTRA: readonly FunctionDef[] = [
  fn("approx_distinct", "(expr)", "Approximate number of distinct values (HyperLogLog).", true),
  fn("approx_percentile", "(expr, percentage)", "Approximate percentile.", true),
  fn("arbitrary", "(expr)", "Any non-null value of the group.", true),
  fn("count_if", "(condition)", "Number of rows where the condition is true.", true),
  fn("max_by", "(value, key)", "Value at the row with the largest key.", true),
  fn("min_by", "(value, key)", "Value at the row with the smallest key.", true),
  fn("listagg", "(expr, separator)", "Concatenates values with a separator.", true),
  fn("map_agg", "(key, value)", "Aggregates pairs into a map.", true),
  fn("histogram", "(expr)", "Map of value to its count.", true),
  fn("every", "(condition)", "True if the condition holds for all rows.", true),
  fn("date_add", "(unit, value, timestamp)", "Adds an interval to a timestamp."),
  fn("date_diff", "(unit, from, to)", "Difference between two timestamps in a unit."),
  fn("date_format", "(timestamp, format)", "Formats a timestamp as text (MySQL-style format)."),
  fn("date_parse", "(text, format)", "Parses text into a timestamp."),
  fn("format_datetime", "(timestamp, format)", "Formats a timestamp (Joda format)."),
  fn("parse_datetime", "(text, format)", "Parses text into a timestamp with time zone."),
  fn("from_unixtime", "(seconds)", "Timestamp from Unix seconds."),
  fn("to_unixtime", "(timestamp)", "Seconds since the Unix epoch."),
  fn("at_timezone", "(timestamp, zone)", "Converts a timestamp to another time zone."),
  fn("cardinality", "(array_or_map)", "Number of elements."),
  fn("element_at", "(array_or_map, key)", "Element at a 1-based index or map key (NULL if absent)."),
  fn("contains", "(array, element)", "True if the array contains the element."),
  fn("transform", "(array, x -> expr)", "Applies a lambda to every element."),
  fn("filter", "(array, x -> condition)", "Keeps elements for which the lambda is true."),
  fn("reduce", "(array, initial, (s, x) -> s, s -> out)", "Folds an array with a lambda."),
  fn("flatten", "(array_of_arrays)", "Flattens one level of nesting."),
  fn("sequence", "(start, stop)", "Array of values from start to stop."),
  fn("json_extract_scalar", "(json, path)", "Scalar at a JSONPath, as text."),
  fn("json_extract", "(json, path)", "JSON at a JSONPath."),
  fn("json_parse", "(text)", "Parses text into JSON."),
  fn("json_format", "(json)", "JSON as text."),
  fn("regexp_like", "(text, pattern)", "True if the regular expression matches."),
  fn("regexp_extract", "(text, pattern)", "First match of a regular expression."),
  fn("split", "(text, delimiter)", "Splits text into an array."),
  fn("strpos", "(text, substring)", "1-based position of a substring (0 if absent)."),
  fn("lpad", "(text, length, fill)", "Left-pads to a length."),
  fn("rpad", "(text, length, fill)", "Right-pads to a length."),
  fn("reverse", "(text)", "Reverses text."),
  fn("typeof", "(expr)", "Type of an expression."),
  fn("try", "(expr)", "NULL instead of an error when the expression fails."),
  fn("try_cast", "(expr AS type)", "Cast that yields NULL instead of failing."),
  fn("rand", "()", "Random value in [0, 1)."),
  fn("uuid", "()", "Random v4 UUID."),
  fn("to_iso8601", "(date_or_timestamp)", "ISO 8601 text."),
];

/** Postgres-only names MySQL does not have. */
const NOT_MYSQL = new Set(["string_agg", "array_agg", "json_agg", "jsonb_agg", "jsonb_build_object", "jsonb_array_elements", "bool_and", "bool_or", "to_char", "to_date", "to_timestamp", "generate_series", "gen_random_uuid", "age", "date_trunc", "now", "nullif"]);

const MYSQL_EXTRA: readonly FunctionDef[] = [
  fn("group_concat", "(expr ORDER BY … SEPARATOR ',')", "Concatenates the group's values.", true),
  fn("json_arrayagg", "(expr)", "The group's values as a JSON array.", true),
  fn("json_objectagg", "(key, value)", "The group's pairs as a JSON object.", true),
  fn("any_value", "(expr)", "Any value of the group (silences ONLY_FULL_GROUP_BY).", true),
  fn("now", "()", "Current date and time."),
  fn("curdate", "()", "Current date."),
  fn("date_format", "(date, format)", "Formats a date ('%Y-%m-%d')."),
  fn("str_to_date", "(text, format)", "Parses text into a date or datetime."),
  fn("date_add", "(date, INTERVAL n unit)", "Adds an interval."),
  fn("date_sub", "(date, INTERVAL n unit)", "Subtracts an interval."),
  fn("datediff", "(end, start)", "Days between two dates."),
  fn("timestampdiff", "(unit, start, end)", "Difference in a unit."),
  fn("unix_timestamp", "(datetime)", "Seconds since the Unix epoch."),
  fn("from_unixtime", "(seconds)", "Datetime from Unix seconds."),
  fn("ifnull", "(expr, fallback)", "Fallback when the value is NULL."),
  fn("if", "(condition, then, else)", "Inline conditional."),
  fn("nullif", "(a, b)", "NULL when a = b."),
  fn("concat_ws", "(separator, a, b, …)", "Concatenates with a separator, skipping NULLs."),
  fn("substring_index", "(text, delimiter, count)", "Part before (or after) the nth delimiter."),
  fn("locate", "(substring, text)", "1-based position of a substring (0 if absent)."),
  fn("instr", "(text, substring)", "1-based position of a substring (0 if absent)."),
  fn("lpad", "(text, length, fill)", "Left-pads to a length."),
  fn("rpad", "(text, length, fill)", "Right-pads to a length."),
  fn("json_extract", "(json, path)", "Value at a JSON path ('$.a.b'); also written json->'$.a'."),
  fn("json_unquote", "(json)", "JSON string as text; json->>'$.a' combines both."),
  fn("json_object", "(key, value, …)", "Builds a JSON object."),
  fn("json_array", "(value, …)", "Builds a JSON array."),
  fn("json_contains", "(json, candidate, path)", "True if the document contains the candidate."),
  fn("json_table", "(json, path COLUMNS (…))", "Rows from a JSON document."),
  fn("regexp_like", "(text, pattern)", "True if the regular expression matches."),
  fn("regexp_replace", "(text, pattern, replacement)", "Replaces regular expression matches."),
  fn("uuid", "()", "Random UUID text."),
  fn("found_rows", "()", "Rows the previous SELECT would have returned without LIMIT."),
  fn("database", "()", "The current database."),
  fn("version", "()", "Server version."),
];

/** Postgres names Spark SQL does not have. */
const NOT_SPARK = new Set(["string_agg", "bool_and", "bool_or", "to_char", "generate_series"]);

const SPARK_EXTRA: readonly FunctionDef[] = [
  fn("approx_count_distinct", "(expr)", "Approximate number of distinct values (HyperLogLog++).", true),
  fn("percentile_approx", "(expr, percentage)", "Approximate percentile.", true),
  fn("collect_list", "(expr)", "Values of the group as an array, with duplicates.", true),
  fn("collect_set", "(expr)", "Distinct values of the group as an array.", true),
  fn("count_if", "(condition)", "Number of rows where the condition is true.", true),
  fn("max_by", "(value, key)", "Value at the row with the largest key.", true),
  fn("min_by", "(value, key)", "Value at the row with the smallest key.", true),
  fn("first", "(expr)", "First value of the group.", true),
  fn("last", "(expr)", "Last value of the group.", true),
  fn("date_format", "(timestamp, pattern)", "Formats a timestamp as text (Java pattern)."),
  fn("date_add", "(date, days)", "Adds days to a date."),
  fn("datediff", "(end, start)", "Days between two dates."),
  fn("date_trunc", "(unit, timestamp)", "Truncates a timestamp to a unit."),
  fn("from_unixtime", "(seconds)", "Text timestamp from Unix seconds."),
  fn("unix_timestamp", "(timestamp)", "Seconds since the Unix epoch."),
  fn("to_date", "(text, pattern)", "Parses text into a date."),
  fn("to_timestamp", "(text, pattern)", "Parses text into a timestamp."),
  fn("size", "(array_or_map)", "Number of elements."),
  fn("element_at", "(array_or_map, key)", "Element at a 1-based index or map key."),
  fn("array_contains", "(array, value)", "True if the array contains the value."),
  fn("explode", "(array_or_map)", "One row per element."),
  fn("get_json_object", "(json, path)", "Value at a JSONPath, as text."),
  fn("from_json", "(json, schema)", "Parses JSON into a struct."),
  fn("to_json", "(expr)", "JSON text of a struct, array or map."),
  fn("named_struct", "(name, value)", "Builds a struct from name/value pairs."),
  fn("map_keys", "(map)", "Keys of a map."),
  fn("map_values", "(map)", "Values of a map."),
  fn("regexp_extract", "(text, pattern, group)", "A group of the first regular expression match."),
  fn("split", "(text, pattern)", "Splits text into an array."),
  fn("instr", "(text, substring)", "1-based position of a substring (0 if absent)."),
  fn("lpad", "(text, length, fill)", "Left-pads to a length."),
  fn("rpad", "(text, length, fill)", "Right-pads to a length."),
  fn("nvl", "(expr, fallback)", "Fallback when the value is NULL."),
  fn("typeof", "(expr)", "Type of an expression."),
  fn("uuid", "()", "Random v4 UUID."),
];

const unique = (list: readonly FunctionDef[]): FunctionDef[] => {
  const seen = new Set<string>();
  return list.filter((f) => (seen.has(f.name) ? false : (seen.add(f.name), true)));
};

const FUNCTION_CACHE = new Map<SqlDialect, readonly FunctionDef[]>();

/** The functions to offer in expressions for a dialect. */
export function functionsFor(d: SqlDialect | null | undefined): readonly FunctionDef[] {
  const key = d ?? "postgres";
  const hit = FUNCTION_CACHE.get(key);
  if (hit) return hit;
  let out: readonly FunctionDef[];
  if (key === "datafusion") out = unique([...FUNCTIONS.filter((f) => !NOT_ICEBERG.has(f.name)), ...DATAFUSION_EXTRA]);
  else if (isTrinoFamily(key)) out = unique([...FUNCTIONS.filter((f) => !NOT_ICEBERG.has(f.name) && !NOT_TRINO.has(f.name)), ...TRINO_EXTRA]);
  else if (key === "spark") out = unique([...FUNCTIONS.filter((f) => !NOT_ICEBERG.has(f.name) && !NOT_SPARK.has(f.name)), ...SPARK_EXTRA]);
  else if (key === "mysql") out = unique([...MYSQL_EXTRA, ...FUNCTIONS.filter((f) => !NOT_MYSQL.has(f.name))]);
  // Redshift: the Postgres list (design: Postgres dialect for editor intelligence on Redshift).
  else out = FUNCTIONS;
  FUNCTION_CACHE.set(key, out);
  return out;
}

// -- types, statements, snippets ---------------------------------------------------------------

const DATAFUSION_TYPES = ["bigint", "int", "smallint", "double", "float", "decimal(38, 10)", "boolean", "string", "varchar", "date", "timestamp", "timestamp with time zone", "binary", "array", "struct"];
const SPARK_TYPES = ["bigint", "int", "smallint", "tinyint", "double", "float", "decimal(38, 10)", "boolean", "string", "varchar(255)", "date", "timestamp", "timestamp_ntz", "binary", "array<string>", "map<string, bigint>", "struct<a: bigint>"];
const ATHENA_TYPES = ["bigint", "integer", "smallint", "double", "real", "decimal(38, 10)", "boolean", "varchar", "date", "timestamp", "timestamp(6) with time zone", "varbinary", "uuid", "array(varchar)", "map(varchar, bigint)", "row(a bigint)"];

const MYSQL_TYPES = ["int", "bigint", "smallint", "tinyint", "decimal(12, 2)", "double", "float", "varchar(255)", "char(36)", "text", "longtext", "json", "date", "datetime(6)", "timestamp", "time", "year", "blob", "varbinary(255)", "binary(16)", "bit(1)", "enum('a', 'b')", "geometry", "unsigned", "signed"];

export function typeNamesFor(d: SqlDialect | null | undefined): readonly string[] {
  return d === "datafusion" ? DATAFUSION_TYPES : isTrinoFamily(d) ? ATHENA_TYPES : d === "spark" ? SPARK_TYPES : d === "mysql" ? MYSQL_TYPES : TYPE_NAMES;
}

const MYSQL_STATEMENT_DROP = new Set(["VACUUM", "COPY", "COMMENT ON", "BEGIN", "EXPLAIN ANALYZE"]);

const ICEBERG_STATEMENT_DROP = new Set(["VACUUM", "COPY", "GRANT", "REVOKE", "COMMENT ON", "BEGIN", "COMMIT", "ROLLBACK", "SET", "ANALYZE", "TRUNCATE", "CREATE INDEX", "EXPLAIN ANALYZE"]);

export function statementStartFor(d: SqlDialect | null | undefined): readonly string[] {
  if (d === "mysql")
    return [...STATEMENT_START_KEYWORDS.filter((k) => !MYSQL_STATEMENT_DROP.has(k)), "REPLACE INTO", "START TRANSACTION", "SHOW TABLES", "SHOW DATABASES", "SHOW CREATE TABLE", "DESCRIBE", "EXPLAIN ANALYZE SELECT", "USE"];
  if (!isIcebergDialect(d)) return STATEMENT_START_KEYWORDS;
  const base = STATEMENT_START_KEYWORDS.filter((k) => !ICEBERG_STATEMENT_DROP.has(k));
  // Athena, Trino and Spark run row-level DML on Iceberg tables; the local engine appends only.
  if (d === "athena") return [...base, "MERGE INTO", "DESCRIBE", "SHOW TABLES", "OPTIMIZE", "VACUUM"];
  if (d === "trino") return [...base, "MERGE INTO", "DESCRIBE", "SHOW TABLES", "SHOW SCHEMAS"];
  if (d === "spark") return [...base, "MERGE INTO", "DESCRIBE", "SHOW TABLES", "SHOW NAMESPACES"];
  return [...base, "DESCRIBE", "SHOW TABLES"];
}

/** Dialect keywords that may follow a complete relation (time travel on Athena). */
export const afterRelationExtras = (d: SqlDialect | null | undefined): readonly string[] =>
  isTrinoFamily(d) ? ["FOR VERSION AS OF", "FOR TIMESTAMP AS OF"] : d === "spark" ? ["VERSION AS OF", "TIMESTAMP AS OF"] : [];

/** The local engine appends only (INSERT INTO works), so its UPDATE / DELETE snippets would just fail. */
const DATAFUSION_SNIPPET_DROP = new Set(["upd", "del"]);
const DATAFUSION_SNIPPETS: readonly SnippetDef[] = [
  {
    label: "selsnap",
    filter: "selsnap",
    detail: 'SELECT … FROM "table@snapshot"',
    body: 'SELECT *\nFROM ${1:namespace}."${2:table}@${3:snapshot_id}"\nLIMIT ${4:100};$0',
    doc: "Time travel: reads the table as of a snapshot id (see the table's Snapshots tab).",
  },
  {
    label: "selmeta",
    filter: "selmeta",
    detail: 'SELECT … FROM "table$snapshots"',
    body: 'SELECT *\nFROM ${1:namespace}."${2:table}\\$snapshots";$0',
    doc: "Metadata table: the snapshot history of a table.",
  },
];
const ATHENA_SNIPPETS: readonly SnippetDef[] = [
  {
    label: "selver",
    filter: "selver",
    detail: "SELECT … FOR VERSION AS OF",
    body: "SELECT *\nFROM ${1:table} FOR VERSION AS OF ${2:snapshot_id}\nLIMIT ${3:100};$0",
    doc: "Time travel: reads the table as of a snapshot id.",
  },
  {
    label: "selts",
    filter: "selts",
    detail: "SELECT … FOR TIMESTAMP AS OF",
    body: "SELECT *\nFROM ${1:table} FOR TIMESTAMP AS OF TIMESTAMP '${2:2026-01-01 00:00:00 UTC}'\nLIMIT ${3:100};$0",
    doc: "Time travel: reads the table as of a point in time.",
  },
  {
    label: "selmeta",
    filter: "selmeta",
    detail: 'SELECT … FROM "table$snapshots"',
    body: 'SELECT *\nFROM "${1:table}\\$snapshots"\nORDER BY committed_at DESC;$0',
    doc: "Metadata table: the snapshot history of a table.",
  },
];

const SPARK_SNIPPETS: readonly SnippetDef[] = [
  {
    label: "selver",
    filter: "selver",
    detail: "SELECT … VERSION AS OF",
    body: "SELECT *\nFROM ${1:table} VERSION AS OF ${2:snapshot_id}\nLIMIT ${3:100};$0",
    doc: "Time travel: reads the table as of a snapshot id.",
  },
  {
    label: "selmeta",
    filter: "selmeta",
    detail: "SELECT … FROM table.snapshots",
    body: "SELECT *\nFROM ${1:table}.snapshots\nORDER BY committed_at DESC;$0",
    doc: "Metadata table: the snapshot history of a table.",
  },
];

export function snippetsFor(d: SqlDialect | null | undefined): readonly SnippetDef[] {
  if (isTrinoFamily(d)) return [...SNIPPETS, ...ATHENA_SNIPPETS];
  if (d === "spark") return [...SNIPPETS, ...SPARK_SNIPPETS];
  if (d === "datafusion") return [...SNIPPETS.filter((s) => !DATAFUSION_SNIPPET_DROP.has(s.filter)), ...DATAFUSION_SNIPPETS];
  return SNIPPETS;
}

// -- time travel and metadata tables ------------------------------------------------------------

/** Metadata tables reachable as `"table$name"`. */
export const METADATA_TABLES: Record<"datafusion" | "athena", readonly { name: string; doc: string }[]> = {
  datafusion: [
    { name: "snapshots", doc: "One row per snapshot: id, parent, operation, commit time, summary." },
    { name: "manifests", doc: "The manifest files of the current snapshot." },
    { name: "history", doc: "Table history: when each snapshot became current." },
  ],
  athena: [
    { name: "snapshots", doc: "One row per snapshot: id, parent, operation, commit time, manifest list." },
    { name: "history", doc: "Table history: when each snapshot became current." },
    { name: "files", doc: "The data files of the current snapshot, with row counts and sizes." },
    { name: "manifests", doc: "The manifest files of the current snapshot." },
    { name: "partitions", doc: "Partition values with file and record counts." },
    { name: "refs", doc: "Branches and tags." },
  ],
};

export interface TableSuffixTarget {
  /** Text before the `@` or `$` with any leading double quote removed (the table name). */
  table: string;
  kind: "snapshot" | "metadata";
  /** What the user has typed after the marker. */
  typed: string;
}

/** Recognises `"orders@12` (time travel) and `"orders$sn` (metadata table) in the word being completed. */
export function tableSuffixTarget(prefix: string): TableSuffixTarget | null {
  const bare = prefix.startsWith('"') ? prefix.slice(1) : prefix;
  const at = bare.indexOf("@");
  if (at > 0) return { table: bare.slice(0, at), kind: "snapshot", typed: bare.slice(at + 1) };
  const dollar = bare.indexOf("$");
  if (dollar > 0) return { table: bare.slice(0, dollar), kind: "metadata", typed: bare.slice(dollar + 1) };
  return null;
}

/** `orders@123` → `orders`; a name without a time-travel suffix comes back unchanged. */
export const stripTimeTravel = (name: string): string => {
  const at = name.lastIndexOf("@");
  return at > 0 ? name.slice(0, at) : name;
};

/** `orders$snapshots` → `orders`. */
export const stripMetadataSuffix = (name: string): string => {
  const d = name.indexOf("$");
  return d > 0 ? name.slice(0, d) : name;
};
