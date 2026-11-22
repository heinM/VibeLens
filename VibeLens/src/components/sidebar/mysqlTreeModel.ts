// Pure model for the MySQL navigator: databases > Tables / Views / Functions / Procedures > objects >
// columns (tables and views). Turns cached catalog answers plus the expansion state into the flat row
// list the tree renders. In MySQL a schema is a database: every object's `schema` is its database.
import type { ColumnInfo, SchemaObject, TableDescription } from "../../ipc/types";
import type { QueryState } from "./pgTreeModel";

/** `` `name` `` with backticks doubled: how MySQL quotes an identifier. */
export const myQuoteIdent = (s: string) => `\`${s.replace(/`/g, "``")}\``;
export const myQualifiedName = (db: string, name: string): string => `${myQuoteIdent(db)}.${myQuoteIdent(name)}`;
export const mySelectAllSql = (db: string, name: string, limit = 100): string => `SELECT * FROM ${myQualifiedName(db, name)} LIMIT ${limit};`;
export const myCountSql = (db: string, name: string): string => `SELECT COUNT(*) FROM ${myQualifiedName(db, name)};`;
export const myShowCreateSql = (db: string, name: string, kind: SchemaObject["kind"]): string =>
  `SHOW CREATE ${kind === "view" ? "VIEW" : kind === "function" ? "FUNCTION" : kind === "procedure" ? "PROCEDURE" : "TABLE"} ${myQualifiedName(db, name)};`;

const SEP = "\u0001";
export const myDbId = (db: string) => `d${SEP}${db}`;
export const myGroupId = (db: string, g: MyGroup) => `g${SEP}${db}${SEP}${g}`;
export const myObjectId = (db: string, o: Pick<SchemaObject, "kind" | "name">) => `o${SEP}${db}${SEP}${o.kind}${SEP}${o.name}`;
/** Key shared by the describe map and the expanded-object list. */
export const myTableKey = (db: string, name: string) => `${db}${SEP}${name}`;

export type MyGroup = "tables" | "views" | "functions" | "procedures";
export const MY_GROUPS: { key: MyGroup; label: string }[] = [
  { key: "tables", label: "Tables" },
  { key: "views", label: "Views" },
  { key: "functions", label: "Functions" },
  { key: "procedures", label: "Procedures" },
];
export const myGroupOf = (kind: SchemaObject["kind"]): MyGroup =>
  kind === "view" || kind === "materializedView" ? "views" : kind === "function" ? "functions" : kind === "procedure" ? "procedures" : "tables";
/** Tables and views have columns to show; routines do not. */
export const myHasColumns = (kind: SchemaObject["kind"]): boolean => kind !== "function" && kind !== "procedure";

/** MySQL's own databases. */
export const isMySystemDatabase = (db: string): boolean => ["information_schema", "mysql", "performance_schema", "sys"].includes(db.toLowerCase());

export type MyRowKind = "db" | "group" | "object" | "column" | "status";

export interface MyRow {
  id: string;
  depth: number;
  kind: MyRowKind;
  label: string;
  expandable: boolean;
  expanded: boolean;
  db?: string;
  group?: MyGroup;
  count?: number;
  object?: SchemaObject;
  column?: ColumnInfo;
  statusKind?: "loading" | "error" | "empty";
}

export interface MyModelInput {
  databases: QueryState<string[]>;
  /** keyed by database name */
  objects: ReadonlyMap<string, QueryState<SchemaObject[]>>;
  /** keyed by myTableKey() */
  describes: ReadonlyMap<string, QueryState<TableDescription>>;
  toggled: ReadonlyMap<string, boolean>;
  showSystem: boolean;
  /** The connection's default database: always listed, even when it is a system one. */
  defaultDb?: string | null;
}

/** Groups are open unless closed; everything else is closed unless opened. */
export const myIsExpanded = (toggled: ReadonlyMap<string, boolean>, id: string): boolean => toggled.get(id) ?? id.startsWith(`g${SEP}`);

const byName = (a: string, b: string) => a.localeCompare(b, undefined, { sensitivity: "base", numeric: true });

