// Insert text at the cursor of the visible Monaco editor (the palette's secondary action).
// Monaco is imported lazily so unit tests and the first paint never pay for it.

/** Returns false when no editor is visible (caller then opens a new SQL tab instead). */
export async function insertIntoActiveEditor(text: string): Promise<boolean> {
  try {
    const { monaco } = await import("../../lib/monaco");
    const editor = monaco.editor.getEditors().find((e) => {
      const node = e.getContainerDomNode();
      return node.isConnected && !node.closest('[aria-hidden="true"]');
    });
    const sel = editor?.getSelection();
    if (!editor || !sel) return false;
    editor.executeEdits("vg-palette", [{ range: sel, text, forceMoveMarkers: true }]);
    editor.focus();
    return true;
  } catch {
    return false;
  }
}
