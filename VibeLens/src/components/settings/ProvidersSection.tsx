import { useState } from "react";
import { Plus } from "lucide-react";
import { asError } from "../../ipc/commands";
import type { ProviderSummary } from "../../ipc/types";
import { uid } from "../../lib/id";
import { useAgentProviders } from "../../hooks/useAgentProviders";
import { useAgentStore } from "../../store/useAgentStore";
import { Button, ErrorBox, Select, Spinner } from "../ui";
import { FormField, Notice } from "../ui/formKit";
import { ProviderCard } from "./ProviderCard";
import { CUSTOM_PREFIX, blankCustom, blankPortkey } from "./providerDraft";

export function ProvidersSection() {
  const { query, providers, usable, selected } = useAgentProviders();
  const setDefault = useAgentStore((s) => s.setDefaultProviderId);
  const [open, setOpen] = useState<string | null>(null);
  const [drafts, setDrafts] = useState<ProviderSummary[]>([]);

  const addCustom = (kind: "openAiCompatible" | "portkey" = "openAiCompatible") => {
    const id = `${CUSTOM_PREFIX}${uid().slice(0, 8)}`;
    setDrafts((d) => [...d, { config: kind === "portkey" ? blankPortkey(id) : blankCustom(id), hasApiKey: false }]);
    setOpen(id);
  };

  return (
    <div className="space-y-4">
      <div>
        <h3 className="text-[13px] font-semibold">AI providers</h3>
        <p className="mt-0.5 text-[12px] text-muted">Model ids and endpoints are configuration, not code: set whichever your provider currently offers.</p>
      </div>

      <Notice tone="info">
        What reaches a provider is decided per connection by its <strong className="font-semibold text-fg">AI access</strong> setting (none, schema only, results with approval, or results). Your prompts and any editor context you attach are always sent to the provider you pick. Keys are stored in the OS keychain.
      </Notice>

      <FormField label="Default provider" htmlFor="st-default" hint={usable.length === 0 ? "Set a model (and a key, if required) on a provider below to enable it." : undefined}>
        <Select id="st-default" value={selected?.config.id ?? ""} disabled={usable.length === 0} onChange={(e) => setDefault(e.target.value)} className="max-w-sm">
          {usable.length === 0 && <option value="">No provider is ready</option>}
          {usable.map((p) => (
            <option key={p.config.id} value={p.config.id}>
              {p.config.label} · {p.config.model}
            </option>
          ))}
        </Select>
      </FormField>

      {query.isPending ? (
        <div className="flex items-center gap-2 text-[12px] text-muted">
          <Spinner className="size-3.5" /> Loading providers
        </div>
      ) : query.isError ? (
        <ErrorBox error={asError(query.error)} />
      ) : (
        <div className="space-y-2">
          {providers.map((p) => (
            <ProviderCard
              key={p.config.id}
              summary={p}
              isNew={false}
              isDefault={selected?.config.id === p.config.id}
              expanded={open === p.config.id}
              onToggle={() => setOpen((o) => (o === p.config.id ? null : p.config.id))}
              onDiscard={() => undefined}
              onSaved={(id) => {
                if (!useAgentStore.getState().defaultProviderId) setDefault(id);
              }}
            />
          ))}
          {drafts
            .filter((d) => !providers.some((p) => p.config.id === d.config.id))
            .map((d) => (
              <ProviderCard
                key={d.config.id}
                summary={d}
                isNew
                isDefault={false}
                expanded={open === d.config.id}
                onToggle={() => setOpen((o) => (o === d.config.id ? null : d.config.id))}
                onDiscard={() => setDrafts((all) => all.filter((x) => x.config.id !== d.config.id))}
                onSaved={(id) => {
                  if (!useAgentStore.getState().defaultProviderId) setDefault(id);
                  setDrafts((all) => all.filter((x) => x.config.id !== id));
                  setOpen(id);
                }}
              />
            ))}
          <div className="mt-1 flex flex-wrap gap-2">
            <Button onClick={() => addCustom()}>
              <Plus className="size-3.5" /> Add custom provider
            </Button>
            <Button onClick={() => addCustom("portkey")}>
              <Plus className="size-3.5" /> Add Portkey gateway
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
