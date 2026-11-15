// SQL template generators for the navigator's "Generate" menu. Pure: they take a table
// description and return text for a new SQL tab. Identifiers are always quoted.
import type { ColumnInfo, TableDescription } from "../../ipc/types";

export const quoteIdent = (s: string) => `"${s.replace(/"/g, '""')}"`;
export const qualified = (t: Pick<TableDescription, "schema" | "name">) => `${quoteIdent(t.schema)}.${quoteIdent(t.name)}`;

/** A harmless, correctly typed example literal for a column type, to make templates runnable after a quick edit. */
export function sampleValue(dataType: string): string {
  const t = dataType.toLowerCase();
  if (/\[\]$/.test(t) || t.startsWith("_")) return "'{}'";
  if (/^(bool|boolean)$/.test(t)) return "false";
  if (/^(int|integer|smallint|bigint|int2|int4|int8|serial|bigserial|smallserial)\b/.test(t)) return "0";
  if (/^(numeric|decimal|real|double|float|money)/.test(t)) return "0";
  if (/^uuid$/.test(t)) return "gen_random_uuid()";
  if (/^timestamp/.test(t)) return "now()";
  if (/^date$/.test(t)) return "CURRENT_DATE";
  if (/^time/.test(t)) return "CURRENT_TIME";
  if (/^interval/.test(t)) return "'0 seconds'";
  if (/^jsonb?$/.test(t)) return "'{}'";
  if (/^bytea$/.test(t)) return "'\\x'";
  return "''";
}

const isGeneratedDefault = (c: ColumnInfo): boolean => !!c.default && /^nextval\(|generated|^gen_random_uuid\(\)|^uuid_generate/i.test(c.default);
const byOrdinal = (a: ColumnInfo, b: ColumnInfo) => a.ordinal - b.ordinal;

/** `SELECT` with every column listed (so the editor can complete and trim it), limited to 100 rows. */
export function generateSelect(t: TableDescription): string {
  const cols = [...t.columns].sort(byOrdinal);
  const list = cols.length ? cols.map((c) => `    ${quoteIdent(c.name)}`).join(",\n") : "    *";
  return `SELECT\n${list}\nFROM ${qualified(t)}\nLIMIT 100;\n`;
}

/** `INSERT` with a typed example value (and a comment with the column type) per column. Generated keys are skipped. */
export function generateInsert(t: TableDescription): string {
  const cols = [...t.columns].sort(byOrdinal).filter((c) => !isGeneratedDefault(c));
  if (cols.length === 0) return `INSERT INTO ${qualified(t)} DEFAULT VALUES;\n`;
  const names = cols.map((c) => `    ${quoteIdent(c.name)}`).join(",\n");
  const values = cols
    .map((c, i) => {
      const v = c.default && !c.nullable ? "DEFAULT" : c.nullable && !c.isPrimaryKey ? "NULL" : sampleValue(c.dataType);
      const comma = i < cols.length - 1 ? "," : "";
      return `    ${v}${comma}  -- ${c.name} ${c.dataType}${c.nullable ? "" : " not null"}`;
    })
    .join("\n");
  return `INSERT INTO ${qualified(t)} (\n${names}\n)\nVALUES (\n${values}\n);\n`;
}

/**
 * `UPDATE` setting every non-key column, guarded by the primary key. Without a primary key the
 * WHERE is `false` so a stray Run can never rewrite the whole table.
 */
export function generateUpdate(t: TableDescription): string {
  const cols = [...t.columns].sort(byOrdinal);
  const pk = cols.filter((c) => c.isPrimaryKey);
  const settable = cols.filter((c) => !c.isPrimaryKey && !isGeneratedDefault(c));
  const set = (settable.length ? settable : cols).map((c, i, arr) => `    ${quoteIdent(c.name)} = ${sampleValue(c.dataType)}${i < arr.length - 1 ? "," : ""}  -- ${c.dataType}`).join("\n");
  const where = pk.length
    ? pk.map((c) => `${quoteIdent(c.name)} = ${sampleValue(c.dataType)}`).join("\n  AND ")
    : "false  -- no primary key: write the condition that picks the rows to change";
  return `UPDATE ${qualified(t)}\nSET\n${set}\nWHERE ${where};\n`;
}

/** Plain browse query used by "Open in new SQL tab". */
export function selectStar(schema: string, name: string): string {
  return `SELECT * FROM ${quoteIdent(schema)}.${quoteIdent(name)} LIMIT 100;`;
}
