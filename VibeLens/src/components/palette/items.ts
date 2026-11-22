// Palette item model and builders. Pure data: React maps `icon` to a glyph and `tone` to a colour.
import type { ConnectionProfile, ConnectionState, Environment, SchemaMetadata, SchemaObject, WorkspaceFile } from "../../ipc/types";
import type { HistoryEntry } from "../../store/useHistoryStore";
import type { Rankable } from "./rank";
import { quoteIdentFor, type SqlDialect } from "../../lib/sqlIntel/dialect";

export type IconKey =
  | "pg"
  | "iceberg"
  | "redshift"
  | "mysql"
  | "valkey"
  | "table"
  | "view"
  | "function"
  | "schema"
  | "file"
  | "fileValkey"
  | "history"
  | "historyFail"
  | "newTab"
  | "newConnection"
  | "settings"
  | "mode"
  | "sun"
  | "moon"
  | "sidebar"
  | "agent"
  | "plug"
  | "unplug"
  | "refresh"
  | "trash"
  | "diagram";

export type Tone = "sky" | "cyan" | "rose" | "emerald" | "violet" | "amber" | "accent" | "muted" | "danger";

export interface PaletteItem extends Rankable {
  group: string;
  icon: IconKey;
  tone: Tone;
  /** Muted inline text after the title (function signature, mode description). */
  subtitle?: string;
  /** Context shown on the right, left of `meta`: connection, then schema. */
  crumbs?: string[];
  /** Right-most text: row estimate, "current"... */
  meta?: string;
  /** Epoch ms; the view renders it as a relative age at paint time so it never goes stale. */
  ts?: number;
  shortcut?: string[];
  env?: Environment;
  state?: ConnectionState;
  /** Verb for Enter, e.g. "Open data". */
  primary: string;
  run: () => void;
  alt?: { label: string; run: () => void };
}

/** Display order of groups in "Go to anything". */
export const GO_GROUP_ORDER = ["Connections", "Tables", "Views", "Functions", "Schemas", "Files", "History"] as const;

/** Group caps for the empty-query browse view (a query shows the best matches overall). */
export const BROWSE_CAPS: Record<string, number> = { Connections: 12, Tables: 40, Views: 12, Functions: 10, Schemas: 12, Files: 15, History: 8 };

export interface GoActions {
  openConnection: (p: ConnectionProfile) => void;
  newTabOnConnection: (p: ConnectionProfile) => void;
  openTable: (p: ConnectionProfile, db: string | null, schema: string, name: string) => void;
  insertName: (p: ConnectionProfile, db: string | null, schema: string, name: string) => void;
  openErd: (p: ConnectionProfile, db: string | null, schema: string) => void;
  openFile: (path: string) => void;
  openHistory: (e: HistoryEntry) => void;
  copy: (text: string) => void;
}

export const compactRows = (n: number | null | undefined): string => {
  if (n === null || n === undefined || n < 0) return "";
  if (n < 1000) return `~${n} rows`;
  if (n < 1_000_000) return `~${(n / 1000).toFixed(n < 10_000 ? 1 : 0).replace(/\.0$/, "")}k rows`;
  if (n < 1_000_000_000) return `~${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0).replace(/\.0$/, "")}M rows`;
  return `~${(n / 1_000_000_000).toFixed(1).replace(/\.0$/, "")}B rows`;
};

/** Where an Iceberg connection's catalog lives, for the palette row. */
function icebergWhere(p: ConnectionProfile): string {
  const c = p.iceberg?.catalog;
  if (!c) return "Apache Iceberg";
  return c.kind === "rest" || c.kind === "sql" ? c.uri : c.kind === "hms" ? c.address : c.kind === "s3Tables" ? `S3 Tables · ${c.aws.region}` : `AWS Glue · ${c.aws.region}`;
}

/** Where a Redshift connection's warehouse is, for the palette row. */
function redshiftWhere(p: ConnectionProfile): string {
  const o = p.redshift;
  if (!o) return "Amazon Redshift";
  const name = o.target.kind === "provisioned" ? o.target.clusterIdentifier : o.target.workgroupName;
  return [name || "Amazon Redshift", o.database, o.aws.region].filter(Boolean).join(" · ");
}

