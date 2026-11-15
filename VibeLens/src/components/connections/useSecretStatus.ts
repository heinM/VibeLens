import { useQuery } from "@tanstack/react-query";
import { api } from "../../ipc/commands";

/** Which keychain entries really exist for a saved profile (booleans only, never secrets). */
export function useSecretStatus(profileId: string | null | undefined) {
  return useQuery({
    queryKey: ["profiles", profileId, "secret-status"],
    queryFn: () => api.profileSecretStatus(profileId as string),
    enabled: !!profileId,
    staleTime: 0,
  });
}
