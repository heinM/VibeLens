import { describe, expect, it } from "vitest";
import {
  SqlGenError,
  buildCount,
  buildDelete,
  buildInsert,
  buildKeysetPredicate,
  buildOrderBy,
  buildSelectPage,
  buildUpdate,
  buildWhere,
  cellEquals,
  cellToEditText,
  isSafeTypeName,
  parseEditedText,
  pgArrayLiteral,
  qualifiedName,
  quoteIdent,
  quoteLiteral,
  shortTypeName,
  splitInList,
  sqlLiteral,
  typeCategory,
  uneditableReason,
  validateRawWhere,
  type ColumnRef,
  type FilterRow,
  type TableTarget,
  planTablePage,
  CTID_ORDER_MAX_ROWS,
  type PlanColumn,
} from "../sqlGen";

const cols: ColumnRef[] = [
  { name: "id", dataType: "bigint" },
  { name: "name", dataType: "character varying" },
  { name: "total", dataType: "numeric(12,2)" },
  { name: "meta", dataType: "jsonb" },
  { name: "raw", dataType: "json" },
  { name: "active", dataType: "boolean" },
  { name: "tags", dataType: "text[]" },
  { name: "blob", dataType: "bytea" },
  { name: "at", dataType: "timestamp with time zone" },
  { name: "Weird \"col\"; DROP", dataType: "text" },
];
const target: TableTarget = { schema: "public", name: "orders", columns: cols };
const f = (column: string, op: FilterRow["op"], value = ""): FilterRow => ({ id: `${column}-${op}`, column, op, value });

describe("quoting", () => {
  it("doubles embedded double quotes in identifiers", () => {
    expect(quoteIdent("a")).toBe('"a"');
    expect(quoteIdent('we"ird')).toBe('"we""ird"');
    expect(quoteIdent('"; DROP TABLE x; --')).toBe('"""; DROP TABLE x; --"');
    expect(quoteIdent("naïve 名前 😀")).toBe('"naïve 名前 😀"');
    expect(qualifiedName("my schema", 'T"1')).toBe('"my schema"."T""1"');
  });
  it("rejects empty and NUL identifiers", () => {
    expect(() => quoteIdent("")).toThrow(SqlGenError);
    expect(() => quoteIdent("a\0b")).toThrow(SqlGenError);
  });
  it("doubles single quotes in literals", () => {
    expect(quoteLiteral("plain")).toBe("'plain'");
    expect(quoteLiteral("O'Brien")).toBe("'O''Brien'");
    expect(quoteLiteral("'; DROP TABLE users; --")).toBe("'''; DROP TABLE users; --'");
    expect(quoteLiteral("")).toBe("''");
    expect(quoteLiteral("héllo 名前 😀\n\ttab")).toBe("'héllo 名前 😀\n\ttab'");
  });
  it("uses E'' with doubled backslashes whenever there is a backslash", () => {
    expect(quoteLiteral("a\\b")).toBe("E'a\\\\b'");
    expect(quoteLiteral("\\")).toBe("E'\\\\'");
    expect(quoteLiteral("it's \\ here")).toBe("E'it''s \\\\ here'");
    // a trailing backslash must not be able to swallow the closing quote
    expect(quoteLiteral("x\\")).toBe("E'x\\\\'");
    expect(quoteLiteral("\\'; DROP TABLE t; --")).toBe("E'\\\\''; DROP TABLE t; --'");
  });
  it("rejects NUL in text", () => {
    expect(() => quoteLiteral("a\0b")).toThrow(/NUL/);
  });
});

