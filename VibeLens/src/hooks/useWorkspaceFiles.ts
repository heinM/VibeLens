import { useQuery } from "@tanstack/react-query";
import { api } from "../ipc/commands";

export const WS_FILES_KEY = ["ws", "files"] as const;

export function useWorkspaceFiles() {
  return useQuery({ queryKey: WS_FILES_KEY, queryFn: api.wsList, staleTime: 15_000 });
}
