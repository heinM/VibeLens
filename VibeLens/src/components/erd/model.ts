// Pure ER model: turns a schema catalog into tables + FK edges, applies the table cap and filter,
// and finds connected clusters. No React, no layout geometry (see layout.ts).
import type { ObjectKind, TableDescription } from "../../ipc/types";
import { fuzzyMatch } from "../palette/fuzzy";

/** Above this many tables only the most-connected ones are drawn. */
export const ERD_MAX_TABLES = 150;
/** Tables with more columns than this start collapsed to their name. */
export const COLLAPSE_THRESHOLD = 12;

const DRAWN_KINDS: ReadonlySet<ObjectKind> = new Set(["table", "partitionedTable", "foreignTable"]);

export interface ErdColumn {
  name: string;
  type: string;
  pk: boolean;
  fk: boolean;
  nullable: boolean;
}

export interface ErdTable {
  /** `schema.name` */
  key: string;
  schema: string;
  name: string;
  kind: ObjectKind;
  rows: number | null;
  comment: string | null;
  columns: ErdColumn[];
  /** Number of FK edges touching this table (self references excluded). Computed on the unfiltered schema. */
  degree: number;
  /** Connected-component index (0 = largest); -1 for tables with no relations in the drawn set. */
  cluster: number;
  source: TableDescription;
}

export interface ErdEdge {
  id: string;
  /** Table holding the FK. */
  from: string;
  /** Referenced table. */
  to: string;
  fromCols: string[];
  toCols: string[];
  name: string;
  self: boolean;
}

export interface ErdModel {
  schema: string;
  tables: ErdTable[];
  edges: ErdEdge[];
  /** Tables of this schema before cap/filter. */
  total: number;
  /** Tables not drawn because of the cap or the filter. */
  hidden: number;
  /** True when the 150-table cap (not the filter) removed tables. */
  capped: boolean;
  clusterCount: number;
}

export const tableKey = (schema: string, name: string) => `${schema}.${name}`;

export function isCollapsible(t: Pick<ErdTable, "columns">): boolean {
  return t.columns.length > COLLAPSE_THRESHOLD;
}

/** Is the table drawn as just its header? */
export function isCollapsed(t: Pick<ErdTable, "columns" | "key">, expanded: ReadonlySet<string>): boolean {
  return isCollapsible(t) && !expanded.has(t.key);
}

export interface BuildOptions {
  limit?: number;
  /** Fuzzy table-name filter applied before the cap. */
  filter?: string;
}

/** Edges between tables of `schema` only; FKs leaving the schema are not drawn (the details panel lists them). */
export function collectEdges(tables: readonly TableDescription[], schema: string): ErdEdge[] {
  const present = new Set(tables.filter((t) => t.schema === schema).map((t) => tableKey(t.schema, t.name)));
  const edges: ErdEdge[] = [];
  for (const t of tables) {
    if (t.schema !== schema) continue;
    const from = tableKey(t.schema, t.name);
    for (const fk of t.foreignKeys) {
      if (fk.referencedSchema !== schema) continue;
      const to = tableKey(fk.referencedSchema, fk.referencedTable);
      if (!present.has(to)) continue;
      edges.push({ id: `${from}>${fk.name}`, from, to, fromCols: fk.columns, toCols: fk.referencedColumns, name: fk.name, self: from === to });
    }
  }
  return edges;
}

