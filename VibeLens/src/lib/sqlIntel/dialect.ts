// SQL dialects the editor can complete for. Postgres is the default; the two Iceberg query
// engines have their own function vocabulary and a few dialect-only constructs (time travel,
// metadata tables). Pure data plus small lookups, so it is unit-tested without Monaco.
import type { ConnectionProfile } from "../../ipc/types";
import { FUNCTIONS, SNIPPETS, STATEMENT_START_KEYWORDS, TYPE_NAMES, type FunctionDef, type SnippetDef } from "./keywords";

/** `datafusion` = Iceberg through the embedded local engine; `athena` = Iceberg through Amazon Athena (Trino). */
export type SqlDialect = "postgres" | "datafusion" | "athena";

export const isIcebergDialect = (d: SqlDialect | null | undefined): boolean => d === "datafusion" || d === "athena";

/** The catalog family a dialect reads its schema from. */
export const catalogEngineOf = (d: SqlDialect | null | undefined): "postgres" | "iceberg" => (isIcebergDialect(d) ? "iceberg" : "postgres");

/** Which dialect SQL tabs on this profile are written in. Valkey has none (null). */
export function dialectOf(profile: Pick<ConnectionProfile, "engine" | "iceberg"> | null | undefined): SqlDialect | null {
  if (!profile) return null;
  if (profile.engine === "postgres") return "postgres";
  if (profile.engine === "iceberg") return profile.iceberg?.queryEngine.kind === "athena" ? "athena" : "datafusion";
  return null;
}

/** How the dialect is named to people and to the AI assistant. */
export const DIALECT_LABEL: Record<SqlDialect, string> = {
  postgres: "PostgreSQL",
  datafusion: "Apache DataFusion SQL",
  athena: "Amazon Athena (Trino) SQL",
};

/** One sentence for the AI prompts: what the engine can and cannot do. */
export const DIALECT_AI_NOTE: Record<SqlDialect, string> = {
  postgres: "",
  datafusion:
    "The tables are Apache Iceberg tables queried through the embedded DataFusion engine. Namespaces play the role of schemas (a.b.table). INSERT INTO works; UPDATE, DELETE and MERGE are not supported. Time travel is written as \"table@<snapshot_id>\" (quoted).",
  athena:
    "The tables are Apache Iceberg tables queried through Amazon Athena (Trino dialect). Namespaces play the role of schemas. Time travel is written as FOR VERSION AS OF <snapshot_id> or FOR TIMESTAMP AS OF TIMESTAMP '...'. Athena bills by data scanned, so keep scans narrow.",
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
  else if (key === "athena") out = unique([...FUNCTIONS.filter((f) => !NOT_ICEBERG.has(f.name) && !NOT_TRINO.has(f.name)), ...TRINO_EXTRA]);
  else out = FUNCTIONS;
  FUNCTION_CACHE.set(key, out);
  return out;
}

// -- types, statements, snippets ---------------------------------------------------------------

const DATAFUSION_TYPES = ["bigint", "int", "smallint", "double", "float", "decimal(38, 10)", "boolean", "string", "varchar", "date", "timestamp", "timestamp with time zone", "binary", "array", "struct"];
const ATHENA_TYPES = ["bigint", "integer", "smallint", "double", "real", "decimal(38, 10)", "boolean", "varchar", "date", "timestamp", "timestamp(6) with time zone", "varbinary", "uuid", "array(varchar)", "map(varchar, bigint)", "row(a bigint)"];

export function typeNamesFor(d: SqlDialect | null | undefined): readonly string[] {
  return d === "datafusion" ? DATAFUSION_TYPES : d === "athena" ? ATHENA_TYPES : TYPE_NAMES;
}

const ICEBERG_STATEMENT_DROP = new Set(["VACUUM", "COPY", "GRANT", "REVOKE", "COMMENT ON", "BEGIN", "COMMIT", "ROLLBACK", "SET", "ANALYZE", "TRUNCATE", "CREATE INDEX", "EXPLAIN ANALYZE"]);

export function statementStartFor(d: SqlDialect | null | undefined): readonly string[] {
  if (!isIcebergDialect(d)) return STATEMENT_START_KEYWORDS;
  const base = STATEMENT_START_KEYWORDS.filter((k) => !ICEBERG_STATEMENT_DROP.has(k));
  // Athena runs row-level DML on Iceberg tables; the local engine appends only.
  return d === "athena" ? [...base, "MERGE INTO", "DESCRIBE", "SHOW TABLES", "OPTIMIZE", "VACUUM"] : [...base, "DESCRIBE", "SHOW TABLES"];
}

/** Dialect keywords that may follow a complete relation (time travel on Athena). */
export const afterRelationExtras = (d: SqlDialect | null | undefined): readonly string[] => (d === "athena" ? ["FOR VERSION AS OF", "FOR TIMESTAMP AS OF"] : []);

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

export function snippetsFor(d: SqlDialect | null | undefined): readonly SnippetDef[] {
  if (d === "athena") return [...SNIPPETS, ...ATHENA_SNIPPETS];
  if (d === "datafusion") return [...SNIPPETS.filter((s) => !DATAFUSION_SNIPPET_DROP.has(s.filter)), ...DATAFUSION_SNIPPETS];
  return SNIPPETS;
}

// -- time travel and metadata tables ------------------------------------------------------------

/** Metadata tables reachable as `"table$name"`. */
export const METADATA_TABLES: Record<"datafusion" | "athena", readonly { name: string; doc: string }[]> = {
  datafusion: [
    { name: "snapshots", doc: "One row per snapshot: id, parent, operation, commit time, summary." },
    { name: "manifests", doc: "The manifest files of the current snapshot." },
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
