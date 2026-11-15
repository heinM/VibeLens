import { describe, expect, it } from "vitest";
import { blankCustom, configToSave, draftFrom, isCustomProvider, isDirty, parseTemperature, validateProvider } from "../providerDraft";

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
