// Hover cards: a table name shows its columns, a column shows its type, flags and comment.
import type { TableDescription } from "../../ipc/types";
import { buildTree } from "./blocks";
import { type Catalog, describeColumnBadges, formatRows, hasSchema, kindLabel, lookupTable, schemaTables } from "./catalog";
import { statementTokens, tokenizeCached } from "./analysis";
import { columnsOf } from "./columns";
import { type Relation, exposedName, scopeAt } from "./scope";
import { type Token, identValue, insideNonCode, isNameToken } from "./tokenizer";

export interface HoverInfo {
  start: number;
  end: number;
  markdown: string;
}

const MAX_COLS = 30;

function tableCard(t: TableDescription, alias?: string | null): string {
  const rows = formatRows(t.estimatedRows);
  const head = `**${t.schema}.${t.name}**${alias ? ` as \`${alias}\`` : ""} · ${kindLabel(t.kind)}${rows ? ` · ${rows}` : ""}`;
  const width = Math.min(28, Math.max(...t.columns.map((c) => c.name.length), 4));
  const lines = t.columns.slice(0, MAX_COLS).map((c) => {
    const flags: string[] = [];
    if (c.isPrimaryKey) flags.push("PK");
    for (const fk of t.foreignKeys.filter((f) => f.columns.includes(c.name))) flags.push(`FK → ${fk.referencedTable}`);
    if (!c.nullable && !c.isPrimaryKey) flags.push("not null");
    return `${c.name.padEnd(width)}  ${c.dataType}${flags.length ? "  " + flags.join(", ") : ""}`;
  });
  const more = t.columns.length > MAX_COLS ? `\n… ${t.columns.length - MAX_COLS} more columns` : "";
  return `${head}${t.comment ? `\n\n${t.comment}` : ""}\n\n\`\`\`plaintext\n${lines.join("\n")}${more}\n\`\`\``;
}

function columnCard(t: TableDescription, name: string): string | null {
  const c = t.columns.find((x) => x.name.toLowerCase() === name.toLowerCase());
  if (!c) return null;
  return `**${t.name}.${c.name}** \`${c.dataType}\`\n\n${describeColumnBadges(t, c).join(" · ")}${c.comment ? `\n\n${c.comment}` : ""}`;
}

/** Name chain around the token at `idx`: [a, b, c] for `a.b.c`, plus the hovered position in it. */
function chainAround(toks: Token[], idx: number): { parts: Token[]; at: number } {
  let lo = idx;
  while (lo >= 2 && toks[lo - 1].kind === "punct" && toks[lo - 1].text === "." && isNameToken(toks[lo - 2])) lo -= 2;
  let hi = idx;
  while (hi + 2 < toks.length && toks[hi + 1].kind === "punct" && toks[hi + 1].text === "." && isNameToken(toks[hi + 2])) hi += 2;
  const parts: Token[] = [];
  for (let i = lo; i <= hi; i += 2) parts.push(toks[i]);
  return { parts, at: (idx - lo) / 2 };
}

