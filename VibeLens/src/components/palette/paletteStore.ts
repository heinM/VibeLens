// Open state of the command palette. A tiny store (not React state in CommandPalette) so the
// global shortcut hook and the palette itself share one source of truth.
import { create } from "zustand";

export type PaletteMode = "go" | "commands";

/** A narrower picker opened by a command: the palette lists only that source until it is left. */
export type PaletteScope = { kind: "iceTables"; profileId: string } | null;

interface PaletteState {
  open: boolean;
  /** Raw input value. A leading ">" means "Commands" mode (VS Code style). */
  query: string;
  /** Flips to true the first time the palette opens; lazily-fetched sources key off it. */
  armed: boolean;
  /** Set while a command's picker (e.g. "Iceberg: open table…") is showing; null for the normal modes. */
  scope: PaletteScope;
  /** Open the palette straight into a picker. */
  openScoped: (scope: NonNullable<PaletteScope>) => void;
  /** Leave the picker and go back to "Go to anything". */
  leaveScope: () => void;
  /** Open in a mode. If already open in that mode, close it (toggle); otherwise switch mode. */
  show: (mode: PaletteMode) => void;
  setQuery: (q: string) => void;
  close: () => void;
}

export const modeOf = (query: string): PaletteMode => (query.startsWith(">") ? "commands" : "go");

export const usePaletteStore = create<PaletteState>((set, get) => ({
  open: false,
  query: "",
  armed: false,
  scope: null,
  show: (mode) => {
    const s = get();
    if (s.open && !s.scope && modeOf(s.query) === mode) {
      set({ open: false });
      return;
    }
    set({ open: true, armed: true, query: mode === "commands" ? ">" : "", scope: null });
  },
  openScoped: (scope) => set({ open: true, armed: true, query: "", scope }),
  leaveScope: () => set({ scope: null, query: "" }),
  setQuery: (query) => set({ query }),
  close: () => set({ open: false }),
}));
