// What the editor tabs keep across a webview reload or a restart, and how it is read back.
//
// Kept in localStorage (a per-machine convenience, like query history):
//   the tab list and the active tab; per tab its kind, title, connection binding (profile, database,
//   Valkey db, table/diagram target), workspace file path, dirty flag and editor text.
// Never kept: result sets, errors, run state, the editor selection. Connections live in the Rust
// process and are re-read by the status poll, so a restored tab only needs its binding back.
//
// Text over 1 MiB is not kept (the tab comes back without it, and not dirty, since there is nothing
// unsaved left to protect; a file-backed tab is then read from its file again).
//
// Everything read back is validated: a tab with anything malformed in it is dropped, never trusted,
// and nothing here throws.
import type { PersistStorage, StorageValue } from "zustand/middleware";
import type { TabKind, WorkspaceTab } from "./useWorkspaceStore";

export const WORKSPACE_STORAGE_KEY = "vg.workspace";
export const WORKSPACE_PERSIST_VERSION = 1;
/** A tab's text is kept only up to this many UTF-8 bytes. */
export const MAX_PERSISTED_TEXT_BYTES = 1024 * 1024;
export const MAX_PERSISTED_TABS = 200;
const MAX_FIELD_CHARS = 4096;
const KINDS: readonly TabKind[] = ["sql", "valkey", "table", "erd", "icetable"];

export interface PersistedTab {
  id: string;
  kind: TabKind;
  title: string;
  profileId: string | null;
  database: string | null;
  valkeyDb: number;
  path: string | null;
  contents: string;
  dirty: boolean;
  objectRef: { schema: string; name: string } | null;
}

export interface PersistedWorkspace {
  tabs: PersistedTab[];
  activeTabId: string | null;
}

/** True when `s` is more than `limit` UTF-8 bytes. Only encodes when the length cannot settle it. */
export function utf8Exceeds(s: string, limit: number): boolean {
  if (s.length > limit) return true; // at least one byte per UTF-16 unit
  if (s.length * 3 <= limit) return false; // at most three bytes per unit
  return new TextEncoder().encode(s).length > limit;
}

/** The slice of store state that is written out. Text over the cap is left out (and dirty cleared with it). */
export function toPersisted(tabs: readonly WorkspaceTab[], activeTabId: string | null): PersistedWorkspace {
  const out: PersistedTab[] = [];
  for (const t of tabs.slice(0, MAX_PERSISTED_TABS)) {
    const tooBig = utf8Exceeds(t.contents, MAX_PERSISTED_TEXT_BYTES);
    out.push({
      id: t.id,
      kind: t.kind,
      title: t.title,
      profileId: t.profileId,
      database: t.database,
      valkeyDb: t.valkeyDb,
      path: t.path,
      contents: tooBig ? "" : t.contents,
      dirty: tooBig ? false : t.dirty,
      objectRef: t.objectRef,
    });
  }
  return { tabs: out, activeTabId };
}

const isObj = (x: unknown): x is Record<string, unknown> => typeof x === "object" && x !== null && !Array.isArray(x);
const str = (x: unknown): x is string => typeof x === "string" && x.length <= MAX_FIELD_CHARS;
const strOrNull = (x: unknown): x is string | null => x === null || str(x);

function sanitizeTab(raw: unknown): PersistedTab | null {
  if (!isObj(raw)) return null;
  const { id, kind, title, profileId, database, valkeyDb, path, contents, dirty, objectRef } = raw;
  if (!str(id) || id === "" || !KINDS.includes(kind as TabKind) || !str(title)) return null;
  if (!strOrNull(profileId ?? null) || !strOrNull(database ?? null) || !strOrNull(path ?? null) || path === "") return null;
  if (typeof contents !== "string" || typeof dirty !== "boolean") return null;
  const db = valkeyDb ?? 0;
  if (typeof db !== "number" || !Number.isInteger(db) || db < 0 || db > 65535) return null;
  let ref: PersistedTab["objectRef"] = null;
  if (objectRef !== undefined && objectRef !== null) {
    if (!isObj(objectRef) || !str(objectRef.schema) || !str(objectRef.name)) return null;
    ref = { schema: objectRef.schema, name: objectRef.name };
  }
  // Text over the cap is never trusted, even if something else wrote it: back to "no text".
  const tooBig = utf8Exceeds(contents, MAX_PERSISTED_TEXT_BYTES);
  return {
    id,
    kind: kind as TabKind,
    title,
    profileId: (profileId as string | null | undefined) ?? null,
    database: (database as string | null | undefined) ?? null,
    valkeyDb: db,
    path: (path as string | null | undefined) ?? null,
    contents: tooBig ? "" : contents,
    dirty: tooBig ? false : dirty,
    objectRef: ref,
  };
}

