// Postgres catalog queries. Keys: ['pg', profileId, db, ...]. Enabled only while connected.
import { queryOptions, useQuery } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import { useIsConnected } from "./useConnectionStatus";

/** Option factories are exported so the sidebar can run a dynamic set of them with useQueries. */
export const pgQueries = {
  databases: (profileId: string) =>
    queryOptions({ queryKey: ["pg", profileId, "*", "databases"] as const, queryFn: () => api.pgListDatabases(profileId) }),
  schemas: (profileId: string, db: string | null) =>
    queryOptions({ queryKey: ["pg", profileId, db, "schemas"] as const, queryFn: () => api.pgListSchemas(profileId, db) }),
  objects: (profileId: string, db: string | null, schema: string) =>
    queryOptions({ queryKey: ["pg", profileId, db, "objects", schema] as const, queryFn: () => api.pgListObjects(profileId, db, schema) }),
  describe: (profileId: string, db: string | null, schema: string, table: string) =>
    queryOptions({
      queryKey: ["pg", profileId, db, "describe", schema, table] as const,
      queryFn: () => api.pgDescribeTable(profileId, db, schema, table),
    }),
};

export function usePgDatabases(profileId: string | null) {
  const connected = useIsConnected(profileId);
  return useQuery({ ...pgQueries.databases(profileId ?? ""), enabled: connected && !!profileId });
}

export function usePgSchemas(profileId: string | null, db: string | null, enabled = true) {
  const connected = useIsConnected(profileId);
  return useQuery({ ...pgQueries.schemas(profileId ?? "", db), enabled: enabled && connected && !!profileId });
}

export function usePgObjects(profileId: string | null, db: string | null, schema: string | null, enabled = true) {
  const connected = useIsConnected(profileId);
  return useQuery({ ...pgQueries.objects(profileId ?? "", db, schema ?? ""), enabled: enabled && connected && !!profileId && !!schema });
}

export function usePgDescribeTable(profileId: string | null, db: string | null, schema: string | null, table: string | null, enabled = true) {
  const connected = useIsConnected(profileId);
  return useQuery({
    ...pgQueries.describe(profileId ?? "", db, schema ?? "", table ?? ""),
    enabled: enabled && connected && !!profileId && !!schema && !!table,
  });
}
