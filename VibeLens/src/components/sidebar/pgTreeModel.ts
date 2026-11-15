// Pure model for the Postgres navigator: turns cached catalog query results plus the expansion
// state into the flat list of visible rows that the virtualised tree renders.
import type { ColumnInfo, DatabaseInfo, ObjectKind, SchemaInfo, SchemaObject, TableDescription } from "../../ipc/types";

export interface QueryState<T> {
  status: "pending" | "error" | "success";
  data?: T;
  error?: string;
}

export type GroupKey = "tables" | "views" | "matviews" | "functions" | "sequences";
export const GROUPS: { key: GroupKey; label: string }[] = [
  { key: "tables", label: "Tables" },
  { key: "views", label: "Views" },
  { key: "matviews", label: "Materialized views" },
  { key: "functions", label: "Functions" },
  { key: "sequences", label: "Sequences" },
];

export const groupOf = (kind: ObjectKind): GroupKey => {
  switch (kind) {
    case "table":
    case "partitionedTable":
    case "foreignTable":
      return "tables";
    case "view":
      return "views";
    case "materializedView":
      return "matviews";
    case "function":
    case "procedure":
      return "functions";
    case "sequence":
      return "sequences";
  }
};

/** Relations whose columns can be shown / which `SELECT *` works on. */
export const isRelation = (kind: ObjectKind): boolean => kind !== "function" && kind !== "procedure";
export const isBrowsable = (kind: ObjectKind): boolean => isRelation(kind);

const SEP = "\u0001";
export const dbId = (db: string) => `db${SEP}${db}`;
export const schemaId = (db: string, schema: string) => `schema${SEP}${db}${SEP}${schema}`;
export const groupId = (db: string, schema: string, g: GroupKey) => `group${SEP}${db}${SEP}${schema}${SEP}${g}`;
export const objectId = (db: string, o: SchemaObject) => `obj${SEP}${db}${SEP}${o.schema}${SEP}${o.kind}${SEP}${o.name}${SEP}${o.signature ?? ""}`;
export const objKey = (db: string, schema: string) => `${db}${SEP}${schema}`;
export const describeKey = (db: string, schema: string, table: string) => `${db}${SEP}${schema}${SEP}${table}`;

export type PgRowKind = "db" | "schema" | "group" | "object" | "column" | "status";

export interface PgRow {
  id: string;
  depth: number;
  kind: PgRowKind;
  label: string;
  expandable: boolean;
  expanded: boolean;
  db?: string;
  schema?: string;
  group?: GroupKey;
  object?: SchemaObject;
  column?: ColumnInfo;
  count?: number;
  isCurrentDb?: boolean;
  statusKind?: "loading" | "error" | "empty";
}

export interface PgModelInput {
  databases: QueryState<DatabaseInfo[]>;
  schemas: ReadonlyMap<string, QueryState<SchemaInfo[]>>;
  objects: ReadonlyMap<string, QueryState<SchemaObject[]>>;
  describes: ReadonlyMap<string, QueryState<TableDescription>>;
  /** Explicit expand/collapse choices; absent ids use the defaults below. */
  toggled: ReadonlyMap<string, boolean>;
  showSystem: boolean;
  currentDb: string | null;
}

export const defaultExpanded = (id: string): boolean => id.startsWith(`group${SEP}`) && id.endsWith(`${SEP}tables`);
export const isExpanded = (toggled: ReadonlyMap<string, boolean>, id: string): boolean => toggled.get(id) ?? defaultExpanded(id);

const byName = (a: { name: string }, b: { name: string }) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });

function status(depth: number, id: string, q: QueryState<unknown>, empty: string): PgRow | null {
  if (q.status === "pending") return { id: `${id}${SEP}loading`, depth, kind: "status", label: "Loading…", expandable: false, expanded: false, statusKind: "loading" };
  if (q.status === "error") return { id: `${id}${SEP}error`, depth, kind: "status", label: q.error ?? "Failed to load", expandable: false, expanded: false, statusKind: "error" };
  if (Array.isArray(q.data) && q.data.length === 0) return { id: `${id}${SEP}empty`, depth, kind: "status", label: empty, expandable: false, expanded: false, statusKind: "empty" };
  return null;
}

