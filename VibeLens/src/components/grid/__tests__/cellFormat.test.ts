import { describe, expect, it } from "vitest";
import {
  cellKind,
  containsTruncated,
  copyText,
  countTruncated,
  displayText,
  escapeField,
  formatDelimited,
  formatTextTable,
  isNumericType,
  isTruncatedCell,
  rowsToObjects,
  truncatedNote,
  withTruncatedNote,
} from "../cellFormat";
import { defuseFormula } from "../exportFormats";
import type { ColumnMeta, TruncatedCell } from "../../../ipc/types";

const cols: ColumnMeta[] = [
  { name: "id", typeName: "int8", typeOid: 20 },
  { name: "note", typeName: "text", typeOid: 25 },
];

describe("cellFormat", () => {
  it("classifies cells", () => {
    expect(cellKind(null)).toBe("null");
    expect(cellKind({ $bytes: "AA==", len: 1 })).toBe("bytes");
    expect(cellKind({ a: 1 })).toBe("json");
    expect(cellKind([1])).toBe("json");
  });
  it("renders bytes and null", () => {
    expect(displayText({ $bytes: "AA==", len: 12 })).toBe("‹bytes 12›");
    expect(displayText(null)).toBe("NULL");
    expect(copyText(null)).toBe("");
  });
  it("detects numeric types", () => {
    expect(isNumericType("int8")).toBe(true);
    expect(isNumericType("numeric")).toBe(true);
    expect(isNumericType("text")).toBe(false);
  });
  it("escapes delimited fields", () => {
    expect(escapeField("a,b", ",")).toBe('"a,b"');
    expect(escapeField('say "hi"', ",")).toBe('"say ""hi"""');
    expect(escapeField("a,b", "\t")).toBe("a,b");
    expect(escapeField("a\tb", "\t")).toBe('"a\tb"');
  });
  it("formats a range as CSV with header", () => {
    const rows = [
      ["1", "x,y"],
      ["2", null],
    ];
    expect(formatDelimited(cols, rows, { r0: 0, r1: 1, c0: 0, c1: 1 }, ",", true)).toBe('id,note\n1,"x,y"\n2,');
  });
  it("defuses formula-looking text in clipboard CSV/TSV only when asked", () => {
    const rows = [
      ["1", "=HYPERLINK(\"http://x\",\"y\")"],
      ["-5", "@SUM(A1)"],
      [7, "+1+1"],
      ["2", "-3.5"],
      ["3", "\tcmd"],
    ];
    const all = { r0: 0, r1: 4, c0: 0, c1: 1 };
    const raw = formatDelimited(cols, rows, all, "\t", true);
    expect(raw).toContain("=HYPERLINK");
    const safe = formatDelimited(cols, rows, all, "\t", true, defuseFormula);
    const lines = safe.split("\n");
    expect(lines[1]).toBe('1\t"\'=HYPERLINK(""http://x"",""y"")"');
    expect(lines[2]).toBe("-5\t'@SUM(A1)"); // the id column holds a plain number string: untouched
    expect(lines[3]).toBe("7\t'+1+1");
    expect(lines[4]).toBe("2\t-3.5"); // a plain number is not a formula
    expect(safe).toContain("\t\"'\tcmd\"");
    const csv = formatDelimited(cols, rows, all, ",", true, defuseFormula);
    expect(csv).toContain("1,\"'=HYPERLINK");
  });
  it("defuses header names that look like formulas", () => {
    const evil: ColumnMeta[] = [{ name: "=cmd()", typeName: "text", typeOid: 25 }];
    expect(formatDelimited(evil, [["x"]], { r0: 0, r1: 0, c0: 0, c1: 0 }, ",", true, defuseFormula)).toBe("'=cmd()\nx");
  });
  it("formats an aligned text table", () => {
    const out = formatTextTable(cols, [["1", "a"], ["22", null]], 10);
    expect(out.split("\n")[0]).toBe("id │ note");
    expect(out).toContain(" 1 │ a");
    expect(out).toContain("22 │ NULL");
  });
});

