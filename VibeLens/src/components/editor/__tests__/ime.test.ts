import { describe, expect, it } from "vitest";
import { isImeKeyEvent } from "../inlineAi/ime";

describe("isImeKeyEvent", () => {
  it("catches Chrome-style composition", () => {
    expect(isImeKeyEvent({ isComposing: true, keyCode: 13 })).toBe(true);
  });
  it("catches Safari's commit Enter (isComposing false, keyCode 229)", () => {
    expect(isImeKeyEvent({ isComposing: false, keyCode: 229 })).toBe(true);
  });
  it("lets a real Enter through", () => {
    expect(isImeKeyEvent({ isComposing: false, keyCode: 13 })).toBe(false);
    expect(isImeKeyEvent({})).toBe(false);
  });
});