function status(depth: number, statusKind: "loading" | "error" | "empty", label: string, id: string): MyRow {
  return { id, depth, kind: "status", label, expandable: false, expanded: false, statusKind };
}

export function buildMyRows(m: MyModelInput): MyRow[] {
  if (m.databases.status === "pending") return [status(0, "loading", "Loading databases…", "root-loading")];
  if (m.databases.status === "error") return [status(0, "error", m.databases.error ?? "Could not list databases", "root-error")];
  const dbs = [...(m.databases.data ?? [])].filter((d) => m.showSystem || !isMySystemDatabase(d) || d === m.defaultDb).sort(byName);
  if (dbs.length === 0) return [status(0, "empty", "No databases", "root-empty")];
  const out: MyRow[] = [];
  for (const db of dbs) {
    const did = myDbId(db);
    const open = myIsExpanded(m.toggled, did);
    out.push({ id: did, depth: 0, kind: "db", label: db, expandable: true, expanded: open, db });
    if (!open) continue;
    const oq = m.objects.get(db);
    if (!oq || oq.status === "pending") {
      out.push(status(1, "loading", "Loading…", `${did}${SEP}loading`));
      continue;
    }
    if (oq.status === "error") {
      out.push(status(1, "error", oq.error ?? "Could not list objects", `${did}${SEP}error`));
      continue;
    }
    const objs = oq.data ?? [];
    if (objs.length === 0) {
      out.push(status(1, "empty", "No tables, views or routines", `${did}${SEP}empty`));
      continue;
    }
    for (const g of MY_GROUPS) {
      const members = objs.filter((o) => myGroupOf(o.kind) === g.key).sort((a, b) => byName(a.name, b.name));
      if (members.length === 0) continue;
      const gid = myGroupId(db, g.key);
      const gOpen = myIsExpanded(m.toggled, gid);
      out.push({ id: gid, depth: 1, kind: "group", label: g.label, expandable: true, expanded: gOpen, db, group: g.key, count: members.length });
      if (!gOpen) continue;
      for (const o of members) {
        const oid = myObjectId(db, o);
        const cols = myHasColumns(o.kind);
        const oOpen = cols && myIsExpanded(m.toggled, oid);
        out.push({ id: oid, depth: 2, kind: "object", label: o.name, expandable: cols, expanded: oOpen, db, object: o });
        if (!oOpen) continue;
        const dq = m.describes.get(myTableKey(db, o.name));
        if (!dq || dq.status === "pending") out.push(status(3, "loading", "Loading…", `${oid}${SEP}loading`));
        else if (dq.status === "error") out.push(status(3, "error", dq.error ?? "Could not describe the table", `${oid}${SEP}error`));
        else if ((dq.data?.columns ?? []).length === 0) out.push(status(3, "empty", "No columns", `${oid}${SEP}empty`));
        else for (const c of dq.data?.columns ?? []) out.push({ id: `${oid}${SEP}c${SEP}${c.name}`, depth: 3, kind: "column", label: c.name, expandable: false, expanded: false, db, object: o, column: c });
      }
    }
  }
  return out;
}

/** Databases whose objects are needed, given the expansion state. */
export function expandedMyDatabases(dbs: readonly string[] | undefined, toggled: ReadonlyMap<string, boolean>): string[] {
  return (dbs ?? []).filter((d) => myIsExpanded(toggled, myDbId(d)));
}

/**
 * Query-key prefixes a Refresh invalidates (keys of `myQueries`: ['my', profile, db, kind, name]). A
 * database refresh covers its object list and every column list under it; an object refresh its columns.
 */
export function myRefreshKeys(profileId: string, row?: Pick<MyRow, "kind" | "db" | "object">): (readonly unknown[])[] {
  if (row?.kind === "object" && row.db && row.object) return [["my", profileId, row.db, "describe", row.object.name]];
  if ((row?.kind === "db" || row?.kind === "group") && row.db) return [["my", profileId, row.db]];
  return [["my", profileId]];
}