describe("types", () => {
  it("classifies format_type and short result type names", () => {
    expect(typeCategory("bigint")).toBe("int");
    expect(typeCategory("int8")).toBe("int");
    expect(typeCategory("numeric(12,2)")).toBe("numeric");
    expect(typeCategory("double precision")).toBe("float");
    expect(typeCategory("character varying(5)")).toBe("text");
    expect(typeCategory("timestamp(3) with time zone")).toBe("datetime");
    expect(typeCategory("timestamptz")).toBe("datetime");
    expect(typeCategory("jsonb")).toBe("jsonb");
    expect(typeCategory("integer[]")).toBe("array");
    expect(typeCategory("_int4")).toBe("array");
    expect(typeCategory("boolean")).toBe("bool");
    expect(typeCategory('"MyEnum"')).toBe("other");
    expect(typeCategory("uuid")).toBe("uuid");
  });
  it("only splices safe type names after ::", () => {
    expect(isSafeTypeName("character varying")).toBe(true);
    expect(isSafeTypeName('"My Enum"')).toBe(true);
    expect(isSafeTypeName("public.my_type[]")).toBe(true);
    expect(isSafeTypeName("numeric(12,2)")).toBe(true);
    expect(isSafeTypeName("text; DROP TABLE x")).toBe(false);
    expect(isSafeTypeName("text'")).toBe(false);
    expect(isSafeTypeName('"unterminated')).toBe(false);
    expect(isSafeTypeName("")).toBe(false);
    expect(isSafeTypeName("a--b")).toBe(false);
  });
  it("shortens type names like the result grid does", () => {
    expect(shortTypeName("character varying(20)")).toBe("varchar(20)");
    expect(shortTypeName("character varying(20)[]")).toBe("varchar(20)[]");
    expect(shortTypeName("integer")).toBe("int4");
    expect(shortTypeName("bigint")).toBe("int8");
    expect(shortTypeName("timestamp with time zone")).toBe("timestamptz");
    expect(shortTypeName("timestamp(3) without time zone")).toBe("timestamp");
    expect(shortTypeName("double precision")).toBe("float8");
    expect(shortTypeName("integer[]")).toBe("int4[]");
    expect(shortTypeName("character(3)")).toBe("bpchar(3)");
    expect(shortTypeName("realm_kind")).toBe("realm_kind");
    expect(shortTypeName("jsonb")).toBe("jsonb");
  });
});

