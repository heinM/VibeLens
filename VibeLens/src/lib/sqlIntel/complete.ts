// Pure completion engine: (catalog, SQL text, cursor offset) → ranked suggestions. No Monaco
// types here, so it is unit-testable and the provider is a thin mapping.
import type { TableDescription } from "../../ipc/types";
import { type Catalog, childNamespaces, describeColumnBadges, foreignKeysByColumn, formatRows, ident, isDefaultSchema, kindLabel, lookupTable, quoteIdent, resolveSchemaQualifier, schemaIdent, schemaTables } from "./catalog";
import { METADATA_TABLES, type SqlDialect, afterRelationExtras, functionsFor, isTrinoFamily, nestedNamespaceIsOneIdent, snippetsFor, statementStartFor, tableSuffixTarget, typeNamesFor } from "./dialect";
import { type Analysis, type Context, analyzeAt, tokenizeCached } from "./analysis";
import { type ColumnEntry, columnsOf, tableOf } from "./columns";
import { joinConditions } from "./joins";
import { AFTER_RELATION_KEYWORDS, EXPRESSION_KEYWORDS } from "./keywords";
import { type Relation, exposedName } from "./scope";

export type SuggestionKind = "column" | "table" | "view" | "schema" | "cte" | "alias" | "keyword" | "function" | "snippet" | "join" | "type";

export interface Suggestion {
  label: string;
  kind: SuggestionKind;
  /** Short type/summary shown beside the label. */
  detail?: string;
  /** Right-aligned dim text in the list (schema, source relation). */
  description?: string;
  /** Markdown shown in the details pane. */
  documentation?: string;
  insertText: string;
  isSnippet?: boolean;
  filterText?: string;
  sortText: string;
  preselect?: boolean;
  /** Re-open the suggest widget after inserting (schema `shop.`, alias `o.`). */
  retrigger?: boolean;
}

export interface CompletionResult {
  suggestions: Suggestion[];
  /** Offsets into the SQL text. */
  replaceStart: number;
  insertEnd: number;
  replaceEnd: number;
}

export interface CompleteOptions {
  /** The character that triggered completion, if it was a trigger character. */
  triggerCharacter?: string;
  /** The SQL dialect of the connection (functions, types, keywords, time-travel hints). Default PostgreSQL. */
  dialect?: SqlDialect | null;
}

const pad = (n: number, w = 4) => String(n).padStart(w, "0");

function columnDocs(c: ColumnEntry): string | undefined {
  if (!c.info || !c.table) return undefined;
  const badges = describeColumnBadges(c.table, c.info).join(" · ");
  return `**${c.table.name}.${c.name}** \`${c.info.dataType}\`\n\n${badges}${c.info.comment ? `\n\n${c.info.comment}` : ""}`;
}

function tableDocs(t: TableDescription, dialect?: SqlDialect | null): string {
  const rows = formatRows(t.estimatedRows);
  const head = `**${t.schema}.${t.name}** · ${kindLabel(t.kind)}${rows ? ` · ${rows}` : ""}`;
  const cols = t.columns
    .slice(0, 12)
    .map((c) => `${c.name} ${c.dataType}${c.isPrimaryKey ? " PK" : ""}`)
    .join("\n");
  const more = t.columns.length > 12 ? `\n… ${t.columns.length - 12} more` : "";
  const travel =
    dialect === "datafusion"
      ? `\n\nTime travel: \`"${t.name}@<snapshot_id>"\``
      : isTrinoFamily(dialect)
        ? `\n\nTime travel: \`${t.name} FOR VERSION AS OF <snapshot_id>\``
        : dialect === "spark"
          ? `\n\nTime travel: \`${t.name} VERSION AS OF <snapshot_id>\``
          : "";
  return `${head}${t.comment ? `\n\n${t.comment}` : ""}\n\n\`\`\`plaintext\n${cols}${more}\n\`\`\`${travel}`;
}

function wordsInBuffer(sql: string): Set<string> {
  const s = new Set<string>();
  for (const t of tokenizeCached(sql)) if (t.kind === "word") s.add(t.text.toLowerCase());
  return s;
}

// -- builders -------------------------------------------------------------------------------------

const isViewKind = (k: TableDescription["kind"]) => k === "view" || k === "materializedView";

/** Escapes the characters that mean something inside a snippet body. */
const snippetEscape = (text: string): string => text.replace(/[\\$}]/g, "\\$&");

