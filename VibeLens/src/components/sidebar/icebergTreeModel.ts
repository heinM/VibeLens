// Pure model for the Iceberg navigator: catalog > namespaces (nested) > tables / views > fields (nested).
// It turns cached query results plus the expansion state into the flat row list the virtualised tree renders.
import type { IcebergField, IcebergNamespace, IcebergObjectKind, IcebergTableInfo, IcebergTableRef } from "../../ipc/types";
import { joinNamespace } from "../iceberg/openIcebergTab";
import type { QueryState } from "./pgTreeModel";

const SEP = "\u0001";
export const CATALOG_ID = "catalog";
export const nsId = (levels: string[]) => `ns${SEP}${joinNamespace(levels)}`;
export const tableId = (levels: string[], name: string) => `tbl${SEP}${joinNamespace(levels)}${SEP}${name}`;
export const fieldId = (parent: string, f: IcebergField) => `${parent}${SEP}f${f.id}`;
/** Key shared by the describe map and the visible-table prefetch. */
export const tableKey = (levels: string[], name: string) => `${joinNamespace(levels)}${SEP}${name}`;

export type IceRowKind = "catalog" | "namespace" | "table" | "field" | "status";

export interface IceRow {
  id: string;
  depth: number;
  kind: IceRowKind;
  label: string;
  expandable: boolean;
  expanded: boolean;
  /** namespace rows: its levels; table rows: the namespace the table lives in. */
  namespace?: string[];
  table?: { name: string; kind: IcebergObjectKind };
  field?: IcebergField;
  /** dotted path of a field below its table, e.g. `ctx.device`. */
  fieldPath?: string;
  /** Present on table rows once the table's metadata has been fetched. */
  info?: IcebergTableInfo;
  statusKind?: "loading" | "error" | "empty";
}

export interface IceModelInput {
  catalogLabel: string;
  topLevel: QueryState<IcebergNamespace[]>;
  /** keyed by joinNamespace(parent) */
  children: ReadonlyMap<string, QueryState<IcebergNamespace[]>>;
  /** keyed by joinNamespace(namespace) */
  tables: ReadonlyMap<string, QueryState<IcebergTableRef[]>>;
  /** keyed by tableKey() */
  describes: ReadonlyMap<string, QueryState<IcebergTableInfo>>;
  toggled: ReadonlyMap<string, boolean>;
}

export const isExpanded = (toggled: ReadonlyMap<string, boolean>, id: string): boolean => toggled.get(id) ?? id === CATALOG_ID;

const byName = <T,>(get: (t: T) => string) => (a: T, b: T) => get(a).localeCompare(get(b), undefined, { sensitivity: "base", numeric: true });

function status(depth: number, statusKind: "loading" | "error" | "empty", label: string, id: string): IceRow {
  return { id, depth, kind: "status", label, expandable: false, expanded: false, statusKind };
}

function pushFields(out: IceRow[], parentId: string, depth: number, fields: IcebergField[], path: string, ctx: IceModelInput, namespace: string[], tableName: string) {
  for (const f of fields) {
    const id = fieldId(parentId, f);
    const expandable = f.children.length > 0;
    const expanded = expandable && isExpanded(ctx.toggled, id);
    const fieldPath = path ? `${path}.${f.name}` : f.name;
    out.push({ id, depth, kind: "field", label: f.name, expandable, expanded, field: f, fieldPath, namespace, table: { name: tableName, kind: "table" } });
    if (expanded) pushFields(out, id, depth + 1, f.children, fieldPath, ctx, namespace, tableName);
  }
}

