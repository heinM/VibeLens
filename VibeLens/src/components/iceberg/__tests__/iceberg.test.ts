import { describe, expect, it } from "vitest";
import type { IcebergField, IcebergSnapshot, IcebergTableInfo } from "../../../ipc/types";
import { DAY_MS, compactNumber, formatDateTime, fromLocalInput, groupDigits, humanBytes, relativeTime, summaryInt, toLocalInput } from "../iceFormat";
import { qualifiedName, quoteAlways, quoteIfNeeded, selectAllSql, selectAsOfSnapshotSql } from "../iceSql";
import { countFields, flattenFields, nestedIds, opTone, partitionRows, planExpire, refsBySnapshot, snapshotDelta, sortRows, tableTotals, typeTone } from "../iceTableModel";
import { previewReducer, type PreviewState } from "../useIcePreview";
import { joinNamespace, namespaceLevels } from "../openIcebergTab";

const f = (id: number, name: string, typeName: string, required = false, children: IcebergField[] = []): IcebergField => ({ id, name, typeName, required, doc: null, children });

// Shape of the pyiceberg `shop.events` fixture.
const events: IcebergField[] = [
  f(1, "event_id", "long", true),
  f(2, "customer_id", "long"),
  f(3, "kind", "string"),
  f(5, "ctx", "struct<device: string, app_version: int>", false, [f(8, "device", "string"), f(9, "app_version", "int")]),
  f(6, "tags", "list<string>", false, [f(10, "element", "string", true)]),
  f(7, "attrs", "map<string, string>", false, [f(11, "key", "string", true), f(12, "value", "string", true)]),
];

const snap = (id: string, ts: number, parent: string | null, op: string, summary: Record<string, string> = {}): IcebergSnapshot => ({
  snapshotId: id,
  parentSnapshotId: parent,
  sequenceNumber: null,
  timestampMs: ts,
  operation: op,
  summary: { operation: op, ...summary },
  manifestList: null,
  schemaId: 0,
});

// Authentic ids from the pyiceberg `shop.orders` fixture: all exceed 2^53.
const A = "457075768782269625";
const B = "2638296332279484192";
const C = "445119697321003351";
const D = "4842051965880479688";
const T0 = 1793504285888;

const orders = (over: Partial<IcebergTableInfo> = {}): IcebergTableInfo => ({
  namespace: ["shop"],
  name: "orders",
  kind: "table",
  location: "file:///w/shop/orders",
  metadataLocation: null,
  formatVersion: 2,
  tableUuid: null,
  lastUpdatedMs: T0 + 400,
  currentSnapshotId: D,
  currentSchemaId: 0,
  fields: [f(1, "id", "long", true), f(5, "placed_at", "timestamptz", true)],
  schemaCount: 1,
  partitionSpec: [{ sourceId: 5, fieldId: 1000, name: "placed_day", transform: "day" }],
  sortOrder: [],
  properties: {},
  snapshots: [
    snap(D, T0 + 400, C, "append", { "added-data-files": "78", "added-records": "3000", "total-data-files": "78", "total-records": "3000", "total-files-size": "224393", "total-delete-files": "0" }),
    snap(C, T0 + 200, B, "delete", { "deleted-data-files": "91", "deleted-records": "3500", "total-records": "0" }),
    snap(B, T0 + 100, A, "append", { "added-data-files": "39", "added-records": "1500", "total-records": "3500" }),
    snap(A, T0, null, "append", { "added-data-files": "52", "added-records": "2000", "total-records": "2000" }),
  ],
  refs: [{ name: "main", snapshotId: D, kind: "branch" }],
  ...over,
});

describe("SQL helpers", () => {
  it("quotes identifiers only when needed", () => {
    expect(quoteIfNeeded("orders")).toBe("orders");
    expect(quoteIfNeeded("Orders")).toBe('"Orders"');
    expect(quoteIfNeeded("my table")).toBe('"my table"');
    expect(quoteIfNeeded("order")).toBe('"order"');
    expect(quoteIfNeeded("9lives")).toBe('"9lives"');
    expect(quoteAlways('a"b')).toBe('"a""b"');
    expect(qualifiedName(["shop"], "orders")).toBe("shop.orders");
    expect(qualifiedName(["lake", "Sales"], "q1-orders")).toBe('lake."Sales"."q1-orders"');
  });
  it("builds the default query", () => {
    expect(selectAllSql(["shop"], "orders")).toBe("SELECT * FROM shop.orders LIMIT 100;");
    expect(selectAllSql(["a", "b"], "user", 5)).toBe('SELECT * FROM a.b."user" LIMIT 5;');
  });
  it("time travel keeps the snapshot id as text, beyond 2^53", () => {
    expect(Number.isSafeInteger(Number(D))).toBe(false);
    expect(selectAsOfSnapshotSql(["shop"], "orders", D)).toBe(`SELECT * FROM "shop"."orders@${D}" LIMIT 100;`);
    expect(selectAsOfSnapshotSql(["lake", "sales"], "o", "1")).toBe('SELECT * FROM "lake"."sales"."o@1" LIMIT 100;');
  });
});