/** `"orders@<snapshot_id>"` and `"orders$snapshots"` offers, once the user has typed the marker after a table name. */
function suffixSuggestions(cat: Catalog | null, a: Analysis, qualifier: string[] | null, dialect: SqlDialect): Suggestion[] {
  // Spark reaches metadata tables as `table.snapshots` (plain qualified names), so only these two quote a suffix.
  if (dialect !== "datafusion" && !isTrinoFamily(dialect)) return [];
  const target = tableSuffixTarget(a.prefix);
  if (!target) return [];
  const t = lookupTable(cat, [...(qualifier ?? []), target.table]);
  if (!t) return [];
  // The user's own text (quote included) is what the editor filters against, so echo it back verbatim.
  const filterText = a.prefix;
  if (target.kind === "snapshot") {
    if (dialect !== "datafusion") return [];
    return [
      {
        label: `${t.name}@<snapshot_id>`,
        kind: "snippet",
        detail: "time travel",
        documentation: `Reads **${t.schema}.${t.name}** as of a snapshot. Snapshot ids are on the table's Snapshots tab or in \`"${t.name}$snapshots"\`.`,
        insertText: `"${snippetEscape(t.name.replace(/"/g, '""'))}@\${1:${snippetEscape(target.typed) || "snapshot_id"}}"`,
        isSnippet: true,
        filterText,
        sortText: "0000",
        preselect: true,
      },
    ];
  }
  return METADATA_TABLES[dialect === "datafusion" ? "datafusion" : "athena"].map((m, i) => ({
    label: `${t.name}$${m.name}`,
    kind: "view" as const,
    detail: "metadata table",
    documentation: `${m.doc}\n\nQuote the name: \`"${t.name}$${m.name}"\``,
    insertText: `"${t.name.replace(/"/g, '""')}$${m.name}"`,
    filterText,
    sortText: `0${String(i).padStart(3, "0")}`,
  }));
}

function relationSuggestions(cat: Catalog | null, a: Analysis, sql: string, qualifier: string[] | null, dialect: SqlDialect): Suggestion[] {
  const out: Suggestion[] = [];
  const force = a.quoted;
  const travel = suffixSuggestions(cat, a, qualifier, dialect);
  if (travel.length) return travel;
  if (qualifier) {
    // `schema.|` or `db.schema.|`; in an Iceberg catalog `a.b.|` names the namespace `a.b`.
    const schema = resolveSchemaQualifier(cat, qualifier);
    // Trino / Athena name a nested namespace as one quoted schema, so `a.` never leads to a deeper level there.
    if (!nestedNamespaceIsOneIdent(dialect)) {
      for (const child of childNamespaces(cat, qualifier)) {
        out.push({ label: child, kind: "schema", detail: "namespace", insertText: `${ident(child, force, dialect)}.`, filterText: child, sortText: `1${child.toLowerCase()}`, retrigger: true });
      }
    }
    if (!cat || schema === null) return out;
    for (const t of schemaTables(cat, schema)) {
      out.push({
        label: t.name,
        kind: isViewKind(t.kind) ? "view" : "table",
        detail: `${kindLabel(t.kind)}${t.estimatedRows !== null ? " · " + formatRows(t.estimatedRows) : ""}`,
        description: t.schema,
        documentation: tableDocs(t, dialect),
        insertText: ident(t.name, force, dialect),
        filterText: t.name,
        sortText: `2${t.name.toLowerCase()}`,
      });
    }
    return out;
  }

  // CTE names first.
  const seenCte = new Set<string>();
  a.scope.ctes.forEach((c, i) => {
    const k = c.name.toLowerCase();
    if (seenCte.has(k)) return;
    seenCte.add(k);
    out.push({ label: c.name, kind: "cte", detail: "CTE", insertText: ident(c.name, force, dialect), sortText: `0${pad(i)}`, filterText: c.name });
  });
  if (!cat) return out;

  const mentioned = wordsInBuffer(sql);
  for (const t of cat.tables) {
    const isDefault = isDefaultSchema(cat, t.schema);
    const qualified = isDefault ? ident(t.name, force, dialect) : `${schemaIdent(cat, t.schema, false, dialect)}.${ident(t.name, false, dialect)}`;
    const used = mentioned.has(t.name.toLowerCase());
    out.push({
      label: t.name,
      kind: isViewKind(t.kind) ? "view" : "table",
      detail: `${t.schema} · ${kindLabel(t.kind)}`,
      description: formatRows(t.estimatedRows) || undefined,
      documentation: tableDocs(t, dialect),
      insertText: qualified,
      filterText: isDefault ? t.name : `${t.name} ${t.schema}.${t.name}`,
      sortText: `${used ? "1" : isDefault ? "2" : "3"}${t.name.toLowerCase()}${t.schema}`,
    });
  }
  // In a dotted catalog only top-level namespaces are offered here; deeper levels come after the dot.
  // Trino / Athena: every namespace is offered whole, as the one quoted schema they read it as.
  const whole = nestedNamespaceIsOneIdent(dialect);
  const shown = new Set<string>();
  for (const s of cat.schemas) {
    const label = cat.dotted && !whole ? s.split(".")[0] : s;
    if (shown.has(label.toLowerCase())) continue;
    shown.add(label.toLowerCase());
    const n = cat.dotted && !whole ? cat.schemas.filter((x) => x === label || x.toLowerCase().startsWith(label.toLowerCase() + ".")).reduce((acc, x) => acc + schemaTables(cat, x).length, 0) : schemaTables(cat, s).length;
    out.push({
      label,
      kind: "schema",
      detail: `${cat.dotted ? "namespace" : "schema"} · ${n} ${n === 1 ? "relation" : "relations"}`,
      insertText: `${cat.dotted ? schemaIdent(cat, label, force, dialect) : ident(label, force, dialect)}.`,
      filterText: label,
      sortText: `4${label.toLowerCase()}`,
      retrigger: true,
    });
  }
  return out;
}