// Review finding H1: a value over 1 MiB arrives as a marker holding only its start. The marker is
// not the value, so nothing here may pass the start off as the whole.
describe("values that were cut when they were read", () => {
  const SECRET_TAIL = "start-of-the-value";
  const cut = (kind: TruncatedCell["$truncated"]["kind"], bytes: number, preview = SECRET_TAIL): TruncatedCell => ({ $truncated: { kind, bytes, preview } });
  const text = cut("text", 52_428_800);

  it("recognises the marker, and only the marker", () => {
    expect(isTruncatedCell(text)).toBe(true);
    expect(isTruncatedCell(cut("bytes", 9, "3q2+"))).toBe(true);
    for (const v of [null, "x", 1, [text], { $bytes: "AA==", len: 1 }, { $truncated: true }, { $truncated: { kind: "text" } }, { ...text, other: 1 }]) {
      expect(isTruncatedCell(v)).toBe(false);
    }
  });

  it("is a kind of its own, not text, bytes or json", () => {
    expect(cellKind(text)).toBe("truncated");
    expect(cellKind(cut("bytes", 9, "3q2+"))).toBe("truncated");
    expect(cellKind(cut("json", 9, '{"a": [1'))).toBe("truncated");
  });

  it("shows the start in the grid behind a plain label", () => {
    const shown = displayText(text);
    expect(shown).toMatch(/^‹cut: 50\.0 MB of text›/);
    expect(shown).toContain(SECRET_TAIL);
    // Binary and unreadable values have no start worth showing.
    expect(displayText(cut("bytes", 3 * 1024 * 1024, "3q2+"))).toBe("‹cut: 3.0 MB of binary data›");
    expect(displayText(cut("other", 2_000_000, ""))).toBe("‹cut: 1.9 MB value›");
  });

  it("copies a note that says the value is cut, never the partial value", () => {
    const note = copyText(text);
    expect(note).toBe(truncatedNote(text));
    expect(note).toBe("‹value not loaded in full: 52,428,800 bytes of text›");
    expect(note).not.toContain(SECRET_TAIL);
    expect(copyText(cut("json", 2_000_000, '{"a": [1'))).not.toContain('{"a"');
  });

  it("puts the note, not the start, into delimited copies and the text view", () => {
    const cols2: ColumnMeta[] = [
      { name: "id", typeName: "int8", typeOid: 20 },
      { name: "body", typeName: "text", typeOid: 25 },
    ];
    const rows = [["1", text]];
    const tsv = formatDelimited(cols2, rows, { r0: 0, r1: 0, c0: 0, c1: 1 }, "\t", false);
    expect(tsv).toBe(`1\t${truncatedNote(text)}`);
    expect(formatTextTable(cols2, rows, 10)).not.toContain(SECRET_TAIL);
    expect(rowsToObjects(cols2, rows, 10)).toEqual([{ id: "1", body: truncatedNote(text) }]);
  });

  it("counts cut values, also inside nested JSON, so a copy can say so", () => {
    const nested = { device: "a", payload: text };
    const rows = [
      ["1", text, null],
      ["2", "ok", nested],
      ["3", "ok", { fine: true }],
    ];
    expect(containsTruncated(nested)).toBe(true);
    expect(containsTruncated({ fine: [1, { deeper: "x" }] })).toBe(false);
    expect(countTruncated(rows)).toBe(2);
    expect(countTruncated(rows, { r0: 1, r1: 2, c0: 0, c1: 1 })).toBe(0);
    expect(countTruncated(rows, { r0: 0, r1: 0, c0: 1, c1: 1 })).toBe(1);
    expect(withTruncatedNote("Copied 3 rows", 0)).toBe("Copied 3 rows");
    expect(withTruncatedNote("Copied 3 rows", 1)).toBe("Copied 3 rows. 1 value was too large to load and is a placeholder, not data");
    expect(withTruncatedNote("Copied 3 rows", 2)).toBe("Copied 3 rows. 2 values were too large to load and are placeholders, not data");
  });
});
