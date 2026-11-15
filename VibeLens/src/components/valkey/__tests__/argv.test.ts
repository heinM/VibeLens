import { describe, expect, it } from "vitest";
import { formatArgv, parseArgv } from "../argv";

const ok = (line: string) => {
  const r = parseArgv(line);
  if (!r.ok) throw new Error(r.error);
  return r.argv;
};

describe("parseArgv", () => {
  it("splits on whitespace", () => expect(ok("  SET  a   b ")).toEqual(["SET", "a", "b"]));
  it("handles double quotes with escapes", () => {
    expect(ok(`SET k "hello world"`)).toEqual(["SET", "k", "hello world"]);
    expect(ok(`SET k "a\\nb\\t\\"q\\" \\x41"`)).toEqual(["SET", "k", 'a\nb\t"q" A']);
  });
  it("handles single quotes", () => {
    expect(ok(`SET k 'it\\'s "fine"'`)).toEqual(["SET", "k", `it's "fine"`]);
    expect(ok(`SET k 'a\\nb'`)).toEqual(["SET", "k", "a\\nb"]);
  });
  it("supports an empty quoted argument", () => expect(ok(`SET k ""`)).toEqual(["SET", "k", ""]));
  it("escapes with backslash outside quotes", () => expect(ok("GET a\\ b")).toEqual(["GET", "a b"]));
  it("rejects unbalanced quotes", () => {
    expect(parseArgv(`SET k "oops`).ok).toBe(false);
    expect(parseArgv(`SET k 'oops`).ok).toBe(false);
  });
  it("rejects a closing quote glued to the next token", () => expect(parseArgv(`SET "a"b`).ok).toBe(false));
  it("returns [] for blank input", () => expect(ok("   ")).toEqual([]));
});

describe("formatArgv", () => {
  it("round-trips awkward values", () => {
    const argv = ["SET", "user:1", 'va"lue with space\nline', "", "back\\slash"];
    expect(ok(formatArgv(argv))).toEqual(argv);
  });
  it("leaves simple tokens bare", () => expect(formatArgv(["GET", "user:1"])).toBe("GET user:1"));
});
