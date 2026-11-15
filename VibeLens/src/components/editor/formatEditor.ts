// Format-SQL glue: formats the selection if there is one, else the whole buffer, as one undoable
// edit, and keeps the caret on the same token.
import type * as MonacoNS from "monaco-editor";
import { formatSql, mapOffsetAcrossFormat } from "../../lib/sqlIntel/format";

type Editor = MonacoNS.editor.IStandaloneCodeEditor;

export type FormatOutcome = { ok: true; changed: boolean; scope: "selection" | "buffer" } | { ok: false; message: string };

function friendly(e: unknown): string {
  const raw = e instanceof Error ? e.message : String(e);
  const first = raw.split("\n")[0].replace(/^Parse error:?\s*/i, "").trim();
  return first.length > 140 ? first.slice(0, 140) + "…" : first;
}

export function formatEditor(editor: Editor): FormatOutcome {
  const model = editor.getModel();
  if (!model) return { ok: false, message: "No document to format." };
  const sel = editor.getSelection();
  const hasSel = !!sel && !sel.isEmpty();
  const range = hasSel && sel ? sel : model.getFullModelRange();
  const scope = hasSel ? "selection" : "buffer";
  const before = model.getValueInRange(range);
  let after: string;
  try {
    after = formatSql(before);
  } catch (e) {
    return { ok: false, message: friendly(e) };
  }
  if (after === before) return { ok: true, changed: false, scope };

  const startOffset = model.getOffsetAt(range.getStartPosition());
  const pos = editor.getPosition();
  const caret = pos ? model.getOffsetAt(pos) - startOffset : 0;
  editor.pushUndoStop();
  editor.executeEdits("vibelens.format", [{ range, text: after, forceMoveMarkers: true }]);
  editor.pushUndoStop();

  if (hasSel) {
    const a = model.getPositionAt(startOffset);
    const b = model.getPositionAt(startOffset + after.length);
    editor.setSelection({ startLineNumber: a.lineNumber, startColumn: a.column, endLineNumber: b.lineNumber, endColumn: b.column });
  } else {
    editor.setPosition(model.getPositionAt(startOffset + mapOffsetAcrossFormat(before, Math.max(0, Math.min(caret, before.length)), after)));
  }
  return { ok: true, changed: true, scope };
}