export function connectionItems(profiles: readonly ConnectionProfile[], stateOf: (id: string) => ConnectionState, a: GoActions): PaletteItem[] {
  return profiles.map((p): PaletteItem => {
    const pg = p.engine === "postgres";
    const ice = p.engine === "iceberg";
    const rs = p.engine === "redshift";
    const my = p.engine === "mysql";
    const state = stateOf(p.id);
    return {
      id: `conn:${p.id}`,
      group: "Connections",
      title: p.name || "(unnamed)",
      search: ice
        ? `${icebergWhere(p)} iceberg lakehouse athena trino spark glue rest ${p.environment}`
        : rs
          ? `${redshiftWhere(p)} redshift warehouse aws ${p.environment}`
          : `${p.host} ${pg ? "postgres postgresql" : my ? "mysql aurora" : "valkey redis"} ${p.environment}`,
      icon: ice ? "iceberg" : rs ? "redshift" : my ? "mysql" : pg ? "pg" : "valkey",
      tone: ice ? "cyan" : rs ? "violet" : my ? "amber" : pg ? "sky" : "rose",
      crumbs: [ice ? icebergWhere(p) : rs ? redshiftWhere(p) : `${p.host}:${p.port}`],
      meta: state === "connected" ? "connected" : state === "connecting" ? "connecting" : state === "error" ? "error" : undefined,
      env: p.environment,
      state,
      primary: state === "connected" ? "Focus" : "Connect",
      run: () => a.openConnection(p),
      alt: { label: pg || ice || rs || my ? "New SQL tab" : "New console", run: () => a.newTabOnConnection(p) },
    };
  });
}

const VIEW_KINDS = new Set(["view", "materializedView"]);

/** Tables, views and schemas of one connection's catalog. */
export function catalogItems(p: ConnectionProfile, db: string | null, meta: SchemaMetadata, a: GoActions): PaletteItem[] {
  const out: PaletteItem[] = [];
  const perSchema = new Map<string, number>();
  for (const t of meta.tables) {
    perSchema.set(t.schema, (perSchema.get(t.schema) ?? 0) + 1);
    const isView = VIEW_KINDS.has(t.kind);
    if (t.kind === "sequence" || t.kind === "function" || t.kind === "procedure") continue;
    out.push({
      id: `obj:${p.id}:${db ?? ""}:${t.schema}.${t.name}`,
      group: isView ? "Views" : "Tables",
      title: t.name,
      search: `${t.schema}.${t.name}`,
      context: p.name,
      icon: isView ? "view" : "table",
      tone: isView ? "emerald" : "sky",
      crumbs: [p.name, t.schema],
      meta: compactRows(t.estimatedRows) || undefined,
      primary: "Open data",
      run: () => a.openTable(p, db, t.schema, t.name),
      alt: { label: "Insert name", run: () => a.insertName(p, db, t.schema, t.name) },
    });
  }
  for (const [schema, count] of perSchema) {
    out.push({
      id: `schema:${p.id}:${db ?? ""}:${schema}`,
      group: "Schemas",
      title: schema,
      search: "schema er diagram erd",
      context: p.name,
      icon: "schema",
      tone: "amber",
      crumbs: [p.name],
      meta: `${count} ${count === 1 ? "table" : "tables"}`,
      primary: "Open ER diagram",
      run: () => a.openErd(p, db, schema),
      alt: { label: "Insert name", run: () => a.insertName(p, db, schema, "") },
    });
  }
  return out;
}

/**
 * Tables and views of a MySQL database. MySQL has no data tab or ER diagram (Postgres features), so the
 * primary action opens a SELECT in a SQL tab and the alternative inserts the backtick-quoted name.
 */
export function mysqlCatalogItems(
  p: ConnectionProfile,
  meta: SchemaMetadata,
  a: { query: (db: string, name: string) => void; insertName: (p: ConnectionProfile, db: string | null, schema: string, name: string) => void },
): PaletteItem[] {
  const out: PaletteItem[] = [];
  for (const t of meta.tables) {
    if (t.kind === "sequence" || t.kind === "function" || t.kind === "procedure") continue;
    const isView = VIEW_KINDS.has(t.kind);
    out.push({
      id: `myobj:${p.id}:${t.schema}.${t.name}`,
      group: isView ? "Views" : "Tables",
      title: t.name,
      search: `${t.schema}.${t.name}`,
      context: p.name,
      icon: isView ? "view" : "table",
      tone: isView ? "emerald" : "sky",
      crumbs: [p.name, t.schema],
      meta: compactRows(t.estimatedRows) || undefined,
      primary: "Query in new tab",
      run: () => a.query(t.schema, t.name),
      alt: { label: "Insert name", run: () => a.insertName(p, t.schema, t.schema, t.name) },
    });
  }
  return out;
}

/** Functions and procedures (the catalog payload has none, so they come from the object lists). */
export function functionItems(p: ConnectionProfile, db: string | null, objects: readonly SchemaObject[], a: GoActions): PaletteItem[] {
  const out: PaletteItem[] = [];
  for (const o of objects) {
    if (o.kind !== "function" && o.kind !== "procedure") continue;
    out.push({
      id: `fn:${p.id}:${db ?? ""}:${o.schema}.${o.name}${o.signature ?? ""}`,
      group: "Functions",
      title: o.name,
      search: `${o.schema}.${o.name} ${o.kind}`,
      context: p.name,
      icon: "function",
      tone: "violet",
      subtitle: o.signature ?? undefined,
      crumbs: [p.name, o.schema],
      primary: "Insert name",
      run: () => a.insertName(p, db, o.schema, o.name),
      alt: { label: "Copy name", run: () => a.copy(`${o.schema}.${o.name}`) },
    });
  }
  return out;
}

