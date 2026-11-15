import { describe, expect, it } from "vitest";
import { COLLAPSE_TABS_ABOVE, PUBLISH_SQL_MAX, collapseTabs, nextToPublish, sqlForPublish } from "../publishResult";

describe("sqlForPublish", () => {
  it("prefers the statement's own SQL over the script", () => {
    expect(sqlForPublish("select 2;", "select 1; select 2; drop table x;")).toBe("select 2;");
  });
  it("falls back to the script, shortened", () => {
    expect(sqlForPublish(null, "  select 1  ")).toBe("select 1");
    const long = "select '" + "x".repeat(5000) + "'";
    const out = sqlForPublish(undefined, long) ?? "";
    expect(out.length).toBeLessThan(PUBLISH_SQL_MAX + 40);
    expect(out.endsWith("-- …truncated")).toBe(true);
  });
  it("is null when there is nothing", () => {
    expect(sqlForPublish("", "  ")).toBeNull();
    expect(sqlForPublish(null, null)).toBeNull();
  });
});

describe("nextToPublish", () => {
  const r = (status: string) => ({ status });
  it("walks the finished prefix once and stops at a running statement", () => {
    const results = [r("done"), r("done"), r("running"), r("running")];
    const a = nextToPublish(results, 0);
    expect(a.items).toHaveLength(2);
    expect(a.next).toBe(2);
    expect(nextToPublish(results, a.next).items).toHaveLength(0);
    results[2] = r("done");
    const b = nextToPublish(results, a.next);
    expect(b.items).toHaveLength(1);
    expect(b.next).toBe(3);
  });
  it("does not hand the same items out again when only a tab was switched", () => {
    const results = [r("done")];
    const first = nextToPublish(results, 0);
    expect(nextToPublish(results, first.next).items).toEqual([]);
  });
});

describe("collapseTabs", () => {
  it("collapses above 50 statements", () => {
    expect(collapseTabs(COLLAPSE_TABS_ABOVE)).toBe(false);
    expect(collapseTabs(COLLAPSE_TABS_ABOVE + 1)).toBe(true);
  });
});
