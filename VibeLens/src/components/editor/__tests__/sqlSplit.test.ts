import { describe, expect, it } from "vitest";
import { splitStatements, statementAtOffset } from "../sqlSplit";

describe("splitStatements", () => {
  it("splits on semicolons and keeps the terminator", () => {
    const s = splitStatements("select 1; select 2;");
    expect(s.map((x) => x.text)).toEqual(["select 1;", "select 2;"]);
  });
  it("keeps an unterminated trailing statement trimmed", () => {
    const s = splitStatements("select 1;\nselect 2   \n");
    expect(s.map((x) => x.text)).toEqual(["select 1;", "select 2"]);
  });
  it("ignores semicolons in strings, identifiers and comments", () => {
    const sql = `select 'a;b', "c;d"; -- x;y\nselect /* ; */ 2;`;
    const s = splitStatements(sql);
    expect(s).toHaveLength(2);
    expect(s[0].text).toBe(`select 'a;b', "c;d";`);
    expect(s[1].text).toBe("-- x;y\nselect /* ; */ 2;");
  });
  it("handles doubled quotes and E-strings", () => {
    expect(splitStatements(`select 'it''s;ok'; select 2`)).toHaveLength(2);
    expect(splitStatements(`select E'a\\';b'; select 2`)).toHaveLength(2);
  });
  it("respects dollar quoting including tagged quotes", () => {
    const sql = `create function f() returns int as $$ begin return 1; end $$ language plpgsql; select 1;`;
    const s = splitStatements(sql);
    expect(s).toHaveLength(2);
    expect(s[0].text.endsWith("plpgsql;")).toBe(true);
    const tagged = splitStatements(`do $body$ begin perform 1; end $body$; select 2;`);
    expect(tagged).toHaveLength(2);
  });
  it("does not treat $1 parameters as dollar quotes", () => {
    expect(splitStatements("select $1; select $2;")).toHaveLength(2);
  });
  it("supports nested block comments", () => {
    expect(splitStatements("select /* a /* ; */ ; */ 1; select 2;")).toHaveLength(2);
  });
  it("drops comment-only and empty statements", () => {
    expect(splitStatements(";;  -- nothing\n")).toHaveLength(0);
  });
});

describe("statementAtOffset", () => {
  const sql = "select 1;\n\nselect 2;\nselect 3";
  it("finds the containing statement", () => {
    expect(statementAtOffset(sql, 3)?.text).toBe("select 1;");
    expect(statementAtOffset(sql, sql.indexOf("select 3") + 2)?.text).toBe("select 3");
  });
  it("treats the position right after a semicolon as that statement", () => {
    expect(statementAtOffset(sql, 9)?.text).toBe("select 1;");
  });
  it("falls back to the previous statement in the gap", () => {
    expect(statementAtOffset(sql, 10)?.text).toBe("select 1;");
  });
  it("returns null for empty scripts", () => {
    expect(statementAtOffset("  \n", 1)).toBeNull();
  });
});
