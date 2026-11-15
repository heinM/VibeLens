// Loads one page of a table for the table browser: keyset pagination on the primary key when it is
// safe (every ordering column is NOT NULL), OFFSET otherwise, an exact count on demand, and
// cancellation of superseded loads. Rows come through api.pgExecute in a READ ONLY transaction.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, asError } from "../ipc/commands";
import type { Cell, ColumnInfo, ErrorPayload, QueryEvent } from "../ipc/types";
import {
  buildCount,
  buildSelectPage,
  buildWhere,
  type FilterRow,
  type KeysetKey,
  type SortKey,
} from "../components/table/sqlGen";

export interface TableDataArgs {
  profileId: string | null;
  database: string | null;
  schema: string;
  name: string;
  /** From pg_describe_table; null until it has loaded. */
  columns: ColumnInfo[] | null;
  filters: FilterRow[];
  rawWhere: string;
  sort: SortKey[];
  pageSize: number;
  enabled: boolean;
}

export type LoadStatus = "idle" | "loading" | "ready" | "error";

export interface TableData {
  status: LoadStatus;
  rows: Cell[][];
  error: ErrorPayload | null;
  elapsedMs: number | null;
  /** Zero-based index of the page currently shown. */
  pageIndex: number;
  hasNext: boolean;
  /** True when this query pages by primary key instead of OFFSET. */
  keyset: boolean;
  /** Filter validation problems by filter id ("raw" for the raw WHERE). Loading is paused while there are any. */
  whereErrors: Record<string, string>;
  whereActive: boolean;
  /** The SELECT that produced (or will produce) the current page, without the "one extra row" probe. */
  sql: string | null;
  /** Exact total if known (count query, or the last page was reached). */
  exactTotal: number | null;
  countStatus: "idle" | "loading" | "error";
  next: () => void;
  prev: () => void;
  first: () => void;
  reload: () => void;
  loadCount: () => void;
}

interface Paging {
  identity: string;
  pageIndex: number;
  /** cursors[i] = key values after which page i+1 starts. */
  cursors: Cell[][];
}

