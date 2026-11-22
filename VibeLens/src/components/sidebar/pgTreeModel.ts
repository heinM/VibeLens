// Pure model for the Postgres navigator: turns cached catalog query results plus the expansion
// state into the flat list of visible rows that the virtualised tree renders.
import type { ColumnInfo, DatabaseInfo, ObjectKind, PartitionInfo, PartitionListing, PartmanConfig, SchemaInfo, SchemaObject, TableDescription } from "../../ipc/types";
import { humanBytes } from "../grid/statsFormat";

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

export type PgRowKind = "db" | "schema" | "group" | "object" | "column" | "partitionSummary" | "status";

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
  /** Set on partition rows (kind "object"): the bound, size and so on shown beside the name. */
  partition?: PartitionInfo;
  count?: number;
  isCurrentDb?: boolean;
  statusKind?: "loading" | "error" | "empty";
}

export interface PgModelInput {
  databases: QueryState<DatabaseInfo[]>;
  schemas: ReadonlyMap<string, QueryState<SchemaInfo[]>>;
  objects: ReadonlyMap<string, QueryState<SchemaObject[]>>;
  describes: ReadonlyMap<string, QueryState<TableDescription>>;
  /** Partition listings of expanded partitioned tables, keyed like `describes` (`describeKey`). */
  partitions: ReadonlyMap<string, QueryState<PartitionListing>>;
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


/** The tree object standing for a partition, so partition rows open and behave like tables. */
export function partitionObject(p: PartitionInfo, parent: { schema: string; name: string }): SchemaObject {
  return {
    schema: p.schema,
    name: p.name,
    kind: p.isPartitioned ? "partitionedTable" : "table",
    comment: null,
    estimatedRows: p.estimatedRows,
    signature: null,
    isPartition: true,
    parentSchema: parent.schema,
    parentName: parent.name,
  };
}

const INTERVAL_UNITS: Record<string, [adjective: string, unit: string]> = {
  sec: ["every second", "seconds"],
  second: ["every second", "seconds"],
  min: ["every minute", "minutes"],
  minute: ["every minute", "minutes"],
  hour: ["hourly", "hours"],
  day: ["daily", "days"],
  week: ["weekly", "weeks"],
  mon: ["monthly", "months"],
  month: ["monthly", "months"],
  year: ["yearly", "years"],
};

/** pg_partman's `partition_interval` in words: `1 day` -> `daily`, `3 mons` -> `every 3 months`, `100000` -> `every 100000 ids`. */
export function partitionIntervalLabel(raw: string): string {
  const text = raw.trim();
  if (/^\d+$/.test(text)) return `every ${text} ids`;
  if (/^0?1:00:00$/.test(text)) return "hourly";
  if (/^(daily|weekly|monthly|quarterly|yearly|hourly)$/i.test(text)) return text.toLowerCase();
  const m = /^(\d+)\s*(sec|second|min|minute|hour|day|week|mon|month|year)s?$/i.exec(text);
  if (!m) return text;
  const n = Number(m[1]);
  const [adjective, unit] = INTERVAL_UNITS[m[2].toLowerCase()];
  if (n === 1) return adjective;
  if (n === 7 && unit === "days") return "weekly";
  return `every ${n} ${unit}`;
}

/** The pg_partman part of the summary row, e.g. `pg_partman · daily · premake 4 · retention 90 days`. */
export function partmanSummary(c: PartmanConfig): string {
  const parts = ["pg_partman"];
  if (c.control) parts.push(`control ${c.control}`);
  if (c.partitionInterval) parts.push(partitionIntervalLabel(c.partitionInterval));
  if (c.premake !== null) parts.push(`premake ${c.premake}`);
  if (c.retention) parts.push(`retention ${c.retention}${c.retentionKeepTable ? ", keep tables" : ""}`);
  if (c.automaticMaintenance) parts.push(`maintenance ${c.automaticMaintenance}`);
  return parts.join(" · ");
}

/** The muted row under a partitioned table: key, partition count and, when managed, the partman line. */
export function partitionSummary(l: PartitionListing): string {
  const n = l.partitions.length;
  const count = `${n}${l.truncated ? "+" : ""} ${n === 1 && !l.truncated ? "partition" : "partitions"}`;
  const parts = [l.partitionKey, count, l.partman ? partmanSummary(l.partman) : null];
  return parts.filter((x): x is string => !!x).join(" · ");
}

/** `FOR VALUES FROM (1) TO (2)` -> `FROM (1) TO (2)`; the default partition reads `DEFAULT`. */
export const boundLabel = (bound: string): string => bound.replace(/^FOR VALUES\s+/i, "");

/** Size and row estimate of a partition, e.g. `~1.2M · 4.10 GB`. */
export function partitionStats(p: PartitionInfo): string {
  const rows = compactCount(p.estimatedRows);
  const size = p.sizeBytes === null ? "" : humanBytes(p.sizeBytes);
  return [rows, size].filter(Boolean).join(" · ");
}

export interface RelationRef {
  db: string;
  schema: string;
  table: string;
}

/** Sub-partitioning deeper than this is not followed (real schemas use two or three levels). */
const MAX_PARTITION_DEPTH = 8;

/**
 * What an expanded partitioned table needs fetched, beyond its own columns: its partition listing,
 * and (as listings arrive) those of expanded sub-partitioned partitions and the columns of expanded
 * partitions. Walks only what is expanded, so nothing is requested for collapsed nodes.
 */
export function collectPartitionRelations(
  db: string,
  parent: SchemaObject,
  toggled: ReadonlyMap<string, boolean>,
  getListing: (ref: RelationRef) => PartitionListing | undefined,
  out: { listings: RelationRef[]; describes: RelationRef[] },
  depth = 0,
): void {
  // A listing is server data: a partition naming itself or an ancestor must not recurse forever.
  if (depth > MAX_PARTITION_DEPTH) return;
  const ref = { db, schema: parent.schema, table: parent.name };
  out.listings.push(ref);
  for (const p of getListing(ref)?.partitions ?? []) {
    const o = partitionObject(p, parent);
    if (!isExpanded(toggled, objectId(db, o))) continue;
    out.describes.push({ db, schema: p.schema, table: p.name });
    if (p.isPartitioned) collectPartitionRelations(db, o, toggled, getListing, out, depth + 1);
  }
}

/** One table-like node: its row and, when expanded, columns, then (partitioned) summary and partitions. */
function pushRelation(rows: PgRow[], input: PgModelInput, db: string, o: SchemaObject, depth: number, partition?: PartitionInfo, level = 0): void {
  const oid = objectId(db, o);
  const relation = isRelation(o.kind);
  const expanded = relation && isExpanded(input.toggled, oid);
  rows.push({ id: oid, depth, kind: "object", label: o.name, expandable: relation, expanded, db, schema: o.schema, object: o, partition });
  if (!expanded) return;

  const key = describeKey(db, o.schema, o.name);
  const dq = input.describes.get(key) ?? { status: "pending" as const };
  if (dq.status !== "success" || !dq.data) {
    const r = status(depth + 1, oid, dq as QueryState<unknown>, "No columns");
    if (r) rows.push(r);
  } else {
    for (const c of [...dq.data.columns].sort((a, b) => a.ordinal - b.ordinal)) {
      rows.push({ id: `${oid}${SEP}col${SEP}${c.name}`, depth: depth + 1, kind: "column", label: c.name, expandable: false, expanded: false, db, schema: o.schema, object: o, column: c });
    }
  }

  if (o.kind !== "partitionedTable" || level > MAX_PARTITION_DEPTH) return;
  const pid = `${oid}${SEP}partitions`;
  const lq = input.partitions.get(key) ?? { status: "pending" as const };
  if (lq.status !== "success" || !lq.data) {
    const r = status(depth + 1, pid, lq as QueryState<unknown>, "No partitions");
    if (r) rows.push(lq.status === "pending" ? { ...r, label: "Loading partitions…" } : r);
    return;
  }
  const listing = lq.data;
  const summary = partitionSummary(listing);
  rows.push({ id: `${pid}${SEP}summary`, depth: depth + 1, kind: "partitionSummary", label: summary, expandable: false, expanded: false, db, schema: o.schema, object: o });
  if (listing.partitions.length === 0) {
    rows.push({ id: `${pid}${SEP}empty`, depth: depth + 1, kind: "status", label: "No partitions", expandable: false, expanded: false, statusKind: "empty" });
    return;
  }
  for (const p of listing.partitions) {
    const child = partitionObject(p, o);
    // Same id as its parent would also read as expanded: skip a partition that names its parent.
    if (objectId(db, child) === oid) continue;
    pushRelation(rows, input, db, child, depth + 1, p, level + 1);
  }
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
      const all = oq.data ?? [];
      if (all.length === 0) {
        rows.push({ id: `${sid}${SEP}empty`, depth: 2, kind: "status", label: "No objects", expandable: false, expanded: false, statusKind: "empty" });
        continue;
      }
      // Partitions are listed under their parent, not beside it.
      const objs = all.filter((o) => !o.isPartition);
      if (objs.length === 0) {
        rows.push({ id: `${sid}${SEP}empty`, depth: 2, kind: "status", label: "Only partitions of tables in other schemas", expandable: false, expanded: false, statusKind: "empty" });
        continue;
      }
      for (const g of GROUPS) {
        const inGroup = objs.filter((o) => groupOf(o.kind) === g.key).sort(byName);
        if (inGroup.length === 0) continue;
        const gid = groupId(d.name, s.name, g.key);
        const gExpanded = isExpanded(toggled, gid);
        rows.push({ id: gid, depth: 2, kind: "group", label: g.label, expandable: true, expanded: gExpanded, db: d.name, schema: s.name, group: g.key, count: inGroup.length });
        if (!gExpanded) continue;
        for (const o of inGroup) pushRelation(rows, input, d.name, o, 3);
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
