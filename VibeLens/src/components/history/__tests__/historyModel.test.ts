import { describe, expect, it } from "vitest";
import { collapseSql, dayKey, dayLabel, filterEntries, groupByDay, timeAgo } from "../historyModel";
import type { HistoryEntry } from "../../../store/useHistoryStore";

const at = (y: number, mo: number, d: number, h = 12, mi = 0) => new Date(y, mo - 1, d, h, mi).getTime();
const entry = (over: Partial<HistoryEntry> & { at: number }): HistoryEntry => ({
  id: String(over.at),
  sql: "select 1",
  profileId: "p1",
  profileName: "Orders DB",
  database: "orders",
  ok: true,
  elapsedMs: 5,
  rowCount: 1,
  error: null,
  ...over,
});

describe("history model", () => {
  const now = at(2026, 10, 31, 15);

  it("labels days relative to now", () => {
    expect(dayLabel(at(2026, 10, 31, 1), now)).toBe("Today");
    expect(dayLabel(at(2026, 10, 30, 23), now)).toBe("Yesterday");
    expect(dayLabel(at(2026, 10, 27), now)).toBe("Tuesday");
    expect(dayLabel(at(2026, 10, 12), now)).toBe("12 Oct 2026");
    expect(dayKey(at(2026, 1, 5))).toBe("2026-01-05");
  });

  it("formats time ago", () => {
    expect(timeAgo(now - 10_000, now)).toBe("just now");
    expect(timeAgo(now - 5 * 60_000, now)).toBe("5m ago");
    expect(timeAgo(now - 3 * 3_600_000, now)).toBe("3h ago");
    expect(timeAgo(now - 2 * 86_400_000, now)).toBe("2d ago");
    expect(timeAgo(now + 5000, now)).toBe("just now");
  });

  it("groups newest-first entries under day headers with counts", () => {
    const rows = groupByDay([entry({ at: at(2026, 10, 31, 14) }), entry({ at: at(2026, 10, 31, 9) }), entry({ at: at(2026, 10, 30, 22) })], now);
    expect(rows.map((r) => (r.type === "day" ? `${r.label}:${r.count}` : "e"))).toEqual(["Today:2", "e", "e", "Yesterday:1", "e"]);
  });

  it("searches sql, connection, database and error; all words must match", () => {
    const list = [
      entry({ at: 3, sql: "SELECT * FROM orders WHERE id = 1" }),
      entry({ at: 2, sql: "update customers set a=1", profileName: "Billing", database: "billing" }),
      entry({ at: 1, sql: "select boom()", ok: false, error: 'function boom() does not exist' }),
    ];
    expect(filterEntries(list, "", false)).toHaveLength(3);
    expect(filterEntries(list, "from orders", false).map((e) => e.at)).toEqual([3]);
    expect(filterEntries(list, "billing update", false).map((e) => e.at)).toEqual([2]);
    expect(filterEntries(list, "does not exist", false).map((e) => e.at)).toEqual([1]);
    expect(filterEntries(list, "", true).map((e) => e.at)).toEqual([1]);
    expect(filterEntries(list, "select nope", false)).toHaveLength(0);
  });

  it("collapses whitespace for previews", () => {
    expect(collapseSql("select\n  a,\n\tb\n from t")).toBe("select a, b from t");
  });
});
