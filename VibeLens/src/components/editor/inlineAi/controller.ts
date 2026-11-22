// Imperative controller for the editor's inline AI: ⌘K prompt bar, streaming preview (diff or ghost
// block) and "Fix with AI". It owns Monaco view zones, decorations and context keys; the React bar
// (InlineAiBar) is a pure view over its state. Everything the model proposes is a preview until the
// user accepts it, and accepting is a single undoable edit.
import type * as MonacoNS from "monaco-editor";
import { createElement } from "react";
import { type Root, createRoot } from "react-dom/client";
import { AI_OFF_HINT, askAgentOnce, InlineAiError } from "../../../lib/aiInline";
import { buildAskPrompt, buildFixPrompt, partialSqlBlock, toolProgress } from "../../../lib/sqlIntel/aiPrompts";
import { diffLines, hunksOf } from "../../../lib/sqlIntel/diff";
import type { SqlDialect } from "../../../lib/sqlIntel/dialect";
import type { AiDataAccess } from "../../../ipc/types";
import { useUIStore } from "../../../store/useUIStore";
import { EDITOR_FONT } from "../monacoTheme";
import { statementAtOffset } from "../sqlSplit";
import { InlineAiBar } from "./InlineAiBar";
import "./inlineAi.css";

type Monaco = typeof MonacoNS;
type Editor = MonacoNS.editor.IStandaloneCodeEditor;

export type AiMode = "selection" | "statement" | "insert" | "fix";
export type AiPhase = "closed" | "prompt" | "running" | "preview" | "error";

export interface AiState {
  phase: AiPhase;
  mode: AiMode;
  instruction: string;
  /** Progress line while running ("Reading the schema"). */
  status: string;
  proposal: string | null;
  error: { message: string; setup: boolean } | null;
  scopeLabel: string;
  /** Statement ⇄ insert can be toggled when the cursor is in a statement. */
  canSwitchScope: boolean;
  /** One-line summary of what accepting will do. */
  summary: string;
  noChange: boolean;
}

export interface AiHost {
  getTarget(): { profileId: string | null; database: string | null; dialect?: SqlDialect | null };
  /** Compact schema digest for the SQL, or "" when the profile does not allow sharing schema. */
  digest(sql: string): string;
  /** The tab's connection policy. `none` means the assistant must learn nothing about this worksheet. */
  access(): AiDataAccess | null;
  /** ⌘K was pressed while the connection's AI access is off. */
  onBlocked?(): void;
}


const CLOSED: AiState = { phase: "closed", mode: "insert", instruction: "", status: "", proposal: null, error: null, scopeLabel: "", canSwitchScope: false, summary: "", noChange: false };

/** Height of the prompt bar zone in px. */
export const BAR_HEIGHT = 84;
const HISTORY_KEY = "vg.aiPromptHistory";

