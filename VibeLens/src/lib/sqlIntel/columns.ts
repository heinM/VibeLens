// Column resolution for relations in scope: catalog tables, CTEs, derived tables (with `SELECT *`
// expansion) and set-returning functions.
import type { ColumnInfo, TableDescription } from "../../ipc/types";
import type { Tree } from "./blocks";
import { type Catalog, lookupTable } from "./catalog";
import { type CteDef, type OutputColumn, type Relation, bodyRelations, exposedName, outputColumns } from "./scope";

export interface ColumnEntry {
  name: string;
  /** Present when the column comes straight from a catalog table. */
  info?: ColumnInfo;
  table?: TableDescription;
}

export interface ColumnEnv {
  cat: Catalog | null;
  tree: Tree;
  ctes: CteDef[];
  depth?: number;
}

const MAX_DEPTH = 4;

function fromTable(t: TableDescription): ColumnEntry[] {
  return t.columns.map((c) => ({ name: c.name, info: c, table: t }));
}

function expand(oc: OutputColumn[], rels: Relation[], env: ColumnEnv): ColumnEntry[] {
  const out: ColumnEntry[] = [];
  for (const c of oc) {
    if (c.kind === "name") out.push({ name: c.name });
    else {
      const q = c.qualifier?.toLowerCase();
      for (const r of rels) {
        if (q && exposedName(r).toLowerCase() !== q) continue;
        out.push(...columnsOf(r, env));
      }
    }
  }
  return out;
}

/** Columns a relation exposes. Unknown tables yield an empty list (never throws). */
export function columnsOf(rel: Relation, env: ColumnEnv): ColumnEntry[] {
  const depth = env.depth ?? 0;
  if (depth > MAX_DEPTH) return [];
  const names = (list: string[]) => list.map((name) => ({ name }));
  switch (rel.kind) {
    case "table": {
      const t = lookupTable(env.cat, rel.parts);
      const cols = t ? fromTable(t) : [];
      return rel.aliasColumns ? cols.map((c, i) => (rel.aliasColumns?.[i] ? { ...c, name: rel.aliasColumns[i] } : c)) : cols;
    }
    case "function":
      return rel.aliasColumns ? names(rel.aliasColumns) : [];
    case "cte": {
      const def = env.ctes.find((c) => c.body === rel.body);
      if (def?.columns) return names(def.columns);
      if (!rel.body) return [];
      const next = { ...env, depth: depth + 1 };
      return expand(outputColumns(env.tree, rel.body), bodyRelations(env.tree, rel.body, env.ctes), next);
    }
    case "derived": {
      if (rel.aliasColumns) return names(rel.aliasColumns);
      if (!rel.body) return [];
      const next = { ...env, depth: depth + 1 };
      return expand(outputColumns(env.tree, rel.body), bodyRelations(env.tree, rel.body, env.ctes), next);
    }
  }
}

/** The catalog table behind a relation, when it is a plain table reference. */
export const tableOf = (rel: Relation, cat: Catalog | null): TableDescription | undefined => (rel.kind === "table" ? lookupTable(cat, rel.parts) : undefined);
