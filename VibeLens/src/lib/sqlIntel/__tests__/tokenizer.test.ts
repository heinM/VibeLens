import { describe, expect, it } from "vitest";
import { identValue, insideNonCode, tokenize } from "../tokenizer";

const kinds = (sql: string) => tokenize(sql).map((t) => `${t.kind}:${t.text}`);

describe("tokenize", () => {
  it("splits words, numbers and punctuation", () => {
    expect(kinds("select a.b, 1.5 from t;")).toEqual(["word:select", "word:a", "punct:.", "word:b", "punct:,", "number:1.5", "word:from", "word:t", "punct:;"]);
  });
  it("treats :: as one token", () => {
    expect(kinds("x::int")).toEqual(["word:x", "punct:::", "word:int"]);
  });
  it("handles strings with doubled quotes and E-strings", () => {
    expect(kinds("select 'it''s', E'a\\'b'")).toEqual(["word:select", "string:'it''s'", "punct:,", "string:E'a\\'b'"]);
  });
  it("handles quoted identifiers with escaped quotes", () => {
    const [t] = tokenize('"we""ird"');
    expect(t.kind).toBe("quoted");
    expect(identValue(t)).toBe('we"ird');
  });
  it("handles dollar quoting and $n parameters", () => {
    expect(kinds("select $1, $$a;b$$, $t$x$t$")).toEqual(["word:select", "param:$1", "punct:,", "string:$$a;b$$", "punct:,", "string:$t$x$t$"]);
  });
  it("keeps comments as tokens, including nested block comments", () => {
    expect(kinds("a -- hi\n/* x /* y */ z */ b")).toEqual(["word:a", "comment:-- hi", "comment:/* x /* y */ z */", "word:b"]);
  });
  it("flags unterminated constructs", () => {
    const [s] = tokenize("'abc");
    expect(s.closed).toBe(false);
    const [q] = tokenize('"abc');
    expect(q.closed).toBe(false);
    const [c] = tokenize("/* abc");
    expect(c.closed).toBe(false);
  });
  it("never loops on odd input", () => {
    expect(tokenize("\u0000 ¤ § ` ~").length).toBeGreaterThan(0);
  });
});

describe("insideNonCode", () => {
  const at = (src: string) => {
    const i = src.indexOf("|");
    const sql = src.replace("|", "");
    return insideNonCode(tokenize(sql), i)?.kind ?? null;
  };
  it("detects strings and comments", () => {
    expect(at("select 'ab|c'")).toBe("string");
    expect(at("select 'abc'|")).toBeNull();
    expect(at("select 'abc|")).toBe("string");
    expect(at("select 1 -- hi|")).toBe("comment");
    expect(at("select /* a| */ 1")).toBe("comment");
    expect(at("select /* a */| 1")).toBeNull();
  });
  it("is quiet in code", () => {
    expect(at("select o.|")).toBeNull();
    expect(at('select "ab|c"')).toBeNull();
  });
});
