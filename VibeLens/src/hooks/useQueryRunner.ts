// Runs SQL (Postgres through pgExecute, Iceberg through iceExecute: see queryDispatch) and accumulates
// the QueryEvent stream into per-statement result sets.
//
// Performance: rows live in plain mutable arrays inside a ref (appending a 500-row page is O(page),
// never O(total)). React is told about changes through a `version` counter that is coalesced to one
// bump per animation frame, so a fast stream causes at most ~60 renders/s regardless of page rate.
// Statement metadata is kept in an index-keyed list that is updated in place (O(1) per event, no
// array copies), so a 5,000-statement script costs the same per event as a single query.
import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { splitStatements } from "../components/editor/sqlSplit";
import { asError } from "../ipc/commands";
import type { Cell, ColumnMeta, EngineKind, Environment, ErrorPayload, ExecOptions, QueryEvent } from "../ipc/types";
import { stripErrorDetail } from "../lib/sqlIntel/redact";
import { useHistoryStore, type HistoryInput } from "../store/useHistoryStore";
import { DDL_TAG, cancelQuery, executeOnEngine, invalidateAfterDdl, statsFromEvent } from "./queryDispatch";

export type ResultStatus = "running" | "done" | "error";

/** What the engine reported about one statement (Iceberg only; Postgres never sends it). */
export interface StatementStats {
  /** `datafusion` (local) or `athena`. */
  engine: string;
  bytesScanned: number | null;
  /** Athena list-price estimate in US dollars, never a bill. */
  costUsdEstimate: number | null;
  /** The engine's own id for the query (the Athena QueryExecutionId). */
  engineQueryId: string | null;
}

export interface ResultSetMeta {
  index: number;
  /** Empty for command-only statements (INSERT/UPDATE/DDL). */
  columns: ColumnMeta[];
  status: ResultStatus;
  commandTag: string | null;
  rowsAffected: number | null;
  /** Rows received so far (final once status !== "running"). */
  rowCount: number;
  truncated: boolean;
  elapsedMs: number | null;
  error: ErrorPayload | null;
  /** This statement's own text when the splitter and the server agree on the statement boundaries. */
  sql?: string | null;
  /** Engine statistics (data scanned, estimated cost, query id) when the engine reports them. */
  stats?: StatementStats | null;
}

export interface RunArgs {
  profileId: string;
  /** Which engine the tab's connection is. Postgres when omitted. */
  engine?: EngineKind | null;
  /** Postgres database. Ignored (and recorded as null) for Iceberg, whose catalog has no databases. */
  database: string | null;
  /** Shown in query history next to each statement. */
  profileName?: string | null;
  /** Used only by the history: production connections are not recorded by default. */
  environment?: Environment | null;
  sql: string;
  options?: Partial<ExecOptions>;
  confirmed?: boolean;
}

export interface QueryRunner {
  /** Increments on every run; use to reset per-run UI state. */
  runId: number;
  running: boolean;
  statementCount: number;
  /**
   * One entry per statement, in order. The array is updated in place while a script streams, so read
   * it during render together with `version` (do not use its identity as a memo dependency).
   */
  results: ResultSetMeta[];
  /** Run-level failure that is not tied to a statement (connect error, parse error, ...). */
  error: ErrorPayload | null;
  /** Wall-clock for the whole script once finished. */
  totalElapsedMs: number | null;
  /** Bumps whenever rows or metadata changed; include in memo deps of row consumers. */
  version: number;
  /** The live row buffer for a statement. Mutated in place; read it only during render alongside `version`. */
  getRows: (statementIndex: number) => Cell[][];
  /**
   * Resolves with null on success (the stream continues asynchronously) or the rejection payload.
   * `confirm_required` is returned (not stored) so the caller can prompt and re-run with confirmed.
   */
  run: (args: RunArgs) => Promise<ErrorPayload | null>;
  cancel: () => void;
  clear: () => void;
}

/** What a run needs to remember to write one history entry per statement. */
export interface HistoryCtx {
  profileId: string;
  profileName: string | null;
  environment: Environment | null;
  database: string | null;
  /** Engine that ran the statements; written to every history entry. Postgres when absent. */
  engine?: EngineKind;
  sql: string;
  /** Scripts above BATCH_FROM statements are written to the history in one go when the run ends. */
  queue: HistoryInput[] | null;
  /** Statement texts when our splitter agrees with the server's statement count, else null. */
  texts: string[] | null;
  count: number;
  /** performance.now() when the current statement started (previous one finished). */
  stmtStart: number;
  /** True once any per-statement entry was written, so the script-level fallback is skipped. */
  recorded: boolean;
}

const MAX_ERROR_CHARS = 300;
/** Above this many statements the history is written once per run: persist serialises on every change. */
export const BATCH_HISTORY_FROM = 50;