export function buildErdModel(catalog: readonly TableDescription[], schema: string, opts: BuildOptions = {}): ErdModel {
  const limit = opts.limit ?? ERD_MAX_TABLES;
  const inSchema = catalog.filter((t) => t.schema === schema && DRAWN_KINDS.has(t.kind));
  const allEdges = collectEdges(inSchema, schema);

  const degree = new Map<string, number>();
  for (const e of allEdges) {
    if (e.self) continue;
    degree.set(e.from, (degree.get(e.from) ?? 0) + 1);
    degree.set(e.to, (degree.get(e.to) ?? 0) + 1);
  }

  let candidates = inSchema;
  const filter = opts.filter?.trim();
  if (filter) candidates = candidates.filter((t) => fuzzyMatch(filter, t.name) !== null);

  let capped = false;
  if (candidates.length > limit) {
    capped = true;
    candidates = [...candidates]
      .sort((a, b) => {
        const da = degree.get(tableKey(a.schema, a.name)) ?? 0;
        const db = degree.get(tableKey(b.schema, b.name)) ?? 0;
        return db - da || (b.estimatedRows ?? 0) - (a.estimatedRows ?? 0) || a.name.localeCompare(b.name);
      })
      .slice(0, limit);
  }

  const keys = new Set(candidates.map((t) => tableKey(t.schema, t.name)));
  const edges = allEdges.filter((e) => keys.has(e.from) && keys.has(e.to));
  const clusters = clusterOf(keys, edges);

  const fkCols = (t: TableDescription) => new Set(t.foreignKeys.flatMap((f) => f.columns));
  const tables: ErdTable[] = candidates
    .map((t): ErdTable => {
      const key = tableKey(t.schema, t.name);
      const fks = fkCols(t);
      return {
        key,
        schema: t.schema,
        name: t.name,
        kind: t.kind,
        rows: t.estimatedRows,
        comment: t.comment,
        columns: [...t.columns]
          .sort((a, b) => a.ordinal - b.ordinal)
          .map((c) => ({ name: c.name, type: c.dataType, pk: c.isPrimaryKey, fk: fks.has(c.name), nullable: c.nullable })),
        degree: degree.get(key) ?? 0,
        cluster: clusters.index.get(key) ?? -1,
        source: t,
      };
    })
    .sort((a, b) => a.name.localeCompare(b.name));

  return {
    schema,
    tables,
    edges,
    total: inSchema.length,
    hidden: inSchema.length - candidates.length,
    capped,
    clusterCount: clusters.count,
  };
}

/**
 * Connected components over the edge list (ignoring direction and self loops). Components of size
 * >= 2 are numbered 0.. by descending size; lone tables are absent from `index` (cluster -1).
 */
export function clusterOf(keys: ReadonlySet<string>, edges: readonly ErdEdge[]): { index: Map<string, number>; count: number; members: string[][] } {
  const parent = new Map<string, string>();
  for (const k of keys) parent.set(k, k);
  const find = (x: string): string => {
    let r = x;
    while (parent.get(r) !== r) r = parent.get(r) as string;
    let c = x;
    while (parent.get(c) !== r) {
      const next = parent.get(c) as string;
      parent.set(c, r);
      c = next;
    }
    return r;
  };
  for (const e of edges) {
    if (e.self || !parent.has(e.from) || !parent.has(e.to)) continue;
    const a = find(e.from);
    const b = find(e.to);
    if (a !== b) parent.set(a, b);
  }
  const groups = new Map<string, string[]>();
  for (const k of keys) {
    const r = find(k);
    const g = groups.get(r);
    if (g) g.push(k);
    else groups.set(r, [k]);
  }
  const multi = [...groups.values()].filter((g) => g.length > 1).sort((a, b) => b.length - a.length || a[0].localeCompare(b[0]));
  const index = new Map<string, number>();
  multi.forEach((g, i) => g.forEach((k) => index.set(k, i)));
  return { index, count: multi.length, members: multi };
}

/** Tables referencing `key` (children) and tables `key` references (parents), by FK. */
export function relationsOf(model: Pick<ErdModel, "edges">, key: string): { parents: ErdEdge[]; children: ErdEdge[] } {
  return {
    parents: model.edges.filter((e) => e.from === key && !e.self),
    children: model.edges.filter((e) => e.to === key && !e.self),
  };
}

/** The set of table keys directly related to `key`, including itself. */
export function neighbourhood(model: Pick<ErdModel, "edges">, key: string): Set<string> {
  const s = new Set<string>([key]);
  for (const e of model.edges) {
    if (e.from === key) s.add(e.to);
    else if (e.to === key) s.add(e.from);
  }
  return s;
}