function loadHistory(): string[] {
  try {
    const v = JSON.parse(window.localStorage.getItem(HISTORY_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string").slice(0, 30) : [];
  } catch {
    return [];
  }
}
function saveHistory(h: string[]): void {
  try {
    window.localStorage.setItem(HISTORY_KEY, JSON.stringify(h.slice(0, 30)));
  } catch {
    /* storage unavailable: history is a convenience */
  }
}

interface Anchor {
  coll: MonacoNS.editor.IEditorDecorationsCollection;
  original: string;
  /** Replace the range, or insert `text` at its start (blank line) / end (after a statement line). */
  kind: "replace" | "insert-blank" | "insert-after";
}

export class InlineAiController {
  private state: AiState = CLOSED;
  private listeners = new Set<() => void>();
  private barNode: HTMLDivElement;
  private root: Root;
  private barZone: string | null = null;
  private previewZones: string[] = [];
  private deco: MonacoNS.editor.IEditorDecorationsCollection;
  private anchor: Anchor | null = null;
  private abort: AbortController | null = null;
  private attempt = 0;
  private renderTimer: number | null = null;
  private colorizeVersion = 0;
  private fixError = "";
  private history = loadHistory();
  private historyIdx = -1;
  private keyActive: MonacoNS.editor.IContextKey<boolean>;
  private keyReady: MonacoNS.editor.IContextKey<boolean>;
  private keyEnabled: MonacoNS.editor.IContextKey<boolean>;
  private keyBlocked: MonacoNS.editor.IContextKey<boolean>;
  private aiOn = false;
  private disposables: MonacoNS.IDisposable[] = [];

  constructor(
    private editor: Editor,
    private monaco: Monaco,
    private host: AiHost,
  ) {
    this.barNode = document.createElement("div");
    this.barNode.className = "vg-ai-bar-zone";
    this.root = createRoot(this.barNode);
    this.root.render(createElement(InlineAiBar, { controller: this }));
    this.deco = editor.createDecorationsCollection([]);
    this.keyActive = editor.createContextKey("vgAiActive", false);
    this.keyReady = editor.createContextKey("vgAiReady", false);
    this.keyEnabled = editor.createContextKey("vgAiEnabled", false);
    this.keyBlocked = editor.createContextKey("vgAiBlocked", false);

    const { KeyMod, KeyCode } = monaco;
    // `editor.addAction` keybindings are scoped to this editor instance (Monaco adds an editorId
    // condition) and are disposed with it. `editor.addCommand` is global: with several worksheets open
    // the newest controller would receive every editor's Esc / Tab / ⌘Enter. The context keys are
    // per editor too, so these only fire while this editor has an AI session in that state.
    const bind = (id: string, label: string, keybindings: number[], precondition: string, run: () => void) =>
      this.disposables.push(editor.addAction({ id, label, keybindings, precondition, run }));
    bind("vibelens.ai.escape", "Close the AI prompt", [KeyCode.Escape], "vgAiActive && !suggestWidgetVisible", () => this.escape());
    bind("vibelens.ai.accept", "Accept the AI suggestion", [KeyMod.CtrlCmd | KeyCode.Enter], "vgAiReady", () => this.accept());
    bind("vibelens.ai.acceptTab", "Accept the AI suggestion (Tab)", [KeyCode.Tab], "vgAiReady && !suggestWidgetVisible && !inSnippetMode", () => this.accept());
    bind("vibelens.ai.blocked", AI_OFF_HINT, [KeyMod.CtrlCmd | KeyCode.KeyK], "vgAiBlocked", () => this.host.onBlocked?.());
    this.disposables.push(
      editor.addAction({
        id: "vibelens.askAi",
        label: "Ask AI…",
        keybindings: [KeyMod.CtrlCmd | KeyCode.KeyK],
        precondition: "vgAiEnabled",
        contextMenuGroupId: "navigation",
        contextMenuOrder: 1.5,
        run: () => this.open(),
      }),
    );
  }

  // -- store plumbing ---------------------------------------------------------------------------

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };
  getState = (): AiState => this.state;
  private set(patch: Partial<AiState>): void {
    this.state = { ...this.state, ...patch };
    this.keyActive.set(this.state.phase !== "closed");
    this.keyReady.set(this.state.phase === "preview" && !!this.state.proposal);
    this.listeners.forEach((l) => l());
  }

  /** AI on/off from the app's mode. Combine with `syncAccess` whenever the connection policy may have changed. */
  setEnabled(enabled: boolean): void {
    this.aiOn = enabled;
    this.syncAccess();
  }

  /** Re-reads the connection's AI access: `none` disables ⌘K for this tab and cancels anything in flight. */
  syncAccess(): void {
    const off = this.host.access() === "none";
    this.keyEnabled.set(this.aiOn && !off);
    this.keyBlocked.set(this.aiOn && off);
    if ((!this.aiOn || off) && this.state.phase !== "closed") this.close(false);
  }

  private blocked(): boolean {
    return this.host.access() === "none";
  }

  // -- opening ----------------------------------------------------------------------------------

  open(): void {
    if (this.blocked()) {
      this.host.onBlocked?.();
      return;
    }
    if (this.state.phase !== "closed") {
      this.focusInput();
      return;
    }
    const model = this.editor.getModel();
    const sel = this.editor.getSelection();
    const pos = this.editor.getPosition();
    if (!model || !sel || !pos) return;
    const { Range } = this.monaco;
    const stickiness = this.monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges;

    let mode: AiMode;
    let range: MonacoNS.Range;
    let kind: Anchor["kind"] = "replace";
    let canSwitch = false;
    if (!sel.isEmpty()) {
      mode = "selection";
      range = Range.lift(sel);
    } else {
      const line = model.getLineContent(pos.lineNumber);
      const stmt = line.trim() ? statementAtOffset(model.getValue(), model.getOffsetAt(pos), { mysql: this.host.getTarget().dialect === "mysql" }) : null;
      if (stmt) {
        mode = "statement";
        canSwitch = true;
        const a = model.getPositionAt(stmt.start);
        const b = model.getPositionAt(stmt.end);
        range = new Range(a.lineNumber, a.column, b.lineNumber, b.column);
      } else {
        mode = "insert";
        const blank = !line.trim();
        kind = blank ? "insert-blank" : "insert-after";
        range = blank ? new Range(pos.lineNumber, 1, pos.lineNumber, model.getLineMaxColumn(pos.lineNumber)) : new Range(pos.lineNumber, model.getLineMaxColumn(pos.lineNumber), pos.lineNumber, model.getLineMaxColumn(pos.lineNumber));
      }
    }
    const coll = this.editor.createDecorationsCollection([{ range, options: { stickiness } }]);
    this.anchor = { coll, original: model.getValueInRange(range), kind };
    this.historyIdx = -1;
    this.mountBar(range.startLineNumber);
    this.set({ ...CLOSED, phase: "prompt", mode, canSwitchScope: canSwitch, ...this.labels(mode, range) });
    this.editor.revealLineInCenterIfOutsideViewport(range.startLineNumber);
    this.focusInput();
  }

  /** "Fix with AI": propose a corrected version of `range` given the server's error. */
  openFix(range: MonacoNS.IRange, error: string): void {
    this.close(false);
    if (this.blocked()) return;
    const model = this.editor.getModel();
    if (!model) return;
    const r = this.monaco.Range.lift(range);
    const coll = this.editor.createDecorationsCollection([{ range: r, options: { stickiness: this.monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges } }]);
    this.anchor = { coll, original: model.getValueInRange(r), kind: "replace" };
    this.fixError = error;
    this.mountBar(r.startLineNumber);
    this.set({ ...CLOSED, phase: "running", mode: "fix", instruction: "Fix this error", status: "Reading the error", ...this.labels("fix", r) });
    this.editor.revealLineInCenterIfOutsideViewport(r.startLineNumber);
    this.run();
  }

  private labels(mode: AiMode, r: MonacoNS.Range): Pick<AiState, "scopeLabel"> {
    const lines = r.endLineNumber - r.startLineNumber + 1;
    const n = `${lines} ${lines === 1 ? "line" : "lines"}`;
    switch (mode) {
      case "selection":
        return { scopeLabel: `Selection, ${n}` };
      case "statement":
        return { scopeLabel: `Statement at cursor, ${n}` };
      case "fix":
        return { scopeLabel: `Failing statement, ${n}` };
      default:
        return { scopeLabel: "New SQL at cursor" };
    }
  }

  toggleScope(): void {
    const a = this.anchor;
    const model = this.editor.getModel();
    const pos = this.editor.getPosition();
    if (this.state.phase !== "prompt" || !a || !model || !pos || !this.state.canSwitchScope) return;
    const { Range } = this.monaco;
    if (this.state.mode === "statement") {
      const line = pos.lineNumber;
      const range = new Range(line, model.getLineMaxColumn(line), line, model.getLineMaxColumn(line));
      a.coll.set([{ range, options: { stickiness: this.monaco.editor.TrackedRangeStickiness.NeverGrowsWhenTypingAtEdges } }]);
      this.anchor = { ...a, original: "", kind: "insert-after" };
      this.set({ mode: "insert", scopeLabel: "New SQL after this line" });
    } else {
      const instruction = this.state.instruction;
      this.anchor?.coll.clear();
      this.anchor = null;
      this.state = { ...CLOSED };
      this.disposeZones();
      this.open();
      this.set({ instruction });
    }
  }

  // -- input events from the bar ----------------------------------------------------------------

  setInstruction(s: string): void {
    this.set({ instruction: s });
  }

  recall(dir: -1 | 1): void {
    if (this.history.length === 0) return;
    this.historyIdx = Math.max(-1, Math.min(this.history.length - 1, this.historyIdx + (dir === -1 ? 1 : -1)));
    this.set({ instruction: this.historyIdx < 0 ? "" : this.history[this.historyIdx] });
  }

  submit(): void {
    const s = this.state;
    if (s.phase === "closed" || s.phase === "running") return;
    if (s.mode !== "fix" && !s.instruction.trim()) return;
    if (s.mode !== "fix") {
      this.history = [s.instruction.trim(), ...this.history.filter((h) => h !== s.instruction.trim())];
      saveHistory(this.history);
    }
    this.run();
  }

  retry(): void {
    this.submit();
  }

  private run(): void {
    const a = this.anchor;
    const model = this.editor.getModel();
    if (!a || !model) return;
    // Last line of defence: with AI access off nothing about this worksheet or connection is sent.
    if (this.blocked()) return this.fail(`${AI_OFF_HINT}.`, false);
    this.abort?.abort();
    const ac = new AbortController();
    this.abort = ac;
    const attempt = ++this.attempt;
    this.clearPreview();
    this.set({ phase: "running", status: "Thinking", proposal: null, error: null, noChange: false });

    const range = a.coll.getRange(0);
    const original = range ? model.getValueInRange(range) : a.original;
    const mode = this.state.mode;
    const target = this.host.getTarget();
    const buffer = model.getValue();
    const digest = this.host.digest(mode === "insert" ? buffer : original + "\n" + buffer);
    const prompt = mode === "fix" ? buildFixPrompt({ sql: original, error: this.fixError, digest, dialect: target.dialect }) : buildAskPrompt({ instruction: this.state.instruction, target: mode === "insert" ? null : original, buffer, digest, dialect: target.dialect });
    const live = (): boolean => attempt === this.attempt;

    askAgentOnce({
      prompt,
      context: { profileId: target.profileId, database: target.database, openFile: null, selectedText: mode === "insert" ? null : original || null },
      signal: ac.signal,
      onText: (text) => {
        if (!live()) return;
        const p = partialSqlBlock(text);
        if (p !== null) {
          this.state = { ...this.state, status: "Writing SQL", proposal: p };
          this.listeners.forEach((l) => l());
          this.scheduleRender();
        }
      },
      onEvent: (ev) => {
        if (!live()) return;
        if (ev.type === "toolCallProposed") this.set({ status: toolProgress(ev.tool, ev.arguments) });
        else if (ev.type === "approvalRequired") {
          this.attempt++;
          ac.abort();
          this.fail(`The assistant wanted to use ${ev.tool.replace(/_/g, " ")}, which needs approval. Inline edits only use read-only lookups. Rephrase, or ask in the agent panel.`, false);
        }
      },
    })
      .then((res) => {
        if (!live()) return;
        const code = res.code ?? partialSqlBlock(res.text);
        if (!code || !code.trim()) {
          this.fail("The assistant did not return any SQL. Try rephrasing, or press Enter to retry.", false);
          return;
        }
        this.state = { ...this.state, proposal: code };
        this.finishPreview();
      })
      .catch((e: unknown) => {
        if (!live()) return;
        if (e instanceof InlineAiError) {
          if (e.payload.code === "cancelled") return;
          const setup = e.payload.code === "invalid" && /provider/i.test(e.payload.message);
          this.fail(setup ? "No AI provider is set up yet." : e.payload.message, setup);
        } else this.fail(e instanceof Error ? e.message : String(e), false);
      });
  }

  private fail(message: string, setup: boolean): void {
    this.clearPreview();
    this.set({ phase: "error", error: { message, setup }, proposal: null });
  }

  private finishPreview(): void {
    if (this.renderTimer !== null) window.clearTimeout(this.renderTimer);
    this.renderTimer = null;
    const a = this.anchor;
    const proposal = this.state.proposal;
    if (!a || proposal === null) return;
    const oldLines = a.kind === "replace" ? a.original.split("\n") : [];
    const newLines = proposal.split("\n");
    const noChange = a.kind === "replace" && a.original.trim() === proposal.trim();
    const removed = a.kind === "replace" ? hunksOf(diffLines(oldLines, newLines)).removed.length : 0;
    const added = a.kind === "replace" ? hunksOf(diffLines(oldLines, newLines)).added.reduce((n, h) => n + h.lines.length, 0) : newLines.length;
    const summary = noChange ? "No changes suggested" : a.kind === "replace" ? `${removed} removed, ${added} added` : `Adds ${newLines.length} ${newLines.length === 1 ? "line" : "lines"}`;
    this.set({ phase: "preview", status: "", summary, noChange });
    this.renderPreview(true);
    this.focusInput();
  }

  // -- accept / reject --------------------------------------------------------------------------

  accept(): void {
    const a = this.anchor;
    const proposal = this.state.proposal;
    const model = this.editor.getModel();
    if (this.state.phase !== "preview" || !a || proposal === null || !model) return;
    const range = a.coll.getRange(0);
    if (!range) return this.close();
    if (this.state.noChange) return this.close();
    const text = a.kind === "insert-after" ? `\n\n${proposal}` : proposal;
    this.editor.pushUndoStop();
    this.editor.executeEdits("vibelens.ai", [{ range, text, forceMoveMarkers: true }]);
    this.editor.pushUndoStop();
    // Leave the caret at the end of what was written.
    const startOffset = model.getOffsetAt(range.getStartPosition());
    this.editor.setPosition(model.getPositionAt(startOffset + text.length));
    this.close(true);
  }

  /** Esc: cancel a running request, otherwise reject and close. */
  escape(): void {
    if (this.state.phase === "running" && this.state.mode !== "fix") {
      this.attempt++;
      this.abort?.abort();
      this.clearPreview();
      this.set({ phase: "prompt", status: "", proposal: null });
      this.focusInput();
      return;
    }
    this.close();
  }

  reject(): void {
    this.close();
  }

  close(refocus = true): void {
    this.attempt++;
    this.abort?.abort();
    this.abort = null;
    if (this.renderTimer !== null) window.clearTimeout(this.renderTimer);
    this.renderTimer = null;
    this.clearPreview();
    this.disposeZones();
    this.anchor?.coll.clear();
    this.anchor = null;
    this.state = { ...CLOSED };
    this.set({});
    if (refocus) this.editor.focus();
  }

  openSettings(): void {
    this.close(false);
    useUIStore.getState().setSettingsOpen(true);
  }

  // -- zones / decorations -------------------------------------------------------------------------

  private gutter(): number {
    return this.editor.getLayoutInfo().contentLeft;
  }

  private mountBar(startLine: number): void {
    this.disposeZones();
    this.barNode.style.setProperty("--vg-gutter", `${this.gutter()}px`);
    this.editor.changeViewZones((acc) => {
      this.barZone = acc.addZone({ afterLineNumber: startLine - 1, heightInPx: BAR_HEIGHT, domNode: this.barNode, suppressMouseDown: true, ordinal: 0 });
    });
  }

  private disposeZones(): void {
    if (this.barZone) {
      const id = this.barZone;
      this.editor.changeViewZones((acc) => acc.removeZone(id));
      this.barZone = null;
    }
  }

  private clearPreview(): void {
    this.deco.clear();
    if (this.previewZones.length) {
      const ids = this.previewZones;
      this.previewZones = [];
      this.editor.changeViewZones((acc) => ids.forEach((id) => acc.removeZone(id)));
    }
  }

  private scheduleRender(): void {
    if (this.renderTimer !== null) return;
    this.renderTimer = window.setTimeout(() => {
      this.renderTimer = null;
      this.renderPreview(false);
    }, 60);
  }

  private makeZone(kind: "add" | "ghost", lines: string[]): HTMLElement {
    const root = document.createElement("div");
    root.className = `vg-ai-zone vg-ai-${kind}`;
    root.style.setProperty("--vg-gutter", `${this.gutter()}px`);
    root.style.fontFamily = EDITOR_FONT;
    const els = lines.map((l) => {
      const d = document.createElement("div");
      d.className = "vg-ai-line";
      d.textContent = l.length ? l : " ";
      root.appendChild(d);
      return d;
    });
    const version = this.colorizeVersion;
    void this.monaco.editor.colorize(lines.join("\n"), "pgsql", { tabSize: 2 }).then((html) => {
      if (version !== this.colorizeVersion) return;
      const parts = html.split(/<br\s*\/?>/i);
      if (parts.length === els.length) parts.forEach((p, i) => (els[i].innerHTML = p || " "));
    });
    return root;
  }

  /**
   * `final` is false while text is still streaming: the partial answer is shown as a ghost block under
   * the code, and the diff (red/green) is only computed once the whole answer is in.
   */
  private renderPreview(final: boolean): void {
    const a = this.anchor;
    const proposal = this.state.proposal;
    const range = a?.coll.getRange(0);
    if (!a || !range || proposal === null) return;
    this.colorizeVersion++;
    const newLines = proposal.split("\n");
    const decos: MonacoNS.editor.IModelDeltaDecoration[] = [];
    const zones: { after: number; node: HTMLElement; lines: number }[] = [];

    if (a.kind === "replace" && !final) {
      zones.push({ after: range.endLineNumber, node: this.makeZone("ghost", newLines), lines: newLines.length });
    } else if (a.kind === "replace") {
      const oldLines = a.original.split("\n");
      const h = hunksOf(diffLines(oldLines, newLines));
      const last = oldLines.length - 1;
      for (const i of h.removed) {
        const line = range.startLineNumber + i;
        const startCol = i === 0 ? range.startColumn : 1;
        const model = this.editor.getModel();
        const endCol = i === last ? range.endColumn : (model?.getLineMaxColumn(line) ?? 1);
        const whole = i > 0 && i < last;
        decos.push({ range: new this.monaco.Range(line, startCol, line, endCol), options: { className: "vg-ai-del", inlineClassName: "vg-ai-del-text", isWholeLine: whole } });
      }
      for (const blk of h.added) zones.push({ after: range.startLineNumber + blk.afterOld, node: this.makeZone("add", blk.lines), lines: blk.lines.length });
    } else {
      zones.push({ after: range.startLineNumber, node: this.makeZone("ghost", newLines), lines: newLines.length });
    }

    this.deco.set(decos);
    const old = this.previewZones;
    this.previewZones = [];
    this.editor.changeViewZones((acc) => {
      old.forEach((id) => acc.removeZone(id));
      for (const z of zones) this.previewZones.push(acc.addZone({ afterLineNumber: z.after, heightInLines: z.lines, domNode: z.node, ordinal: 1 }));
    });
    if (final) this.ensureVisible(range.startLineNumber, zones.reduce((m, z) => Math.max(m, z.after + z.lines), range.endLineNumber));
  }

  /** Scrolls just enough that both the prompt bar and the end of the proposal are on screen. */
  private ensureVisible(startLine: number, endLine: number): void {
    const ed = this.editor;
    const height = ed.getLayoutInfo().height;
    const top = Math.max(0, ed.getTopForLineNumber(startLine) - BAR_HEIGHT - 4);
    const model = ed.getModel();
    const bottom = ed.getBottomForLineNumber(Math.min(endLine, model?.getLineCount() ?? endLine)) + 8;
    const scrollTop = ed.getScrollTop();
    if (bottom - scrollTop <= height && top >= scrollTop) return;
    ed.setScrollTop(Math.max(0, Math.min(top, bottom - height)));
  }

  focusInput(): void {
    window.setTimeout(() => this.barNode.querySelector<HTMLInputElement>("input")?.focus(), 0);
  }

  dispose(): void {
    this.close(false);
    this.disposables.forEach((d) => d.dispose());
    this.root.unmount();
  }
}
