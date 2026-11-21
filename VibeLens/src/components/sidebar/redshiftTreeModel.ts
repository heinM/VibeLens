// Pure model for the Redshift navigator: schemas > tables and views > columns, for the connection's own
// database. Turns cached Data API answers plus the expansion state into the flat row list the tree renders.
import type { RedshiftColumn, RedshiftTableRef } from "../../ipc/types";
import type { QueryState } from "./pgTreeModel";

/** Same quoting as `useTabActions.quoteIdent` (kept local so this model stays free of stores). */
const quoteIdent = (s: string) => `"${s.replace(/"/g, '""')}"`;

const SEP = "\u0001";
export const rsSchemaId = (schema: string) => `s${SEP}${schema}`;
export const rsTableId = (schema: string, name: string) => `t${SEP}${schema}${SEP}${name}`;
/** Key shared by the describe map and the expanded-table list. */
export const rsTableKey = (schema: string, name: string) => `${schema}${SEP}${name}`;

export type RsRowKind = "schema" | "table" | "column" | "status";

export interface RsRow {
  id: string;
  depth: number;
  kind: RsRowKind;
  label: string;
  expandable: boolean;
  expanded: boolean;
  schema?: string;
  table?: RedshiftTableRef;
  column?: RedshiftColumn;
  statusKind?: "loading" | "error" | "empty";
}

export interface RsModelInput {
  schemas: QueryState<string[]>;
  /** keyed by schema name */
  tables: ReadonlyMap<string, QueryState<RedshiftTableRef[]>>;
  /** keyed by rsTableKey() */
  describes: ReadonlyMap<string, QueryState<RedshiftColumn[]>>;
  toggled: ReadonlyMap<string, boolean>;
  showSystem: boolean;
}

export const rsIsExpanded = (toggled: ReadonlyMap<string, boolean>, id: string): boolean => toggled.get(id) ?? false;

/** Redshift's own schemas: the catalog, information_schema and the pg_* internals (pg_automv, pg_internal, …). */
export const isSystemSchema = (s: string): boolean => {
  const n = s.toLowerCase();
  return n === "information_schema" || n.startsWith("pg_");
};

/** Views are reported as VIEW (and MATERIALIZED VIEW where Redshift says so). */
export const isViewKind = (kind: string): boolean => /\bVIEW\b/i.test(kind);

/** `varchar(256)`, `numeric(18,2)`, `int4`. */
export function columnType(c: RedshiftColumn): string {
  const t = c.typeName;
  if (/\(/.test(t)) return t;
  const lc = t.toLowerCase();
  if ((lc.includes("char") || lc === "varbyte" || lc === "varbinary") && c.length != null && c.length > 0) return `${t}(${c.length})`;
  if ((lc === "numeric" || lc === "decimal") && c.precision != null && c.precision > 0) return `${t}(${c.precision}${c.scale != null ? `,${c.scale}` : ""})`;
  return t;
}

/** `"schema"."table"` as the query tab writes it. */
export const rsQualifiedName = (schema: string, name: string): string => `${quoteIdent(schema)}.${quoteIdent(name)}`;
export const rsSelectAllSql = (schema: string, name: string, limit = 100): string => `SELECT * FROM ${rsQualifiedName(schema, name)} LIMIT ${limit};`;

const byName = (a: string, b: string) => a.localeCompare(b, undefined, { sensitivity: "base", numeric: true });

function status(depth: number, statusKind: "loading" | "error" | "empty", label: string, id: string): RsRow {
  return { id, depth, kind: "status", label, expandable: false, expanded: false, statusKind };
}

export function buildRsRows(m: RsModelInput): RsRow[] {
  const out: RsRow[] = [];
  if (m.schemas.status === "pending") return [status(0, "loading", "Loading schemas…", "root-loading")];
  if (m.schemas.status === "error") return [status(0, "error", m.schemas.error ?? "Could not list schemas", "root-error")];
  const schemas = [...(m.schemas.data ?? [])].filter((s) => m.showSystem || !isSystemSchema(s)).sort(byName);
  if (schemas.length === 0) return [status(0, "empty", "No schemas", "root-empty")];

  for (const schema of schemas) {
    const sid = rsSchemaId(schema);
    const expanded = rsIsExpanded(m.toggled, sid);
    out.push({ id: sid, depth: 0, kind: "schema", label: schema, expandable: true, expanded, schema });
    if (!expanded) continue;
    const tq = m.tables.get(schema);
    if (!tq || tq.status === "pending") {
      out.push(status(1, "loading", "Loading…", `${sid}${SEP}loading`));
      continue;
    }
    if (tq.status === "error") {
      out.push(status(1, "error", tq.error ?? "Could not list tables", `${sid}${SEP}error`));
      continue;
    }
    const tables = [...(tq.data ?? [])].sort((a, b) => byName(a.name, b.name));
    if (tables.length === 0) out.push(status(1, "empty", "No tables or views", `${sid}${SEP}empty`));
    for (const t of tables) {
      const tid = rsTableId(schema, t.name);
      const tExpanded = rsIsExpanded(m.toggled, tid);
      out.push({ id: tid, depth: 1, kind: "table", label: t.name, expandable: true, expanded: tExpanded, schema, table: t });
      if (!tExpanded) continue;
      const dq = m.describes.get(rsTableKey(schema, t.name));
      if (!dq || dq.status === "pending") out.push(status(2, "loading", "Loading…", `${tid}${SEP}loading`));
      else if (dq.status === "error") out.push(status(2, "error", dq.error ?? "Could not describe the table", `${tid}${SEP}error`));
      else if ((dq.data ?? []).length === 0) out.push(status(2, "empty", "No columns", `${tid}${SEP}empty`));
      else for (const c of dq.data ?? []) out.push({ id: `${tid}${SEP}c${SEP}${c.name}`, depth: 2, kind: "column", label: c.name, expandable: false, expanded: false, schema, table: t, column: c });
    }
  }
  return out;
}

/** Schemas and tables whose children are needed, given the expansion state. */
export function expandedRsSchemas(schemas: readonly string[] | undefined, toggled: ReadonlyMap<string, boolean>): string[] {
  return (schemas ?? []).filter((s) => rsIsExpanded(toggled, rsSchemaId(s)));
}
