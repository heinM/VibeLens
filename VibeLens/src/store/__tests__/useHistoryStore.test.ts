import { describe, expect, it, vi } from "vitest";
import {
  MAX_AGE_MS,
  MAX_ENTRIES,
  MAX_SQL_CHARS,
  TRUNCATED_MARK,
  addEntries,
  createHistoryStorage,
  halveStoredPayload,
  historyAllowed,
  pruneStored,
  sanitizeInput,
  type HistoryEntry,
  type HistoryInput,
} from "../useHistoryStore";
import { REDACTED } from "../../lib/sqlIntel/redact";

const input = (sql: string, extra: Partial<HistoryInput> = {}): HistoryInput => ({ sql, profileId: "p1", profileName: "dev", database: "db", ok: true, elapsedMs: 3, rowCount: null, error: null, ...extra });
const entry = (i: number, at = Date.now()): HistoryEntry => ({ id: `e${i}`, sql: `select ${i}`, profileId: "p1", profileName: "dev", database: "db", at, ok: true, elapsedMs: 1, rowCount: 1, error: null });

describe("sanitizeInput", () => {
  it("redacts secrets in SQL", () => {
    expect(sanitizeInput(input("CREATE ROLE a PASSWORD 'pw'"))?.sql).toBe(`CREATE ROLE a PASSWORD '${REDACTED}'`);
  });
  it("strips DETAIL from errors", () => {
    const e = sanitizeInput(input("insert into t values (1)", { ok: false, error: 'duplicate key value violates unique constraint "k"\nDETAIL: Key (v)=(4111-1111-1111-1111) already exists.' }));
    expect(e?.error).toBe('duplicate key value violates unique constraint "k"');
  });
  it("caps the stored SQL at 16 KB with a marker", () => {
    const e = sanitizeInput(input("select '" + "x".repeat(40_000) + "'"));
    expect(e?.sql.endsWith(TRUNCATED_MARK)).toBe(true);
    expect(e?.sql.length).toBeLessThan(MAX_SQL_CHARS + 100);
  });
  it("ignores blank SQL", () => {
    expect(sanitizeInput(input("   \n"))).toBeNull();
  });
});

describe("addEntries", () => {
  it("puts the newest first and collapses an immediate re-run", () => {
    let list = addEntries([], [input("select 1")]);
    list = addEntries(list, [input("select 2")]);
    list = addEntries(list, [input("select 2")]);
    expect(list.map((e) => e.sql)).toEqual(["select 2", "select 1"]);
  });
  it("adds a whole script in one go and caps the list", () => {
    const many = Array.from({ length: MAX_ENTRIES + 50 }, (_, i) => input(`select ${i}`));
    const list = addEntries([], many);
    expect(list).toHaveLength(MAX_ENTRIES);
    expect(list[0].sql).toBe(`select ${MAX_ENTRIES + 49}`);
  });
  it("does not record prod profiles unless opted in", () => {
    expect(addEntries([], [input("select 1", { environment: "prod" })], Date.now(), () => null)).toEqual([]);
    expect(addEntries([], [input("select 1", { environment: "prod" })], Date.now(), () => "0")).toEqual([]);
    expect(addEntries([], [input("select 1", { environment: "prod" })], Date.now(), () => "1")).toHaveLength(1);
    expect(addEntries([], [input("select 1", { environment: "dev" })], Date.now(), () => null)).toHaveLength(1);
    expect(addEntries([], [input("select 1")], Date.now(), () => null)).toHaveLength(1);
  });
  it("returns the same array when nothing was recorded", () => {
    const list: HistoryEntry[] = [entry(1)];
    expect(addEntries(list, [input("   ")])).toBe(list);
  });
});

describe("historyAllowed", () => {
  it("only gates prod", () => {
    expect(historyAllowed("dev", () => null)).toBe(true);
    expect(historyAllowed("prod", () => null)).toBe(false);
    expect(historyAllowed("prod", () => "1")).toBe(true);
  });
  it("fails closed when storage throws", () => {
    expect(
      historyAllowed("prod", () => {
        throw new Error("denied");
      }),
    ).toBe(false);
  });
});

describe("pruneStored", () => {
  it("drops entries older than 90 days and malformed ones", () => {
    const now = Date.now();
    const out = pruneStored([entry(1, now), entry(2, now - MAX_AGE_MS - 1000), { nope: true }, null, entry(3, now - MAX_AGE_MS + 60_000)], now);
    expect(out.map((e) => e.id)).toEqual(["e1", "e3"]);
  });
  it("tolerates garbage", () => {
    expect(pruneStored(undefined)).toEqual([]);
    expect(pruneStored("x")).toEqual([]);
  });
});

describe("history storage under quota", () => {
  const payload = (n: number) => JSON.stringify({ state: { entries: Array.from({ length: n }, (_, i) => entry(i)) }, version: 2 });
  const quota = () => Object.assign(new Error("full"), { name: "QuotaExceededError" });

  it("halves the entries of a payload", () => {
    const h = halveStoredPayload(payload(10));
    expect(h?.kept).toBe(5);
    expect(JSON.parse(h?.value ?? "{}").state.entries).toHaveLength(5);
    expect(halveStoredPayload("not json")).toBeNull();
    expect(halveStoredPayload(payload(1))).toBeNull();
  });
  it("retries once with the oldest half dropped, and reports it", () => {
    let calls = 0;
    const stored = new Map<string, string>();
    const onPruned = vi.fn();
    const s = createHistoryStorage(
      () => ({
        getItem: (k) => stored.get(k) ?? null,
        setItem: (k, v) => {
          if (++calls === 1) throw quota();
          stored.set(k, v);
        },
        removeItem: (k) => void stored.delete(k),
      }),
      onPruned,
    );
    expect(() => s.setItem("vg.history", payload(8))).not.toThrow();
    expect(JSON.parse(stored.get("vg.history") ?? "{}").state.entries).toHaveLength(4);
    expect(onPruned).toHaveBeenCalledWith(4);
    expect(calls).toBe(2);
  });
  it("drops the write silently when it still does not fit", () => {
    const s = createHistoryStorage(() => ({
      getItem: () => null,
      setItem: () => {
        throw quota();
      },
      removeItem: () => undefined,
    }));
    expect(() => s.setItem("k", payload(8))).not.toThrow();
  });
  it("survives a storage that throws on every call", () => {
    const boom = () => {
      throw new Error("blocked");
    };
    const s = createHistoryStorage(() => ({ getItem: boom, setItem: boom, removeItem: boom }));
    expect(s.getItem("k")).toBeNull();
    expect(() => s.setItem("k", "v")).not.toThrow();
    expect(() => s.removeItem("k")).not.toThrow();
  });
});
