// Query history: the statements run from SQL tabs and applied from the table editor, newest first.
// A per-machine convenience kept in this app's localStorage (capped at 1000 entries and 90 days),
// never synced and never holding result data.
//
// What is stored is deliberately less than what was run:
//  * SQL goes through `redactForHistory` (role passwords, conninfo passwords, pgcrypto keys, cloud
//    secrets) and is cut at 16 KB;
//  * error text loses its DETAIL / CONTEXT / WHERE lines, because Postgres puts row values there;
//  * table-editor applies are recorded as a one-line summary by the caller, not as the script;
//  * profiles marked `prod` record nothing unless localStorage['vg.history.prod'] === "1".
// Writing to storage can never throw out of `record`: a full quota prunes the oldest half and
// retries once, then gives up silently, so history cannot break running a query.
import { create } from "zustand";
import { createJSONStorage, persist, type StateStorage } from "zustand/middleware";
import type { EngineKind, Environment } from "../ipc/types";
import { uid } from "../lib/id";
import { redactForHistory, stripErrorDetail } from "../lib/sqlIntel/redact";

export interface HistoryEntry {
  id: string;
  sql: string;
  profileId: string | null;
  profileName: string | null;
  database: string | null;
  /** epoch ms */
  at: number;
  ok: boolean;
  elapsedMs: number | null;
  rowCount: number | null;
  error: string | null;
  /** Which engine ran it. Absent on entries written before Iceberg support: those are Postgres. */
  engine?: EngineKind;
}

/** What callers hand to `record`. `environment` is only used to decide whether to record at all. */
export type HistoryInput = Omit<HistoryEntry, "id" | "at"> & { environment?: Environment | null };

export const MAX_ENTRIES = 1000;
export const MAX_AGE_MS = 90 * 24 * 3600 * 1000;
export const MAX_SQL_CHARS = 16 * 1024;
export const TRUNCATED_MARK = "\n…[truncated]";
export const PROD_OPT_IN_KEY = "vg.history.prod";
const STORAGE_KEY = "vg.history";

/** Production connections are not recorded unless the user opted in by setting the key to "1". */
export function historyAllowed(environment: Environment | null | undefined, readFlag: () => string | null = defaultFlagReader): boolean {
  if (environment !== "prod") return true;
  try {
    return readFlag() === "1";
  } catch {
    return false;
  }
}

function defaultFlagReader(): string | null {
  return window.localStorage.getItem(PROD_OPT_IN_KEY);
}

/** Redacts, strips and bounds one input into a stored entry (without id / time). */
export function sanitizeInput(e: HistoryInput): Omit<HistoryEntry, "id" | "at"> | null {
  const raw = e.sql.trim();
  if (!raw) return null;
  const cut = raw.length > MAX_SQL_CHARS ? raw.slice(0, MAX_SQL_CHARS) + TRUNCATED_MARK : raw;
  // Truncate first: the redactor then never sees a multi-megabyte script.
  const sql = redactForHistory(cut);
  return {
    sql,
    profileId: e.profileId,
    profileName: e.profileName,
    database: e.database,
    ok: e.ok,
    elapsedMs: e.elapsedMs,
    rowCount: e.rowCount,
    error: e.error === null ? null : stripErrorDetail(e.error),
    ...(e.engine ? { engine: e.engine } : {}),
  };
}

/** Newest first, capped; an immediate re-run of the same statement replaces the previous entry. */
export function addEntries(entries: readonly HistoryEntry[], inputs: readonly HistoryInput[], now: number = Date.now(), readFlag?: () => string | null): HistoryEntry[] {
  let out = entries as HistoryEntry[];
  for (const input of inputs) {
    if (!historyAllowed(input.environment, readFlag)) continue;
    const clean = sanitizeInput(input);
    if (!clean) continue;
    const entry: HistoryEntry = { ...clean, id: uid(), at: now };
    const rest = out[0]?.sql === clean.sql && out[0]?.profileId === clean.profileId ? out.slice(1) : out;
    out = [entry, ...rest];
  }
  return out === entries ? out : out.slice(0, MAX_ENTRIES);
}

