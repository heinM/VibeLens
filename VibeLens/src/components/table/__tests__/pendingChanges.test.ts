import { describe, expect, it } from "vitest";
import {
  EMPTY_PENDING,
  addInsert,
  buildApplyScript,
  describeTableEdit,
  guardKeyedStatement,
  hasPending,
  missingRequired,
  pendingCounts,
  removeInsert,
  revertRow,
  rowKeyOf,
  setCell,
  setInsertCell,
  toggleDelete,
} from "../pendingChanges";
import type { TableTarget } from "../sqlGen";

const target: TableTarget = {
  schema: "public",
  name: "orders",
  columns: [
    { name: "id", dataType: "bigint" },
    { name: "status", dataType: "text" },
    { name: "total", dataType: "numeric" },
  ],
};

describe("pending changes", () => {
  it("keys rows by primary-key values in key order", () => {
    expect(rowKeyOf(["a", "b"], { b: 2, a: "1" })).toBe('["1",2]');
  });

  it("records a cell edit and drops it again when reverted to the original", () => {
    const k = rowKeyOf(["id"], { id: "1" });
    let p = setCell(EMPTY_PENDING, k, { id: "1" }, "status", "paid", "pending");
    expect(pendingCounts(p)).toMatchObject({ updates: 1, cells: 1, total: 1 });
    p = setCell(p, k, { id: "1" }, "total", "5", "4");
    expect(pendingCounts(p).cells).toBe(2);
    p = setCell(p, k, { id: "1" }, "status", "pending", "pending");
    expect(pendingCounts(p).cells).toBe(1);
    p = setCell(p, k, { id: "1" }, "total", 4, "4");
    expect(hasPending(p)).toBe(false);
  });

  it("is immutable", () => {
    const k = "k";
    const a = setCell(EMPTY_PENDING, k, { id: 1 }, "status", "x", "y");
    const b = setCell(a, k, { id: 1 }, "status", "z", "y");
    expect(a.updates[k].set.status).toBe("x");
    expect(b.updates[k].set.status).toBe("z");
    expect(EMPTY_PENDING.updates).toEqual({});
  });

  it("ignores edits to rows marked for deletion and drops existing edits when deleting", () => {
    const k = "k";
    let p = setCell(EMPTY_PENDING, k, { id: 1 }, "status", "x", "y");
    p = toggleDelete(p, k, { id: 1 });
    expect(pendingCounts(p)).toMatchObject({ updates: 0, deletes: 1 });
    expect(setCell(p, k, { id: 1 }, "status", "q", "y")).toBe(p);
    p = toggleDelete(p, k, { id: 1 });
    expect(hasPending(p)).toBe(false);
  });

  it("tracks inserted rows", () => {
    let p = addInsert(EMPTY_PENDING, "n1");
    p = setInsertCell(p, "n1", "status", "new");
    p = setInsertCell(p, "n1", "total", "3");
    expect(pendingCounts(p)).toMatchObject({ inserts: 1, cells: 2 });
    p = setInsertCell(p, "n1", "total", undefined);
    expect(p.inserts[0].values).toEqual({ status: "new" });
    p = removeInsert(p, "n1");
    expect(hasPending(p)).toBe(false);
  });

  it("reverts a whole row", () => {
    let p = setCell(EMPTY_PENDING, "k", { id: 1 }, "status", "x", "y");
    p = revertRow(p, "k");
    expect(hasPending(p)).toBe(false);
  });

  it("builds BEGIN / UPDATE / INSERT / DELETE / COMMIT", () => {
    let p = setCell(EMPTY_PENDING, "k1", { id: "1" }, "status", "paid", "open");
    p = addInsert(p, "n1", { status: "new", total: "9.5" });
    p = toggleDelete(p, "k2", { id: "2" });
    const s = buildApplyScript(target, p);
    expect(s.sql).toBe(
      [
        "BEGIN;",
        "DO $vg$",
        "#variable_conflict use_column",
        "BEGIN",
        `  UPDATE "public"."orders" SET "status" = 'paid'::text WHERE "id" = '1'::bigint;`,
        "  IF NOT FOUND THEN RAISE EXCEPTION 'VibeLens: change 1 matched no row in public.orders (it was changed or deleted elsewhere). Nothing was applied.'; END IF;",
        "END $vg$;",
        `INSERT INTO "public"."orders" ("status", "total") VALUES ('new'::text, '9.5'::numeric);`,
        "DO $vg$",
        "#variable_conflict use_column",
        "BEGIN",
        `  DELETE FROM "public"."orders" WHERE "id" = '2'::bigint;`,
        "  IF NOT FOUND THEN RAISE EXCEPTION 'VibeLens: change 3 matched no row in public.orders (it was changed or deleted elsewhere). Nothing was applied.'; END IF;",
        "END $vg$;",
        "COMMIT;",
      ].join("\n"),
    );
    expect(s.statements).toHaveLength(3);
    expect(s.counts.total).toBe(3);
  });

  it("guards keyed statements with a collision-free dollar tag and escaped message", () => {
    const t = { ...target, schema: "o'brien", name: "100%" };
    const g = guardKeyedStatement("UPDATE x SET a = '$vg$' WHERE id = 1;", 7, t);
    expect(g.startsWith("DO $vg1$\n#variable_conflict use_column\nBEGIN\n")).toBe(true);
    expect(g.endsWith("END $vg1$;")).toBe(true);
    expect(g).toContain("matched no row in o''brien.100%% (");
  });

  it("builds the RAISE message with quoteLiteral: backslashes switch to E'' and % stays doubled", () => {
    const t = { ...target, schema: "public", name: "we\\ird'100%" };
    const g = guardKeyedStatement("UPDATE x SET a = 1;", 2, t);
    expect(g).toContain("RAISE EXCEPTION E'VibeLens: change 2 matched no row in public.we\\\\ird''100%% (");
    // A name that would end the literal in a plain string stays inside the literal.
    const evil = guardKeyedStatement("UPDATE x SET a = 1;", 1, { ...target, name: "x\\'; DROP TABLE t; --" });
    expect(evil).toContain("E'VibeLens: change 1 matched no row in public.x\\\\''; DROP TABLE t; --");
  });

  it("avoids a dollar tag that occurs in the table name", () => {
    const g = guardKeyedStatement("UPDATE x SET a = 1;", 1, { ...target, name: "t$vg$x" });
    expect(g.startsWith("DO $vg1$\n")).toBe(true);
    expect(g.endsWith("END $vg1$;")).toBe(true);
  });

  it("summarises an apply for the history without values", () => {
    expect(describeTableEdit({ updates: 3, inserts: 1, deletes: 0 }, "shop", "orders")).toBe("-- Table edit: 3 updates, 1 insert in shop.orders");
    expect(describeTableEdit({ updates: 1, inserts: 0, deletes: 2 }, "shop", "orders")).toBe("-- Table edit: 1 update, 2 deletes in shop.orders");
    expect(describeTableEdit({ updates: 0, inserts: 0, deletes: 0 }, "s", "t")).toBe("-- Table edit: no changes in s.t");
    expect(describeTableEdit({ updates: 1, inserts: 0, deletes: 0 }, "s", "t\nDROP TABLE x;")).not.toContain("\n");
    const script = buildApplyScript(target, setCell(EMPTY_PENDING, "k1", { id: "1" }, "status", "4111-1111-1111-1111", "open"));
    expect(describeTableEdit(script.counts, "public", "orders")).not.toContain("4111");
  });

  it("flags new rows that miss NOT NULL columns without defaults", () => {
    const cols = [
      { name: "id", nullable: false, default: "nextval('s')" },
      { name: "customer", nullable: false, default: null },
      { name: "note", nullable: true, default: null },
    ];
    let p = addInsert(EMPTY_PENDING, "a");
    p = addInsert(p, "b", { customer: "7" });
    expect(missingRequired(cols, p.inserts)).toEqual([{ row: 1, columns: ["customer"] }]);
    expect(missingRequired(cols, removeInsert(p, "a").inserts)).toEqual([]);
  });
});

// Review finding H1.
describe("values that were cut when they were read", () => {
  const cut = { $truncated: { kind: "text", bytes: 52_428_800, preview: "start-of-the-value" } };

  it("stop the apply script instead of being written back", () => {
    // A duplicated row carries the original row's cells, including one that was cut.
    const p = addInsert(EMPTY_PENDING, "new-1", { id: "7", status: cut });
    expect(() => buildApplyScript(target, p)).toThrow(/status.*too large to load in full|too large to load in full/);
    // An edit to another column of the same row is fine: the cut cell is not part of the UPDATE.
    const k = rowKeyOf(["id"], { id: "1" });
    const edit = setCell(EMPTY_PENDING, k, { id: "1" }, "total", "5", "4");
    expect(buildApplyScript(target, edit).statements).toHaveLength(1);
    expect(buildApplyScript(target, edit).sql).not.toContain("start-of-the-value");
  });
});
