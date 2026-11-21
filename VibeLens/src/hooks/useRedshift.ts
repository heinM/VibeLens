// Redshift catalog queries through the Data API. Keys: ['rs', profileId, database, ...], `database`
// null meaning the connection's own. Enabled only while connected.
import { queryOptions, useQuery } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import { useIsConnected } from "./useConnectionStatus";

/** Catalog calls are AWS API calls: keep answers for a minute rather than refetching on every expand. */
const STALE_MS = 60_000;

export const rsQueries = {
  databases: (profileId: string) => queryOptions({ queryKey: ["rs", profileId, "*", "databases"] as const, queryFn: () => api.rsListDatabases(profileId), staleTime: STALE_MS }),
  schemas: (profileId: string, db: string | null) =>
    queryOptions({ queryKey: ["rs", profileId, db, "schemas"] as const, queryFn: () => api.rsListSchemas(profileId, db), staleTime: STALE_MS }),
  tables: (profileId: string, db: string | null, schema: string) =>
    queryOptions({ queryKey: ["rs", profileId, db, "tables", schema] as const, queryFn: () => api.rsListTables(profileId, db, schema), staleTime: STALE_MS }),
  describe: (profileId: string, db: string | null, schema: string, name: string) =>
    queryOptions({ queryKey: ["rs", profileId, db, "describe", schema, name] as const, queryFn: () => api.rsDescribeTable(profileId, db, schema, name), staleTime: STALE_MS }),
};

export function useRsDatabases(profileId: string | null) {
  const connected = useIsConnected(profileId);
  return useQuery({ ...rsQueries.databases(profileId ?? ""), enabled: connected && !!profileId });
}
