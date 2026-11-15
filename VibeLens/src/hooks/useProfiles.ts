import { useCallback } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import type { ConnectionProfile } from "../ipc/types";

export const PROFILES_KEY = ["profiles"] as const;

export function useProfiles() {
  return useQuery({ queryKey: PROFILES_KEY, queryFn: api.listProfiles, staleTime: 60_000 });
}

/** The profile with the given id, or undefined while loading / when id is null. */
export function useProfile(id: string | null | undefined): ConnectionProfile | undefined {
  const { data } = useProfiles();
  return id ? data?.find((p) => p.id === id) : undefined;
}

/** Call after the wizard saves/deletes a profile so lists and trees refresh. */
export function useInvalidateProfiles() {
  const qc = useQueryClient();
  return useCallback(() => qc.invalidateQueries({ queryKey: PROFILES_KEY }), [qc]);
}
