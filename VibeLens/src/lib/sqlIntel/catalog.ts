// Indexed view over a SchemaMetadata, built once per fetched catalog (WeakMap-memoised), so the
// completion hot path is hash lookups and never walks thousands of tables.
import type { ColumnInfo, ForeignKeyInfo, SchemaMetadata, TableDescription } from "../../ipc/types";
import { RESERVED_WORDS } from "./keywords";

export interface Catalog {
  meta: SchemaMetadata;
  /** Browsable relations only (tables, partitions roots, views, matviews, foreign tables). */
  tables: TableDescription[];
  /** lower(schema) → relations in that schema, name-sorted. */
  bySchema: Map<string, TableDescription[]>;
  /** lower(name) → relations with that name in any schema. */
  byName: Map<string, TableDescription[]>;
  /** Schema display names: the default schema first (`public` for Postgres), then alphabetical. */
  schemas: string[];
  /** Iceberg: schema names are dotted namespace paths (`lake.sales`), so qualified names have more than two parts. */
  dotted: boolean;
  /** Schema that bare table names resolve to and that completion leaves unqualified (lower-case). */
  defaultSchema: string | null;
}

export interface CatalogOptions {
  /** Schemas are dotted namespace paths (Iceberg). */
  dotted?: boolean;
  /** Schema that unqualified names resolve to. Defaults to `public` (Postgres); pass null for none. */
  defaultSchema?: string | null;
}

const BROWSABLE = new Set(["table", "partitionedTable", "view", "materializedView", "foreignTable"]);
const memo = new WeakMap<SchemaMetadata, Catalog>();

export function indexCatalog(meta: SchemaMetadata, opts: CatalogOptions = {}): Catalog {
  const dotted = opts.dotted ?? false;
  const defaultSchema = opts.defaultSchema === undefined ? "public" : opts.defaultSchema === null ? null : opts.defaultSchema.toLowerCase();
  const hit = memo.get(meta);
  if (hit && hit.dotted === dotted && hit.defaultSchema === defaultSchema) return hit;
  const tables = meta.tables.filter((t) => BROWSABLE.has(t.kind));
  const bySchema = new Map<string, TableDescription[]>();
  const byName = new Map<string, TableDescription[]>();
  const schemaNames = new Map<string, string>();
  for (const t of tables) {
    const sk = t.schema.toLowerCase();
    schemaNames.set(sk, t.schema);
    (bySchema.get(sk) ?? bySchema.set(sk, []).get(sk)!).push(t);
    const nk = t.name.toLowerCase();
    (byName.get(nk) ?? byName.set(nk, []).get(nk)!).push(t);
  }
  for (const list of bySchema.values()) list.sort((a, b) => a.name.localeCompare(b.name));
  const isDefault = (n: string) => defaultSchema !== null && n.toLowerCase() === defaultSchema;
  const schemas = [...schemaNames.values()].sort((a, b) => (isDefault(a) ? -1 : isDefault(b) ? 1 : a.localeCompare(b)));
  const cat: Catalog = { meta, tables, bySchema, byName, schemas, dotted, defaultSchema };
  memo.set(meta, cat);
  return cat;
}

/** `orders@123` (Iceberg time travel) and `orders$snapshots` (metadata table) resolve to the table `orders`. */
const baseTableName = (name: string): string => {
  const cut = name.search(/[@$]/);
  return cut > 0 ? name.slice(0, cut) : name;
};

/** True when `schema` is the catalog's default schema (unqualified names resolve there). */
export const isDefaultSchema = (cat: Catalog | null, schema: string): boolean => !!cat && cat.defaultSchema !== null && schema.toLowerCase() === cat.defaultSchema;

/**
 * Resolves a possibly-qualified table reference. A bare name prefers the default schema (`public`),
 * then the only match, then the first match (search_path is unknown to the client). In a dotted
 * (Iceberg) catalog everything before the last part is tried as one namespace path first.
 */
export function lookupTable(cat: Catalog | null, parts: string[]): TableDescription | undefined {
  if (!cat || parts.length === 0) return undefined;
  const last = parts[parts.length - 1].toLowerCase();
  const bases = [last];
  if (cat.dotted && baseTableName(last) !== last) bases.push(baseTableName(last));
  if (parts.length >= 2) {
    const schemas = cat.dotted && parts.length >= 3 ? [parts.slice(0, -1).join(".").toLowerCase(), parts[parts.length - 2].toLowerCase()] : [parts[parts.length - 2].toLowerCase()];
    for (const schema of schemas) {
      const list = cat.bySchema.get(schema);
      if (!list) continue;
      for (const name of bases) {
        const t = list.find((x) => x.name.toLowerCase() === name);
        if (t) return t;
      }
    }
    return undefined;
  }
  for (const name of bases) {
    const all = cat.byName.get(name);
    if (!all) continue;
    return all.find((t) => isDefaultSchema(cat, t.schema)) ?? all[0];
  }
  return undefined;
}

