// Which connection / database / schema "the current ER diagram" means. Pure.
import type { ConnectionProfile } from "../../ipc/types";
import type { WorkspaceTab } from "../../store/useWorkspaceStore";

export interface ErdTarget {
  profileId: string;
  database: string | null;
  schema: string;
}

export interface ErdTargetInput {
  activeTab: Pick<WorkspaceTab, "kind" | "profileId" | "database" | "objectRef"> | null;
  activeProfileId: string | null;
  activeDatabase: string | null;
  profiles: readonly ConnectionProfile[] | undefined;
  /** Schemas known for the chosen connection, biggest first (see `schemasByTableCount`), to pick a sensible default. */
  schemasOf?: (profileId: string, database: string | null) => string[];
}

/**
 * The active tab wins (a table or diagram tab knows its schema; a SQL tab knows its connection),
 * then the sidebar selection. The schema is the tab's own, else the catalog's biggest schema
 * (`public` wins ties), else `public`. Null when no Postgres connection is in play.
 */
export function resolveErdTarget(input: ErdTargetInput): ErdTarget | null {
  const { activeTab: tab, profiles } = input;
  const pgProfile = (id: string | null | undefined) => (id ? profiles?.find((p) => p.id === id && p.engine === "postgres") : undefined);

  let profile = tab && tab.kind !== "valkey" ? pgProfile(tab.profileId) : undefined;
  let database: string | null = profile ? (tab?.database ?? null) : null;
  if (!profile) {
    profile = pgProfile(input.activeProfileId);
    database = profile ? input.activeDatabase : null;
  }
  if (!profile) return null;

  let schema = tab && (tab.kind === "table" || tab.kind === "erd") && tab.profileId === profile.id ? tab.objectRef?.schema : undefined;
  if (!schema) {
    schema = input.schemasOf?.(profile.id, database)[0] ?? "public";
  }
  return { profileId: profile.id, database, schema };
}

/** Distinct schema names of a catalog, most tables first; `public` wins ties, then alphabetical. */
export function schemasByTableCount(tables: readonly { schema: string }[] | undefined): string[] {
  const counts = new Map<string, number>();
  for (const t of tables ?? []) counts.set(t.schema, (counts.get(t.schema) ?? 0) + 1);
  return [...counts.entries()]
    .sort((a, b) => b[1] - a[1] || (a[0] === "public" ? -1 : b[0] === "public" ? 1 : a[0].localeCompare(b[0])))
    .map(([name]) => name);
}
