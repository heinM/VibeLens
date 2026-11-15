// Pure helpers behind the Iceberg table tab: schema flattening, partition/sort rows, snapshot deltas,
// and an estimate of what "expire snapshots" will remove. Snapshot ids are strings throughout: never Number() them.
import type { IcebergField, IcebergRef, IcebergSnapshot, IcebergTableInfo } from "../../ipc/types";
import { summaryInt } from "./iceFormat";
import { partitionExpr } from "../sidebar/icebergTreeModel";

/* ----------------------------------------------------------------- schema */

export interface FlatField {
  field: IcebergField;
  depth: number;
  /** Dotted path from the top, e.g. `ctx.device`. */
  path: string;
  expandable: boolean;
  expanded: boolean;
}

/** Depth-first list of visible field rows. `collapsed` holds ids of nested fields the user folded; nested fields start open. */
export function flattenFields(fields: IcebergField[], collapsed: ReadonlySet<number>): FlatField[] {
  const out: FlatField[] = [];
  const walk = (list: IcebergField[], depth: number, prefix: string) => {
    for (const f of list) {
      const expandable = f.children.length > 0;
      const expanded = expandable && !collapsed.has(f.id);
      const path = prefix ? `${prefix}.${f.name}` : f.name;
      out.push({ field: f, depth, path, expandable, expanded });
      if (expanded) walk(f.children, depth + 1, path);
    }
  };
  walk(fields, 0, "");
  return out;
}

export function countFields(fields: IcebergField[]): number {
  return fields.reduce((n, f) => n + 1 + countFields(f.children), 0);
}

/** Ids of every field that has children (for "collapse all"). */
export function nestedIds(fields: IcebergField[]): number[] {
  const out: number[] = [];
  const walk = (list: IcebergField[]) => list.forEach((f) => (f.children.length ? (out.push(f.id), walk(f.children)) : undefined));
  walk(fields);
  return out;
}

/** field id -> dotted path, across all nesting. */
export function fieldIndex(fields: IcebergField[]): Map<number, { path: string; field: IcebergField }> {
  const m = new Map<number, { path: string; field: IcebergField }>();
  const walk = (list: IcebergField[], prefix: string) => {
    for (const f of list) {
      const path = prefix ? `${prefix}.${f.name}` : f.name;
      m.set(f.id, { path, field: f });
      walk(f.children, path);
    }
  };
  walk(fields, "");
  return m;
}

export type TypeTone = "number" | "text" | "bool" | "time" | "nested" | "other";
export function typeTone(typeName: string): TypeTone {
  const t = typeName.trim().toLowerCase();
  if (/^(struct|list|map)\b/.test(t)) return "nested";
  if (/^(int|long|float|double|decimal)/.test(t)) return "number";
  if (/^(string|uuid|binary|fixed)/.test(t)) return "text";
  if (t === "boolean") return "bool";
  if (/^(date|time|timestamp)/.test(t)) return "time";
  return "other";
}

/* ----------------------------------------------------------- partitioning */

export interface PartitionRow {
  fieldId: number;
  /** `day(placed_at)` */
  expression: string;
  /** Name of the derived partition column, e.g. `placed_day`. */
  name: string;
  source: string;
  transform: string;
  explain: string;
}

export function transformExplain(transform: string): string {
  const b = /^(bucket|truncate)\[(\d+)\]$/.exec(transform);
  if (b) return b[1] === "bucket" ? `Hash of the value spread over ${b[2]} buckets.` : `Value cut to its first ${b[2]} characters or rounded down to a multiple of ${b[2]}.`;
  switch (transform) {
    case "identity":
      return "One partition per distinct value.";
    case "year":
      return "One partition per calendar year.";
    case "month":
      return "One partition per calendar month.";
    case "day":
      return "One partition per calendar day.";
    case "hour":
      return "One partition per hour.";
    case "void":
      return "Always null. A partition field that was dropped from the spec.";
    default:
      return "";
  }
}

export function partitionRows(info: IcebergTableInfo): PartitionRow[] {
  const idx = fieldIndex(info.fields);
  return info.partitionSpec.map((p) => {
    const source = idx.get(p.sourceId)?.path ?? `field #${p.sourceId}`;
    return { fieldId: p.fieldId, expression: partitionExpr(p.transform, source), name: p.name, source, transform: p.transform, explain: transformExplain(p.transform) };
  });
}