describe("sqlLiteral", () => {
  it("emits NULL for null / undefined whatever the type", () => {
    expect(sqlLiteral(null, "bigint")).toBe("NULL");
    expect(sqlLiteral(undefined, "text")).toBe("NULL");
  });
  it("casts to the column type", () => {
    expect(sqlLiteral("42", "bigint")).toBe("'42'::bigint");
    expect(sqlLiteral(42, "integer")).toBe("'42'::integer");
    expect(sqlLiteral("12.50", "numeric(12,2)")).toBe("'12.50'::numeric");
    expect(sqlLiteral("hi", "character varying")).toBe("'hi'::character varying");
    expect(sqlLiteral("hi", "character(5)")).toBe("'hi'::bpchar");
    expect(sqlLiteral("101", "bit(3)")).toBe("'101'::varbit");
    expect(sqlLiteral("hi", "character varying[]")).toContain("::character varying[]");
    expect(sqlLiteral("2026-10-31T10:00:00Z", "timestamp with time zone")).toBe("'2026-10-31T10:00:00Z'::timestamp with time zone");
    expect(sqlLiteral("paid", '"Order Status"')).toBe("'paid'::\"Order Status\"");
  });
  it("refuses JSON whose numbers would round", () => {
    expect(parseEditedText('{"id":9007199254740993}', "jsonb").ok).toBe(false);
    expect(parseEditedText('{"id":1.5,"a":[1]}', "jsonb").ok).toBe(true);
    expect(parseEditedText("[9007199254740993]", "bigint[]").ok).toBe(false);
  });
  it("keeps huge numerics exact", () => {
    expect(sqlLiteral("9007199254740993", "bigint")).toBe("'9007199254740993'::bigint");
    expect(sqlLiteral("-9223372036854775808", "bigint")).toBe("'-9223372036854775808'::bigint");
    const big = "1" + "0".repeat(60) + ".123456789";
    expect(sqlLiteral(big, "numeric")).toBe(`'${big}'::numeric`);
    expect(sqlLiteral("1.5e300", "float8")).toBe("'1.5e300'::float8");
    expect(sqlLiteral("NaN", "numeric")).toBe("'NaN'::numeric");
    expect(sqlLiteral("-Infinity", "double precision")).toBe("'-Infinity'::double precision");
  });
  it("refuses non-numeric text in numeric columns instead of passing it on", () => {
    expect(() => sqlLiteral("1; DROP TABLE x", "bigint")).toThrow(SqlGenError);
    expect(() => sqlLiteral("12abc", "numeric")).toThrow(SqlGenError);
    expect(() => sqlLiteral("1.5", "integer")).toThrow(SqlGenError);
    expect(() => sqlLiteral({ a: 1 }, "int4")).toThrow(SqlGenError);
  });
  it("renders booleans as keywords", () => {
    expect(sqlLiteral(true, "boolean")).toBe("TRUE");
    expect(sqlLiteral(false, "boolean")).toBe("FALSE");
    expect(sqlLiteral("t", "boolean")).toBe("'t'::boolean");
  });
  it("escapes quotes and backslashes in text", () => {
    expect(sqlLiteral("O'Reilly", "text")).toBe("'O''Reilly'::text");
    expect(sqlLiteral("C:\\temp\\new", "text")).toBe("E'C:\\\\temp\\\\new'::text");
    expect(sqlLiteral("名前 😀", "text")).toBe("'名前 😀'::text");
  });
  it("falls back to no cast for an unsafe type string, still escaping the value", () => {
    expect(sqlLiteral("x", "text; DROP TABLE y")).toBe("'x'");
  });
  it("serialises json / jsonb values, including strings and numbers", () => {
    expect(sqlLiteral({ a: "it's", b: [1, null] }, "jsonb")).toBe(`'{"a":"it''s","b":[1,null]}'::jsonb`);
    expect(sqlLiteral("abc", "jsonb")).toBe(`'"abc"'::jsonb`);
    expect(sqlLiteral(5, "json")).toBe("'5'::json");
    expect(sqlLiteral({ p: "a\\b" }, "jsonb")).toBe(`E'{"p":"a\\\\\\\\b"}'::jsonb`);
  });
  it("builds array literals", () => {
    expect(pgArrayLiteral([1, null, 3])).toBe("{1,NULL,3}");
    expect(pgArrayLiteral(["a b", 'q"uote', "back\\slash", "NULL"])).toBe('{"a b","q\\"uote","back\\\\slash","NULL"}');
    expect(pgArrayLiteral([[1, 2], [3, 4]])).toBe("{{1,2},{3,4}}");
    expect(pgArrayLiteral([])).toBe("{}");
    expect(sqlLiteral(["a", "it's"], "text[]")).toBe(`'{"a","it''s"}'::text[]`);
    expect(sqlLiteral("{1,2}", "integer[]")).toBe("'{1,2}'::integer[]");
  });
  it("encodes bytea via decode() so escape settings cannot matter", () => {
    expect(sqlLiteral({ $bytes: "3q2+7w==", len: 4 }, "bytea")).toBe("decode('deadbeef', 'hex')");
    expect(sqlLiteral({ $bytes: "", len: 0 }, "bytea")).toBe("decode('', 'hex')");
    expect(sqlLiteral("\\xdead", "bytea")).toBe("E'\\\\xdead'::bytea");
  });
  it("can omit casts for exports", () => {
    expect(sqlLiteral("42", "int8", { cast: false })).toBe("42");
    expect(sqlLiteral("NaN", "float8", { cast: false })).toBe("'NaN'");
    expect(sqlLiteral("x", "text", { cast: false })).toBe("'x'");
  });
});

