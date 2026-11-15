// "Recent picks" for the palette: a short most-recently-used list of item ids in localStorage.
// Every access is wrapped in try/catch (private windows, blocked storage); without storage the
// list lives in memory for the session.

const KEY = "vg.palette.mru";
export const MRU_LIMIT = 40;

let memory: string[] = [];

export interface MruStorage {
  getItem(k: string): string | null;
  setItem(k: string, v: string): void;
}

function defaultStorage(): MruStorage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

/** Parse a stored MRU list defensively: anything that is not an array of strings is ignored. */
export function parseMru(raw: string | null): string[] {
  if (!raw) return [];
  try {
    const v: unknown = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter((x): x is string => typeof x === "string").slice(0, MRU_LIMIT);
  } catch {
    return [];
  }
}

/** Move `id` to the front of `list`, dropping duplicates and trimming to the limit. */
export function pushMru(list: readonly string[], id: string, limit = MRU_LIMIT): string[] {
  return [id, ...list.filter((x) => x !== id)].slice(0, limit);
}

export function readMru(storage: MruStorage | null = defaultStorage()): string[] {
  try {
    if (!storage) return memory;
    return parseMru(storage.getItem(KEY));
  } catch {
    return memory;
  }
}

export function recordMru(id: string, storage: MruStorage | null = defaultStorage()): string[] {
  const next = pushMru(readMru(storage), id);
  memory = next;
  try {
    storage?.setItem(KEY, JSON.stringify(next));
  } catch {
    /* not remembered across restarts */
  }
  return next;
}

/** id -> rank (0 = most recent). */
export function mruRanks(list: readonly string[]): Map<string, number> {
  const m = new Map<string, number>();
  list.forEach((id, i) => m.set(id, i));
  return m;
}