function columnSuggestions(cols: ColumnEntry[], opts: { group: string; relName?: string; qualifyWith?: string | null; force: boolean; ambiguous?: Set<string>; outer?: boolean; dialect?: SqlDialect | null }): Suggestion[] {
  return cols.map((c, i) => {
    const ambiguous = opts.ambiguous?.has(c.name.toLowerCase());
    const bare = ident(c.name, opts.force, opts.dialect);
    const insertText = ambiguous && opts.qualifyWith ? `${opts.qualifyWith}.${bare}` : bare;
    const fk = c.table && c.info ? (foreignKeysByColumn(c.table).get(c.name)?.length ?? 0) > 0 : false;
    const flag = c.info?.isPrimaryKey ? " · PK" : fk ? " · FK" : "";
    return {
      label: c.name,
      kind: "column" as const,
      detail: c.info ? `${c.info.dataType}${flag}` : undefined,
      description: opts.relName && (ambiguous || opts.outer) ? opts.relName : undefined,
      documentation: columnDocs(c),
      insertText,
      filterText: c.name,
      sortText: `${opts.group}${pad(c.info?.ordinal ?? i)}${c.name.toLowerCase()}`,
    };
  });
}

function scopeColumns(a: Analysis, cat: Catalog | null, force: boolean, includeInto: boolean, dialect: SqlDialect | null): Suggestion[] {
  const env = { cat, tree: a.tree, ctes: a.scope.ctes };
  const rels = a.scope.relations.filter((r) => r.role !== "into" || includeInto);
  const colsPerRel = rels.map((r) => ({ r, cols: columnsOf(r, env) }));
  const counts = new Map<string, number>();
  for (const { cols } of colsPerRel) for (const c of cols) counts.set(c.name.toLowerCase(), (counts.get(c.name.toLowerCase()) ?? 0) + 1);
  const ambiguous = new Set([...counts].filter(([, n]) => n > 1).map(([k]) => k));
  const out: Suggestion[] = [];
  colsPerRel.forEach(({ r, cols }, idx) => {
    out.push(...columnSuggestions(cols, { group: `1${pad(idx, 2)}`, relName: exposedName(r), qualifyWith: ident(exposedName(r), false, dialect), force, ambiguous, dialect }));
  });
  // Correlated outer-query columns.
  const outerEnv = env;
  a.scope.outer.forEach((r, idx) => {
    out.push(...columnSuggestions(columnsOf(r, outerEnv), { group: `3${pad(idx, 2)}`, relName: exposedName(r), qualifyWith: ident(exposedName(r), false, dialect), force, ambiguous, outer: true, dialect }));
  });
  return out;
}

