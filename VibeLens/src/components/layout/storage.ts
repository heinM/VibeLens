// localStorage wrappers that never throw (private windows, blocked storage, thumbnail capture).
import type { LayoutStorage } from "react-resizable-panels";

export function readStorage(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeStorage(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    /* storage unavailable: layout simply is not remembered */
  }
}

/** Storage adapter handed to react-resizable-panels' useDefaultLayout. */
export const layoutStorage: LayoutStorage = {
  getItem: (key) => readStorage(`vg.layout.${key}`),
  setItem: (key, value) => writeStorage(`vg.layout.${key}`, value),
};

export type ThemeName = "dark" | "light";

export function readTheme(): ThemeName {
  // Dark-first: only an explicit user choice (saved by the theme toggle) switches to light.
  return readStorage("vg.theme") === "light" ? "light" : "dark";
}
export const writeTheme = (t: ThemeName) => writeStorage("vg.theme", t);

import type { AiMode } from "../../store/useUIStore";

export function readAiMode(): AiMode | null {
  const v = readStorage("vg.aiMode");
  return v === "classic" || v === "assist" || v === "agentFirst" ? v : null;
}
export const writeAiMode = (m: AiMode) => writeStorage("vg.aiMode", m);

export function readBool(key: string, fallback: boolean): boolean {
  const v = readStorage(key);
  return v === "1" ? true : v === "0" ? false : fallback;
}
export const writeBool = (key: string, v: boolean) => writeStorage(key, v ? "1" : "0");

/** Panels clip instead of scrolling; their content manages its own scroll containers. */
export const panelStyle = { overflow: "hidden" } as const;
