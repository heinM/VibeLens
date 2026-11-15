import type { PortkeyOptions, PortkeyRouting, ProviderConfig, ProviderKind } from "../../ipc/types";
import { keepSecret, type SecretEdit } from "../connections/draft";

export interface ProviderDraft {
  config: ProviderConfig;
  /** Free text so "0.", "" and "1e" can exist mid-edit. Empty = provider default. */
  temperature: string;
  apiKey: SecretEdit;
}

export const CUSTOM_PREFIX = "custom-";
export const isCustomProvider = (id: string) => id.startsWith(CUSTOM_PREFIX);

export function draftFrom(config: ProviderConfig): ProviderDraft {
  const portkey = config.kind === "portkey" ? structuredClone(config.portkey ?? defaultPortkey()) : (config.portkey ?? null);
  return { config: { ...config, extraHeaders: { ...config.extraHeaders }, portkey }, temperature: config.temperature == null ? "" : String(config.temperature), apiKey: keepSecret() };
}

export const PORTKEY_BASE_URL = "https://api.portkey.ai/v1";

export const defaultPortkey = (): PortkeyOptions => ({ apiFormat: "chatCompletions", routing: { kind: "modelCatalog" }, user: null });

/** A Portkey gateway entry next to the built-in one (e.g. a second workspace or a self-hosted gateway). */
export function blankPortkey(id: string): ProviderConfig {
  return {
    id,
    label: "Portkey gateway",
    kind: "portkey" satisfies ProviderKind,
    baseUrl: PORTKEY_BASE_URL,
    model: "",
    maxTokens: 8192,
    temperature: null,
    extraHeaders: {},
    requiresApiKey: true,
    portkey: defaultPortkey(),
  };
}

/** The routing value the user types, whichever kind is selected. */
export function routingValue(r: PortkeyRouting): string {
  switch (r.kind) {
    case "provider":
      return r.slug;
    case "virtualKey":
      return r.virtualKey;
    case "config":
      return r.config;
    default:
      return "";
  }
}

export function withRoutingValue(r: PortkeyRouting, value: string): PortkeyRouting {
  switch (r.kind) {
    case "provider":
      return { kind: "provider", slug: value };
    case "virtualKey":
      return { kind: "virtualKey", virtualKey: value };
    case "config":
      return { kind: "config", config: value };
    default:
      return r;
  }
}

/** Mirrors `portkey::validate` in vibelens-core, so mistakes show before saving. */
export function portkeyError(c: ProviderConfig): string | undefined {
  if (c.kind !== "portkey") return undefined;
  const o = c.portkey ?? defaultPortkey();
  const r = o.routing;
  const model = c.model.trim();
  switch (r.kind) {
    case "modelCatalog":
      if (model && !model.startsWith("@")) return "With Model Catalog routing the model must name its provider: @provider-slug/model-id. Or choose another routing.";
      return undefined;
    case "provider":
      return r.slug.trim() ? undefined : "Enter the provider slug, e.g. @bedrock-prod.";
    case "virtualKey":
      return r.virtualKey.trim() ? undefined : "Enter the virtual key.";
    case "config": {
      const v = r.config.trim();
      if (!v) return "Enter a config id (pc-...) or inline config JSON.";
      if (v.startsWith("{")) {
        try {
          JSON.parse(v);
        } catch {
          return "The inline config is not valid JSON.";
        }
        if (v.includes('"api_key"')) return "The inline config contains an api_key: save it as a config in Portkey and use its id instead.";
      }
      return undefined;
    }
  }
}

export function blankCustom(id: string): ProviderConfig {
  return {
    id,
    label: "Custom provider",
    kind: "openAiCompatible" satisfies ProviderKind,
    baseUrl: "",
    model: "",
    maxTokens: 4096,
    temperature: null,
    extraHeaders: {},
    requiresApiKey: true,
  };
}

export type ProviderErrors = Partial<Record<"label" | "baseUrl" | "maxTokens" | "temperature" | "portkey", string>>;

export function parseTemperature(text: string): number | null | "invalid" {
  const t = text.trim();
  if (t === "") return null;
  const n = Number(t);
  return Number.isFinite(n) && n >= 0 && n <= 2 ? n : "invalid";
}

export function validateProvider(d: ProviderDraft): ProviderErrors {
  const e: ProviderErrors = {};
  if (!d.config.label.trim()) e.label = "Give the provider a name.";
  const url = d.config.baseUrl.trim();
  try {
    const u = new URL(url);
    if (u.protocol !== "https:" && u.protocol !== "http:") throw new Error("scheme");
  } catch {
    e.baseUrl = url ? "Enter a full URL starting with http:// or https://." : "Base URL is required.";
  }
  const mt = d.config.maxTokens;
  if (!Number.isInteger(mt) || mt < 1 || mt > 1_000_000) e.maxTokens = "A whole number from 1 to 1,000,000.";
  if (parseTemperature(d.temperature) === "invalid") e.temperature = "A number from 0 to 2, or leave empty.";
  const pk = portkeyError(d.config);
  if (pk) e.portkey = pk;
  return e;
}

/** The config to persist, or null if the draft is invalid. */
export function configToSave(d: ProviderDraft): ProviderConfig | null {
  if (Object.keys(validateProvider(d)).length > 0) return null;
  const temperature = parseTemperature(d.temperature);
  return {
    ...d.config,
    label: d.config.label.trim(),
    baseUrl: d.config.baseUrl.trim().replace(/\/+$/, ""),
    model: d.config.model.trim(),
    temperature: temperature === "invalid" ? null : temperature,
  };
}

export function isDirty(d: ProviderDraft, original: ProviderConfig): boolean {
  if (d.apiKey.mode === "remove" || (d.apiKey.mode === "set" && d.apiKey.value !== "")) return true;
  const o = draftFrom(original);
  return JSON.stringify([d.config, d.temperature]) !== JSON.stringify([o.config, o.temperature]);
}
