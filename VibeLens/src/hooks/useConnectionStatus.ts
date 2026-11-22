import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import type { ConnectionStatus } from "../ipc/types";
import { useHostKeyPrompt } from "../components/connections/hostKeyPrompt";
import { connectTrustingHostKey } from "../components/connections/hostKey";

export const STATUS_KEY = ["connection-status"] as const;

/** All connection statuses, polled every 5 s (idle reapers and tunnels can drop a connection at any time). */
export function useConnectionStatuses() {
  return useQuery({
    queryKey: STATUS_KEY,
    queryFn: api.connectionStatus,
    refetchInterval: 5000,
    staleTime: 0,
  });
}

/** Status for one profile, or undefined if the backend has not reported it (= disconnected). */
export function useConnectionStatus(profileId: string | null | undefined): ConnectionStatus | undefined {
  const { data } = useConnectionStatuses();
  return profileId ? data?.find((s) => s.profileId === profileId) : undefined;
}

export function useIsConnected(profileId: string | null | undefined): boolean {
  return useConnectionStatus(profileId)?.state === "connected";
}

/**
 * Connects a profile. When its SSH host has no pinned key, the user is shown the key the server
 * presented and decides; trusting pins it on the profile and connects again (see `hostKey.ts`).
 */
export function useConnect() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (profileId: string) =>
      connectTrustingHostKey(profileId, {
        connect: api.connect,
        ask: useHostKeyPrompt.getState().ask,
        trust: async (id, key) => {
          await api.trustHostKey(id, key);
          await qc.invalidateQueries({ queryKey: ["profiles"] });
        },
      }),
    onSuccess: (status) => {
      qc.setQueryData<ConnectionStatus[]>(STATUS_KEY, (old) => [...(old ?? []).filter((s) => s.profileId !== status.profileId), status]);
      void qc.invalidateQueries({ queryKey: STATUS_KEY });
    },
    onError: () => void qc.invalidateQueries({ queryKey: STATUS_KEY }),
  });
}

export function useDisconnect() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (profileId: string) => api.disconnect(profileId),
    onSuccess: (_void, profileId) => {
      // Catalog data from a dead connection is stale by definition.
      qc.removeQueries({ queryKey: ["pg", profileId] });
      qc.removeQueries({ queryKey: ["vk", profileId] });
      void qc.invalidateQueries({ queryKey: STATUS_KEY });
    },
  });
}
