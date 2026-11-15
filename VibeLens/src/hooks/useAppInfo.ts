import { useQuery } from "@tanstack/react-query";
import { api } from "../ipc/commands";

export const APP_INFO_KEY = ["app-info"] as const;

/** Static facts about the running app (version, vault backend, directories). */
export function useAppInfo() {
  return useQuery({ queryKey: APP_INFO_KEY, queryFn: api.appInfo, staleTime: Infinity });
}
