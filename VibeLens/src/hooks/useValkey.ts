// Valkey queries. Keys: ['vk', profileId, ...].
import { queryOptions, useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import { useIsConnected } from "./useConnectionStatus";
import type { KeyEntry } from "../ipc/types";
import { useMemo } from "react";

export const vkQueries = {
  dbs: (profileId: string) => queryOptions({ queryKey: ["vk", profileId, "dbs"] as const, queryFn: () => api.vkListDbs(profileId) }),
  value: (profileId: string, db: number, key: string, maxItems?: number) =>
    queryOptions({
      queryKey: ["vk", profileId, db, "value", key, maxItems ?? null] as const,
      queryFn: () => api.vkGetValue(profileId, db, key, maxItems),
      staleTime: 0,
    }),
};

export function useValkeyDbs(profileId: string | null) {
  const connected = useIsConnected(profileId);
  return useQuery({ ...vkQueries.dbs(profileId ?? ""), enabled: connected && !!profileId });
}

/** Cursor-paged SCAN. `keys` is the de-duplicated concatenation of all pages loaded so far. */
export function useValkeyScan(profileId: string | null, db: number | null, pattern: string, count?: number, enabled = true) {
  const connected = useIsConnected(profileId);
  const q = useInfiniteQuery({
    queryKey: ["vk", profileId, db, "scan", pattern, count ?? null],
    initialPageParam: "0",
    queryFn: ({ pageParam }) => api.vkScan(profileId ?? "", db ?? 0, pattern || "*", pageParam, count),
    getNextPageParam: (last) => (last.done ? undefined : last.cursor),
    enabled: enabled && connected && !!profileId && db !== null,
    staleTime: Infinity, // SCAN pages are replayed by refetch; refresh is an explicit user action
  });
  const keys = useMemo<KeyEntry[]>(() => {
    const seen = new Set<string>();
    const out: KeyEntry[] = [];
    for (const page of q.data?.pages ?? []) {
      for (const k of page.keys) {
        if (!seen.has(k.key)) {
          seen.add(k.key);
          out.push(k);
        }
      }
    }
    return out;
  }, [q.data]);
  const done = q.data ? (q.data.pages[q.data.pages.length - 1]?.done ?? false) : false;
  return { ...q, keys, done };
}

export function useValkeyValue(profileId: string | null, db: number, key: string | null, maxItems?: number) {
  const connected = useIsConnected(profileId);
  return useQuery({ ...vkQueries.value(profileId ?? "", db, key ?? "", maxItems), enabled: connected && !!profileId && key !== null });
}