function aliasSuggestions(a: Analysis, dialect: SqlDialect | null): Suggestion[] {
  const seen = new Set<string>();
  const out: Suggestion[] = [];
  for (const r of [...a.scope.relations, ...a.scope.outer]) {
    const name = exposedName(r);
    if (!name || seen.has(name.toLowerCase())) continue;
    seen.add(name.toLowerCase());
    const target = r.parts.join(".") || "subquery";
    out.push({
      label: name,
      kind: "alias",
      detail: r.alias ? `alias · ${target}` : r.kind === "table" ? "table in query" : r.kind === "cte" ? "CTE" : "subquery",
      insertText: `${ident(name, false, dialect)}.`,
      filterText: name,
      sortText: `2${name.toLowerCase()}`,
      retrigger: true,
    });
  }
  return out;
}

function functionSuggestions(dialect: SqlDialect | null): Suggestion[] {
  return functionsFor(dialect).map((f) => ({
    label: f.name,
    kind: "function" as const,
    detail: `${f.name}${f.signature}`,
    documentation: f.doc,
    insertText: f.name === "count" ? "count(${1:*})" : `${f.name}(\${1})`,
    isSnippet: true,
    filterText: f.name,
    sortText: `5${f.agg ? "1" : "0"}${f.name}`,
  }));
}

function keywordSuggestions(words: readonly string[], group = "6", first: string[] = []): Suggestion[] {
  return words.map((w, i) => ({
    label: w,
    kind: "keyword" as const,
    insertText: w,
    filterText: w,
    sortText: `${group}${first.includes(w) ? "0" : "1"}${pad(i)}`,
  }));
}

function snippetSuggestions(prefix: string, dialect: SqlDialect | null): Suggestion[] {
  const typed = prefix.toLowerCase();
  return snippetsFor(dialect).map((s) => ({
    label: s.label,
    kind: "snippet" as const,
    detail: s.detail,
    documentation: "```sql\n" + s.body.replace(/\$\{\d:?([^}]*)\}/g, "$1").replace(/\$0/g, "") + "\n```\n" + s.doc,
    insertText: s.body,
    isSnippet: true,
    filterText: s.filter,
    // A snippet whose trigger is typed in full outranks the keyword that merely starts with it.
    sortText: typed === s.filter ? "00" + s.filter : "7" + s.filter,
  }));
}

function typeSuggestions(dialect: SqlDialect | null): Suggestion[] {
  return typeNamesFor(dialect).map((t, i) => ({ label: t, kind: "type" as const, insertText: t, filterText: t, sortText: `2${pad(i)}` }));
}

function joinSuggestions(cat: Catalog | null, ctx: Extract<Context, { kind: "on" }>, dialect: SqlDialect | null): Suggestion[] {
  if (!ctx.joinEligible || !ctx.right) return [];
  return joinConditions(cat, ctx.right, ctx.lefts, ctx.onText, dialect).map((j, i) => ({
    label: j.text,
    kind: "join" as const,
    detail: j.detail,
    description: "join",
    documentation: j.doc,
    insertText: j.text,
    filterText: j.text,
    sortText: `${j.fk ? "000" : "001"}${pad(i)}`,
    preselect: i === 0 && j.fk,
  }));
}

function qualifiedSuggestions(cat: Catalog | null, a: Analysis, qualifier: string[], dialect: SqlDialect): Suggestion[] {
  const force = a.quoted;
  const env = { cat, tree: a.tree, ctes: a.scope.ctes };
  const q = qualifier[qualifier.length - 1].toLowerCase();
  const out: Suggestion[] = [];

  if (qualifier.length === 1) {
    const rels = [...a.scope.relations, ...a.scope.outer].filter((r) => exposedName(r).toLowerCase() === q);
    if (rels.length) {
      const seen = new Set<string>();
      const cols: ColumnEntry[] = [];
      for (const r of rels) for (const c of columnsOf(r, env)) if (!seen.has(c.name)) (seen.add(c.name), cols.push(c));
      out.push(...columnSuggestions(cols, { group: "1", force, dialect }));
      return out;
    }
    // A table that is not in the query yet (`SELECT orders.| FROM …` written before the FROM).
    const t = lookupTable(cat, qualifier);
    if (t && resolveSchemaQualifier(cat, qualifier) === null) {
      return columnSuggestions(t.columns.map((c) => ({ name: c.name, info: c, table: t })), { group: "1", force, dialect });
    }
    if (cat && (resolveSchemaQualifier(cat, qualifier) !== null || childNamespaces(cat, qualifier).length > 0)) return relationSuggestions(cat, a, "", qualifier, dialect);
    return out;
  }
  // schema.table.| or db.schema.table.| (an Iceberg namespace path may be any number of levels)
  const t = lookupTable(cat, cat?.dotted ? qualifier : qualifier.slice(-2));
  if (t) return columnSuggestions(t.columns.map((c) => ({ name: c.name, info: c, table: t })), { group: "1", force, dialect });
  if (cat?.dotted && (resolveSchemaQualifier(cat, qualifier) !== null || childNamespaces(cat, qualifier).length > 0)) return relationSuggestions(cat, a, "", qualifier, dialect);
  return out;
}

