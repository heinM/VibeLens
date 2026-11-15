// Monaco wrapper for SQL tabs: pgsql language, app themes, run/save keybindings, selection reporting
// and a subtle marker for the statement under the cursor.
import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import Editor, { type OnMount } from "@monaco-editor/react";
import type * as MonacoNS from "monaco-editor";
import type { AiDataAccess, SchemaMetadata } from "../../ipc/types";
import { schemaMetadataKey } from "../../lib/schemaCache";
import { schemaDigest } from "../../lib/sqlIntel/aiPrompts";
import { catalogEngineOf, type SqlDialect } from "../../lib/sqlIntel/dialect";
import { selectAiEnabled, useUIStore } from "../../store/useUIStore";
import { Spinner } from "../ui";
import { type FormatOutcome, formatEditor } from "./formatEditor";
import { InlineAiController } from "./inlineAi/controller";
import { EDITOR_FONT, defineVibeLensThemes, editorThemeFor } from "./monacoTheme";
import { clearModelTarget, installSqlIntel, setModelTarget, warmCatalog } from "./sqlProvider";
import { splitStatements, statementAtOffset } from "./sqlSplit";

export type CodeEditor = MonacoNS.editor.IStandaloneCodeEditor;

export interface SqlEditorHandlers {
  runCurrent: () => void;
  runScript: () => void;
  /** Run the selection or statement inside a READ ONLY transaction, whatever the toggle says. */
  runReadOnly: () => void;
  /** Format the selection, else the whole buffer (⇧⌥F). */
  format: () => void;
  save: () => void;
}

/** A range of the buffer that follows edits (used to find the failing statement again later). */
export interface TrackedRange {
  getRange: () => MonacoNS.IRange | null;
  clear: () => void;
}

/** Imperative controls the pane drives from its toolbar. Set once the editor has mounted. */
export interface SqlEditorControls {
  format: () => FormatOutcome;
  askAi: () => void;
  /** Offer a corrected version of `range` given the server's error text. */
  fixWithAi: (range: MonacoNS.IRange, error: string) => void;
  trackRange: (startOffset: number, endOffset: number) => TrackedRange | null;
}

export interface SqlEditorProps {
  value: string;
  onChange: (value: string) => void;
  onSelectionChange: (text: string | null) => void;
  handlers: React.RefObject<SqlEditorHandlers>;
  editorRef: React.RefObject<CodeEditor | null>;
  controlsRef: React.RefObject<SqlEditorControls | null>;
  language?: string;
  /**
   * The tab's connection and database: completion and AI read the cached catalog for this target.
   * `dialect` picks the catalog source and vocabulary (Postgres when omitted; DataFusion or Athena for Iceberg).
   */
  target: { profileId: string | null; database: string | null; dialect?: SqlDialect | null; defaultSchema?: string | null };
  aiDataAccess: AiDataAccess | null;
  /** ⌘K pressed while the connection's AI access is off: show the hint. */
  onAiBlocked?: () => void;
}

export interface Runnable {
  sql: string;
  kind: "selection" | "statement";
  /** Offsets of `sql` within the buffer. */
  start: number;
  end: number;
}

/** The text a ⌘Enter should run: the selection, else the statement under the cursor. */
export function runnableAtCursor(editor: CodeEditor): Runnable | null {
  const model = editor.getModel();
  const sel = editor.getSelection();
  if (!model || !sel) return null;
  if (!sel.isEmpty()) {
    const text = model.getValueInRange(sel);
    const start = model.getOffsetAt(sel.getStartPosition());
    return text.trim() ? { sql: text, kind: "selection", start, end: start + text.length } : null;
  }
  const pos = editor.getPosition();
  if (!pos) return null;
  const stmt = statementAtOffset(model.getValue(), model.getOffsetAt(pos));
  return stmt ? { sql: stmt.text, kind: "statement", start: stmt.start, end: stmt.end } : null;
}

