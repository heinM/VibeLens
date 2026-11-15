// Runs the table preview (`SELECT * ... LIMIT 100`) through api.iceExecute in read-only mode and folds the
// QueryEvent stream into one small result set. Not a general runner: one statement, a hundred rows.
import { useCallback, useEffect, useReducer, useRef } from "react";
import { api, asError } from "../../ipc/commands";
import type { Cell, ColumnMeta, ErrorPayload, QueryEvent } from "../../ipc/types";

export const PREVIEW_ROWS = 100;

export interface PreviewState {
  status: "idle" | "running" | "done" | "error";
  columns: ColumnMeta[];
  rows: Cell[][];
  elapsedMs: number | null;
  truncated: boolean;
  bytesScanned: number | null;
  error: ErrorPayload | null;
  /** Bumps on every run so consumers can reset selection. */
  runId: number;
}

const initial: PreviewState = { status: "idle", columns: [], rows: [], elapsedMs: null, truncated: false, bytesScanned: null, error: null, runId: 0 };

type Action = { type: "start" } | { type: "event"; ev: QueryEvent } | { type: "fail"; error: ErrorPayload } | { type: "reset" };

export function previewReducer(s: PreviewState, a: Action): PreviewState {
  switch (a.type) {
    case "reset":
      return { ...initial, runId: s.runId };
    case "start":
      return { ...initial, status: "running", runId: s.runId + 1 };
    case "fail":
      return { ...s, status: "error", error: a.error };
    case "event": {
      const ev = a.ev;
      switch (ev.type) {
        case "columns":
          return ev.statementIndex === 0 ? { ...s, columns: ev.columns } : s;
        case "rows":
          return ev.statementIndex === 0 ? { ...s, rows: s.rows.concat(ev.rows) } : s;
        case "statementDone":
          return ev.statementIndex === 0 ? { ...s, elapsedMs: ev.elapsedMs, truncated: ev.truncated } : s;
        case "stats":
          return { ...s, bytesScanned: ev.bytesScanned };
        case "finished":
          return s.status === "error" ? s : { ...s, status: "done", elapsedMs: s.elapsedMs ?? ev.elapsedMs };
        case "failed":
          return { ...s, status: "error", error: ev.error };
        default:
          return s;
      }
    }
  }
}

export function useIcePreview(profileId: string, sql: string) {
  const [state, dispatch] = useReducer(previewReducer, initial);
  const token = useRef(0);
  const queryId = useRef<string | null>(null);

  const run = useCallback(async () => {
    const mine = ++token.current;
    if (queryId.current) void api.pgCancel(queryId.current).catch(() => undefined);
    queryId.current = null;
    dispatch({ type: "start" });
    try {
      const id = await api.iceExecute({
        profileId,
        sql,
        options: { readOnly: true, maxRows: PREVIEW_ROWS, pageSize: PREVIEW_ROWS, timeoutMs: 60_000 },
        onEvent: (ev) => {
          if (token.current === mine) dispatch({ type: "event", ev });
        },
      });
      if (token.current === mine) queryId.current = id;
    } catch (e) {
      if (token.current === mine) dispatch({ type: "fail", error: asError(e) });
    }
  }, [profileId, sql]);

  const reset = useCallback(() => {
    token.current += 1;
    dispatch({ type: "reset" });
  }, []);

  useEffect(
    () => () => {
      token.current += 1;
      if (queryId.current) void api.pgCancel(queryId.current).catch(() => undefined);
    },
    [],
  );

  return { state, run, reset };
}