/** Drops malformed entries and anything older than 90 days. */
export function pruneStored(entries: unknown, now: number = Date.now()): HistoryEntry[] {
  if (!Array.isArray(entries)) return [];
  return entries
    .filter((x): x is HistoryEntry => !!x && typeof x === "object" && typeof (x as HistoryEntry).sql === "string" && typeof (x as HistoryEntry).at === "number" && now - (x as HistoryEntry).at <= MAX_AGE_MS)
    .slice(0, MAX_ENTRIES);
}

interface HistoryState {
  entries: HistoryEntry[];
  record: (e: HistoryInput) => void;
  /** One state change (and so one storage write) for a whole script. */
  recordMany: (es: readonly HistoryInput[]) => void;
  clear: () => void;
  remove: (id: string) => void;
}

// ---------------------------------------------------------------------------------------------
// Storage that survives a full quota
// ---------------------------------------------------------------------------------------------

type Backing = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/** Halves the entries of a serialised persist payload; null when it cannot be understood or is already tiny. */
export function halveStoredPayload(value: string): { value: string; kept: number } | null {
  try {
    const parsed = JSON.parse(value) as { state?: { entries?: unknown[] } };
    const list = parsed.state?.entries;
    if (!parsed.state || !Array.isArray(list) || list.length < 2) return null;
    const kept = Math.floor(list.length / 2);
    parsed.state.entries = list.slice(0, kept);
    return { value: JSON.stringify(parsed), kept };
  } catch {
    return null;
  }
}

/** persist storage whose `setItem` never throws: on quota errors prune the oldest half and retry once. */
export function createHistoryStorage(backing: () => Backing, onPruned?: (kept: number) => void): StateStorage {
  return {
    getItem: (name) => {
      try {
        return backing().getItem(name);
      } catch {
        return null;
      }
    },
    setItem: (name, value) => {
      try {
        backing().setItem(name, value);
        return;
      } catch {
        /* quota or storage unavailable: fall through to prune */
      }
      try {
        const half = halveStoredPayload(value);
        if (!half) return;
        backing().setItem(name, half.value);
        onPruned?.(half.kept);
      } catch {
        /* still full: drop this write; history is a convenience */
      }
    },
    removeItem: (name) => {
      try {
        backing().removeItem(name);
      } catch {
        /* ignore */
      }
    },
  };
}

const memory = new Map<string, string>();
const memoryBacking: Backing = { getItem: (k) => memory.get(k) ?? null, setItem: (k, v) => void memory.set(k, v), removeItem: (k) => void memory.delete(k) };

function localBacking(): Backing {
  try {
    return window.localStorage;
  } catch {
    return memoryBacking;
  }
}

export const useHistoryStore = create<HistoryState>()(
  persist(
    (set) => ({
      entries: [],
      record: (e) =>
        set((s) => {
          const next = addEntries(s.entries, [e]);
          return next === s.entries ? s : { entries: next };
        }),
      recordMany: (es) =>
        set((s) => {
          const next = addEntries(s.entries, es);
          return next === s.entries ? s : { entries: next };
        }),
      clear: () => set({ entries: [] }),
      remove: (id) => set((s) => ({ entries: s.entries.filter((x) => x.id !== id) })),
    }),
    {
      name: STORAGE_KEY,
      version: 2,
      storage: createJSONStorage(() =>
        createHistoryStorage(localBacking, (kept) =>
          // The persisted copy was halved to fit; bring the in-memory list in line (after this write finishes).
          queueMicrotask(() => useHistoryStore.setState((s) => ({ entries: s.entries.slice(0, kept) }))),
        ),
      ),
      partialize: (s) => ({ entries: s.entries }),
      // v1 stored SQL as typed: scrub what is already on disk once.
      migrate: (persisted, version) => {
        const entries = pruneStored((persisted as { entries?: unknown } | null)?.entries);
        if (version >= 2) return { entries };
        return {
          entries: entries.map((x) => ({
            ...x,
            sql: redactForHistory(x.sql.length > MAX_SQL_CHARS ? x.sql.slice(0, MAX_SQL_CHARS) + TRUNCATED_MARK : x.sql),
            error: x.error === null || x.error === undefined ? null : stripErrorDetail(x.error),
          })),
        };
      },
      // Every load: drop anything older than 90 days.
      merge: (persisted, current) => ({ ...current, entries: pruneStored((persisted as { entries?: unknown } | null)?.entries) }),
    },
  ),
);