export function SqlEditor({ value, onChange, onSelectionChange, handlers, editorRef, controlsRef, language = "pgsql", target, aiDataAccess, onAiBlocked }: SqlEditorProps) {
  const theme = useUIStore((s) => s.theme);
  const aiEnabled = useUIStore(selectAiEnabled);
  const qc = useQueryClient();
  const aiRef = useRef<InlineAiController | null>(null);
  const targetRef = useRef(target);
  targetRef.current = target;
  const accessRef = useRef(aiDataAccess);
  accessRef.current = aiDataAccess;
  const blockedRef = useRef(onAiBlocked);
  blockedRef.current = onAiBlocked;
  const decoRef = useRef<MonacoNS.editor.IEditorDecorationsCollection | null>(null);
  const disposables = useRef<MonacoNS.IDisposable[]>([]);
  const timers = useRef<{ sel?: number; deco?: number }>({});
  const lastSel = useRef<string | null>(null);

  useEffect(
    () => () => {
      window.clearTimeout(timers.current.sel);
      window.clearTimeout(timers.current.deco);
      disposables.current.forEach((d) => d.dispose());
      disposables.current = [];
      const model = editorRef.current?.getModel();
      if (model) clearModelTarget(model.uri.toString());
      aiRef.current?.dispose();
      aiRef.current = null;
      controlsRef.current = null;
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  // Completion and hover resolve the catalog through this model → target map, and the catalog is
  // fetched once per (connection, database), ahead of the first keystroke.
  useEffect(() => {
    const model = editorRef.current?.getModel();
    if (model) setModelTarget(model.uri.toString(), target);
    warmCatalog(target);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target.profileId, target.database, target.dialect, target.defaultSchema]);

  useEffect(() => {
    aiRef.current?.setEnabled(aiEnabled);
  }, [aiEnabled]);
  // The connection's AI access can change while the tab is open (edited in the connection dialog).
  useEffect(() => {
    aiRef.current?.syncAccess();
  }, [aiDataAccess]);

  const onMount: OnMount = (editor, monaco) => {
    editorRef.current = editor;
    decoRef.current = editor.createDecorationsCollection([]);
    installSqlIntel(monaco, qc);
    const model0 = editor.getModel();
    if (model0) setModelTarget(model0.uri.toString(), targetRef.current);
    warmCatalog(targetRef.current);
    if (import.meta.env.DEV) (window as unknown as { __vgMonaco?: unknown }).__vgMonaco = monaco;

    // ⌘K Ask AI: the controller registers its own action, keys and zones.
    const ai = new InlineAiController(editor, monaco, {
      getTarget: () => targetRef.current,
      access: () => accessRef.current,
      onBlocked: () => blockedRef.current?.(),
      digest: (sql) => {
        if (accessRef.current === "none") return "";
        const t = targetRef.current;
        const meta = t.profileId ? qc.getQueryData<SchemaMetadata>(schemaMetadataKey(t.profileId, t.database, catalogEngineOf(t.dialect))) : undefined;
        return schemaDigest(meta, sql, 12, catalogEngineOf(t.dialect) === "iceberg" ? { dotted: true, defaultSchema: t.defaultSchema ?? null } : {});
      },
    });
    aiRef.current = ai;
    ai.setEnabled(useUIStore.getState().aiMode === "assist" || useUIStore.getState().aiMode === "agentFirst");

    // Format SQL (⇧⌥F): selection if any, else the whole buffer.
    editor.addAction({
      id: "vibelens.format",
      label: "Format SQL",
      keybindings: [monaco.KeyMod.Shift | monaco.KeyMod.Alt | monaco.KeyCode.KeyF],
      contextMenuGroupId: "1_modification",
      contextMenuOrder: 1.5,
      run: () => handlers.current?.format(),
    });
    controlsRef.current = {
      format: () => formatEditor(editor),
      askAi: () => ai.open(),
      fixWithAi: (range, error) => ai.openFix(range, error),
      trackRange: (startOffset, endOffset) => {
        const model = editor.getModel();
        if (!model) return null;
        const a = model.getPositionAt(startOffset);
        const b = model.getPositionAt(endOffset);
        const coll = editor.createDecorationsCollection([
          { range: new monaco.Range(a.lineNumber, a.column, b.lineNumber, b.column), options: { stickiness: monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges } },
        ]);
        return { getRange: () => coll.getRange(0), clear: () => coll.clear() };
      },
    };

    // `precondition: "!vgAiReady"` on ⌘Enter: while an AI proposal is waiting, ⌘Enter accepts it
    // (the controller binds the same keys with `vgAiReady`) instead of also running the statement.
    const add = (id: string, label: string, keybindings: number[], run: () => void, precondition?: string) =>
      disposables.current.push(editor.addAction({ id, label, keybindings, precondition, contextMenuGroupId: "navigation", contextMenuOrder: 1, run }));

    add("vibelens.run", "Run selection or statement", [monaco.KeyMod.CtrlCmd | monaco.KeyCode.Enter], () => handlers.current?.runCurrent(), "!vgAiReady");
    add("vibelens.runReadOnly", "Run in read-only transaction", [monaco.KeyMod.CtrlCmd | monaco.KeyMod.Alt | monaco.KeyCode.Enter], () => handlers.current?.runReadOnly());
    add("vibelens.runScript", "Run whole script", [monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.Enter], () => handlers.current?.runScript());
    disposables.current.push(
      editor.addAction({
        id: "vibelens.save",
        label: "Save worksheet",
        keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS],
        run: () => handlers.current?.save(),
      }),
    );

    const refreshDecoration = () => {
      const model = editor.getModel();
      const pos = editor.getPosition();
      if (!model || !pos) return;
      const text = model.getValue();
      // Only worth marking when the script has more than one statement.
      const stmts = splitStatements(text);
      if (stmts.length < 2) {
        decoRef.current?.set([]);
        return;
      }
      const stmt = statementAtOffset(text, model.getOffsetAt(pos));
      if (!stmt) return decoRef.current?.set([]);
      const a = model.getPositionAt(stmt.start);
      const b = model.getPositionAt(stmt.end);
      decoRef.current?.set([
        {
          range: new monaco.Range(a.lineNumber, a.column, b.lineNumber, b.column),
          options: { className: "vg-stmt-active", isWholeLine: false, linesDecorationsClassName: "vg-stmt-bar" },
        },
      ]);
    };

    disposables.current.push(
      editor.onDidChangeCursorSelection(() => {
        window.clearTimeout(timers.current.sel);
        timers.current.sel = window.setTimeout(() => {
          const model = editor.getModel();
          const sel = editor.getSelection();
          const text = model && sel && !sel.isEmpty() ? model.getValueInRange(sel) : null;
          if (text !== lastSel.current) {
            lastSel.current = text;
            onSelectionChange(text);
          }
        }, 150);
        window.clearTimeout(timers.current.deco);
        timers.current.deco = window.setTimeout(refreshDecoration, 80);
      }),
      editor.onDidChangeModelContent(() => {
        window.clearTimeout(timers.current.deco);
        timers.current.deco = window.setTimeout(refreshDecoration, 120);
      }),
    );
    refreshDecoration();
    editor.focus();
  };

  return (
    <Editor
      language={language}
      theme={editorThemeFor(theme)}
      value={value}
      onChange={(v) => onChange(v ?? "")}
      beforeMount={defineVibeLensThemes}
      onMount={onMount}
      loading={<div className="flex h-full items-center justify-center"><Spinner /></div>}
      options={{
        minimap: { enabled: false },
        fontFamily: EDITOR_FONT,
        fontSize: 13,
        lineHeight: 20,
        fontLigatures: false,
        scrollBeyondLastLine: false,
        automaticLayout: true,
        tabSize: 2,
        padding: { top: 10, bottom: 10 },
        renderLineHighlight: "line",
        overviewRulerLanes: 0,
        hideCursorInOverviewRuler: true,
        lineNumbersMinChars: 3,
        glyphMargin: false,
        fixedOverflowWidgets: true,
        smoothScrolling: true,
        cursorSmoothCaretAnimation: "off",
        guides: { indentation: false },
        scrollbar: { verticalScrollbarSize: 10, horizontalScrollbarSize: 10, useShadows: false },
        stickyScroll: { enabled: false },
        contextmenu: true,
        quickSuggestions: { other: true, comments: false, strings: false },
        quickSuggestionsDelay: 0,
        // Our provider knows the schema; word-based guesses from the buffer would only add noise.
        wordBasedSuggestions: "off",
        suggestSelection: "first",
        snippetSuggestions: "inline",
        suggest: { showWords: false, showInlineDetails: true, localityBonus: false, shareSuggestSelections: false, insertMode: "replace", filterGraceful: true },
        parameterHints: { enabled: false },
      }}
    />
  );
}
