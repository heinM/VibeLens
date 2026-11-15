// Prompt construction and answer handling for the editor's inline AI (⌘K and "Fix with AI").
// Pure functions so the exact wording is unit-tested.
import { type Catalog, type CatalogOptions, indexCatalog, lookupTable } from "./catalog";
import { DIALECT_AI_NOTE, DIALECT_LABEL, type SqlDialect } from "./dialect";
import { type Token, identValue } from "./tokenizer";
import { tokenizeCached } from "./analysis";
import type { SchemaMetadata, TableDescription } from "../../ipc/types";

export const MAX_BUFFER_CHARS = 6000;
export const MAX_SELECTION_CHARS = 8000;

const truncate = (s: string, max: number): string => (s.length > max ? s.slice(0, max) + "\n-- … truncated" : s);

/** The rules block; `dialect` names the SQL flavour (PostgreSQL unless the connection is an Iceberg engine). */
const rulesFor = (dialect?: SqlDialect | null): string =>
  [
    "Answer with exactly ONE ```sql fenced code block containing only the SQL, and at most one short sentence before it.",
    `Target ${DIALECT_LABEL[dialect ?? "postgres"]}. Do not run anything that changes data or schema; if you need to look something up, use read-only schema tools only.`,
    ...(dialect && DIALECT_AI_NOTE[dialect] ? [DIALECT_AI_NOTE[dialect]] : []),
    "Keep the existing style (identifier case, indentation, aliases) and terminate statements with a semicolon only if the original did.",
  ].join("\n");

/**
 * A compact column digest for the tables a piece of SQL mentions, so the model can often answer
 * without any tool round-trips. Callers must only pass a catalog when the profile allows schema to
 * be shared with the model.
 */
export function schemaDigest(meta: SchemaMetadata | undefined, sql: string, maxTables = 12, catalogOptions: CatalogOptions = {}): string {
  if (!meta) return "";
  const cat: Catalog = indexCatalog(meta, catalogOptions);
  const seen = new Set<TableDescription>();
  const toks = tokenizeCached(sql).filter((t) => t.kind !== "comment");
  const nameOf = (t: Token | undefined) => (t && (t.kind === "word" || t.kind === "quoted") ? identValue(t) : null);
  for (let i = 0; i < toks.length && seen.size < maxTables; i++) {
    const name = nameOf(toks[i]);
    if (name === null) continue;
    const isDot = (k: number) => toks[k]?.kind === "punct" && toks[k].text === ".";
    if (isDot(i - 1)) continue; // the tail of a qualified name was handled with its head
    // `schema.table` (or `a.b.table` in an Iceberg catalog) resolves exactly; a bare name falls back
    // to the catalog's default schema.
    const chain = [name];
    for (let k = i + 1; isDot(k) && nameOf(toks[k + 1]) !== null; k += 2) chain.push(nameOf(toks[k + 1]) as string);
    const t = (chain.length > 1 ? lookupTable(cat, chain) : undefined) ?? lookupTable(cat, [name]);
    if (t) seen.add(t);
  }
  const lines: string[] = [];
  for (const t of seen) {
    const cols = t.columns.map((c) => `${c.name} ${c.dataType}${c.isPrimaryKey ? " PK" : ""}`).join(", ");
    const fks = t.foreignKeys.map((f) => `${f.columns.join(",")} -> ${f.referencedTable}(${f.referencedColumns.join(",")})`).join("; ");
    lines.push(`${t.schema}.${t.name}(${cols})${fks ? ` FK: ${fks}` : ""}`);
  }
  return lines.join("\n");
}

export interface AskPromptInput {
  instruction: string;
  /** SQL being rewritten (selection or statement at the cursor). Empty for "insert new SQL". */
  target: string | null;
  buffer: string;
  digest?: string;
  /** SQL flavour of the connection; omitted means PostgreSQL. */
  dialect?: SqlDialect | null;
}

export function buildAskPrompt({ instruction, target, buffer, digest, dialect }: AskPromptInput): string {
  const parts: string[] = [];
  if (target && target.trim()) {
    parts.push(`Rewrite the SQL below according to this instruction: ${instruction.trim()}`);
    parts.push(`SQL to rewrite:\n\`\`\`sql\n${truncate(target, MAX_SELECTION_CHARS)}\n\`\`\``);
    parts.push("Return the complete replacement for exactly that SQL (not the rest of the worksheet).");
  } else {
    parts.push(`Write a ${DIALECT_LABEL[dialect ?? "postgres"]} query for this instruction: ${instruction.trim()}`);
    parts.push("Return only the new SQL to insert at the cursor.");
  }
  if (digest) parts.push(`Relevant tables:\n${digest}`);
  else parts.push("Look up the schema with your tools if you need table or column names.");
  if (buffer.trim() && buffer.trim() !== (target ?? "").trim()) {
    parts.push(`For context, the worksheet currently contains (may be truncated):\n\`\`\`sql\n${truncate(buffer, MAX_BUFFER_CHARS)}\n\`\`\``);
  }
  parts.push(rulesFor(dialect));
  return parts.join("\n\n");
}

export interface FixPromptInput {
  sql: string;
  error: string;
  digest?: string;
  dialect?: SqlDialect | null;
}

export function buildFixPrompt({ sql, error, digest, dialect }: FixPromptInput): string {
  const parts = [
    `This ${DIALECT_LABEL[dialect ?? "postgres"]} statement failed. Fix it so it runs, changing as little as possible.`,
    `Statement:\n\`\`\`sql\n${truncate(sql, MAX_SELECTION_CHARS)}\n\`\`\``,
    `Error from the server:\n${error.slice(0, 800)}`,
  ];
  if (digest) parts.push(`Relevant tables:\n${digest}`);
  else parts.push("Check table and column names against the schema with your tools before answering.");
  parts.push(rulesFor(dialect));
  return parts.join("\n\n");
}

/**
 * The SQL inside the (possibly still streaming) answer. Returns the text after the first opening
 * ``` fence up to the closing fence, or to the end of the text while the fence is still open.
 */
export function partialSqlBlock(text: string): string | null {
  const open = /```[a-zA-Z0-9_+-]*[ \t]*\r?\n/.exec(text);
  if (!open) return null;
  const rest = text.slice(open.index + open[0].length);
  const close = rest.indexOf("```");
  const body = close >= 0 ? rest.slice(0, close) : rest;
  return body.replace(/\s+$/, "") || null;
}

/** Human-readable progress line for an agent tool call. */
export function toolProgress(tool: string, args: unknown): string {
  const a = (args && typeof args === "object" ? (args as Record<string, unknown>) : {}) as Record<string, unknown>;
  const table = typeof a.table === "string" ? a.table : null;
  switch (tool) {
    case "get_schema":
      return "Reading the schema";
    case "describe_table":
      return table ? `Looking at ${table}` : "Looking at a table";
    case "run_sql":
      return "Checking with a read-only query";
    case "list_connections":
      return "Checking the connection";
    default:
      return `Using ${tool.replace(/_/g, " ")}`;
  }
}
