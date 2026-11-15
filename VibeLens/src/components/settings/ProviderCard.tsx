import { useEffect, useId, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Check, ChevronRight, KeyRound, Trash2 } from "lucide-react";
import { api, asError } from "../../ipc/commands";
import type { ErrorPayload, PortkeyApiFormat, PortkeyOptions, PortkeyRouting, ProviderKind, ProviderSummary } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { AGENT_PROVIDERS_KEY } from "../../hooks/useAgentProviders";
import { useAgentStore } from "../../store/useAgentStore";
import { Badge, Button, ConfirmDialog, ErrorBox, Input } from "../ui";
import { FormField, NumberField, SegmentedControl, Switch, invalidRing } from "../ui/formKit";
import { secretArg } from "../connections/draft";
import { SecretField } from "../connections/SecretField";
import { PORTKEY_BASE_URL, configToSave, defaultPortkey, draftFrom, isCustomProvider, isDirty, routingValue, validateProvider, withRoutingValue, type ProviderDraft } from "./providerDraft";

const KIND_LABEL: Record<ProviderKind, string> = { anthropic: "Anthropic", openAiCompatible: "OpenAI-compatible", portkey: "Portkey" };

const ROUTING_KINDS: { value: PortkeyRouting["kind"]; label: string }[] = [
  { value: "modelCatalog", label: "Model Catalog" },
  { value: "provider", label: "Provider" },
  { value: "virtualKey", label: "Virtual key" },
  { value: "config", label: "Config" },
];

const ROUTING_FIELD: Record<Exclude<PortkeyRouting["kind"], "modelCatalog">, { label: string; placeholder: string; hint: string }> = {
  provider: { label: "Provider slug", placeholder: "@bedrock-prod", hint: "Sent as x-portkey-provider. A Model Catalog slug (with @) or a provider name such as anthropic." },
  virtualKey: { label: "Virtual key", placeholder: "bedrock-claude-a1b2c3", hint: "Sent as x-portkey-virtual-key (older Portkey workspaces, and what capitec/pg-proxy uses)." },
  config: { label: "Config", placeholder: "pc-fallback-1a2b3c", hint: "Sent as x-portkey-config: a saved config id, or inline JSON without api_key (fallbacks, retries, caching)." },
};

/** Switches routing kind, carrying the typed value across (except into Config, which means something else). */
function routingOfKind(kind: PortkeyRouting["kind"], carry: string): PortkeyRouting {
  switch (kind) {
    case "provider":
      return { kind, slug: carry };
    case "virtualKey":
      return { kind, virtualKey: carry };
    case "config":
      return { kind, config: "" };
    default:
      return { kind: "modelCatalog" };
  }
}