describe("edit text", () => {
  it("round-trips cells to editor text", () => {
    expect(cellToEditText(null)).toBe("");
    expect(cellToEditText("a")).toBe("a");
    expect(cellToEditText(5)).toBe("5");
    expect(cellToEditText(true)).toBe("true");
    expect(cellToEditText({ a: 1 })).toBe('{\n  "a": 1\n}');
    expect(cellToEditText({ $bytes: "3q2+7w==", len: 4 })).toBe("\\xdeadbeef");
  });
  it("validates by column type", () => {
    expect(parseEditedText(" 42 ", "bigint")).toEqual({ ok: true, cell: "42" });
    expect(parseEditedText("4x", "bigint").ok).toBe(false);
    expect(parseEditedText("", "bigint").ok).toBe(false);
    expect(parseEditedText("1.5", "numeric")).toEqual({ ok: true, cell: "1.5" });
    expect(parseEditedText("Yes", "boolean")).toEqual({ ok: true, cell: true });
    expect(parseEditedText("0", "boolean")).toEqual({ ok: true, cell: false });
    expect(parseEditedText("maybe", "boolean").ok).toBe(false);
    expect(parseEditedText('{"a":1}', "jsonb")).toEqual({ ok: true, cell: { a: 1 } });
    expect(parseEditedText("{a:1}", "jsonb").ok).toBe(false);
    expect(parseEditedText("[1,2]", "integer[]")).toEqual({ ok: true, cell: [1, 2] });
    expect(parseEditedText("{1,2}", "integer[]")).toEqual({ ok: true, cell: "{1,2}" });
    expect(parseEditedText("", "text")).toEqual({ ok: true, cell: "" });
  });
  it("compares cells the way a person would", () => {
    expect(cellEquals(5, "5")).toBe(true);
    expect(cellEquals(null, null)).toBe(true);
    expect(cellEquals(null, "")).toBe(false);
    expect(cellEquals({ a: 1 }, { a: 1 })).toBe(true);
    expect(cellEquals({ a: 1 }, { a: 2 })).toBe(false);
    expect(cellEquals("a", "b")).toBe(false);
    expect(cellEquals(true, "true")).toBe(false);
  });
});

describe("filters", () => {
  it("builds comparisons with casts", () => {
    const w = buildWhere([f("id", "=", "42"), f("name", "<>", "O'Neil"), f("total", ">=", "10.5")], cols);
    expect(w.errors).toEqual({});
    expect(w.sql).toBe(`"id" = '42'::bigint\n  AND "name" <> 'O''Neil'::character varying\n  AND "total" >= '10.5'::numeric`);
  });
  it("handles IS NULL / IS NOT NULL without a value", () => {
    expect(buildWhere([f("name", "IS NULL")], cols).sql).toBe(`"name" IS NULL`);
    expect(buildWhere([f("name", "IS NOT NULL", "ignored")], cols).sql).toBe(`"name" IS NOT NULL`);
  });
  it("LIKE / ILIKE compare the text form, so they work on any column", () => {
    expect(buildWhere([f("id", "LIKE", "4%")], cols).sql).toBe(`"id"::text LIKE '4%'`);
    expect(buildWhere([f("name", "ILIKE", "%o'b%")], cols).sql).toBe(`"name"::text ILIKE '%o''b%'`);
    expect(buildWhere([f("name", "LIKE", "100\\%")], cols).sql).toBe(`"name"::text LIKE E'100\\\\%'`);
  });
  it("IN splits on commas, respecting quotes", () => {
    expect(buildWhere([f("id", "IN", "1, 2,3")], cols).sql).toBe(`"id" IN ('1'::bigint, '2'::bigint, '3'::bigint)`);
    expect(buildWhere([f("name", "IN", `a, 'b,c', "d""e"`)], cols).sql).toBe(
      `"name" IN ('a'::character varying, 'b,c'::character varying, 'd"e'::character varying)`,
    );
    expect(splitInList("")).toEqual([]);
    expect(splitInList("'', x")).toEqual(["", "x"]);
    expect(splitInList("'it''s'")).toEqual(["it's"]);
  });
  it("reports errors per filter instead of generating bad SQL", () => {
    const w = buildWhere([f("id", "=", "abc"), f("nope", "=", "1"), f("id", "IN", ""), f("meta", "=", "{bad")], cols);
    expect(w.sql).toBeNull();
    expect(Object.keys(w.errors)).toHaveLength(4);
    expect(w.errors["id-="]).toMatch(/not a valid int8/);
  });
  it("compares json through text and jsonb natively", () => {
    expect(buildWhere([f("raw", "=", '{"a":1}')], cols).sql).toBe(`"raw"::text = '{"a":1}'`);
    expect(buildWhere([f("meta", "=", '{"a":1}')], cols).sql).toBe(`"meta" = '{"a":1}'::jsonb`);
  });
  it("only splices known operators", () => {
    const w = buildWhere([{ id: "x", column: "id", op: "= 1 OR 1=1 --" as never, value: "1" }], cols);
    expect(w.sql).toBeNull();
    expect(w.errors.x).toMatch(/operator/);
  });
  it("quotes weird column names", () => {
    const w = buildWhere([f('Weird "col"; DROP', "=", "x")], cols);
    expect(w.sql).toBe(`"Weird ""col""; DROP" = 'x'::text`);
  });
  it("keeps injection attempts inside the literal", () => {
    const w = buildWhere([f("name", "=", "x' OR '1'='1")], cols);
    expect(w.sql).toBe(`"name" = 'x'' OR ''1''=''1'::character varying`);
  });
  it("accepts a raw WHERE, fenced in parentheses", () => {
    const w = buildWhere([f("id", ">", "5")], cols, " total > 10 OR name = 'x' ");
    expect(w.sql).toBe(`"id" > '5'::bigint\n  AND (\ntotal > 10 OR name = 'x'\n)`);
  });
  it("ignores a blank / comment-only raw WHERE", () => {
    expect(buildWhere([], cols, "  ").sql).toBeNull();
    expect(buildWhere([], cols, "-- nothing\n").sql).toBeNull();
  });
});

