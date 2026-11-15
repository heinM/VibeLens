// Unsaved edits of a table tab with undo / redo. The state itself is immutable (see
// components/table/pendingChanges), so history is just a stack of snapshots.
import { useCallback, useReducer } from "react";
import { EMPTY_PENDING, type PendingState } from "../components/table/pendingChanges";

interface History {
  past: PendingState[];
  present: PendingState;
  future: PendingState[];
}

type Action = { type: "apply"; fn: (p: PendingState) => PendingState } | { type: "undo" } | { type: "redo" } | { type: "reset" };

const LIMIT = 200;

function reducer(h: History, a: Action): History {
  switch (a.type) {
    case "apply": {
      const next = a.fn(h.present);
      if (next === h.present) return h;
      return { past: [...h.past.slice(-(LIMIT - 1)), h.present], present: next, future: [] };
    }
    case "undo": {
      if (h.past.length === 0) return h;
      return { past: h.past.slice(0, -1), present: h.past[h.past.length - 1], future: [h.present, ...h.future] };
    }
    case "redo": {
      if (h.future.length === 0) return h;
      return { past: [...h.past, h.present], present: h.future[0], future: h.future.slice(1) };
    }
    case "reset":
      return { past: [], present: EMPTY_PENDING, future: [] };
  }
}

export interface TableEdits {
  pending: PendingState;
  /** Apply a pure transformation (setCell, toggleDelete, ...) as one undoable step. */
  update: (fn: (p: PendingState) => PendingState) => void;
  undo: () => void;
  redo: () => void;
  canUndo: boolean;
  canRedo: boolean;
  /** Drop every pending change and the history (after a successful apply, or Discard). */
  reset: () => void;
}

export function useTableEdits(): TableEdits {
  const [h, dispatch] = useReducer(reducer, { past: [], present: EMPTY_PENDING, future: [] });
  const update = useCallback((fn: (p: PendingState) => PendingState) => dispatch({ type: "apply", fn }), []);
  const undo = useCallback(() => dispatch({ type: "undo" }), []);
  const redo = useCallback(() => dispatch({ type: "redo" }), []);
  const reset = useCallback(() => dispatch({ type: "reset" }), []);
  return { pending: h.present, update, undo, redo, canUndo: h.past.length > 0, canRedo: h.future.length > 0, reset };
}