function pushNamespace(out: IceRow[], ns: IcebergNamespace, depth: number, ctx: IceModelInput) {
  const levels = ns.levels;
  const id = nsId(levels);
  const expanded = isExpanded(ctx.toggled, id);
  out.push({ id, depth, kind: "namespace", label: levels[levels.length - 1] ?? "", expandable: true, expanded, namespace: levels });
  if (!expanded) return;

  const key = joinNamespace(levels);
  const kids = ctx.children.get(key);
  const tbls = ctx.tables.get(key);
  const d = depth + 1;

  if (kids?.status === "error") out.push(status(d, "error", kids.error ?? "Could not list namespaces", `${id}${SEP}err-ns`));
  for (const child of [...(kids?.data ?? [])].sort(byName((n) => n.levels[n.levels.length - 1] ?? ""))) pushNamespace(out, child, d, ctx);

  if (tbls?.status === "error") out.push(status(d, "error", tbls.error ?? "Could not list tables", `${id}${SEP}err-tbl`));
  for (const t of [...(tbls?.data ?? [])].sort(byName((x) => x.name))) {
    const tid = tableId(levels, t.name);
    const tExpanded = isExpanded(ctx.toggled, tid);
    const dq = ctx.describes.get(tableKey(levels, t.name));
    out.push({ id: tid, depth: d, kind: "table", label: t.name, expandable: true, expanded: tExpanded, namespace: levels, table: { name: t.name, kind: t.kind }, info: dq?.data });
    if (!tExpanded) continue;
    if (!dq || dq.status === "pending") out.push(status(d + 1, "loading", "Loading schema…", `${tid}${SEP}load`));
    else if (dq.status === "error") out.push(status(d + 1, "error", dq.error ?? "Could not read the table", `${tid}${SEP}err`));
    else if (dq.data) pushFields(out, tid, d + 1, dq.data.fields, "", ctx, levels, t.name);
  }

  const pending = (kids?.status ?? "pending") === "pending" || (tbls?.status ?? "pending") === "pending";
  const nothing = (kids?.data?.length ?? 0) === 0 && (tbls?.data?.length ?? 0) === 0;
  if (pending && nothing) out.push(status(d, "loading", "Loading…", `${id}${SEP}load`));
  else if (!pending && nothing && kids?.status !== "error" && tbls?.status !== "error") out.push(status(d, "empty", "No tables or namespaces", `${id}${SEP}empty`));
}

export function buildIceRows(ctx: IceModelInput): IceRow[] {
  const out: IceRow[] = [];
  const expanded = isExpanded(ctx.toggled, CATALOG_ID);
  out.push({ id: CATALOG_ID, depth: 0, kind: "catalog", label: ctx.catalogLabel, expandable: true, expanded });
  if (!expanded) return out;
  const top = ctx.topLevel;
  if (top.status === "pending") out.push(status(1, "loading", "Loading namespaces…", `${CATALOG_ID}${SEP}load`));
  else if (top.status === "error") out.push(status(1, "error", top.error ?? "Could not list namespaces", `${CATALOG_ID}${SEP}err`));
  else if ((top.data?.length ?? 0) === 0) out.push(status(1, "empty", "No namespaces yet", `${CATALOG_ID}${SEP}empty`));
  else for (const ns of [...(top.data ?? [])].sort(byName((n) => n.levels[n.levels.length - 1] ?? ""))) pushNamespace(out, ns, 1, ctx);
  return out;
}

/** Expanded namespaces, in the order the tree meets them, so the caller can fetch their children. */
export function expandedNamespaces(topLevel: IcebergNamespace[] | undefined, children: ReadonlyMap<string, IcebergNamespace[] | undefined>, toggled: ReadonlyMap<string, boolean>): string[][] {
  const out: string[][] = [];
  if (!isExpanded(toggled, CATALOG_ID)) return out;
  const walk = (list: IcebergNamespace[] | undefined) => {
    for (const ns of list ?? []) {
      if (!isExpanded(toggled, nsId(ns.levels))) continue;
      out.push(ns.levels);
      walk(children.get(joinNamespace(ns.levels)));
    }
  };
  walk(topLevel);
  return out;
}

/** "day(placed_at) → placed_day" style, used in the P chip tooltip. */
export function partitionSummary(info: IcebergTableInfo): string {
  if (info.partitionSpec.length === 0) return "";
  const nameOf = new Map<number, string>();
  const walk = (fs: IcebergField[]) => fs.forEach((f) => (nameOf.set(f.id, f.name), walk(f.children)));
  walk(info.fields);
  return info.partitionSpec.map((p) => partitionExpr(p.transform, nameOf.get(p.sourceId) ?? `#${p.sourceId}`) + (p.name ? ` → ${p.name}` : "")).join(", ");
}

/** `bucket[8]` + `customer_id` -> `bucket(8, customer_id)`; `day` + `placed_at` -> `day(placed_at)`; identity -> bare column. */
export function partitionExpr(transform: string, column: string): string {
  const m = /^(bucket|truncate)\[(\d+)\]$/.exec(transform);
  if (m) return `${m[1]}(${m[2]}, ${column})`;
  if (transform === "identity") return column;
  return `${transform}(${column})`;
}

/** total-records from the current snapshot summary, as a number (counts fit in 2^53), or null. */
export function totalRecords(info: IcebergTableInfo): number | null {
  const cur = info.snapshots.find((s) => s.snapshotId === info.currentSnapshotId);
  const v = cur?.summary["total-records"];
  if (v === undefined) return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

/** Top-level type label shown next to a field: structs collapse to `struct`, the rest keep their parameters. */
export function shortType(f: IcebergField): string {
  if (f.typeName.startsWith("struct")) return "struct";
  return f.typeName;
}
