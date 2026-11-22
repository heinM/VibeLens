// The editor splitter in MySQL mode decides what "run statement at cursor" sends. It must read the
// same boundaries as the server (and the Rust splitter): # comments, backticks, backslash escapes,
// non-nesting block comments, and whole BEGIN … END bodies of stored routines.
import { describe, expect, it } from "vitest";
import { splitStatements, statementAtOffset } from "../sqlSplit";

const texts = (sql: string) => splitStatements(sql, { mysql: true }).map((s) => s.text);

describe("splitStatements in MySQL mode", () => {
  it("ignores ; inside backticks, # comments and backslash-escaped strings", () => {
    expect(texts("SELECT `a;b`; SELECT 2")).toEqual(["SELECT `a;b`;", "SELECT 2"]);
    expect(texts("SELECT 1 # ; not a split\n; SELECT 2")).toEqual(["SELECT 1 # ; not a split\n;", "SELECT 2"]);
    expect(texts("SELECT 'it\\'s; fine'; SELECT \"x\\\";y\"")).toEqual(["SELECT 'it\\'s; fine';", 'SELECT "x\\";y"']);
  });
  it("does not nest block comments, as MySQL does not", () => {
    expect(texts("SELECT /* a /* b */ 1; SELECT 2")).toEqual(["SELECT /* a /* b */ 1;", "SELECT 2"]);
  });
  it("keeps a stored routine's BEGIN … END body in one statement", () => {
    const sql = "CREATE PROCEDURE p()\nBEGIN\n  DECLARE x INT;\n  IF x > 0 THEN SET x = 1; END IF;\n  SET x = CASE WHEN x THEN 1 ELSE 2 END;\nEND;\nSELECT 1;";
    const parts = texts(sql);
    expect(parts).toHaveLength(2);
    expect(parts[0].startsWith("CREATE PROCEDURE")).toBe(true);
    expect(parts[0].endsWith("END;")).toBe(true);
    expect(parts[1]).toBe("SELECT 1;");
  });
  it("an ordinary BEGIN is a transaction, not a block", () => {
    expect(texts("BEGIN; SELECT 1; COMMIT;")).toHaveLength(3);
  });
  it("leaves the Postgres reading unchanged without the option", () => {
    expect(splitStatements("SELECT $$a;b$$; SELECT 2").map((s) => s.text)).toEqual(["SELECT $$a;b$$;", "SELECT 2"]);
    expect(texts("SELECT $$a;b$$; SELECT 2")).toHaveLength(3);
  });
  it("finds the statement under the cursor inside a routine body", () => {
    const sql = "SELECT 0;\nCREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW BEGIN SET NEW.a = 1; SET NEW.b = 2; END;";
    const s = statementAtOffset(sql, sql.indexOf("NEW.b"), { mysql: true });
    expect(s?.text.startsWith("CREATE TRIGGER")).toBe(true);
  });
});