export interface SortRow {
  expression: string;
  direction: "ASC" | "DESC";
  nulls: "NULLS FIRST" | "NULLS LAST";
}
export function sortRows(info: IcebergTableInfo): SortRow[] {
  const idx = fieldIndex(info.fields);
  return info.sortOrder.map((s) => {
    const source = idx.get(s.sourceId)?.path ?? `field #${s.sourceId}`;
    return {
      expression: partitionExpr(s.transform, source),
      direction: s.direction.toLowerCase().startsWith("desc") ? "DESC" : "ASC",
      nulls: /last/i.test(s.nullOrder) ? "NULLS LAST" : "NULLS FIRST",
    };
  });
}

/** Source field ids used by the partition spec / sort order, to tag columns in the schema view. */
export function partitionSources(info: IcebergTableInfo): Set<number> {
  return new Set(info.partitionSpec.filter((p) => p.transform !== "void").map((p) => p.sourceId));
}
export function sortSources(info: IcebergTableInfo): Set<number> {
  return new Set(info.sortOrder.map((s) => s.sourceId));
}

/* -------------------------------------------------------------- snapshots */

export type OpTone = "ok" | "warn" | "danger" | "muted";
export function opTone(op: string | null): OpTone {
  switch ((op ?? "").toLowerCase()) {
    case "append":
      return "ok";
    case "overwrite":
      return "warn";
    case "delete":
      return "danger";
    default:
      return "muted"; // replace, and anything a newer writer invents
  }
}

export interface SnapshotDelta {
  filesAdded: number | null;
  filesRemoved: number | null;
  recordsAdded: number | null;
  recordsRemoved: number | null;
  totalRecords: number | null;
}
export function snapshotDelta(s: IcebergSnapshot): SnapshotDelta {
  const q = s.summary;
  return {
    filesAdded: summaryInt(q, "added-data-files"),
    filesRemoved: summaryInt(q, "deleted-data-files"),
    recordsAdded: summaryInt(q, "added-records"),
    recordsRemoved: summaryInt(q, "deleted-records"),
    totalRecords: summaryInt(q, "total-records"),
  };
}

/** snapshotId -> refs pointing at it. */
export function refsBySnapshot(refs: IcebergRef[]): Map<string, IcebergRef[]> {
  const m = new Map<string, IcebergRef[]>();
  for (const r of refs) m.set(r.snapshotId, [...(m.get(r.snapshotId) ?? []), r]);
  return m;
}

/** Head-of-table figures from the current snapshot's summary. */
export interface TableTotals {
  records: number | null;
  dataFiles: number | null;
  sizeBytes: number | null;
  deleteFiles: number | null;
  positionDeletes: number | null;
  equalityDeletes: number | null;
}
export function tableTotals(info: IcebergTableInfo): TableTotals | null {
  const cur = info.snapshots.find((s) => s.snapshotId === info.currentSnapshotId);
  if (!cur) return null;
  const q = cur.summary;
  return {
    records: summaryInt(q, "total-records"),
    dataFiles: summaryInt(q, "total-data-files"),
    sizeBytes: summaryInt(q, "total-files-size"),
    deleteFiles: summaryInt(q, "total-delete-files"),
    positionDeletes: summaryInt(q, "total-position-deletes"),
    equalityDeletes: summaryInt(q, "total-equality-deletes"),
  };
}

/* ----------------------------------------------------------------- expire */

/**
 * Estimate which snapshots an expire call would remove, mirroring the engine's rules: only snapshots strictly
 * older than the cutoff go; the current snapshot and every branch/tag head stay; each branch also keeps its
 * newest `retainLast` snapshots. The server has the final word (it also honours table properties), so the UI
 * labels this as an estimate.
 */
export function planExpire(info: Pick<IcebergTableInfo, "snapshots" | "refs" | "currentSnapshotId">, olderThanMs: number, retainLast: number): { expire: string[]; keep: string[] } {
  const byId = new Map(info.snapshots.map((s) => [s.snapshotId, s]));
  const protectedIds = new Set<string>();
  if (info.currentSnapshotId) protectedIds.add(info.currentSnapshotId);
  const heads = [...info.refs.map((r) => ({ id: r.snapshotId, branch: r.kind.toLowerCase() === "branch" })), ...(info.currentSnapshotId ? [{ id: info.currentSnapshotId, branch: true }] : [])];
  for (const h of heads) {
    protectedIds.add(h.id);
    if (!h.branch) continue;
    let cur = byId.get(h.id);
    for (let i = 0; cur && i < Math.max(1, retainLast); i++) {
      protectedIds.add(cur.snapshotId);
      cur = cur.parentSnapshotId ? byId.get(cur.parentSnapshotId) : undefined;
    }
  }
  const expire: string[] = [];
  const keep: string[] = [];
  for (const s of info.snapshots) (s.timestampMs < olderThanMs && !protectedIds.has(s.snapshotId) ? expire : keep).push(s.snapshotId);
  return { expire, keep };
}
