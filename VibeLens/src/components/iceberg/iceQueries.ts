// TanStack Query option factories for Iceberg catalog reads. Keys: ['ice', profileId, ...]
// so one invalidate on ['ice', profileId] refreshes the whole navigator and every open table tab.
import { queryOptions } from "@tanstack/react-query";
import { api } from "../../ipc/commands";
import { joinNamespace } from "./openIcebergTab";

export const iceQueries = {
  /** Child namespaces of `parent` (top level when empty). */
  namespaces: (profileId: string, parent: string[]) =>
    queryOptions({
      queryKey: ["ice", profileId, "ns", joinNamespace(parent)] as const,
      queryFn: () => api.iceListNamespaces(profileId, parent.length ? parent : null),
      staleTime: 30_000,
    }),
  tables: (profileId: string, namespace: string[]) =>
    queryOptions({
      queryKey: ["ice", profileId, "tables", joinNamespace(namespace)] as const,
      queryFn: () => api.iceListTables(profileId, namespace),
      staleTime: 30_000,
    }),
  describe: (profileId: string, namespace: string[], name: string) =>
    queryOptions({
      queryKey: ["ice", profileId, "describe", joinNamespace(namespace), name] as const,
      queryFn: () => api.iceDescribeTable(profileId, namespace, name),
      staleTime: 60_000,
    }),
};
