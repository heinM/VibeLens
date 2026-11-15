// "Insert into editor": put SQL from an agent answer into the active SQL tab, at the cursor or over
// the selection. Uses the live Monaco editor when it can be found, else edits the tab's text.
import { useWorkspaceStore } from "../../store/useWorkspaceStore";

export interface Insertion {
  /** Offsets in the original text that the replacement covers. */
  from: number;
  to: number;
  replacement: string;
  /** Where the inserted SQL itself starts and ends in the new text (without separator padding). */
  selectFrom: number;
  selectTo: number;
}

/**
 * Where and how to put `sql` into `text`.
 * - Non-empty selection: replace it, no padding.
 * - Cursor only: insert as its own block, separated from neighbouring statements by a blank line.
 */
export function computeInsertion(text: string, start: number, end: number, sql: string): Insertion {
  const body = sql.replace(/^\n+/, "").replace(/\s+$/, "");
  const from = Math.max(0, Math.min(start, end, text.length));
  const to = Math.min(text.length, Math.max(start, end));
  if (to > from) {
    return { from, to, replacement: body, selectFrom: from, selectTo: from + body.length };
  }
  const before = text.slice(0, from);
  const after = text.slice(from);
  const prefix = before.trim() === "" ? "" : before.endsWith("\n\n") ? "" : before.endsWith("\n") ? "\n" : "\n\n";
  const suffix = after.trim() === "" ? "\n" : after.startsWith("\n\n") ? "" : after.startsWith("\n") ? "\n" : "\n\n";
  const replacement = prefix + body + suffix;
  return { from, to: from, replacement, selectFrom: from + prefix.length, selectTo: from + prefix.length + body.length };
}

export const applyInsertion = (text: string, ins: Insertion): string => text.slice(0, ins.from) + ins.replacement + text.slice(ins.to);

export type InsertOutcome =
  /** Inserted through the live editor (cursor or selection). */
  | "editor"
  /** Monaco wasn't reachable; the text was appended to the tab's contents. */
  | "appended"
  /** There is no active SQL tab. */
  | "no-tab";

/** True when the active tab can take SQL. */
export function hasActiveSqlTab(): boolean {
  return useWorkspaceStore.getState().activeTab()?.kind === "sql";
}

export async function insertIntoActiveEditor(sql: string): Promise<InsertOutcome> {
  const store = useWorkspaceStore.getState();
  const tab = store.activeTab();
  if (!tab || tab.kind !== "sql") return "no-tab";

  try {
    const { monaco } = await import("../../lib/monaco");
    const visible = monaco.editor.getEditors().filter((ed) => {
      const dom = ed.getDomNode();
      if (!dom || !dom.isConnected) return false;
      const r = dom.getBoundingClientRect();
      return r.width > 0 && r.height > 0 && getComputedStyle(dom).visibility !== "hidden";
    });
    const ed = visible.find((e) => e.getModel()?.getValue() === tab.contents) ?? (visible.length === 1 ? visible[0] : undefined);
    const model = ed?.getModel();
    const sel = ed?.getSelection();
    if (ed && model && sel) {
      const text = model.getValue();
      const ins = computeInsertion(text, model.getOffsetAt(sel.getStartPosition()), model.getOffsetAt(sel.getEndPosition()), sql);
      const a = model.getPositionAt(ins.from);
      const b = model.getPositionAt(ins.to);
      ed.executeEdits("vibelens-agent", [{ range: new monaco.Range(a.lineNumber, a.column, b.lineNumber, b.column), text: ins.replacement, forceMoveMarkers: true }]);
      // Select what was inserted: Cmd+Enter then runs exactly that.
      const s = model.getPositionAt(ins.selectFrom);
      const e = model.getPositionAt(ins.selectFrom + (ins.selectTo - ins.selectFrom));
      ed.setSelection(new monaco.Selection(s.lineNumber, s.column, e.lineNumber, e.column));
      ed.revealLineInCenterIfOutsideViewport(s.lineNumber);
      ed.focus();
      return "editor";
    }
  } catch {
    // fall through to the store
  }

  const text = tab.contents;
  const ins = computeInsertion(text, text.length, text.length, sql);
  store.updateTab(tab.id, { contents: applyInsertion(text, ins), dirty: true });
  return "appended";
}
