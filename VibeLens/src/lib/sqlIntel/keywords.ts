// Static vocabulary for completion: keywords by context, common functions, types, snippets, and the
// Postgres reserved-word list that decides whether an identifier must be double-quoted.

/** Words that cannot be used as bare identifiers in Postgres (reserved + "cannot be function or type"). */
export const RESERVED_WORDS: ReadonlySet<string> = new Set(
  (
    "all analyse analyze and any array as asc asymmetric authorization binary both case cast check collate collation column concurrently constraint create cross " +
    "current_catalog current_date current_role current_schema current_time current_timestamp current_user default deferrable desc distinct do else end except false " +
    "fetch for foreign freeze from full grant group having ilike in initially inner intersect into is isnull join lateral leading left like limit localtime localtimestamp " +
    "natural not notnull null offset on only or order outer overlaps placing primary references returning right select session_user similar some symmetric system_user " +
    "table tablesample then to trailing true union unique user using variadic verbose when where window with"
  ).split(" "),
);

/** Words that end an implicit alias (`FROM t <alias>`): clause starters and join modifiers. */
export const ALIAS_STOP_WORDS: ReadonlySet<string> = new Set(
  (
    "where on join inner left right full cross natural outer lateral group order having limit offset union intersect except using set returning window fetch for values select from " +
    "with tablesample as and or not when then else end returning into window partition over filter within do conflict nothing"
  ).split(" "),
);

/** Keywords that open a clause; used to find "what clause is the cursor in". */
export const CLAUSE_WORDS: ReadonlySet<string> = new Set(
  "select from join where on group order having limit offset set returning values using window union intersect except fetch into update delete insert".split(" "),
);

export const SET_OPERATORS: ReadonlySet<string> = new Set(["union", "intersect", "except"]);

export const JOIN_MODIFIERS: ReadonlySet<string> = new Set(["inner", "left", "right", "full", "cross", "natural", "outer", "lateral"]);

/** Statement-leading keywords. */
export const STATEMENT_START_KEYWORDS: readonly string[] = [
  "SELECT", "INSERT INTO", "UPDATE", "DELETE FROM", "WITH", "CREATE TABLE", "CREATE INDEX", "CREATE VIEW", "ALTER TABLE", "DROP TABLE", "TRUNCATE",
  "EXPLAIN", "EXPLAIN ANALYZE", "BEGIN", "COMMIT", "ROLLBACK", "SHOW", "SET", "VACUUM", "ANALYZE", "COPY", "GRANT", "REVOKE", "COMMENT ON",
];

/** Keywords that can follow a complete relation reference in FROM/JOIN/UPDATE. */
export const AFTER_RELATION_KEYWORDS: readonly string[] = [
  "WHERE", "JOIN", "INNER JOIN", "LEFT JOIN", "LEFT OUTER JOIN", "RIGHT JOIN", "FULL JOIN", "CROSS JOIN", "NATURAL JOIN", "LATERAL", "ON", "USING", "AS",
  "SET", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET", "UNION", "UNION ALL", "INTERSECT", "EXCEPT", "RETURNING", "WINDOW", "FETCH FIRST", "FOR UPDATE", "TABLESAMPLE",
];

/** Keywords that make sense inside an expression (SELECT list, WHERE, ON, ORDER BY…). */
export const EXPRESSION_KEYWORDS: readonly string[] = [
  "AS", "DISTINCT", "FROM", "CASE", "WHEN", "THEN", "ELSE", "END", "AND", "OR", "NOT", "IN", "IS NULL", "IS NOT NULL", "LIKE", "ILIKE", "BETWEEN", "EXISTS", "ANY", "ALL",
  "ASC", "DESC", "NULLS FIRST", "NULLS LAST", "NULL", "TRUE", "FALSE", "WHERE", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET", "OVER", "PARTITION BY", "FILTER",
  "UNION", "UNION ALL", "INTERSECT", "EXCEPT", "JOIN", "LEFT JOIN", "INNER JOIN", "SELECT", "INTERVAL", "CURRENT_DATE", "CURRENT_TIMESTAMP",
];

export interface FunctionDef {
  name: string;
  /** Text shown after the name in the list and used in the snippet tabstop. */
  signature: string;
  doc: string;
  /** Aggregate/window functions rank a little lower in WHERE-ish clauses; unused for now. */
  agg?: boolean;
}

