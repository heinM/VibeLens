import { describe, expect, it } from "vitest";
import { ALREADY_EXPLAIN, planExplain, startsWithExplain } from "../explainGuard";

describe("startsWithExplain", () => {
  it("sees EXPLAIN after whitespace and comments", () => {
    for (const s of ["explain select 1", "  \n EXPLAIN (ANALYZE) delete from t", "-- hi\nexplain analyze delete from t", "/* a */ /* b /* nested */ */ Explain select 1", "--x\n--y\n\nEXPLAIN VERBOSE select 1"]) expect(startsWithExplain(s), s).toBe(true);
  });
  it("is false for everything else", () => {
    for (const s of ["select 1", "explainer()", "select 'explain'", "with e as (select 1) select * from e", "-- explain\nselect 1", "delete from explain", ""]) expect(startsWithExplain(s), s).toBe(false);
  });
});

describe("planExplain", () => {
  it("refuses a statement that is already an EXPLAIN, in both modes", () => {
    for (const mode of ["plan", "analyze"] as const) {
      expect(planExplain("EXPLAIN ANALYZE DELETE FROM t", mode)).toEqual({ ok: false, message: ALREADY_EXPLAIN });
      expect(planExplain("-- c\n/* c */ explain analyze update t set a = 1", mode).ok).toBe(false);
    }
  });
  it("wraps plain statements and always asks for a READ ONLY transaction", () => {
    expect(planExplain("select 1;", "plan")).toEqual({ ok: true, readOnly: true, sql: "EXPLAIN (FORMAT JSON, VERBOSE, COSTS) select 1" });
    expect(planExplain("delete from t where id = 1", "plan")).toEqual({ ok: true, readOnly: true, sql: "EXPLAIN (FORMAT JSON, VERBOSE, COSTS) delete from t where id = 1" });
    expect(planExplain(" select 1 ;; ", "analyze")).toEqual({ ok: true, readOnly: true, sql: "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) select 1" });
  });
});