describe("validateRawWhere", () => {
  it("accepts ordinary conditions", () => {
    for (const ok of [
      "a = 1",
      "a = 'x;y' AND b = \"c;d\"",
      "a IN (1,2,(3))",
      "a = E'it\\'s;'",
      "a = $$;$$",
      "a = $q$ ; ) $q$",
      "a = 1 -- trailing; comment",
      "a /* ; ( */ = 1",
      "a /* /* nested ; */ */ = 1",
      "name = 'it''s'",
    ])
      expect(validateRawWhere(ok), ok).toEqual({ ok: true });
  });
  it("rejects statement breaks and unbalanced input", () => {
    for (const bad of ["a = 1; DROP TABLE t", "a = (1", "a = 1)", "a = 'open", 'a = "open', "a /* open", "a = $$ open", "1=1); DELETE FROM t; --"])
      expect(validateRawWhere(bad).ok, bad).toBe(false);
  });
});

describe("validateRawWhere identifier boundaries", () => {
  // The server reads `ÀE'...'` as identifier `ÀE` followed by an ordinary string, in which a
  // backslash is a plain character. The scanner must agree, or `\'` would hide a statement break.
  const probes = [
    "À" + "E'\\') OR (true) --'",
    "ÀE'\\'); DROP TABLE t; --'",
    "éE'\\'; select 1; --'",
    "ñe'\\'; select 1; --'",
    "_E'\\'; select 1; --'",
    "xE'\\'; select 1; --'",
    "$E'\\'; select 1; --'",
    "1E'\\'; select 1; --'",
    "\u{1D4D0}E'\\'; select 1; --'", // astral letter before E
    "日本E'\\'; select 1; --'",
  ];
  it("does not treat E after any identifier character as an escape-string prefix", () => {
    for (const p of probes) expect(validateRawWhere(p).ok, p).toBe(false);
  });
  it("still treats a standalone E'..' as an escape string", () => {
    for (const ok of ["a = E'it\\'s;'", "a=E'x\\'y'", "E'\\'' = a", "(E'\\'')", "a = \"é\"E'\\'z;'"]) expect(validateRawWhere(ok), ok).toEqual({ ok: true });
  });
  it("accepts a non-ASCII-prefixed ordinary string whose backslash is literal and balanced", () => {
    expect(validateRawWhere("ÀE'a\\b' = x")).toEqual({ ok: true });
  });
  it("does not open a dollar quote in the middle of an identifier", () => {
    expect(validateRawWhere("x$$ ; $$").ok).toBe(false);
    expect(validateRawWhere("a = $$;$$").ok).toBe(true);
    expect(validateRawWhere("a = $é$;$é$").ok).toBe(true);
  });
});

