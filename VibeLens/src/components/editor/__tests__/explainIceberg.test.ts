// Explain on Iceberg connections: a plain text EXPLAIN, always read-only, never wrapped twice.
import { describe, expect, it } from "vitest";
import { ALREADY_EXPLAIN, ICEBERG_NO_ANALYZE, ICEBERG_ONE_STATEMENT, planExplain } from "../explainGuard";

describe("planExplain for Iceberg", () => {
  for (const dialect of ["datafusion", "athena"] as const) {
    describe(dialect, () => {
      it("sends a plain text EXPLAIN, read-only, flagged as text", () => {
        expect(planExplain("select * from shop.orders limit 5;", "plan", dialect)).toEqual({ ok: true, readOnly: true, text: true, sql: "EXPLAIN select * from shop.orders limit 5" });
        expect(planExplain(" select 1 ;; ", "plan", dialect)).toMatchObject({ sql: "EXPLAIN select 1" });
      });
      it("never asks for Postgres JSON options", () => {
        const p = planExplain("select 1", "plan", dialect);
        expect(p.ok && p.sql).not.toMatch(/FORMAT|VERBOSE|COSTS|ANALYZE/i);
      });
      it("refuses an EXPLAIN of an EXPLAIN in both modes (L3 guard)", () => {
        for (const mode of ["plan", "analyze"] as const) {
          expect(planExplain("EXPLAIN select 1", mode, dialect)).toEqual({ ok: false, message: ALREADY_EXPLAIN });
          expect(planExplain("-- c\n/* c */ explain analyze delete from t", mode, dialect).ok).toBe(false);
        }
      });
      it("has no Explain Analyze: it would execute the statement", () => {
        expect(planExplain("select 1", "analyze", dialect)).toEqual({ ok: false, message: ICEBERG_NO_ANALYZE });
      });
      it("is read-only even for statements that write", () => {
        for (const sql of ["delete from shop.orders where id = 1", "insert into shop.orders values (1)", "drop table shop.orders"]) {
          const p = planExplain(sql, "plan", dialect);
          expect(p).toMatchObject({ ok: true, readOnly: true });
        }
      });
      it("refuses a script, so statements after the first cannot run un-explained", () => {
        expect(planExplain("select 1; delete from shop.orders", "plan", dialect)).toEqual({ ok: false, message: ICEBERG_ONE_STATEMENT });
        expect(planExplain("select 1;\n-- done\n", "plan", dialect).ok).toBe(true);
      });
    });
  }
});

describe("planExplain for Postgres is unchanged", () => {
  it("still wraps with the JSON options and no text flag", () => {
    expect(planExplain("select 1;", "plan")).toEqual({ ok: true, readOnly: true, sql: "EXPLAIN (FORMAT JSON, VERBOSE, COSTS) select 1" });
    expect(planExplain("select 1", "analyze", "postgres")).toEqual({ ok: true, readOnly: true, sql: "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) select 1" });
    // A script is still handed to the server (its READ ONLY transaction is the guard there).
    expect(planExplain("select 1; select 2", "plan", null).ok).toBe(true);
  });
});