export function ProviderCard({
  summary,
  isNew,
  isDefault,
  expanded,
  onToggle,
  onDiscard,
  onSaved,
}: {
  summary: ProviderSummary;
  isNew: boolean;
  isDefault: boolean;
  expanded: boolean;
  onToggle: () => void;
  /** Only for unsaved custom providers. */
  onDiscard: () => void;
  onSaved: (id: string) => void;
}) {
  const qc = useQueryClient();
  const uid = useId();
  const [draft, setDraft] = useState<ProviderDraft>(() => draftFrom(summary.config));
  const [showErrors, setShowErrors] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<ErrorPayload | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const sig = JSON.stringify([summary.config, summary.hasApiKey]);
  useEffect(() => {
    setDraft(draftFrom(summary.config));
    setShowErrors(false);
  }, [sig]); // eslint-disable-line react-hooks/exhaustive-deps

  const c = draft.config;
  const errors = validateProvider(draft);
  const e = (k: keyof typeof errors) => (showErrors ? errors[k] : undefined);
  const dirty = isNew || isDirty(draft, summary.config);
  const custom = isCustomProvider(c.id);
  const set = (patch: Partial<ProviderDraft["config"]>) => {
    setError(null);
    setDraft((d) => ({ ...d, config: { ...d.config, ...patch } }));
  };

  const modelMissing = summary.config.model.trim() === "";
  const keyState = !summary.config.requiresApiKey ? "No key needed" : summary.hasApiKey ? "Key stored" : "No key";
  const usable = !modelMissing && (!summary.config.requiresApiKey || summary.hasApiKey);

  const save = async () => {
    setShowErrors(true);
    const cfg = configToSave(draft);
    if (!cfg) return;
    setSaving(true);
    setError(null);
    try {
      await api.agentSaveProvider(cfg, secretArg(draft.apiKey));
      await qc.invalidateQueries({ queryKey: AGENT_PROVIDERS_KEY });
      onSaved(cfg.id);
    } catch (err) {
      setError(asError(err));
    } finally {
      setSaving(false);
    }
  };

  const remove = async () => {
    setSaving(true);
    try {
      await api.agentDeleteProvider(c.id);
      if (useAgentStore.getState().defaultProviderId === c.id) useAgentStore.getState().setDefaultProviderId(null);
      await qc.invalidateQueries({ queryKey: AGENT_PROVIDERS_KEY });
      setConfirmDelete(false);
    } catch (err) {
      setConfirmDelete(false);
      setError(asError(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className={cn("rounded-lg border bg-bg/50", expanded ? "border-border" : "border-border/80")}>
      <button
        type="button"
        aria-expanded={expanded}
        onClick={onToggle}
        className="flex min-h-11 w-full items-center gap-2.5 rounded-lg px-3 py-2 text-left outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
      >
        <ChevronRight className={cn("size-3.5 shrink-0 text-muted transition-transform", expanded && "rotate-90")} />
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-2">
            <span className="truncate text-[13px] font-medium">{isNew ? "New custom provider" : summary.config.label}</span>
            <Badge>{KIND_LABEL[summary.config.kind] ?? summary.config.kind}</Badge>
            {isDefault && usable && <Badge className="border-accent/40 bg-accent/10 text-accent">Default</Badge>}
          </span>
          <span className="mt-0.5 flex items-center gap-2 text-[11.5px] text-muted">
            {isNew ? (
              <span>Not saved yet</span>
            ) : modelMissing ? (
              <span className="text-warn">No model set</span>
            ) : (
              <span className="truncate font-mono">{summary.config.model}</span>
            )}
          </span>
        </span>
        {!isNew && (
          <span className={cn("flex shrink-0 items-center gap-1 text-[11.5px]", summary.config.requiresApiKey && !summary.hasApiKey ? "text-warn" : "text-muted")}>
            <KeyRound className="size-3" /> {keyState}
          </span>
        )}
      </button>

      {expanded && (
        <div className="space-y-3 border-t border-border px-3 pb-3 pt-3">
          <div className="grid gap-3 sm:grid-cols-2">
            <FormField label="Name" htmlFor={`${uid}-label`} error={e("label")}>
              <Input id={`${uid}-label`} value={c.label} className={cn(e("label") && invalidRing)} onChange={(ev) => set({ label: ev.target.value })} />
            </FormField>
            <FormField
              label="Model"
              htmlFor={`${uid}-model`}
              hint={
                c.kind === "anthropic"
                  ? undefined
                  : c.kind === "portkey"
                    ? "With Model Catalog routing: @provider-slug/model-id, e.g. @bedrock-prod/anthropic.claude-sonnet-…"
                    : "The model id exactly as your provider lists it."
              }
            >
              <Input
                id={`${uid}-model`}
                value={c.model}
                spellCheck={false}
                autoComplete="off"
                placeholder={c.kind === "anthropic" ? "e.g. claude-sonnet-5-5" : c.kind === "portkey" ? "@provider-slug/model-id" : "model id from your provider"}
                className="font-mono"
                onChange={(ev) => set({ model: ev.target.value })}
              />
            </FormField>
          </div>
          <FormField
            label="Base URL"
            htmlFor={`${uid}-url`}
            error={e("baseUrl")}
            hint={
              c.kind === "portkey"
                ? `Hosted: ${PORTKEY_BASE_URL}. Self-hosted gateway: e.g. http://localhost:8787/v1 (turn off the API key below if it has no auth).`
                : custom
                  ? "Any OpenAI Chat Completions compatible endpoint, including local ones."
                  : undefined
            }
          >
            <Input
              id={`${uid}-url`}
              value={c.baseUrl}
              spellCheck={false}
              autoComplete="off"
              placeholder={c.kind === "anthropic" ? "https://api.anthropic.com" : c.kind === "portkey" ? PORTKEY_BASE_URL : "e.g. http://localhost:11434/v1"}
              className={cn("font-mono", e("baseUrl") && invalidRing)}
              onChange={(ev) => set({ baseUrl: ev.target.value })}
            />
          </FormField>
          <div className="grid gap-3 sm:grid-cols-2">
            <FormField label="Max output tokens" htmlFor={`${uid}-mt`} error={e("maxTokens")}>
              <NumberField id={`${uid}-mt`} value={c.maxTokens} min={1} invalid={!!e("maxTokens")} onChange={(v) => set({ maxTokens: v ?? Number.NaN })} />
            </FormField>
            <FormField label="Temperature" htmlFor={`${uid}-temp`} optional error={e("temperature")} hint="0 to 2. Empty uses the provider's default.">
              <Input
                id={`${uid}-temp`}
                inputMode="decimal"
                value={draft.temperature}
                placeholder="Default"
                className={cn(e("temperature") && invalidRing)}
                onChange={(ev) => {
                  setError(null);
                  setDraft((d) => ({ ...d, temperature: ev.target.value }));
                }}
              />
            </FormField>
          </div>

          {c.kind === "portkey" && <PortkeyFields uid={uid} options={c.portkey ?? defaultPortkey()} error={e("portkey")} onChange={(portkey) => set({ portkey })} />}

          <Switch
            checked={c.requiresApiKey}
            onChange={(requiresApiKey) => set({ requiresApiKey })}
            label="Requires an API key"
            description="Turn off for local servers such as Ollama or LM Studio."
          />
          {c.requiresApiKey && (
            <SecretField
              label={c.kind === "portkey" ? "Portkey API key" : "API key"}
              edit={draft.apiKey}
              onChange={(apiKey) => {
                setError(null);
                setDraft((d) => ({ ...d, apiKey }));
              }}
              hasStored={summary.hasApiKey}
              storedLabel="Stored in OS keychain"
              hint={c.kind === "portkey" ? "Your Portkey key (sent as x-portkey-api-key, never as Authorization). Provider keys stay in Portkey." : "Write-only. Saved to the OS keychain and never shown again."}
              placeholder="Paste a key"
            />
          )}

          <ErrorBox error={error} />
          <div className="flex items-center gap-2">
            {custom && !isNew && (
              <Button variant="ghost" className="!text-danger" onClick={() => setConfirmDelete(true)} disabled={saving}>
                <Trash2 className="size-3.5" /> Delete provider
              </Button>
            )}
            <span className="flex-1" />
            {isNew ? (
              <Button variant="ghost" onClick={onDiscard} disabled={saving}>
                Discard
              </Button>
            ) : (
              <Button variant="ghost" disabled={!dirty || saving} onClick={() => { setDraft(draftFrom(summary.config)); setShowErrors(false); setError(null); }}>
                Revert
              </Button>
            )}
            <Button variant="primary" onClick={() => void save()} loading={saving} disabled={!dirty}>
              {!dirty && !isNew ? <><Check className="size-3.5" /> Saved</> : isNew ? "Add provider" : "Save changes"}
            </Button>
          </div>
        </div>
      )}

      <ConfirmDialog
        open={confirmDelete}
        title="Delete provider"
        danger
        confirmText="Delete provider"
        onCancel={() => setConfirmDelete(false)}
        onConfirm={() => void remove()}
        body={<p>Delete <strong className="font-semibold">{summary.config.label}</strong>? Its stored API key is removed from the keychain.</p>}
      />
    </div>
  );
}

function PortkeyFields({ uid, options, error, onChange }: { uid: string; options: PortkeyOptions; error?: string; onChange: (o: PortkeyOptions) => void }) {
  const r = options.routing;
  const field = r.kind === "modelCatalog" ? null : ROUTING_FIELD[r.kind];
  return (
    <div className="space-y-3 rounded-md border border-border p-3">
      <FormField label="API format" hint="Anthropic Messages keeps Claude's native tool use end to end (Anthropic, Bedrock, Vertex). Chat Completions works for every provider.">
        <SegmentedControl<PortkeyApiFormat>
          ariaLabel="Portkey API format"
          value={options.apiFormat}
          onChange={(apiFormat) => onChange({ ...options, apiFormat })}
          options={[
            { value: "chatCompletions", label: "Chat Completions" },
            { value: "anthropicMessages", label: "Anthropic Messages" },
          ]}
        />
      </FormField>
      <FormField label="Routing" error={r.kind === "modelCatalog" ? error : undefined} hint={r.kind === "modelCatalog" ? "The model id names the provider (@slug/model); no routing header is sent." : undefined}>
        <SegmentedControl
          ariaLabel="Portkey routing"
          value={r.kind}
          onChange={(kind) => kind !== r.kind && onChange({ ...options, routing: routingOfKind(kind, routingValue(r)) })}
          options={ROUTING_KINDS}
        />
      </FormField>
      {field && (
        <FormField label={field.label} htmlFor={`${uid}-pk-route`} error={error} hint={field.hint}>
          <Input
            id={`${uid}-pk-route`}
            value={routingValue(r)}
            spellCheck={false}
            autoComplete="off"
            placeholder={field.placeholder}
            className={cn("font-mono", error && invalidRing)}
            onChange={(ev) => onChange({ ...options, routing: withRoutingValue(r, ev.target.value) })}
          />
        </FormField>
      )}
      <FormField label="User for Portkey logs" htmlFor={`${uid}-pk-user`} optional hint="Sent as _user in x-portkey-metadata so cost and audit logs show who asked. Empty sends “vibelens”.">
        <Input
          id={`${uid}-pk-user`}
          value={options.user ?? ""}
          spellCheck={false}
          autoComplete="off"
          placeholder="you@company.com"
          className="max-w-80"
          onChange={(ev) => onChange({ ...options, user: ev.target.value.trim() ? ev.target.value : null })}
        />
      </FormField>
    </div>
  );
}