describe("select / order / keyset", () => {
  it("builds ORDER BY", () => {
    expect(buildOrderBy([])).toBe("");
    expect(buildOrderBy([{ column: "a", dir: "asc" }, { column: 'b"c', dir: "desc" }])).toBe('ORDER BY "a" ASC, "b""c" DESC');
  });
  it("builds a keyset predicate for one ascending key", () => {
    expect(buildKeysetPredicate([{ column: "id", dataType: "bigint", dir: "asc" }], ["200"])).toBe(`"id" > '200'::bigint`);
  });
  it("expands multi-key and mixed-direction keysets", () => {
    const p = buildKeysetPredicate(
      [
        { column: "created", dataType: "timestamptz", dir: "desc" },
        { column: "id", dataType: "bigint", dir: "asc" },
      ],
      ["2026-01-01T00:00:00Z", "7"],
    );
    expect(p).toBe(`("created" < '2026-01-01T00:00:00Z'::timestamptz\n    OR ("created" = '2026-01-01T00:00:00Z'::timestamptz AND "id" > '7'::bigint))`);
  });
  it("rejects mismatched keyset values", () => {
    expect(() => buildKeysetPredicate([{ column: "id", dataType: "bigint", dir: "asc" }], [])).toThrow(SqlGenError);
  });
  it("builds a page query", () => {
    const sql = buildSelectPage({
      schema: "public",
      table: "orders",
      columns: ["id", "name"],
      where: `"id" > '5'::bigint`,
      orderBy: [{ column: "id", dir: "asc" }],
      limit: 201,
      offset: 400,
      after: { keys: [{ column: "id", dataType: "bigint", dir: "asc" }], values: ["9"] },
    });
    expect(sql).toBe(
      `SELECT "id", "name"\nFROM "public"."orders"\nWHERE ("id" > '5'::bigint)\n  AND "id" > '9'::bigint\nORDER BY "id" ASC\nLIMIT 201\nOFFSET 400`,
    );
  });
  it("omits WHERE / OFFSET when unused", () => {
    expect(buildSelectPage({ schema: "s", table: "t", columns: ["a"], limit: 10 })).toBe(`SELECT "a"\nFROM "s"."t"\nLIMIT 10`);
  });
  it("builds a count", () => {
    expect(buildCount("s", "t")).toBe(`SELECT count(*) FROM "s"."t"`);
    expect(buildCount("s", "t", `"a" = 1`)).toBe(`SELECT count(*) FROM "s"."t"\nWHERE "a" = 1`);
  });
});

describe("DML", () => {
  it("builds UPDATE with the original primary key", () => {
    expect(buildUpdate(target, { id: "42" }, { name: "Zoë", total: "9.99" })).toBe(
      `UPDATE "public"."orders" SET "name" = 'Zoë'::character varying, "total" = '9.99'::numeric WHERE "id" = '42'::bigint;`,
    );
  });
  it("supports composite keys", () => {
    expect(buildUpdate(target, { id: "1", name: "a" }, { active: true })).toBe(
      `UPDATE "public"."orders" SET "active" = TRUE WHERE "id" = '1'::bigint AND "name" = 'a'::character varying;`,
    );
  });
  it("sets NULL and json / bytea / array columns", () => {
    expect(buildUpdate(target, { id: "1" }, { name: null, meta: { k: "v" }, blob: { $bytes: "3q2+7w==", len: 4 }, tags: ["x", "y z"] })).toBe(
      `UPDATE "public"."orders" SET "name" = NULL, "meta" = '{"k":"v"}'::jsonb, "blob" = decode('deadbeef', 'hex'), "tags" = '{"x","y z"}'::text[] WHERE "id" = '1'::bigint;`,
    );
  });
  it("handles weird column names in SET", () => {
    expect(buildUpdate(target, { id: "1" }, { 'Weird "col"; DROP': "v" })).toBe(`UPDATE "public"."orders" SET "Weird ""col""; DROP" = 'v'::text WHERE "id" = '1'::bigint;`);
  });
  it("builds INSERT with only the filled columns, or DEFAULT VALUES", () => {
    expect(buildInsert(target, { name: "a'b", total: "1" })).toBe(`INSERT INTO "public"."orders" ("name", "total") VALUES ('a''b'::character varying, '1'::numeric);`);
    expect(buildInsert(target, {})).toBe(`INSERT INTO "public"."orders" DEFAULT VALUES;`);
  });
  it("builds DELETE", () => {
    expect(buildDelete(target, { id: "7" })).toBe(`DELETE FROM "public"."orders" WHERE "id" = '7'::bigint;`);
  });
  it("refuses empty updates, unknown columns and missing keys", () => {
    expect(() => buildUpdate(target, { id: "1" }, {})).toThrow(SqlGenError);
    expect(() => buildUpdate(target, { id: "1" }, { nope: 1 })).toThrow(/does not exist/);
    expect(() => buildDelete(target, {})).toThrow(/primary key/);
  });
  it("never lets a value escape its quotes", () => {
    const evil = `x'); DROP TABLE orders; --`;
    const sql = buildUpdate(target, { id: "1" }, { name: evil });
    expect(sql).toBe(`UPDATE "public"."orders" SET "name" = 'x''); DROP TABLE orders; --'::character varying WHERE "id" = '1'::bigint;`);
    const evil2 = `\\'); DROP TABLE orders; --`;
    expect(buildInsert(target, { name: evil2 })).toBe(`INSERT INTO "public"."orders" ("name") VALUES (E'\\\\''); DROP TABLE orders; --'::character varying);`);
  });
});