export function buildPgRows(input: PgModelInput): PgRow[] {
  const rows: PgRow[] = [];
  const { databases, toggled, showSystem, currentDb } = input;

  if (databases.status !== "success" || !databases.data) {
    const s = status(0, "dbs", databases, "No databases");
    return s ? [s] : [];
  }

  const dbs = databases.data.filter((d) => showSystem || currentDb === d.name || (!d.isTemplate && d.allowConnections)).sort(byName);
  for (const d of dbs) {
    const id = dbId(d.name);
    const expanded = isExpanded(toggled, id);
    rows.push({ id, depth: 0, kind: "db", label: d.name, expandable: true, expanded, db: d.name, isCurrentDb: d.name === currentDb });
    if (!expanded) continue;

    const sq = input.schemas.get(d.name) ?? { status: "pending" as const };
    const sRow = sq.status === "success" ? null : status(1, id, sq, "No schemas");
    if (sRow) {
      rows.push(sRow);
      continue;
    }
    const schemas = (sq.data ?? []).filter((s) => showSystem || !s.isSystem).sort(byName);
    if (schemas.length === 0) rows.push({ id: `${id}${SEP}empty`, depth: 1, kind: "status", label: "No schemas", expandable: false, expanded: false, statusKind: "empty" });

    for (const s of schemas) {
      const sid = schemaId(d.name, s.name);
      const sExpanded = isExpanded(toggled, sid);
      rows.push({ id: sid, depth: 1, kind: "schema", label: s.name, expandable: true, expanded: sExpanded, db: d.name, schema: s.name });
      if (!sExpanded) continue;

      const oq = input.objects.get(objKey(d.name, s.name)) ?? { status: "pending" as const };
      if (oq.status !== "success") {
        const r = status(2, sid, oq, "No objects");
        if (r) rows.push(r);
        continue;
      }
      const objs = oq.data ?? [];
      if (objs.length === 0) {
        rows.push({ id: `${sid}${SEP}empty`, depth: 2, kind: "status", label: "No objects", expandable: false, expanded: false, statusKind: "empty" });
        continue;
      }
      for (const g of GROUPS) {
        const inGroup = objs.filter((o) => groupOf(o.kind) === g.key).sort(byName);
        if (inGroup.length === 0) continue;
        const gid = groupId(d.name, s.name, g.key);
        const gExpanded = isExpanded(toggled, gid);
        rows.push({ id: gid, depth: 2, kind: "group", label: g.label, expandable: true, expanded: gExpanded, db: d.name, schema: s.name, group: g.key, count: inGroup.length });
        if (!gExpanded) continue;
        for (const o of inGroup) {
          const oid = objectId(d.name, o);
          const relation = isRelation(o.kind);
          const oExpanded = relation && isExpanded(toggled, oid);
          rows.push({ id: oid, depth: 3, kind: "object", label: o.name, expandable: relation, expanded: oExpanded, db: d.name, schema: s.name, object: o });
          if (!oExpanded) continue;
          const dq = input.describes.get(describeKey(d.name, s.name, o.name)) ?? { status: "pending" as const };
          if (dq.status !== "success" || !dq.data) {
            const r = status(4, oid, dq as QueryState<unknown>, "No columns");
            if (r) rows.push(r);
            continue;
          }
          for (const c of [...dq.data.columns].sort((a, b) => a.ordinal - b.ordinal)) {
            rows.push({ id: `${oid}${SEP}col${SEP}${c.name}`, depth: 4, kind: "column", label: c.name, expandable: false, expanded: false, db: d.name, schema: s.name, object: o, column: c });
          }
        }
      }
    }
  }
  if (rows.length === 0) rows.push({ id: "dbs\u0001empty", depth: 0, kind: "status", label: "No databases", expandable: false, expanded: false, statusKind: "empty" });
  return rows;
}

export function compactCount(n: number | null | undefined): string {
  if (n === null || n === undefined || n < 0) return "";
  if (n < 1000) return `~${n}`;
  if (n < 1_000_000) return `~${(n / 1000).toFixed(n < 10_000 ? 1 : 0).replace(/\.0$/, "")}k`;
  if (n < 1_000_000_000) return `~${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0).replace(/\.0$/, "")}M`;
  return `~${(n / 1_000_000_000).toFixed(1).replace(/\.0$/, "")}B`;
}