export const FUNCTIONS: readonly FunctionDef[] = [
  { name: "count", signature: "(*)", doc: "Number of input rows (or non-null values).", agg: true },
  { name: "sum", signature: "(expr)", doc: "Sum of values.", agg: true },
  { name: "avg", signature: "(expr)", doc: "Average of values.", agg: true },
  { name: "min", signature: "(expr)", doc: "Minimum value.", agg: true },
  { name: "max", signature: "(expr)", doc: "Maximum value.", agg: true },
  { name: "string_agg", signature: "(expr, delimiter)", doc: "Concatenates values with a delimiter.", agg: true },
  { name: "array_agg", signature: "(expr)", doc: "Collects values into an array.", agg: true },
  { name: "json_agg", signature: "(expr)", doc: "Aggregates values as a JSON array.", agg: true },
  { name: "jsonb_agg", signature: "(expr)", doc: "Aggregates values as a JSONB array.", agg: true },
  { name: "bool_and", signature: "(expr)", doc: "True if all values are true.", agg: true },
  { name: "bool_or", signature: "(expr)", doc: "True if any value is true.", agg: true },
  { name: "coalesce", signature: "(a, b)", doc: "First non-null argument." },
  { name: "nullif", signature: "(a, b)", doc: "NULL if a = b, else a." },
  { name: "greatest", signature: "(a, b)", doc: "Largest argument." },
  { name: "least", signature: "(a, b)", doc: "Smallest argument." },
  { name: "now", signature: "()", doc: "Current transaction timestamp (timestamptz)." },
  { name: "date_trunc", signature: "(field, source)", doc: "Truncates a timestamp to the given precision, e.g. 'day'." },
  { name: "extract", signature: "(field FROM source)", doc: "Extracts a date/time field." },
  { name: "age", signature: "(timestamp)", doc: "Interval between now and the timestamp." },
  { name: "to_char", signature: "(value, format)", doc: "Formats a value as text." },
  { name: "to_date", signature: "(text, format)", doc: "Parses text into a date." },
  { name: "to_timestamp", signature: "(text, format)", doc: "Parses text into a timestamptz." },
  { name: "lower", signature: "(text)", doc: "Lower-cases text." },
  { name: "upper", signature: "(text)", doc: "Upper-cases text." },
  { name: "length", signature: "(text)", doc: "Number of characters." },
  { name: "substring", signature: "(text FROM start FOR count)", doc: "Extracts a substring." },
  { name: "trim", signature: "(text)", doc: "Removes leading and trailing spaces." },
  { name: "replace", signature: "(text, from, to)", doc: "Replaces occurrences of a substring." },
  { name: "split_part", signature: "(text, delimiter, n)", doc: "Nth field of a delimited string." },
  { name: "concat", signature: "(a, b)", doc: "Concatenates arguments, ignoring NULLs." },
  { name: "regexp_replace", signature: "(source, pattern, replacement)", doc: "Regular-expression replace." },
  { name: "round", signature: "(numeric, digits)", doc: "Rounds to the given number of decimals." },
  { name: "ceil", signature: "(numeric)", doc: "Rounds up." },
  { name: "floor", signature: "(numeric)", doc: "Rounds down." },
  { name: "abs", signature: "(numeric)", doc: "Absolute value." },
  { name: "row_number", signature: "()", doc: "Window: number of the current row within its partition.", agg: true },
  { name: "rank", signature: "()", doc: "Window: rank with gaps.", agg: true },
  { name: "dense_rank", signature: "()", doc: "Window: rank without gaps.", agg: true },
  { name: "lag", signature: "(expr, offset)", doc: "Window: value from a preceding row.", agg: true },
  { name: "lead", signature: "(expr, offset)", doc: "Window: value from a following row.", agg: true },
  { name: "generate_series", signature: "(start, stop)", doc: "Set-returning: series of values." },
  { name: "unnest", signature: "(array)", doc: "Set-returning: expands an array to rows." },
  { name: "jsonb_build_object", signature: "(key, value)", doc: "Builds a JSONB object." },
  { name: "jsonb_array_elements", signature: "(jsonb)", doc: "Expands a JSONB array to rows." },
  { name: "gen_random_uuid", signature: "()", doc: "Random v4 UUID." },
  { name: "md5", signature: "(text)", doc: "MD5 hash as hex." },
  { name: "random", signature: "()", doc: "Random value in [0, 1)." },
];

export const TYPE_NAMES: readonly string[] = [
  "text", "integer", "bigint", "smallint", "numeric", "boolean", "date", "timestamp", "timestamptz", "timestamp with time zone", "interval", "uuid", "jsonb", "json",
  "varchar", "char", "real", "double precision", "bytea", "inet", "cidr", "serial", "bigserial", "int", "int4", "int8", "float8", "money", "regclass", "text[]",
];

export interface SnippetDef {
  /** Trigger word shown in the list. */
  label: string;
  /** Text the user types to match (no punctuation). */
  filter: string;
  detail: string;
  body: string;
  doc: string;
}

export const SNIPPETS: readonly SnippetDef[] = [
  { label: "sel*", filter: "sel", detail: "SELECT * FROM …", body: "SELECT *\nFROM ${1:table}\nLIMIT ${2:100};$0", doc: "Select every column, capped at a row limit." },
  { label: "selc", filter: "selc", detail: "SELECT count(*) …", body: "SELECT count(*)\nFROM ${1:table}\nWHERE ${2:condition};$0", doc: "Count rows matching a condition." },
  { label: "selj", filter: "selj", detail: "SELECT … JOIN …", body: "SELECT ${1:*}\nFROM ${2:table} t\nJOIN ${3:other} o ON o.${4:t_id} = t.${5:id}\nWHERE ${6:condition};$0", doc: "Two-table join skeleton." },
  { label: "ins", filter: "ins", detail: "INSERT INTO … VALUES", body: "INSERT INTO ${1:table} (${2:columns})\nVALUES (${3:values});$0", doc: "Insert one row." },
  { label: "upd", filter: "upd", detail: "UPDATE … SET … WHERE", body: "UPDATE ${1:table}\nSET ${2:column} = ${3:value}\nWHERE ${4:condition};$0", doc: "Update rows. The WHERE placeholder is on purpose: fill it in before running." },
  { label: "del", filter: "del", detail: "DELETE FROM … WHERE", body: "DELETE FROM ${1:table}\nWHERE ${2:condition};$0", doc: "Delete rows. The WHERE placeholder is on purpose: fill it in before running." },
  { label: "cte", filter: "cte", detail: "WITH name AS (…)", body: "WITH ${1:name} AS (\n  SELECT ${2:*}\n  FROM ${3:table}\n)\nSELECT *\nFROM ${1:name};$0", doc: "Common table expression." },
];