function expressionSuggestions(cat: Catalog | null, a: Analysis, clause: Context & { kind: "expression" }, force: boolean, dialect: SqlDialect | null): Suggestion[] {
  const includeInto = clause.clause === "returning" || clause.clause === "conflict" || !a.scope.arm.some((it) => "t" in it && it.t.text.toLowerCase() === "select");
  const out = scopeColumns(a, cat, force, includeInto, dialect);
  if (clause.clause === "select" && a.scope.relations.length) {
    out.push({ label: "*", kind: "keyword", detail: "all columns", insertText: "*", sortText: "100", filterText: "*" });
  }
  out.push(...aliasSuggestions(a, dialect), ...functionSuggestions(dialect), ...keywordSuggestions(EXPRESSION_KEYWORDS));
  return out;
}

// -- entry point ------------------------------------------------------------------------------------

export function complete(cat: Catalog | null, sql: string, offset: number, opts: CompleteOptions = {}): CompletionResult | null {
  const a = analyzeAt(sql, offset);
  if (!a) return null;
  const ctx = a.ctx;
  const force = a.quoted;
  const trig = opts.triggerCharacter;
  const dialect = opts.dialect ?? null;
  const rel: SqlDialect = dialect ?? "postgres";
  let suggestions: Suggestion[] = [];

  // A space or comma only opens the list where a name is certainly expected next.
  if ((trig === " " || trig === ",") && !(ctx.kind === "relation" || (ctx.kind === "on" && ctx.joinEligible))) {
    return { suggestions: [], replaceStart: a.replaceStart, insertEnd: a.insertEnd, replaceEnd: a.replaceEnd };
  }

  switch (ctx.kind) {
    case "none":
      break;
    case "statementStart":
      suggestions = [...snippetSuggestions(a.prefix, dialect), ...keywordSuggestions(statementStartFor(dialect), "6", ["SELECT"])];
      break;
    case "relation":
      suggestions = relationSuggestions(cat, a, sql, ctx.qualifier, rel);
      break;
    case "qualified":
      suggestions = qualifiedSuggestions(cat, a, ctx.qualifier, rel);
      break;
    case "columnList": {
      const t = lookupTable(cat, ctx.table);
      if (t) suggestions = columnSuggestions(t.columns.map((c) => ({ name: c.name, info: c, table: t })), { group: "1", force, dialect });
      break;
    }
    case "type":
      suggestions = typeSuggestions(dialect);
      break;
    case "afterRelation": {
      if (a.prefix.replace(/^"/, "").length < 2) break; // too likely to be an alias being typed
      const words = ctx.clause === "values" ? ["VALUES", "SELECT", "DEFAULT VALUES"] : [...AFTER_RELATION_KEYWORDS, ...afterRelationExtras(dialect)];
      const first = ctx.clause === "join" ? ["ON", "USING"] : ctx.clause === "set" || ctx.clause === "other" ? ["SET", "WHERE"] : ["WHERE", "JOIN"];
      suggestions = keywordSuggestions(words, "1", first);
      break;
    }
    case "on":
      suggestions = [...joinSuggestions(cat, ctx, dialect), ...expressionSuggestions(cat, a, { kind: "expression", clause: "on" }, force, dialect)];
      break;
    case "expression":
      suggestions = expressionSuggestions(cat, a, ctx, force, dialect);
      break;
  }
  return { suggestions, replaceStart: a.replaceStart, insertEnd: a.insertEnd, replaceEnd: a.replaceEnd };
}

/** Used by hover and tests: the catalog table a relation refers to. */
export const relationTable = (r: Relation, cat: Catalog | null) => tableOf(r, cat);
export { quoteIdent };
