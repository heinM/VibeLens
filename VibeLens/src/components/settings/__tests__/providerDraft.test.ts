import { describe, expect, it } from "vitest";
import { blankCustom, configToSave, contextWindowError, draftFrom, isCustomProvider, isDirty, looksLikeOllama, parseTemperature, validateProvider } from "../providerDraft";

describe("provider drafts", () => {
  const cfg = { ...blankCustom("custom-abc12345"), baseUrl: "http://localhost:11434/v1/", model: " m ", requiresApiKey: false };

  it("recognises custom ids", () => {
    expect(isCustomProvider("custom-1")).toBe(true);
    expect(isCustomProvider("anthropic")).toBe(false);
  });
  it("validates url, tokens and temperature", () => {
    const d = draftFrom({ ...cfg, baseUrl: "localhost", maxTokens: 0 });
    d.temperature = "3";
    expect(Object.keys(validateProvider(d)).sort()).toEqual(["baseUrl", "maxTokens", "temperature"]);
    expect(validateProvider(draftFrom(cfg))).toEqual({});
  });
  it("parses temperature", () => {
    expect(parseTemperature("")).toBeNull();
    expect(parseTemperature("0.2")).toBe(0.2);
    expect(parseTemperature("x")).toBe("invalid");
  });
  it("builds the config to save", () => {
    const d = draftFrom(cfg);
    d.temperature = "0.5";
    expect(configToSave(d)).toMatchObject({ baseUrl: "http://localhost:11434/v1", model: "m", temperature: 0.5 });
    d.config.label = "";
    expect(configToSave(d)).toBeNull();
  });
  it("tracks dirtiness including key edits", () => {
    const d = draftFrom(cfg);
    expect(isDirty(d, cfg)).toBe(false);
    d.config.model = "other";
    expect(isDirty(d, cfg)).toBe(true);
    const k = draftFrom(cfg);
    k.apiKey = { mode: "remove", value: "" };
    expect(isDirty(k, cfg)).toBe(true);
    k.apiKey = { mode: "set", value: "" };
    expect(isDirty(k, cfg)).toBe(false);
  });
});

describe("context window", () => {
  const cfg = { ...blankCustom("custom-abc12345"), baseUrl: "http://localhost:11434/v1", maxTokens: 4096 };

  it("is optional and saved as null when empty", () => {
    expect(contextWindowError(cfg)).toBeUndefined();
    expect(configToSave(draftFrom(cfg))?.contextWindow).toBeNull();
    expect(configToSave(draftFrom({ ...cfg, contextWindow: 16384 }))?.contextWindow).toBe(16384);
  });
  it("mirrors the Rust bounds and must exceed the output limit", () => {
    expect(contextWindowError({ ...cfg, contextWindow: 512 })).toMatch(/1,024/);
    expect(contextWindowError({ ...cfg, contextWindow: 20_000_000 })).toMatch(/10,000,000/);
    expect(contextWindowError({ ...cfg, contextWindow: 4096.5 })).toMatch(/whole number/);
    expect(contextWindowError({ ...cfg, contextWindow: Number.NaN })).toMatch(/whole number/);
    expect(contextWindowError({ ...cfg, contextWindow: 4096 })).toMatch(/larger than max output tokens/);
    expect(contextWindowError({ ...cfg, contextWindow: 4097 })).toBeUndefined();
    expect(Object.keys(validateProvider(draftFrom({ ...cfg, contextWindow: 100 })))).toEqual(["contextWindow"]);
  });
  it("recognises Ollama for the settings hint", () => {
    expect(looksLikeOllama({ id: "ollama", baseUrl: "" })).toBe(true);
    expect(looksLikeOllama(cfg)).toBe(true);
    expect(looksLikeOllama({ id: "custom-x", baseUrl: "https://api.x.ai/v1" })).toBe(false);
    expect(looksLikeOllama({ id: "custom-x", baseUrl: "not a url" })).toBe(false);
  });
  it("counts a changed window as an edit", () => {
    expect(isDirty(draftFrom({ ...cfg, contextWindow: 8192 }), cfg)).toBe(true);
  });
});
