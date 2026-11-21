// Iceberg entries of the command palette: which Iceberg connection the commands act on, the list of
// tables across namespaces ("Iceberg: open table…"), and how they open. Pure builders plus one async
// walker over the catalog; the React side lives in CommandPalette.tsx.
import type { QueryClient } from "@tanstack/react-query";
import type { ConnectionProfile, ConnectionState, IcebergTableRef } from "../../ipc/types";
import { useUIStore } from "../../store/useUIStore";
import { useWorkspaceStore } from "../../store/useWorkspaceStore";
import { requestAutoRun } from "../editor/autoRun";
import { iceQueries } from "../iceberg/iceQueries";
import { selectAllSql } from "../iceberg/iceSql";
import { dialectOf } from "../../lib/sqlIntel/dialect";
import { joinNamespace } from "../iceberg/openIcebergTab";
import type { PaletteItem } from "./items";

export const ICEBERG_TABLES_GROUP = "Iceberg tables";

/** Where the Iceberg commands act: the active tab's connection, else the active connection, else the first connected one. */
export function resolveIcebergTarget(env: {
  profiles: readonly ConnectionProfile[];
  activeTabProfileId: string | null | undefined;
  activeProfileId: string | null | undefined;
  stateOf: (id: string) => ConnectionState;
}): ConnectionProfile | null {
  const ice = (id: string | null | undefined) => (id ? env.profiles.find((p) => p.id === id && p.engine === "iceberg") : undefined);
  return ice(env.activeTabProfileId) ?? ice(env.activeProfileId) ?? env.profiles.find((p) => p.engine === "iceberg" && env.stateOf(p.id) === "connected") ?? null;
}

/** Opens an empty (or pre-filled) worksheet bound to an Iceberg connection. `newSqlTab` is Postgres-only. */
export function openIcebergQueryTab(profile: Pick<ConnectionProfile, "id">, opts: { contents?: string; title?: string; autoRun?: boolean } = {}): string {
  const ws = useWorkspaceStore.getState();
  let n = 1;
  while (ws.tabs.some((t) => t.title === `query-${n}.sql`)) n++;
  const id = ws.openTab({ kind: "sql", title: opts.title ?? `query-${n}.sql`, profileId: profile.id, database: null, contents: opts.contents ?? "" });
  useUIStore.getState().setActiveTarget(profile.id, null);
  if (opts.autoRun) requestAutoRun(id);
  return id;
}

// -- the table list ---------------------------------------------------------------------------------

export interface IcebergTableList {
  tables: IcebergTableRef[];
  /** True when the walk stopped at the namespace limit. */
  truncated: boolean;
}

/**
 * Every table and view under every namespace of a connection. Walks the namespace tree breadth-first
 * and lists tables a few namespaces at a time, through the same cached queries as the navigator, so a
 * browsed catalog costs nothing extra.
 */
export async function listIcebergTables(qc: QueryClient, profileId: string, opts: { maxNamespaces?: number; concurrency?: number } = {}): Promise<IcebergTableList> {
  const max = opts.maxNamespaces ?? 400;
  const width = Math.max(1, opts.concurrency ?? 6);
  const namespaces: string[][] = [];
  let frontier: string[][] = [[]];
  let truncated = false;
  while (frontier.length > 0 && !truncated) {
    const next: string[][] = [];
    for (let i = 0; i < frontier.length && !truncated; i += width) {
      const batch = await Promise.all(frontier.slice(i, i + width).map((parent) => qc.fetchQuery(iceQueries.namespaces(profileId, parent))));
      for (const children of batch) {
        for (const ns of children) {
          if (namespaces.length >= max) {
            truncated = true;
            break;
          }
          namespaces.push(ns.levels);
          next.push(ns.levels);
        }
      }
    }
    frontier = next;
  }
  const tables: IcebergTableRef[] = [];
  for (let i = 0; i < namespaces.length; i += width) {
    const lists = await Promise.all(namespaces.slice(i, i + width).map((ns) => qc.fetchQuery(iceQueries.tables(profileId, ns))));
    for (const l of lists) tables.push(...l);
  }
  return { tables, truncated };
}

export interface IcebergTableActions {
  openTable: (p: ConnectionProfile, namespace: string[], name: string) => void;
  queryTable: (p: ConnectionProfile, namespace: string[], name: string) => void;
}

/** Palette rows for the "Iceberg: open table…" picker. */
export function icebergTableItems(p: ConnectionProfile, tables: readonly IcebergTableRef[], a: IcebergTableActions): PaletteItem[] {
  return tables.map((t): PaletteItem => {
    const dotted = t.namespace.join(".");
    const isView = t.kind === "view";
    return {
      id: `icetable:${p.id}:${joinNamespace(t.namespace)}.${t.name}`,
      group: ICEBERG_TABLES_GROUP,
      title: t.name,
      search: `${dotted}.${t.name}`,
      context: p.name,
      icon: isView ? "view" : "table",
      tone: isView ? "emerald" : "cyan",
      crumbs: [p.name, dotted],
      primary: isView ? "Open view" : "Open table",
      run: () => a.openTable(p, t.namespace, t.name),
      alt: { label: "New query", run: () => a.queryTable(p, t.namespace, t.name) },
    };
  });
}

/** The default actions of the picker: the explorer tab, and a SELECT in a new worksheet. */
export function defaultIcebergTableActions(openTab: (profileId: string, namespace: string[], name: string) => void): IcebergTableActions {
  return {
    openTable: (p, namespace, name) => {
      useUIStore.getState().setActiveTarget(p.id, null);
      openTab(p.id, namespace, name);
    },
    queryTable: (p, namespace, name) => void openIcebergQueryTab(p, { title: `${name}.sql`, contents: selectAllSql(namespace, name, 100, dialectOf(p)), autoRun: true }),
  };
}
