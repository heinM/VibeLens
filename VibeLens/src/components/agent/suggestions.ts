// Starter prompts for the agent's empty state, chosen from what is open and what AI access allows.
import type { AiDataAccess, EngineKind } from "../../ipc/types";
import { canAttachResults } from "./lastResult";

export type SuggestionIcon = "schema" | "slow" | "index" | "migrate" | "review" | "query" | "doc" | "keys" | "ttl" | "pattern" | "size" | "list";

export interface Suggestion {
  id: string;
  icon: SuggestionIcon;
  title: string;
  /** One line under the title. */
  hint: string;
  /** What goes into the composer. */
  prompt: string;
  /** True when the prompt still needs the user to finish it. The composer takes focus either way. */
  incomplete: boolean;
  /** Set when this connection's AI access can't support the request. */
  disabledReason: string | null;
}

export interface SuggestionContext {
  engine: EngineKind | null;
  connectionName: string | null;
  /** Postgres or Redshift database name. */
  database: string | null;
  access: AiDataAccess | null;
  /** The active tab has text the agent could review. */
  hasEditorText: boolean;
}

const NEEDS_RESULTS = "Needs results access. This connection's AI access doesn't let query results leave your machine.";

function s(id: string, icon: SuggestionIcon, title: string, hint: string, prompt: string, extra: Partial<Pick<Suggestion, "incomplete" | "disabledReason">> = {}): Suggestion {
  return { id, icon, title, hint, prompt, incomplete: extra.incomplete ?? false, disabledReason: extra.disabledReason ?? null };
}

export function suggestionsFor(ctx: SuggestionContext): Suggestion[] {
  const results = canAttachResults(ctx.access);
  const gate = results ? null : NEEDS_RESULTS;

  if (ctx.engine === "postgres") {
    const db = ctx.database?.trim() || "database";
    return [
      s("pg-schema", "schema", `Map out the ${db} schema`, "Tables, keys and how they relate", `Map out the ${db} schema: the main tables, their keys and how they relate. Group them by domain and point out anything that looks odd.`),
      s(
        "pg-slow",
        "slow",
        "Find slow queries in pg_stat_statements",
        "Total time, mean time, calls",
        "Find the slowest queries in pg_stat_statements: show total time, mean time and calls, and say which ones are worth optimising and why. If the extension isn't installed, tell me how to enable it.",
        { disabledReason: gate },
      ),
      s("pg-index", "index", "Suggest missing indexes", "Foreign keys and filters without support", "From the schema, find foreign keys and likely filter columns that have no supporting index. Give me CREATE INDEX statements and name the write cost of each."),
      s("pg-migrate", "migrate", "Write a migration…", "You describe it, I write up and down scripts", "Write a migration, with a rollback script, that ", { incomplete: true }),
      ctx.hasEditorText
        ? s("pg-review", "review", "Review the SQL in my editor", "Correctness, performance, indexes", "Review the SQL in my editor for correctness and performance. Suggest indexes or rewrites if they would help, and say what each would cost.")
        : s("pg-query", "query", "Write a query…", "Describe the result you want", "Write a query that ", { incomplete: true }),
      s("pg-doc", "doc", "Document the schema", "COMMENT ON for every table and column", "Write COMMENT ON statements for the tables and columns in this schema, based on their names, types and relationships. Mark any you are guessing at."),
    ];
  }

  if (ctx.engine === "mysql") {
    // MySQL: performance_schema instead of pg_stat_statements; no COMMENT ON (comments live in ALTER TABLE).
    const db = ctx.database?.trim() || "database";
    return [
      s("my-schema", "schema", `Map out the ${db} database`, "Tables, keys and how they relate", `Map out the ${db} database on this MySQL server: the main tables, their primary and foreign keys and how they relate. Group them by domain and point out anything that looks odd.`),
      s(
        "my-slow",
        "slow",
        "Find slow queries in performance_schema",
        "Total latency, average latency, calls",
        "Find the slowest statement digests in performance_schema.events_statements_summary_by_digest (or sys.statement_analysis): show total and average latency and calls, and say which ones are worth optimising and why.",
        { disabledReason: gate },
      ),
      s("my-index", "index", "Suggest missing indexes", "Foreign keys and filters without support", "From the schema, find foreign keys and likely filter columns that have no supporting index. Give me ALTER TABLE … ADD INDEX statements and name the write cost of each."),
      ctx.hasEditorText
        ? s("my-review", "review", "Review the SQL in my editor", "Correctness, performance, indexes", "Review the MySQL SQL in my editor for correctness and performance. Suggest indexes or rewrites if they would help, and say what each would cost.")
        : s("my-query", "query", "Write a query…", "Describe the result you want", "Write a MySQL query that ", { incomplete: true }),
    ];
  }

  if (ctx.engine === "redshift") {
    // Redshift: no pg_stat_statements, no CREATE INDEX, no COMMENT-heavy migrations. Its own system views instead.
    const db = ctx.database?.trim() || "database";
    return [
      s("rs-schema", "schema", `Map out the ${db} schema`, "Tables, sort and distribution keys", `Map out the ${db} schema in this Redshift warehouse: the main tables, how they relate, and their sort and distribution keys. Group them by domain and point out anything that looks odd.`),
      s(
        "rs-slow",
        "slow",
        "Find slow queries in SYS_QUERY_HISTORY",
        "Elapsed time, queue time, rows",
        "Find the slowest recent queries in SYS_QUERY_HISTORY: show elapsed time, queue time and rows returned, and say which ones are worth optimising and why.",
        { disabledReason: gate },
      ),
      s("rs-keys", "index", "Review sort and distribution keys", "From the table design in SVV_TABLE_INFO", "Review the sort and distribution keys of the largest tables (SVV_TABLE_INFO) and suggest changes, with the ALTER TABLE statements and what each would cost to apply."),
      ctx.hasEditorText
        ? s("rs-review", "review", "Review the SQL in my editor", "Correctness and Redshift performance", "Review the SQL in my editor for correctness and Redshift performance (joins on distribution keys, filters on sort keys, scans). Suggest rewrites if they would help.")
        : s("rs-query", "query", "Write a query…", "Describe the result you want", "Write a Redshift query that ", { incomplete: true }),
    ];
  }

  if (ctx.engine === "valkey") {
    return [
      s("vk-keyspace", "keys", "Summarise the keyspace", "Prefixes, types and what each is for", "Summarise the keyspace: scan the key names, group them by prefix and type, and say what each group appears to be used for."),
      s("vk-names", "pattern", "Explain the key naming", "Convention and inconsistencies", "Infer the key naming convention from the key names and point out keys that break it."),
      s("vk-ttl", "ttl", "Find keys without a TTL", "Candidates for unbounded growth", "Find keys that have no TTL, grouped by prefix, and say which groups look like they should expire.", { disabledReason: gate }),
      s("vk-big", "size", "Find the biggest keys", "Memory hogs by prefix", "Find the largest keys by memory usage and group them by prefix. Say what could be trimmed.", { disabledReason: gate }),
      s("vk-write", "migrate", "Write commands for…", "Describe it, get the exact commands", "Write the Valkey commands to ", { incomplete: true }),
    ];
  }

  return [s("none-list", "list", "List my connections", "What is set up, and where", "Which connections do I have set up, and what is each one for?")];
}