export function useTableData(a: TableDataArgs): TableData {
  const { profileId, database, schema, name, columns, filters, rawWhere, sort, pageSize, enabled } = a;

  const colRefs = useMemo(() => (columns ?? []).map((c) => ({ name: c.name, dataType: c.dataType })), [columns]);
  const where = useMemo(() => buildWhere(filters, colRefs, rawWhere), [filters, colRefs, rawWhere]);
  const hasWhereErrors = Object.keys(where.errors).length > 0;

  // Ordering and the keyset decision.
  const plan = useMemo(() => {
    if (!columns) return null;
    const pk = columns.filter((c) => c.isPrimaryKey);
    const known = new Set(columns.map((c) => c.name));
    const userSort = sort.filter((s) => known.has(s.column));
    const orderBy: SortKey[] = [...userSort];
    for (const c of pk) if (!orderBy.some((k) => k.column === c.name)) orderBy.push({ column: c.name, dir: "asc" });
    const byName = new Map(columns.map((c) => [c.name, c]));
    const keyset = pk.length > 0 && orderBy.every((k) => byName.get(k.column)?.nullable === false);
    const keys: KeysetKey[] = orderBy.map((k) => ({ column: k.column, dataType: byName.get(k.column)?.dataType ?? "text", dir: k.dir }));
    const keyIdx = orderBy.map((k) => columns.findIndex((c) => c.name === k.column));
    return { orderBy, keyset, keys, keyIdx, names: columns.map((c) => c.name) };
  }, [columns, sort]);

  const identity = useMemo(
    () => JSON.stringify([profileId, database, schema, name, where.sql, plan?.orderBy, plan?.names, pageSize]),
    [profileId, database, schema, name, where.sql, plan, pageSize],
  );

  const [paging, setPaging] = useState<Paging>({ identity: "", pageIndex: 0, cursors: [] });
  const pageIndex = paging.identity === identity ? paging.pageIndex : 0;
  const cursors = paging.identity === identity ? paging.cursors : [];

  const [nonce, setNonce] = useState(0);
  const [state, setState] = useState<{ status: LoadStatus; rows: Cell[][]; error: ErrorPayload | null; elapsedMs: number | null; hasNext: boolean; sql: string | null; page: number }>({
    status: "idle",
    rows: [],
    error: null,
    elapsedMs: null,
    hasNext: false,
    sql: null,
    page: 0,
  });
  const [count, setCount] = useState<{ identity: string; value: number | null; status: "idle" | "loading" | "error" }>({ identity: "", value: null, status: "idle" });

  const token = useRef(0);
  const queryId = useRef<string | null>(null);
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (queryId.current) void api.pgCancel(queryId.current).catch(() => undefined);
    };
  }, []);

  const cursorForPage = cursors[pageIndex - 1] ?? null;
  const cursorKey = JSON.stringify(cursorForPage);

  useEffect(() => {
    if (!enabled || !profileId || !plan || !columns || hasWhereErrors) return;
    const my = ++token.current;
    if (queryId.current) void api.pgCancel(queryId.current).catch(() => undefined);

    let sqlText: string;
    let probeSql: string;
    try {
      const useKeyset = plan.keyset && cursorForPage !== null;
      const base = {
        schema,
        table: name,
        columns: plan.names,
        where: where.sql,
        orderBy: plan.orderBy,
        after: useKeyset ? { keys: plan.keys, values: cursorForPage } : null,
        offset: plan.keyset ? 0 : pageIndex * pageSize,
      };
      sqlText = buildSelectPage({ ...base, limit: pageSize });
      probeSql = buildSelectPage({ ...base, limit: pageSize + 1 });
    } catch (e) {
      setState((s) => ({ ...s, status: "error", error: { code: "invalid", message: e instanceof Error ? e.message : String(e) } }));
      return;
    }

    setState((s) => ({ ...s, status: "loading", error: null, sql: sqlText }));
    const buffer: Cell[][] = [];
    let elapsed: number | null = null;
    let failed: ErrorPayload | null = null;

    const finish = () => {
      if (!alive.current || my !== token.current) return;
      queryId.current = null;
      if (failed) {
        setState((s) => ({ ...s, status: "error", error: failed }));
        return;
      }
      const hasNext = buffer.length > pageSize;
      const rows = hasNext ? buffer.slice(0, pageSize) : buffer;
      const last = rows[rows.length - 1];
      const nextCursor = hasNext && last && plan.keyset ? plan.keyIdx.map((i) => last[i] ?? null) : null;
      setState({ status: "ready", rows, error: null, elapsedMs: elapsed, hasNext, sql: sqlText, page: pageIndex });
      // Remember where the next page starts (keyset); pages after this one are no longer valid.
      setPaging((p) => {
        const b = p.identity === identity ? p : { identity, pageIndex: 0, cursors: [] as Cell[][] };
        const cs = b.cursors.slice(0, pageIndex);
        if (nextCursor) cs[pageIndex] = nextCursor;
        return { ...b, cursors: cs };
      });
    };

    const onEvent = (ev: QueryEvent) => {
      if (my !== token.current) return;
      switch (ev.type) {
        case "started":
          queryId.current = ev.queryId;
          break;
        case "rows":
          for (const r of ev.rows) buffer.push(r);
          break;
        case "statementDone":
          elapsed = ev.elapsedMs;
          break;
        case "failed":
          failed = ev.error;
          finish();
          break;
        case "finished":
          finish();
          break;
      }
    };

    void api
      .pgExecute({ profileId, database, sql: probeSql, options: { readOnly: true, maxRows: pageSize + 1, pageSize: 500 }, onEvent })
      .catch((e) => {
        if (!alive.current || my !== token.current) return;
        failed = asError(e);
        finish();
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, identity, hasWhereErrors, pageIndex, cursorKey, nonce]);

  const go = useCallback(
    (target: number) => setPaging((p) => ({ identity, pageIndex: Math.max(0, target), cursors: p.identity === identity ? p.cursors : [] })),
    [identity],
  );
  const next = useCallback(() => {
    if (!state.hasNext) return;
    // Keyset needs the cursor of the page we are on; it is recorded as soon as the page loads.
    go(pageIndex + 1);
  }, [state.hasNext, go, pageIndex]);
  const prev = useCallback(() => go(pageIndex - 1), [go, pageIndex]);
  const first = useCallback(() => go(0), [go]);
  const reload = useCallback(() => {
    setCount((c) => ({ ...c, value: null, status: "idle" }));
    setNonce((n) => n + 1);
  }, []);

  const loadCount = useCallback(() => {
    if (!profileId) return;
    setCount({ identity, value: null, status: "loading" });
    let value: number | null = null;
    let failed = false;
    void api
      .pgExecute({
        profileId,
        database,
        sql: buildCount(schema, name, where.sql),
        options: { readOnly: true, maxRows: 1 },
        onEvent: (ev) => {
          if (ev.type === "rows") {
            const n = Number(ev.rows[0]?.[0]);
            if (Number.isFinite(n)) value = n;
          } else if (ev.type === "failed" || ev.type === "finished") {
            if (ev.type === "failed") failed = true;
            if (!alive.current) return;
            setCount((c) => (c.identity === identity ? { identity, value, status: failed || value === null ? "error" : "idle" } : c));
          }
        },
      })
      .catch(() => alive.current && setCount((c) => (c.identity === identity ? { ...c, status: "error" } : c)));
  }, [profileId, database, schema, name, where.sql, identity]);

  // Exact total: from a count query, or implied by landing on the last page.
  const lastPageTotal = state.status === "ready" && !state.hasNext ? state.page * pageSize + state.rows.length : null;
  const exactTotal = lastPageTotal ?? (count.identity === identity ? count.value : null);

  return {
    status: hasWhereErrors && state.status === "idle" ? "idle" : state.status,
    rows: state.rows,
    error: state.error,
    elapsedMs: state.elapsedMs,
    pageIndex: state.page,
    hasNext: state.hasNext,
    keyset: plan?.keyset ?? false,
    whereErrors: where.errors,
    whereActive: where.sql !== null,
    sql: state.sql,
    exactTotal,
    countStatus: count.identity === identity ? count.status : "idle",
    next,
    prev,
    first,
    reload,
    loadCount,
  };
}
