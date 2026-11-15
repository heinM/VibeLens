import { useQuery } from "@tanstack/react-query";
import { api } from "../ipc/commands";
import type { ProviderSummary } from "../ipc/types";
import { useAgentStore } from "../store/useAgentStore";

export const AGENT_PROVIDERS_KEY = ["agent-providers"] as const;

/** A provider can be used once a model is set and, when required, an API key is stored. */
export const isProviderUsable = (p: ProviderSummary): boolean => p.config.model.trim() !== "" && (!p.config.requiresApiKey || p.hasApiKey);

export function useAgentProviders() {
  const query = useQuery({ queryKey: AGENT_PROVIDERS_KEY, queryFn: () => api.agentListProviders(), refetchOnMount: "always" });
  const defaultId = useAgentStore((s) => s.defaultProviderId);
  const providers = query.data ?? [];
  const usable = providers.filter(isProviderUsable);
  const selected = usable.find((p) => p.config.id === defaultId) ?? usable[0] ?? null;
  return { query, providers, usable, selected };
}