export function fileItems(files: readonly WorkspaceFile[], a: GoActions): PaletteItem[] {
  return files.map((f): PaletteItem => {
    const slash = f.path.lastIndexOf("/");
    const vk = /\.(valkey|redis)$/i.test(f.path);
    return {
      id: `file:${f.path}`,
      group: "Files",
      title: slash >= 0 ? f.path.slice(slash + 1) : f.path,
      search: f.path,
      icon: vk ? "fileValkey" : "file",
      tone: vk ? "rose" : "sky",
      crumbs: slash >= 0 ? [f.path.slice(0, slash)] : undefined,
      primary: "Open",
      run: () => a.openFile(f.path),
    };
  });
}

/** One-line preview of a statement: comment lines dropped, whitespace collapsed, truncated. */
export function sqlPreview(sql: string, max = 110): string {
  const code = sql.split("\n").filter((l) => l.trim() && !/^\s*--/.test(l));
  const s = (code.length ? code : sql.split("\n")).join(" ").replace(/\s+/g, " ").trim();
  return s.length > max ? s.slice(0, max - 1) + "…" : s;
}

export function timeAgo(now: number, at: number): string {
  const s = Math.max(0, Math.round((now - at) / 1000));
  if (s < 45) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h} h ago`;
  const d = Math.round(h / 24);
  if (d < 14) return `${d} d ago`;
  return new Date(at).toISOString().slice(0, 10);
}

export function historyItems(entries: readonly HistoryEntry[], a: GoActions, cap = 300): PaletteItem[] {
  return entries.slice(0, cap).map((e): PaletteItem => ({
    id: `hist:${e.id}`,
    group: "History",
    title: sqlPreview(e.sql),
    search: e.sql.slice(0, 300),
    context: e.profileName ?? "",
    icon: e.ok ? "history" : "historyFail",
    tone: e.ok ? "muted" : "danger",
    crumbs: e.profileName ? [e.profileName] : undefined,
    ts: e.at,
    primary: "Open in new SQL tab",
    run: () => a.openHistory(e),
    alt: { label: "Copy SQL", run: () => a.copy(e.sql) },
  }));
}

export interface Row<T> {
  type: "header" | "item";
  label?: string;
  count?: number;
  index?: number;
  entry?: T;
}

/**
 * Turn ranked items into rows with group headers. Groups are ordered by their best score (so the
 * best match is always on top) unless `order` is given, in which case that order is used and
 * `caps` limits each group.
 */
export function groupRanked<T extends { item: { group: string }; score: number }>(
  ranked: readonly T[],
  opts: { order?: readonly string[]; caps?: Record<string, number>; start?: number; /** Group sizes before capping, when `ranked` was pre-capped by the caller. */ totals?: ReadonlyMap<string, number> } = {},
): Row<T>[] {
  const groups = new Map<string, T[]>();
  for (const r of ranked) {
    const g = groups.get(r.item.group);
    if (g) g.push(r);
    else groups.set(r.item.group, [r]);
  }
  let names: string[];
  if (opts.order) {
    names = [...opts.order.filter((n) => groups.has(n)), ...[...groups.keys()].filter((n) => !opts.order?.includes(n))];
  } else {
    names = [...groups.keys()].sort((a, b) => (groups.get(b)?.[0]?.score ?? 0) - (groups.get(a)?.[0]?.score ?? 0));
  }
  const rows: Row<T>[] = [];
  let index = opts.start ?? 0;
  for (const name of names) {
    const all = groups.get(name) as T[];
    const list = opts.caps?.[name] !== undefined ? all.slice(0, opts.caps[name]) : all;
    rows.push({ type: "header", label: name, count: opts.totals?.get(name) ?? all.length });
    for (const entry of list) rows.push({ type: "item", index: index++, entry });
  }
  return rows;
}

const PLAIN_IDENT = /^[a-z_][a-z0-9_$]*$/;
const RESERVED = new Set(["user", "order", "group", "table", "select", "from", "where", "limit", "offset", "desc", "asc", "end", "all", "analyse", "analyze", "and", "any", "as", "check", "column", "constraint", "default", "distinct", "do", "else", "for", "grant", "having", "in", "into", "not", "null", "on", "or", "primary", "references", "then", "to", "union", "unique", "using", "when", "with"]);

/** Quote an identifier only when it needs it (mixed case, spaces, reserved words), in the dialect's quotes (Spark: backticks). */
export const identForSql = (s: string, dialect?: SqlDialect | null): string =>
  PLAIN_IDENT.test(s) && !RESERVED.has(s) && !(dialect === "spark" && s.includes("$")) ? s : quoteIdentFor(s, dialect);

/** Text inserted into the editor for an object: `schema.name`, or just `schema` when `name` is empty. */
export function insertionText(schema: string, name: string, dialect?: SqlDialect | null): string {
  return name ? `${identForSql(schema, dialect)}.${identForSql(name, dialect)}` : identForSql(schema, dialect);
}