/** Writes (or queues) one history entry. History is a convenience: nothing in here may throw. */
export function record(ctx: HistoryCtx, sql: string, ok: boolean, elapsedMs: number | null, rowCount: number | null, error: string | null): void {
  ctx.recorded = true;
  try {
    const entry: HistoryInput = {
      sql,
      profileId: ctx.profileId,
      profileName: ctx.profileName,
      environment: ctx.environment,
      database: ctx.database,
      ok,
      elapsedMs: elapsedMs === null ? null : Math.round(elapsedMs),
      rowCount,
      error: error === null ? null : stripErrorDetail(error).slice(0, MAX_ERROR_CHARS),
      ...(ctx.engine ? { engine: ctx.engine } : {}),
    };
    if (ctx.queue) ctx.queue.push(entry);
    else useHistoryStore.getState().record(entry);
  } catch {
    /* never let history break a query */
  }
}

/** Writes everything a long script queued, in a single store update. */
export function flushHistory(ctx: HistoryCtx | null): void {
  if (!ctx?.queue || ctx.queue.length === 0) return;
  const batch = ctx.queue;
  ctx.queue = [];
  try {
    useHistoryStore.getState().recordMany(batch);
  } catch {
    /* ignore */
  }
}

export function useQueryRunner(): QueryRunner {
  const qc = useQueryClient();
  const historyRef = useRef<HistoryCtx | null>(null);
  const rowsRef = useRef<Map<number, Cell[][]>>(new Map());
  /** Statement metadata in arrival order, updated in place; `metaPos` maps a statement index to its slot. */
  const metaRef = useRef<ResultSetMeta[]>([]);
  const metaPos = useRef<Map<number, number>>(new Map());
  const queryIdRef = useRef<string | null>(null);
  const cancelWantedRef = useRef(false);
  const tokenRef = useRef(0);
  const frameRef = useRef<number | null>(null);
  const mountedRef = useRef(true);

  const [version, setVersion] = useState(0);
  const [runId, setRunId] = useState(0);
  const [running, setRunning] = useState(false);
  const [statementCount, setStatementCount] = useState(0);
  const [error, setError] = useState<ErrorPayload | null>(null);
  const [totalElapsedMs, setTotalElapsedMs] = useState<number | null>(null);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      flushHistory(historyRef.current);
      if (frameRef.current !== null) cancelAnimationFrame(frameRef.current);
      // Leaving a tab open with a long query running is fine, but closing it should stop the work.
      if (queryIdRef.current) void cancelQuery(queryIdRef.current).catch(() => undefined);
    };
  }, []);

  const bump = useCallback(() => {
    if (frameRef.current !== null) return;
    frameRef.current = requestAnimationFrame(() => {
      frameRef.current = null;
      if (mountedRef.current) setVersion((v) => v + 1);
    });
  }, []);

  const bumpNow = useCallback(() => {
    if (frameRef.current !== null) {
      cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    }
    if (mountedRef.current) setVersion((v) => v + 1);
  }, []);

  const ensureMeta = (index: number): ResultSetMeta => {
    const metas = metaRef.current;
    const pos = metaPos.current.get(index);
    if (pos !== undefined) return metas[pos];
    const h = historyRef.current;
    const sql = h ? (h.texts?.[index] ?? (h.count === 1 ? h.sql : null)) : null;
    const m: ResultSetMeta = { index, columns: [], status: "running", commandTag: null, rowsAffected: null, rowCount: 0, truncated: false, elapsedMs: null, error: null, sql };
    const last = metas[metas.length - 1];
    if (!last || last.index < index) {
      metaPos.current.set(index, metas.length);
      metas.push(m);
    } else {
      // Out-of-order arrival never happens on the wire; keep the list sorted anyway.
      metas.push(m);
      metas.sort((a, b) => a.index - b.index);
      metas.forEach((x, i) => metaPos.current.set(x.index, i));
    }
    return m;
  };
  /** Replaces one statement's meta object (so memoised views see a new reference); O(1). */
  const patchMeta = (index: number, patch: Partial<ResultSetMeta>) => {
    const cur = ensureMeta(index);
    const pos = metaPos.current.get(index);
    if (pos !== undefined) metaRef.current[pos] = { ...cur, ...patch };
  };

  const getRows = useCallback((i: number): Cell[][] => {
    let r = rowsRef.current.get(i);
    if (!r) {
      r = [];
      rowsRef.current.set(i, r);
    }
    return r;
  }, []);

  const handleEvent = useCallback(
    (token: number, ev: QueryEvent) => {
      if (token !== tokenRef.current) return; // stale stream from a previous run
      switch (ev.type) {
        case "started": {
          queryIdRef.current = ev.queryId;
          setStatementCount(ev.statementCount);
          const h = historyRef.current;
          if (h) {
            const split = splitStatements(h.sql).map((x) => x.text);
            h.count = ev.statementCount;
            h.texts = split.length === ev.statementCount ? split : null;
            h.queue = ev.statementCount > BATCH_HISTORY_FROM ? [] : null;
            h.stmtStart = performance.now();
          }
          if (cancelWantedRef.current) void cancelQuery(ev.queryId).catch(() => undefined);
          break;
        }
        case "columns":
          patchMeta(ev.statementIndex, { columns: ev.columns });
          bumpNow();
          return;
        case "rows": {
          const buf = getRows(ev.statementIndex);
          for (let i = 0; i < ev.rows.length; i++) buf.push(ev.rows[i]);
          patchMeta(ev.statementIndex, { rowCount: buf.length });
          break;
        }
        case "stats":
          // Arrives before the statement's own `statementDone`; kept on the result for the status line.
          patchMeta(ev.statementIndex, { stats: statsFromEvent(ev) });
          break;
        case "statementDone": {
          patchMeta(ev.statementIndex, {
            status: "done",
            commandTag: ev.commandTag,
            rowsAffected: ev.rowsAffected,
            rowCount: ev.rowCount,
            truncated: ev.truncated,
            elapsedMs: ev.elapsedMs,
          });
          const h = historyRef.current;
          if (h) {
            const text = h.texts?.[ev.statementIndex] ?? (h.count === 1 ? h.sql : null);
            if (text !== null) record(h, text, true, ev.elapsedMs, ev.rowsAffected ?? ev.rowCount, null);
            h.stmtStart = performance.now();
            // New tables, columns or indexes: refresh the shared catalog so completion sees them.
            if (ev.commandTag && DDL_TAG.test(ev.commandTag)) invalidateAfterDdl(qc, h.engine, h.profileId);
          }
          break;
        }
        case "finished": {
          queryIdRef.current = null;
          setTotalElapsedMs(ev.elapsedMs);
          setRunning(false);
          bumpNow();
          const h = historyRef.current;
          // The splitter and the server disagreed on statement boundaries: keep one entry for the script.
          if (h && !h.recorded && h.texts === null && h.count > 1) record(h, h.sql, true, ev.elapsedMs, null, null);
          flushHistory(h);
          return;
        }
        case "failed": {
          queryIdRef.current = null;
          if (ev.statementIndex !== null) {
            patchMeta(ev.statementIndex, { status: "error", error: ev.error, rowCount: getRows(ev.statementIndex).length });
          } else {
            setError(ev.error);
          }
          setRunning(false);
          bumpNow();
          const h = historyRef.current;
          if (h) {
            const text = ev.statementIndex === null ? h.sql : (h.texts?.[ev.statementIndex] ?? h.sql);
            record(h, text, false, ev.statementIndex === null ? null : performance.now() - h.stmtStart, null, ev.error.message);
            flushHistory(h);
          }
          return;
        }
      }
      bump();
    },
    [bump, bumpNow, getRows, qc],
  );

  const reset = useCallback(() => {
    rowsRef.current = new Map();
    metaRef.current = [];
    metaPos.current = new Map();
    queryIdRef.current = null;
    cancelWantedRef.current = false;
    setError(null);
    setTotalElapsedMs(null);
    setStatementCount(0);
  }, []);

  const run = useCallback(
    async (args: RunArgs): Promise<ErrorPayload | null> => {
      const token = ++tokenRef.current;
      flushHistory(historyRef.current); // a superseded run must not lose what it had queued
      reset();
      const engine: EngineKind = args.engine ?? "postgres";
      historyRef.current = {
        profileId: args.profileId,
        profileName: args.profileName ?? null,
        environment: args.environment ?? null,
        // An Iceberg catalog has no databases: a stale tab.database must not be recorded against it.
        // (Redshift keeps it: a tab can run in another database of the warehouse.)
        database: engine === "iceberg" ? null : args.database,
        engine,
        sql: args.sql,
        queue: null,
        texts: null,
        count: 0,
        stmtStart: performance.now(),
        recorded: false,
      };
      setRunId(token);
      setRunning(true);
      bumpNow();
      try {
        // The queryId also arrives as the first `started` event, which is what cancel() uses.
        await executeOnEngine(engine, {
          profileId: args.profileId,
          database: args.database,
          sql: args.sql,
          options: args.options,
          confirmed: args.confirmed,
          onEvent: (ev) => handleEvent(token, ev),
        });
        return null;
      } catch (e) {
        if (token !== tokenRef.current) return null;
        const err = asError(e);
        setRunning(false);
        if (err.code === "confirm_required") {
          // Not executed (the caller will ask and re-run), so nothing goes into the history.
          historyRef.current = null;
          reset();
          bumpNow();
          return err;
        }
        // The run never started streaming (connection, parse or policy error): one failed entry.
        setError(err);
        bumpNow();
        if (historyRef.current && !historyRef.current.recorded) record(historyRef.current, args.sql, false, null, null, err.message);
        flushHistory(historyRef.current);
        return err;
      }
    },
    [handleEvent, reset, bumpNow],
  );

  const cancel = useCallback(() => {
    cancelWantedRef.current = true;
    if (queryIdRef.current) void cancelQuery(queryIdRef.current).catch(() => undefined);
  }, []);

  const clear = useCallback(() => {
    tokenRef.current++;
    reset();
    setRunning(false);
    bumpNow();
  }, [reset, bumpNow]);

  // `version` is state, so every change re-renders the owner; `results` is the live list, not a copy.
  const results = metaRef.current;
  return { runId, running, statementCount, results, error, totalElapsedMs, version, getRows, run, cancel, clear };
}
