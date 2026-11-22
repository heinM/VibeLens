// Everything "Go to anything" can jump to, as one memoised item list. Metadata comes from the
// shared schema cache (same query keys as the editor and ER diagrams), so opening the palette
// never waits on the network once a connection is up.
import { useCallback, useMemo } from "react";
import { useQueries } from "@tanstack/react-query";
import { api } from "../../ipc/commands";
import type { ConnectionProfile, SchemaMetadata, SchemaObject } from "../../ipc/types";
import { useConnect, useConnectionStatuses } from "../../hooks/useConnectionStatus";
import { useProfiles } from "../../hooks/useProfiles";
import { pgQueries } from "../../hooks/usePg";
import { useTabActions } from "../../hooks/useTabActions";
import { useWorkspaceFiles } from "../../hooks/useWorkspaceFiles";
import { schemaMetadataKey } from "../../lib/schemaCache";
import { useHistoryStore } from "../../store/useHistoryStore";
import { useUIStore } from "../../store/useUIStore";
import { useWorkspaceStore } from "../../store/useWorkspaceStore";
import { copyToClipboard } from "../ui/clipboard";
import { openErdTab, openTableTab } from "../table/openTableTab";
import { insertIntoActiveEditor } from "./insertIntoEditor";
import { openIcebergQueryTab } from "./icebergPalette";
import { dialectOf } from "../../lib/sqlIntel/dialect";
import { catalogItems, connectionItems, fileItems, functionItems, historyItems, insertionText, mysqlCatalogItems, type GoActions, type PaletteItem } from "./items";
import { mySelectAllSql } from "../sidebar/mysqlTreeModel";
import { usePaletteStore } from "./paletteStore";

const MAX_FUNCTION_SCHEMAS = 16;

type MetaCombined = { metas: (SchemaMetadata | undefined)[]; loading: number };
const combineMeta = (results: { data: SchemaMetadata | undefined; isFetching: boolean }[]): MetaCombined => ({
  metas: results.map((r) => r.data),
  loading: results.filter((r) => r.isFetching).length,
});
type ObjCombined = { objects: (SchemaObject[] | undefined)[] };
const combineObjects = (results: { data: SchemaObject[] | undefined }[]): ObjCombined => ({ objects: results.map((r) => r.data) });

export interface PaletteCatalog {
  items: PaletteItem[];
  /** Catalog fetches in flight (shown as "indexing" in the palette header). */
  loading: number;
  connectedPg: ConnectionProfile[];
}

