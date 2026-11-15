import { describe, expect, it } from "vitest";
import { cellKind, copyText, displayText, escapeField, formatDelimited, formatTextTable, isNumericType } from "../cellFormat";
import { defuseFormula } from "../exportFormats";
import type { ColumnMeta } from "../../../ipc/types";

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