// Review finding H1: the marker for a value that was cut when it was read must never reach SQL.
// Before this rule existed, `sqlLiteral` wrote the marker object itself as the value.
describe("values that were cut when they were read", () => {
  const START = "start-of-the-value";
  const cut = (kind = "text") => ({ $truncated: { kind, bytes: 52_428_800, preview: START } });

  it("has no SQL literal, whatever the column type", () => {
    for (const type of ["text", "character varying", "bytea", "jsonb", "json", "text[]", "xml", "citext"]) {
      expect(() => sqlLiteral(cut(), type), type).toThrow(SqlGenError);
      expect(() => sqlLiteral(cut(), type), type).toThrow(/too large to load in full/);
    }
    // Also when it sits inside a JSON value (an Iceberg struct with one huge field).
    expect(() => sqlLiteral({ device: "a", payload: cut() }, "jsonb")).toThrow(/too large to load in full/);
  });

  it("is never written by UPDATE or INSERT, nor used to find a row", () => {
    expect(() => buildUpdate(target, { id: "1" }, { name: cut() })).toThrow(/too large to load in full/);
    expect(() => buildInsert(target, { id: "2", name: cut() })).toThrow(/too large to load in full/);
    expect(() => buildUpdate(target, { name: cut() }, { total: "5" })).toThrow(/too large to load in full/);
    expect(() => buildDelete(target, { name: cut() })).toThrow(/too large to load in full/);
  });

  it("cannot be a paging position", () => {
    expect(() => buildKeysetPredicate([{ column: "name", dataType: "text", dir: "asc" }], [cut()])).toThrow(/too large to load in full/);
  });

  it("has no edit text and cannot be edited or filtered on", () => {
    expect(cellToEditText(cut())).toBe("");
    expect(cellToEditText(cut("json"))).not.toContain(START);
    expect(uneditableReason(cut())).toMatch(/too large to load in full .*50\.0 MB/);
    expect(uneditableReason("ordinary")).toBeNull();
    expect(uneditableReason({ $bytes: "AA==", len: 1 })).toBeNull();
    expect(uneditableReason(null)).toBeNull();
  });
});