/** The schema (namespace path) a qualifier like `a.b` or `db.schema` names, if it exists in the catalog. */
export function resolveSchemaQualifier(cat: Catalog | null, qualifier: string[]): string | null {
  if (!cat || qualifier.length === 0) return null;
  if (cat.dotted && hasSchema(cat, qualifier.join("."))) return cat.bySchema.get(qualifier.join(".").toLowerCase())?.[0]?.schema ?? qualifier.join(".");
  const one = qualifier.length >= 2 ? qualifier[qualifier.length - 1] : qualifier[0];
  return hasSchema(cat, one) ? (cat.bySchema.get(one.toLowerCase())?.[0]?.schema ?? one) : null;
}

/** Next namespace levels below `parent` (dotted catalogs): `lake` → `sales`, `marketing`. */
export function childNamespaces(cat: Catalog | null, parent: string[]): string[] {
  if (!cat || !cat.dotted || parent.length === 0) return [];
  const head = parent.join(".").toLowerCase() + ".";
  const out = new Map<string, string>();
  for (const s of cat.schemas) {
    if (!s.toLowerCase().startsWith(head)) continue;
    const next = s.slice(head.length).split(".")[0];
    if (next) out.set(next.toLowerCase(), next);
  }
  return [...out.values()].sort();
}

export const hasSchema = (cat: Catalog | null, name: string): boolean => !!cat && cat.bySchema.has(name.toLowerCase());
export const schemaTables = (cat: Catalog | null, name: string): TableDescription[] => cat?.bySchema.get(name.toLowerCase()) ?? [];

// -- per-table derived data (FK per column) ------------------------------------------------------

const fkMemo = new WeakMap<TableDescription, Map<string, ForeignKeyInfo[]>>();

/** column name → foreign keys that include it. */
export function foreignKeysByColumn(t: TableDescription): Map<string, ForeignKeyInfo[]> {
  let m = fkMemo.get(t);
  if (!m) {
    m = new Map();
    for (const fk of t.foreignKeys) for (const c of fk.columns) (m.get(c) ?? m.set(c, []).get(c)!).push(fk);
    fkMemo.set(t, m);
  }
  return m;
}

export function describeColumnBadges(t: TableDescription, c: ColumnInfo): string[] {
  const out: string[] = [];
  if (c.isPrimaryKey) out.push("Primary key");
  for (const fk of foreignKeysByColumn(t).get(c.name) ?? []) {
    const i = fk.columns.indexOf(c.name);
    const target = fk.referencedColumns[i] ?? fk.referencedColumns[0] ?? "?";
    out.push(`Foreign key → ${fk.referencedSchema === "public" ? "" : fk.referencedSchema + "."}${fk.referencedTable}.${target}`);
  }
  out.push(c.nullable ? "nullable" : "not null");
  if (c.default) out.push(`default ${c.default}`);
  return out;
}

export function formatRows(n: number | null): string {
  if (n === null || n < 0) return "";
  if (n >= 1e9) return `~${(n / 1e9).toFixed(1).replace(/\.0$/, "")}B rows`;
  if (n >= 1e6) return `~${(n / 1e6).toFixed(1).replace(/\.0$/, "")}M rows`;
  if (n >= 1e3) return `~${(n / 1e3).toFixed(1).replace(/\.0$/, "")}k rows`;
  return `~${Math.round(n)} rows`;
}

export const kindLabel = (k: TableDescription["kind"]): string =>
  k === "view" ? "view" : k === "materializedView" ? "materialized view" : k === "foreignTable" ? "foreign table" : k === "partitionedTable" ? "partitioned table" : "table";

// -- identifier quoting ---------------------------------------------------------------------------

/** True if `name` must be double-quoted to be read back as the same identifier. */
export function needsQuoting(name: string): boolean {
  return !/^[a-z_][a-z0-9_$]*$/.test(name) || RESERVED_WORDS.has(name);
}

export function quoteIdent(name: string): string {
  return `"${name.replace(/"/g, '""')}"`;
}

/** The text to insert for an identifier: bare when safe, quoted otherwise (or when forced). */
export function ident(name: string, force = false): string {
  return force || needsQuoting(name) ? quoteIdent(name) : name;
}

/** A schema name as SQL: a dotted catalog (Iceberg) writes each namespace level as its own identifier. */
export function schemaIdent(cat: Catalog | null, schema: string, force = false): string {
  return cat?.dotted ? schema.split(".").map((p) => ident(p, force)).join(".") : ident(schema, force);
}