/** Validates whatever was stored. Anything not understood is dropped; garbage as a whole gives an empty workspace. */
export function sanitizePersisted(raw: unknown): PersistedWorkspace {
  if (!isObj(raw) || !Array.isArray(raw.tabs)) return { tabs: [], activeTabId: null };
  const seen = new Set<string>();
  const tabs: PersistedTab[] = [];
  for (const item of raw.tabs.slice(0, MAX_PERSISTED_TABS)) {
    const tab = sanitizeTab(item);
    if (!tab || seen.has(tab.id)) continue;
    seen.add(tab.id);
    tabs.push(tab);
  }
  const wanted = typeof raw.activeTabId === "string" ? raw.activeTabId : null;
  const activeTabId = wanted !== null && seen.has(wanted) ? wanted : (tabs[0]?.id ?? null);
  return { tabs, activeTabId };
}

/** A restored tab as the store holds it: the saved fields, and no selection. */
export function toWorkspaceTab(t: PersistedTab): WorkspaceTab {
  return { ...t, selectedText: null };
}

/** Tabs bound to a profile that is not in `profileIds` come back unbound. Returns the same array when nothing changed. */
export function unbindMissingProfiles(tabs: WorkspaceTab[], profileIds: ReadonlySet<string>): WorkspaceTab[] {
  if (!tabs.some((t) => t.profileId !== null && !profileIds.has(t.profileId))) return tabs;
  return tabs.map((t) => (t.profileId !== null && !profileIds.has(t.profileId) ? { ...t, profileId: null, database: null, valkeyDb: 0 } : t));
}

// ---------------------------------------------------------------------------------------------
// Storage: debounced, and never throws
// ---------------------------------------------------------------------------------------------

export type Backing = Pick<Storage, "getItem" | "setItem" | "removeItem">;

export interface DebouncedStorage<S> extends PersistStorage<S> {
  /** Writes anything still waiting. Call when the page is about to go away. */
  flush: () => void;
}

export interface DebounceOptions<S> {
  delayMs: number;
  /** A write is forced after this long even if changes keep arriving, so a long typing burst is not all at risk. */
  maxWaitMs: number;
  /** A smaller copy of a value to try once when the full one does not fit (quota). null = nothing smaller. */
  shrink?: (v: StorageValue<S>) => StorageValue<S> | null;
}

/**
 * persist storage that holds the latest value and serialises it only when the timer fires, so typing
 * costs neither a stringify nor a localStorage write per keystroke. Reads see the pending value.
 * An unchanged payload is not written again; a failing write is retried once with `shrink`, then dropped.
 */
export function createDebouncedStorage<S>(backing: () => Backing, opts: DebounceOptions<S>): DebouncedStorage<S> {
  const pending = new Map<string, StorageValue<S>>();
  const written = new Map<string, string>();
  let timer: ReturnType<typeof setTimeout> | null = null;
  let firstAt = 0;

  const tryWrite = (name: string, text: string): boolean => {
    try {
      backing().setItem(name, text);
      written.set(name, text);
      return true;
    } catch {
      return false;
    }
  };

  const flush = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    for (const [name, value] of pending) {
      let text: string;
      try {
        text = JSON.stringify(value);
      } catch {
        continue;
      }
      if (written.get(name) === text) continue;
      if (tryWrite(name, text)) continue;
      const smaller = opts.shrink?.(value);
      if (smaller) {
        try {
          tryWrite(name, JSON.stringify(smaller));
        } catch {
          /* give up: tabs are a convenience */
        }
      }
    }
    pending.clear();
  };

  return {
    getItem: (name) => {
      const waiting = pending.get(name);
      if (waiting) return waiting;
      try {
        const text = backing().getItem(name);
        if (text === null) return null;
        written.set(name, text);
        const parsed: unknown = JSON.parse(text);
        return isObj(parsed) && "state" in parsed ? (parsed as unknown as StorageValue<S>) : null;
      } catch {
        return null; // unreadable or not JSON: start empty
      }
    },
    setItem: (name, value) => {
      pending.set(name, value);
      const now = Date.now();
      if (timer === null) firstAt = now;
      else clearTimeout(timer);
      if (now - firstAt >= opts.maxWaitMs) {
        flush();
        return;
      }
      timer = setTimeout(flush, opts.delayMs);
    },
    removeItem: (name) => {
      pending.delete(name);
      written.delete(name);
      try {
        backing().removeItem(name);
      } catch {
        /* ignore */
      }
    },
    flush,
  };
}

/** The retry after a quota failure: keep text only for the tabs that cannot be recovered (dirty) and the active one. */
export function shrinkWorkspace(v: StorageValue<PersistedWorkspace>): StorageValue<PersistedWorkspace> | null {
  const tabs = v.state.tabs.map((t) => (t.dirty || t.id === v.state.activeTabId || t.contents === "" ? t : { ...t, contents: "" }));
  return tabs.some((t, i) => t !== v.state.tabs[i]) ? { ...v, state: { ...v.state, tabs } } : null;
}
