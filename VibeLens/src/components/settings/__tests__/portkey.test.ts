import { describe, expect, it } from "vitest";
import { blankPortkey, configToSave, draftFrom, portkeyError, routingValue, validateProvider, withRoutingValue } from "../providerDraft";

describe("Portkey provider drafts", () => {
  const base = { ...blankPortkey("custom-pk000001"), model: "@bedrock-prod/anthropic.claude-sonnet" };

  it("defaults to the hosted gateway, chat completions and model catalog routing", () => {
    const c = blankPortkey("custom-x");
    expect(c.kind).toBe("portkey");
    expect(c.baseUrl).toBe("https://api.portkey.ai/v1");
    expect(c.portkey).toEqual({ apiFormat: "chatCompletions", routing: { kind: "modelCatalog" }, user: null });
    expect(validateProvider(draftFrom(base))).toEqual({});
  });

  it("model catalog routing needs an @slug model", () => {
    expect(portkeyError({ ...base, model: "gpt-5" })).toMatch(/@provider-slug/);
    expect(portkeyError({ ...base, model: "" })).toBeUndefined();
  });

  it("explicit routing needs its value, and inline configs must be JSON without keys", () => {
    const withRouting = (routing: NonNullable<typeof base.portkey>["routing"]) => ({ ...base, model: "claude", portkey: { ...base.portkey!, routing } });
    expect(portkeyError(withRouting({ kind: "provider", slug: " " }))).toBeDefined();
    expect(portkeyError(withRouting({ kind: "provider", slug: "@anthropic-prod" }))).toBeUndefined();
    expect(portkeyError(withRouting({ kind: "virtualKey", virtualKey: "vk" }))).toBeUndefined();
    expect(portkeyError(withRouting({ kind: "config", config: "{bad" }))).toMatch(/JSON/);
    expect(portkeyError(withRouting({ kind: "config", config: '{"provider":"openai","api_key":"sk-1"}' }))).toMatch(/api_key/);
    expect(portkeyError(withRouting({ kind: "config", config: "pc-123" }))).toBeUndefined();
    expect(validateProvider(draftFrom(withRouting({ kind: "provider", slug: "" }))).portkey).toBeDefined();
  });

  it("reads and writes the routing value whatever the kind", () => {
    expect(routingValue({ kind: "provider", slug: "@p" })).toBe("@p");
    expect(withRoutingValue({ kind: "virtualKey", virtualKey: "" }, "vk-1")).toEqual({ kind: "virtualKey", virtualKey: "vk-1" });
    expect(withRoutingValue({ kind: "modelCatalog" }, "x")).toEqual({ kind: "modelCatalog" });
  });

  it("drafts deep-copy the options so edits never touch the saved config", () => {
    const d = draftFrom(base);
    d.config.portkey!.routing = { kind: "provider", slug: "@x" };
    expect(base.portkey!.routing).toEqual({ kind: "modelCatalog" });
    expect(configToSave(draftFrom(base))?.portkey).toEqual(base.portkey);
  });
});