describe("the order of a table page", () => {
  const col = (name: string, o: Partial<PlanColumn> = {}): PlanColumn => ({ name, dataType: "integer", nullable: false, isPrimaryKey: false, ...o });
  const withPk = [col("id", { isPrimaryKey: true }), col("name", { dataType: "text", nullable: true })];
  const noPk = [col("n"), col("body", { dataType: "text", nullable: true })];
  const small = { kind: "table" as const, estimatedRows: 1200 };
  const order = (p: { orderBy: readonly { column: string; dir: string }[] }) => p.orderBy.map((k) => `${k.column} ${k.dir}`);

  it("orders by the primary key and pages by it when every ordering column is NOT NULL", () => {
    const p = planTablePage(withPk, [], small);
    expect(order(p)).toEqual(["id asc"]);
    expect(p).toMatchObject({ keyset: true, stableOrder: true, keyIdx: [0], names: ["id", "name"] });
    // A nullable sort column cannot be paged by key; the primary key still breaks ties.
    const sorted = planTablePage(withPk, [{ column: "name", dir: "desc" }, { column: "gone", dir: "asc" }], small);
    expect(order(sorted)).toEqual(["name desc", "id asc"]);
    expect(sorted).toMatchObject({ keyset: false, stableOrder: true });
  });

  // Without a primary key nothing fixed the order: `LIMIT n OFFSET m` over an unordered scan
  // can hand out the same row on two pages and never show another.
  it("breaks ties by ctid for a table without a primary key", () => {
    const p = planTablePage(noPk, [], small);
    expect(order(p)).toEqual(["tableoid asc", "ctid asc"]);
    expect(p).toMatchObject({ keyset: false, stableOrder: true });
    expect(buildSelectPage({ schema: "public", table: "log", columns: p.names, orderBy: p.orderBy, limit: 100, offset: 200 })).toBe(
      'SELECT "n", "body"\nFROM "public"."log"\nORDER BY "tableoid" ASC, "ctid" ASC\nLIMIT 100\nOFFSET 200',
    );
    // After the user's own sort, which leaves ties wherever values repeat.
    expect(order(planTablePage(noPk, [{ column: "body", dir: "desc" }], small))).toEqual(["body desc", "tableoid asc", "ctid asc"]);
    // A materialized view is stored like a table.
    expect(order(planTablePage(noPk, [], { kind: "materializedView", estimatedRows: 10 }))).toEqual(["ctid asc"]);
  });

  it("adds tableoid for a partitioned table, where ctid repeats from one partition to the next", () => {
    expect(order(planTablePage(noPk, [], { kind: "partitionedTable", estimatedRows: 1200 }))).toEqual(["tableoid asc", "ctid asc"]);
    expect(order(planTablePage(noPk, [{ column: "n", dir: "asc" }], { kind: "partitionedTable", estimatedRows: 1200 }))).toEqual(["n asc", "tableoid asc", "ctid asc"]);
  });

  it("leaves views and foreign tables alone, and says their order is not fixed", () => {
    for (const kind of ["view", "foreignTable", null] as const) {
      const p = planTablePage(noPk, [], { kind, estimatedRows: 10 });
      expect(order(p)).toEqual([]);
      expect(p.stableOrder).toBe(false);
      // The user's sort is kept as it is; it does not make the order total.
      const sorted = planTablePage(noPk, [{ column: "n", dir: "asc" }], { kind, estimatedRows: 10 });
      expect(order(sorted)).toEqual(["n asc"]);
      expect(sorted.stableOrder).toBe(false);
    }
  });

  // Ordering by ctid alone makes the server read the whole table for every page (measured on
  // Postgres 17: 94 ms against 0.04 ms for the first page of 3 million rows, growing with the
  // table). Opening a very large table must not do that, so it stays unordered and says so.
  it("does not order a large or unmeasured table by ctid alone, and says so", () => {
    for (const estimatedRows of [CTID_ORDER_MAX_ROWS + 1, 500_000_000, null, -1, 0]) {
      const p = planTablePage(noPk, [], { kind: "table", estimatedRows });
      expect(order(p)).toEqual([]);
      expect(p.stableOrder).toBe(false);
    }
    expect(planTablePage(noPk, [], { kind: "table", estimatedRows: CTID_ORDER_MAX_ROWS }).stableOrder).toBe(true);
    // With a sort the server orders the rows anyway: the tie-break rides along.
    const sorted = planTablePage(noPk, [{ column: "n", dir: "desc" }], { kind: "table", estimatedRows: 500_000_000 });
    expect(order(sorted)).toEqual(["n desc", "tableoid asc", "ctid asc"]);
    expect(sorted.stableOrder).toBe(true);
    // A primary key needs none of this, whatever the size.
    expect(order(planTablePage(withPk, [], { kind: "table", estimatedRows: 500_000_000 }))).toEqual(["id asc"]);
  });
});