/** Always-mounted: begins fetching a connection's catalog as soon as it connects. */
export function usePaletteCatalog(): PaletteCatalog {
  const { data: profiles } = useProfiles();
  const { data: statuses } = useConnectionStatuses();
  const armed = usePaletteStore((s) => s.armed);
  const files = useWorkspaceFiles().data;
  const history = useHistoryStore((s) => s.entries);
  const connect = useConnect();
  const { newSqlTab, newValkeyConsole } = useTabActions();

  const stateOf = useCallback((id: string) => statuses?.find((s) => s.profileId === id)?.state ?? "disconnected", [statuses]);

  const connectedPg = useMemo(
    () => (profiles ?? []).filter((p) => p.engine === "postgres" && statuses?.some((s) => s.profileId === p.id && s.state === "connected")),
    [profiles, statuses],
  );

  const connectedMy = useMemo(
    () => (profiles ?? []).filter((p) => p.engine === "mysql" && statuses?.some((s) => s.profileId === p.id && s.state === "connected")),
    [profiles, statuses],
  );
  const { metas: myMetas, loading: myLoading } = useQueries({
    queries: connectedMy.map((p) => ({
      queryKey: schemaMetadataKey(p.id, p.database ?? null, "mysql"),
      queryFn: () => api.mySchemaMetadata(p.id, p.database ?? null),
      staleTime: 5 * 60_000,
    })),
    combine: combineMeta,
  });

  const { metas, loading } = useQueries({
    queries: connectedPg.map((p) => ({
      queryKey: schemaMetadataKey(p.id, p.database ?? null),
      queryFn: () => api.pgSchemaMetadata(p.id, p.database ?? null),
      staleTime: 5 * 60_000,
    })),
    combine: combineMeta,
  });

  // Functions are not in the catalog payload: read the (cached, sidebar-shared) object lists of
  // each schema, but only once the palette has been opened, and for a bounded number of schemas.
  const fnTargets = useMemo(() => {
    if (!armed) return [] as { p: ConnectionProfile; schema: string }[];
    const out: { p: ConnectionProfile; schema: string }[] = [];
    connectedPg.forEach((p, i) => {
      const schemas = [...new Set((metas[i]?.tables ?? []).map((t) => t.schema))].slice(0, MAX_FUNCTION_SCHEMAS);
      for (const schema of schemas) out.push({ p, schema });
    });
    return out;
  }, [armed, connectedPg, metas]);
  const { objects } = useQueries({
    queries: fnTargets.map((t) => pgQueries.objects(t.p.id, t.p.database ?? null, t.schema)),
    combine: combineObjects,
  });

  const actions = useMemo<GoActions>(
    () => ({
      openConnection: (p) => {
        const ui = useUIStore.getState();
        if (!ui.sidebarOpen) ui.toggleSidebar();
        ui.setActiveTarget(p.id, null);
        if (stateOf(p.id) !== "connected" && stateOf(p.id) !== "connecting") connect.mutate(p.id);
      },
      newTabOnConnection: (p) => {
        useUIStore.getState().setActiveTarget(p.id, null);
        if (stateOf(p.id) === "disconnected") connect.mutate(p.id);
        if (p.engine === "postgres") newSqlTab({ profileId: p.id, database: p.database ?? null });
        else if (p.engine === "iceberg") openIcebergQueryTab(p);
        else if (p.engine === "redshift" || p.engine === "mysql") newSqlTab({ profileId: p.id, database: null });
        else newValkeyConsole({ profileId: p.id });
      },
      openTable: (p, db, schema, name) => {
        useUIStore.getState().setActiveTarget(p.id, db);
        openTableTab(p.id, db, schema, name);
      },
      insertName: (p, db, schema, name) => {
        const text = insertionText(schema, name, dialectOf(p));
        void insertIntoActiveEditor(text).then((ok) => {
          if (!ok) newSqlTab({ profileId: p.id, database: db, contents: text });
        });
      },
      openErd: (p, db, schema) => {
        useUIStore.getState().setActiveTarget(p.id, db);
        openErdTab(p.id, db, schema);
      },
      openFile: (path) => {
        void useWorkspaceStore.getState().openWorkspaceFile(path, useUIStore.getState().activeProfileId);
      },
      openHistory: (e) => {
        // An Iceberg statement reopens on its Iceberg connection (`newSqlTab` is Postgres-only).
        if (e.engine === "iceberg" && e.profileId) openIcebergQueryTab({ id: e.profileId }, { contents: e.sql });
        else newSqlTab({ profileId: e.profileId, database: e.database, contents: e.sql });
      },
      copy: (text) => void copyToClipboard(text),
    }),
    [connect, newSqlTab, newValkeyConsole, stateOf],
  );

  const items = useMemo(() => {
    const out: PaletteItem[] = connectionItems(profiles ?? [], stateOf, actions);
    connectedPg.forEach((p, i) => {
      const m = metas[i];
      if (m) out.push(...catalogItems(p, p.database ?? null, m, actions));
    });
    connectedMy.forEach((p, i) => {
      const m = myMetas[i];
      if (m)
        out.push(
          ...mysqlCatalogItems(p, m, {
            query: (db, name) => newSqlTab({ profileId: p.id, database: db, title: name, contents: mySelectAllSql(db, name), autoRun: true }),
            insertName: actions.insertName,
          }),
        );
    });
    fnTargets.forEach((t, i) => {
      const o = objects[i];
      if (o) out.push(...functionItems(t.p, t.p.database ?? null, o, actions));
    });
    out.push(...fileItems(files ?? [], actions));
    out.push(...historyItems(history, actions));
    return out;
  }, [profiles, stateOf, actions, connectedPg, metas, connectedMy, myMetas, newSqlTab, fnTargets, objects, files, history]);

  return { items, loading: loading + myLoading, connectedPg };
}
