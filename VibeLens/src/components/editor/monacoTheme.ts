import type { Monaco } from "@monaco-editor/react";

export type EditorThemeName = "vibelens-dark" | "vibelens-light";

/** Registers the two app-matched Monaco themes (idempotent). Colours mirror the tokens in index.css. */
export function defineVibeLensThemes(monaco: Monaco): void {
  monaco.editor.defineTheme("vibelens-dark", {
    base: "vs-dark",
    inherit: true,
    rules: [
      { token: "keyword", foreground: "7dd3fc" },
      { token: "operator", foreground: "c4b5fd" },
      { token: "string", foreground: "86efac" },
      { token: "number", foreground: "fbbf24" },
      { token: "comment", foreground: "5f6f82", fontStyle: "italic" },
      { token: "predefined", foreground: "a5b4fc" },
      { token: "type", foreground: "67e8f9" },
      { token: "identifier", foreground: "e6edf3" },
      { token: "variable", foreground: "e6edf3" },
    ],
    colors: {
      "editor.background": "#0b0f14",
      "editor.foreground": "#e6edf3",
      "editorLineNumber.foreground": "#4b5868",
      "editorLineNumber.activeForeground": "#8b98a8",
      "editor.lineHighlightBackground": "#11161d",
      "editor.lineHighlightBorder": "#11161d",
      "editor.selectionBackground": "#5eead438",
      "editor.inactiveSelectionBackground": "#5eead41f",
      "editor.selectionHighlightBackground": "#5eead41f",
      "editor.wordHighlightBackground": "#5eead41a",
      "editorCursor.foreground": "#5eead4",
      "editorIndentGuide.background1": "#1a222c",
      "editorWhitespace.foreground": "#232c38",
      "editorWidget.background": "#11161d",
      "editorWidget.border": "#232c38",
      "editorSuggestWidget.background": "#11161d",
      "editorSuggestWidget.border": "#232c38",
      "editorSuggestWidget.selectedBackground": "#171e27",
      "editorHoverWidget.background": "#11161d",
      "editorHoverWidget.border": "#232c38",
      "input.background": "#0b0f14",
      "input.border": "#232c38",
      focusBorder: "#5eead466",
      "scrollbarSlider.background": "#232c3899",
      "scrollbarSlider.hoverBackground": "#2e3a4a",
      "scrollbarSlider.activeBackground": "#3a4859",
      "editorBracketMatch.background": "#5eead426",
      "editorBracketMatch.border": "#5eead455",
    },
  });
  monaco.editor.defineTheme("vibelens-light", {
    base: "vs",
    inherit: true,
    rules: [
      { token: "keyword", foreground: "0369a1" },
      { token: "operator", foreground: "6d28d9" },
      { token: "string", foreground: "15803d" },
      { token: "number", foreground: "b45309" },
      { token: "comment", foreground: "8a94a3", fontStyle: "italic" },
      { token: "predefined", foreground: "4338ca" },
      { token: "type", foreground: "0e7490" },
    ],
    colors: {
      "editor.background": "#f7f8fa",
      "editor.foreground": "#111827",
      "editorLineNumber.foreground": "#a3acb9",
      "editorLineNumber.activeForeground": "#6b7280",
      "editor.lineHighlightBackground": "#eef1f5",
      "editor.lineHighlightBorder": "#eef1f5",
      "editor.selectionBackground": "#0d948833",
      "editor.inactiveSelectionBackground": "#0d948820",
      "editorCursor.foreground": "#0d9488",
      "editorIndentGuide.background1": "#e5e8ed",
      "editorWidget.background": "#ffffff",
      "editorWidget.border": "#dfe3e8",
      "editorSuggestWidget.background": "#ffffff",
      "editorSuggestWidget.border": "#dfe3e8",
      "editorSuggestWidget.selectedBackground": "#f0f2f5",
      focusBorder: "#0d948866",
    },
  });
}

export const editorThemeFor = (theme: "dark" | "light"): EditorThemeName => (theme === "dark" ? "vibelens-dark" : "vibelens-light");

export const EDITOR_FONT = '"JetBrains Mono", ui-monospace, "SF Mono", Menlo, Consolas, monospace';