describe("namespace round-trip", () => {
  it("escapes dots inside a level", () => {
    const levels = ["a.b", "c"];
    expect(namespaceLevels(joinNamespace(levels))).toEqual(levels);
  });
});

describe("schema flattening", () => {
  it("lists nested fields depth-first with dotted paths", () => {
    const rows = flattenFields(events, new Set());
    expect(rows.map((r) => r.path)).toEqual(["event_id", "customer_id", "kind", "ctx", "ctx.device", "ctx.app_version", "tags", "tags.element", "attrs", "attrs.key", "attrs.value"]);
    expect(rows.find((r) => r.path === "ctx.device")?.depth).toBe(1);
    expect(countFields(events)).toBe(11);
  });
  it("hides the children of folded fields", () => {
    const rows = flattenFields(events, new Set([5, 7]));
    expect(rows.map((r) => r.path)).toEqual(["event_id", "customer_id", "kind", "ctx", "tags", "tags.element", "attrs"]);
    expect(rows.find((r) => r.path === "ctx")).toMatchObject({ expandable: true, expanded: false });
    expect(nestedIds(events)).toEqual([5, 6, 7]);
  });
  it("tones types", () => {
    expect(typeTone("decimal(12, 2)")).toBe("number");
    expect(typeTone("timestamptz")).toBe("time");
    expect(typeTone("list<string>")).toBe("nested");
    expect(typeTone("string")).toBe("text");
    expect(typeTone("boolean")).toBe("bool");
    expect(typeTone("variant")).toBe("other");
  });
});

describe("partitioning and sort", () => {
  it("renders the spec as expressions on the source column", () => {
    expect(partitionRows(orders())).toEqual([
      expect.objectContaining({ expression: "day(placed_at)", name: "placed_day", source: "placed_at", explain: "One partition per calendar day." }),
    ]);
    const ev = orders({
      fields: events,
      partitionSpec: [
        { sourceId: 2, fieldId: 1000, name: "customer_bucket", transform: "bucket[8]" },
        { sourceId: 3, fieldId: 1001, name: "kind", transform: "identity" },
        { sourceId: 8, fieldId: 1002, name: "dev", transform: "truncate[2]" },
      ],
    });
    expect(partitionRows(ev).map((p) => p.expression)).toEqual(["bucket(8, customer_id)", "kind", "truncate(2, ctx.device)"]);
  });
  it("renders the sort order", () => {
    const rows = sortRows(orders({ sortOrder: [{ sourceId: 5, transform: "day", direction: "desc", nullOrder: "nulls-last" }, { sourceId: 1, transform: "identity", direction: "asc", nullOrder: "nulls-first" }] }));
    expect(rows).toEqual([
      { expression: "day(placed_at)", direction: "DESC", nulls: "NULLS LAST" },
      { expression: "id", direction: "ASC", nulls: "NULLS FIRST" },
    ]);
  });
});

describe("snapshots", () => {
  it("colours operations", () => {
    expect([opTone("append"), opTone("overwrite"), opTone("delete"), opTone("replace"), opTone(null), opTone("weird")]).toEqual(["ok", "warn", "danger", "muted", "muted", "muted"]);
  });
  it("reads deltas from the summary", () => {
    const [d, c] = orders().snapshots;
    expect(snapshotDelta(d)).toEqual({ filesAdded: 78, filesRemoved: null, recordsAdded: 3000, recordsRemoved: null, totalRecords: 3000 });
    expect(snapshotDelta(c)).toMatchObject({ filesRemoved: 91, recordsRemoved: 3500, totalRecords: 0 });
    expect(summaryInt({ x: "abc" }, "x")).toBeNull();
  });
  it("groups refs by snapshot and reads the totals of the current snapshot", () => {
    expect(refsBySnapshot([{ name: "main", snapshotId: D, kind: "branch" }, { name: "v1", snapshotId: D, kind: "tag" }]).get(D)).toHaveLength(2);
    expect(tableTotals(orders())).toMatchObject({ records: 3000, dataFiles: 78, sizeBytes: 224393, deleteFiles: 0 });
    expect(tableTotals(orders({ currentSnapshotId: null, snapshots: [] }))).toBeNull();
  });
});