export function hoverAt(cat: Catalog | null, sql: string, offset: number): HoverInfo | null {
  if (!cat) return null;
  const all = tokenizeCached(sql);
  if (insideNonCode(all, offset)) return null;
  const toks = statementTokens(all, offset);
  const idx = toks.findIndex((t) => isNameToken(t) && t.start <= offset && offset <= t.end);
  if (idx < 0) return null;
  const tok = toks[idx];
  const tree = buildTree(toks);
  const scope = scopeAt(tree, offset);
  const env = { cat, tree, ctes: scope.ctes };
  const rels: Relation[] = [...scope.relations, ...scope.outer];
  const { parts, at } = chainAround(toks, idx);
  const names = parts.map(identValue);
  const range = { start: tok.start, end: tok.end };
  const hit = (markdown: string): HoverInfo => ({ ...range, markdown });
  const relByName = (n: string) => rels.find((r) => exposedName(r).toLowerCase() === n.toLowerCase());

  // The hovered token is the table/alias in the FROM clause itself.
  const own = scope.relations.find((r) => (tok.start >= r.nameStart && tok.end <= r.nameEnd) || (tok.start >= r.aliasStart && tok.end <= r.aliasEnd && r.aliasStart >= 0));
  if (own && own.kind === "table") {
    // In `shop.orders`, hovering `shop` is the schema, not the table.
    const isSchemaPart = own.parts.length > 1 && at < names.length - 1 && tok.end <= own.nameEnd && tok.start >= own.nameStart && !(tok.start >= own.aliasStart && own.aliasStart >= 0);
    if (!isSchemaPart) {
      const t = lookupTable(cat, own.parts);
      if (t) return hit(tableCard(t, own.alias && own.alias !== t.name ? own.alias : null));
    }
  }

  if (names.length === 1) {
    const n = names[0];
    // alias or table name used as a qualifier or bare reference
    const rel = relByName(n);
    if (rel && toks[idx + 1]?.text === ".") {
      const t = rel.kind === "table" ? lookupTable(cat, rel.parts) : undefined;
      if (t) return hit(tableCard(t, rel.alias));
      const cols = columnsOf(rel, env);
      if (cols.length) return hit(`**${exposedName(rel)}** · ${rel.kind === "cte" ? "CTE" : "subquery"}\n\n\`\`\`plaintext\n${cols.slice(0, MAX_COLS).map((c) => c.name).join("\n")}\n\`\`\``);
    }
    // bare column among the relations in scope
    const matches: { rel: Relation; entry: ReturnType<typeof columnsOf>[number] }[] = [];
    for (const r of scope.relations) for (const c of columnsOf(r, env)) if (c.name.toLowerCase() === n.toLowerCase()) matches.push({ rel: r, entry: c });
    if (matches.length === 1 || (matches.length > 1 && matches[0].entry.table)) {
      const m = matches[0];
      if (m.entry.table) {
        const card = columnCard(m.entry.table, m.entry.name);
        if (card) return hit(matches.length > 1 ? `${card}\n\n_Also in ${matches.slice(1).map((x) => exposedName(x.rel)).join(", ")}; qualify to disambiguate._` : card);
      }
      return hit(`**${exposedName(m.rel)}.${m.entry.name}**`);
    }
    const t = lookupTable(cat, [n]);
    if (t) return hit(tableCard(t));
    if (hasSchema(cat, n)) return hit(`**${n}** · schema · ${schemaTables(cat, n).length} relations`);
    return null;
  }

  // Qualified chain.
  const upTo = names.slice(0, at + 1);
  if (at === names.length - 1) {
    const qual = names.slice(0, -1);
    // alias.column / table.column
    if (qual.length === 1) {
      const rel = relByName(qual[0]);
      if (rel) {
        const cols = columnsOf(rel, env);
        const col = cols.find((c) => c.name.toLowerCase() === names[at].toLowerCase());
        if (col?.table) {
          const card = columnCard(col.table, col.name);
          if (card) return hit(card);
        }
        if (col) return hit(`**${exposedName(rel)}.${col.name}**`);
      }
    }
    // schema.table.column
    if (qual.length >= 2) {
      const t = lookupTable(cat, qual.slice(-2));
      const card = t ? columnCard(t, names[at]) : null;
      if (card) return hit(card);
    }
    // schema.table
    const t = lookupTable(cat, upTo.slice(-2));
    if (t && hasSchema(cat, qual[qual.length - 1])) return hit(tableCard(t));
    return null;
  }
  // hovered a qualifier part
  if (at === 0 && !relByName(names[0]) && hasSchema(cat, names[0])) return hit(`**${names[0]}** · schema · ${schemaTables(cat, names[0]).length} relations`);
  const rel = relByName(upTo[at]);
  const t = rel ? (rel.kind === "table" ? lookupTable(cat, rel.parts) : undefined) : lookupTable(cat, upTo.slice(-2));
  if (t) return hit(tableCard(t, rel?.alias));
  return null;
}
