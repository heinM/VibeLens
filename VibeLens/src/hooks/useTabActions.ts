import { useCallback } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useUIStore } from "../store/useUIStore";
import { useWorkspaceStore } from "../store/useWorkspaceStore";
import type { ConnectionProfile } from "../ipc/types";
import { PROFILES_KEY } from "./useProfiles";
import { requestAutoRun } from "../components/editor/autoRun";

export const quoteIdent = (s: string) => `"${s.replace(/"/g, '""')}"`;

export interface NewSqlTabOptions {
  profileId?: string | null;
  database?: string | null;
  contents?: string;
  title?: string;
  autoRun?: boolean;
}

/** Imperative helpers for opening tabs with sensible default targets. Safe to call from event handlers. */
export function useTabActions() {
  const qc = useQueryClient();

  const profileOf = useCallback(
    (id: string | null | undefined) => (id ? qc.getQueryData<ConnectionProfile[]>(PROFILES_KEY)?.find((p) => p.id === id) : undefined),
    [qc],
  );

  const nextTitle = (prefix: string, ext: string) => {
    const tabs = useWorkspaceStore.getState().tabs;
    let n = 1;
    while (tabs.some((t) => t.title === `${prefix}-${n}.${ext}`)) n++;
    return `${prefix}-${n}.${ext}`;
  };

  const newSqlTab = useCallback(
    (opts: NewSqlTabOptions = {}): string => {
      const ui = useUIStore.getState();
      let profileId = opts.profileId !== undefined ? opts.profileId : ui.activeProfileId;
      let database = opts.database !== undefined ? opts.database : ui.activeDatabase;
      const p = profileOf(profileId);
      // SQL tabs run on Postgres, Iceberg and Redshift; a Valkey connection gets a console instead.
      // A Redshift tab keeps its database (null = the connection's own).
      if (p && p.engine === "valkey") {
        profileId = null;
        database = null;
      } else if (p && p.engine === "iceberg") {
        database = null; // Iceberg has no database; the toolbar shows the default namespace
      }
      const id = useWorkspaceStore.getState().openTab({
        kind: "sql",
        title: opts.title ?? nextTitle("query", "sql"),
        profileId: profileId ?? null,
        database: database ?? null,
        contents: opts.contents ?? "",
      });
      if (opts.autoRun) requestAutoRun(id);
      return id;
    },
    [profileOf],
  );

  const newValkeyConsole = useCallback(
    (opts: { profileId?: string | null; db?: number } = {}): string => {
      const ui = useUIStore.getState();
      let profileId = opts.profileId !== undefined ? opts.profileId : ui.activeProfileId;
      const p = profileOf(profileId);
      if (p && p.engine !== "valkey") profileId = null;
      return useWorkspaceStore.getState().openTab({
        kind: "valkey",
        title: nextTitle("console", "valkey"),
        profileId: profileId ?? null,
        valkeyDb: opts.db ?? 0,
      });
    },
    [profileOf],
  );

  /** `SELECT * FROM "schema"."table" LIMIT 100;` in a new tab, already running. */
  const openTable = useCallback(
    (profileId: string, database: string | null, schema: string, table: string): string =>
      newSqlTab({
        profileId,
        database,
        title: table,
        contents: `SELECT * FROM ${quoteIdent(schema)}.${quoteIdent(table)} LIMIT 100;`,
        autoRun: true,
      }),
    [newSqlTab],
  );

  return { newSqlTab, newValkeyConsole, openTable };
}
