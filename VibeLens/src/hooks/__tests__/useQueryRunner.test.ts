import { beforeEach, describe, expect, it, vi } from "vitest";
import { BATCH_HISTORY_FROM, flushHistory, record, type HistoryCtx } from "../useQueryRunner";
import { useHistoryStore } from "../../store/useHistoryStore";

const ctx = (over: Partial<HistoryCtx> = {}): HistoryCtx => ({
  profileId: "p",
  profileName: "dev",
  environment: "dev",
  database: "db",
  sql: "select 1",
  queue: null,
  texts: null,
  count: 1,
  stmtStart: 0,
  recorded: false,
  ...over,
});

beforeEach(() => useHistoryStore.setState({ entries: [] }));

describe("runner history", () => {
  it("records a single statement straight away", () => {
    const c = ctx();
    record(c, "select 1", true, 12.4, 1, null);
    expect(useHistoryStore.getState().entries).toHaveLength(1);
    expect(useHistoryStore.getState().entries[0].elapsedMs).toBe(12);
    expect(c.recorded).toBe(true);
  });

  it("queues a long script and writes it in one store update", () => {
    const c = ctx({ queue: [], count: BATCH_HISTORY_FROM + 70 });
    const seen: number[] = [];
    const unsub = useHistoryStore.subscribe((s) => seen.push(s.entries.length));
    for (let i = 0; i < BATCH_HISTORY_FROM + 70; i++) record(c, `select ${i}`, true, 1, 1, null);
    expect(useHistoryStore.getState().entries).toHaveLength(0);
    flushHistory(c);
    unsub();
    expect(useHistoryStore.getState().entries).toHaveLength(BATCH_HISTORY_FROM + 70);
    expect(seen).toEqual([BATCH_HISTORY_FROM + 70]); // exactly one change notification
    flushHistory(c); // idempotent
    expect(useHistoryStore.getState().entries).toHaveLength(BATCH_HISTORY_FROM + 70);
  });

  it("never throws when the store does", () => {
    const spy = vi.spyOn(useHistoryStore.getState(), "record").mockImplementation(() => {
      throw new DOMException("full", "QuotaExceededError");
    });
    expect(() => record(ctx(), "select 1", true, 1, 1, null)).not.toThrow();
    spy.mockRestore();
  });

  it("strips DETAIL from the stored error and skips prod by default", () => {
    record(ctx(), "insert", false, 1, null, "duplicate key\nDETAIL: Key (v)=(secret) already exists.");
    expect(useHistoryStore.getState().entries[0].error).toBe("duplicate key");
    useHistoryStore.setState({ entries: [] });
    record(ctx({ environment: "prod" }), "select 2", true, 1, 1, null);
    expect(useHistoryStore.getState().entries).toHaveLength(0);
  });
});
