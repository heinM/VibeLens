// Keyboard events produced while an input method (IME) is composing must not trigger shortcuts.
// Chrome marks them with `isComposing`; Safari sends the commit keydown (the Enter that accepts a
// candidate) after composition has ended, with `isComposing === false` but `keyCode === 229`.

export function isImeKeyEvent(e: { isComposing?: boolean; keyCode?: number }): boolean {
  return e.isComposing === true || e.keyCode === 229;
}
