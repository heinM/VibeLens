import { describe, expect, it } from "vitest";
import { composeRows } from "../tableRows";
import { EMPTY_PENDING, addInsert, rowKeyOf, setCell, setInsertCell, toggleDelete } from "../pendingChanges";

const cols = ["id", "name", "n"];
const loaded = [
  ["1", "a", 5],
  ["2", "b", 6],
  ["3", "c", 7],
];

describe("composeRows", () => {
  it("passes rows through untouched without pending changes", () => {
    const r = composeRows(cols, ["id"], loaded, EMPTY_PENDING);
    expect(r.rows).toEqual(loaded);
    expect(r.rows[0]).toBe(loaded[0]);
    expect(r.newCount).toBe(0);
  });
  it("overlays edits, marks deletes, pins new rows on top", () => {
    let p = setCell(EMPTY_PENDING, rowKeyOf(["id"], { id: "2" }), { id: "2" }, "name", "B!", "b");
    p = toggleDelete(p, rowKeyOf(["id"], { id: "3" }), { id: "3" });
    p = addInsert(p, "n1");
    p = setInsertCell(p, "n1", "name", "fresh");
    const r = composeRows(cols, ["id"], loaded, p);
    expect(r.rows.map((x) => x[1])).toEqual(["fresh", "a", "B!", "c"]);
    expect(r.meta[0]).toEqual({ kind: "new", id: "n1" });
    expect(r.meta[2]).toMatchObject({ kind: "row", loaded: 1, deleted: false });
    expect((r.meta[2] as unknown as { edited: Set<string> }).edited.has("name")).toBe(true);
    expect(r.meta[3]).toMatchObject({ loaded: 2, deleted: true });
    expect(loaded[1][1]).toBe("b"); // inputs are never mutated
  });
  it("has no keys for tables without a primary key", () => {
    const r = composeRows(cols, [], loaded, EMPTY_PENDING);
    expect(r.meta[0]).toMatchObject({ key: null, pk: null });
  });
});
