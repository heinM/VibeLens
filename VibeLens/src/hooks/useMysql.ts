// MySQL catalog queries. Keys: ['my', profileId, database, ...] ('*' for the server-wide database list),
// so a refresh or DDL invalidates everything under ['my', profileId]. Enabled only while connected.
import { queryOptions, useQuery } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import { useIsConnected } from "./useConnectionStatus";

const STALE_MS = 60_000;

export const myQueries = {
  databases: (profileId: string) => queryOptions({ queryKey: ["my", profileId, "*", "databases"] as const, queryFn: () => api.myListDatabases(profileId), staleTime: STALE_MS }),
  objects: (profileId: string, db: string) =>
    queryOptions({ queryKey: ["my", profileId, db, "objects"] as const, queryFn: () => api.myListObjects(profileId, db), staleTime: STALE_MS }),
  describe: (profileId: string, db: string, table: string) =>
    queryOptions({ queryKey: ["my", profileId, db, "describe", table] as const, queryFn: () => api.myDescribeTable(profileId, db, table), staleTime: STALE_MS }),
};

export function useMyDatabases(profileId: string | null) {
  const connected = useIsConnected(profileId);
  return useQuery({ ...myQueries.databases(profileId ?? ""), enabled: connected && !!profileId });
}