describe("planExpire", () => {
  const info = orders();
  it("expires everything older than the cutoff except the head", () => {
    const p = planExpire(info, T0 + 300, 1);
    expect(p.expire.sort()).toEqual([A, B, C].sort());
    expect(p.keep).toEqual([D]);
  });
  it("only expires strictly older snapshots", () => {
    expect(planExpire(info, T0 + 100, 1).expire).toEqual([A]);
  });
  it("keeps the newest N along the branch", () => {
    const p = planExpire(info, T0 + 10_000, 3);
    expect(p.expire).toEqual([A]);
    expect(p.keep.sort()).toEqual([B, C, D].sort());
  });
  it("never expires a tag's snapshot", () => {
    const tagged = orders({ refs: [{ name: "main", snapshotId: D, kind: "branch" }, { name: "release", snapshotId: B, kind: "tag" }] });
    expect(planExpire(tagged, T0 + 10_000, 1).expire.sort()).toEqual([A, C].sort());
  });
  it("treats retain 0 as 1", () => {
    expect(planExpire(info, T0 + 10_000, 0).keep).toEqual([D]);
  });
});

describe("formatting", () => {
  it("humanises sizes and counts", () => {
    expect(humanBytes(224393)).toBe("219 KB");
    expect(humanBytes("259358")).toBe("253 KB");
    expect(humanBytes(0)).toBe("0 B");
    expect(humanBytes(1536)).toBe("1.50 KB");
    expect(humanBytes(5 * 1024 ** 3)).toBe("5.00 GB");
    expect(humanBytes(null)).toBe("–");
    expect(groupDigits(1234567)).toBe("1,234,567");
    expect(groupDigits("3500")).toBe("3,500");
    expect(compactNumber(999)).toBe("999");
    expect(compactNumber(3000)).toBe("3k");
    expect(compactNumber(1_250_000)).toBe("1.3M");
    expect(compactNumber(null)).toBe("");
  });
  it("describes ages", () => {
    const now = 1_000_000_000_000;
    expect(relativeTime(now - 10_000, now)).toBe("just now");
    expect(relativeTime(now - 5 * 60_000, now)).toBe("5 min ago");
    expect(relativeTime(now - 3 * 3600_000, now)).toBe("3 h ago");
    expect(relativeTime(now - 5 * DAY_MS, now)).toBe("5 days ago");
    expect(relativeTime(now - 1 * DAY_MS, now)).toBe("1 day ago");
  });
  it("round-trips datetime-local values", () => {
    const ms = new Date(2026, 9, 31, 14, 3).getTime();
    expect(toLocalInput(ms)).toBe("2026-10-31T14:03");
    expect(fromLocalInput("2026-10-31T14:03")).toBe(ms);
    expect(fromLocalInput("")).toBeNull();
    expect(formatDateTime(ms)).toBe("2026-10-31 14:03:00");
  });
});

describe("previewReducer", () => {
  const start = previewReducer({ status: "idle", columns: [], rows: [], elapsedMs: null, truncated: false, bytesScanned: null, error: null, runId: 0 } as PreviewState, { type: "start" });
  it("accumulates the event stream into one result", () => {
    let s = start;
    expect(s).toMatchObject({ status: "running", runId: 1 });
    s = previewReducer(s, { type: "event", ev: { type: "columns", statementIndex: 0, columns: [{ name: "id", typeName: "long", typeOid: 0 }] } });
    s = previewReducer(s, { type: "event", ev: { type: "rows", statementIndex: 0, rows: [[1], [2]] } });
    s = previewReducer(s, { type: "event", ev: { type: "rows", statementIndex: 0, rows: [[3]] } });
    s = previewReducer(s, { type: "event", ev: { type: "statementDone", statementIndex: 0, commandTag: null, rowsAffected: null, rowCount: 3, truncated: true, elapsedMs: 42 } });
    s = previewReducer(s, { type: "event", ev: { type: "stats", statementIndex: 0, engine: "athena", bytesScanned: 10485760, costUsdEstimate: null, engineQueryId: null } });
    s = previewReducer(s, { type: "event", ev: { type: "finished", elapsedMs: 50 } });
    expect(s).toMatchObject({ status: "done", rows: [[1], [2], [3]], elapsedMs: 42, truncated: true, bytesScanned: 10485760 });
  });
  it("keeps the error when a failure arrives before 'finished'", () => {
    let s = previewReducer(start, { type: "event", ev: { type: "failed", statementIndex: null, error: { code: "other", message: "boom" } } });
    s = previewReducer(s, { type: "event", ev: { type: "finished", elapsedMs: 1 } });
    expect(s.status).toBe("error");
    expect(s.error?.message).toBe("boom");
  });
  it("ignores other statements and resets cleanly", () => {
    const s = previewReducer(start, { type: "event", ev: { type: "rows", statementIndex: 1, rows: [[9]] } });
    expect(s.rows).toEqual([]);
    expect(previewReducer(s, { type: "reset" })).toMatchObject({ status: "idle", runId: 1 });
  });
});
