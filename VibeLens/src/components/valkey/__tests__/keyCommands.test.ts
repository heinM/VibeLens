import { describe, expect, it } from "vitest";
import { hashEntries, humanTtl, recreateCommands, zsetEntries } from "../keyCommands";
import type { ValkeyValue } from "../../../ipc/types";

const base = { key: "user:1", ttlMs: -1, size: 2, truncated: false } as const;
const v = (keyType: ValkeyValue["keyType"], value: unknown, ttlMs: number = -1): ValkeyValue => ({ ...base, keyType, value, ttlMs });

describe("recreateCommands", () => {
  it("string", () => expect(recreateCommands(v("string", "hello world"))).toBe('SET user:1 "hello world"'));
  it("hash with ttl", () =>
    expect(recreateCommands(v("hash", { name: "Ada", note: "a b" }, 5000))).toBe('HSET user:1 name Ada note "a b"\nPEXPIRE user:1 5000'));
  it("list and set", () => {
    expect(recreateCommands(v("list", ["a", "b"]))).toBe("RPUSH user:1 a b");
    expect(recreateCommands(v("set", ["x"]))).toBe("SADD user:1 x");
  });
  it("zset puts score before member", () => expect(recreateCommands(v("zset", [{ member: "m", score: 1.5 }]))).toBe("ZADD user:1 1.5 m"));
  it("stream", () => expect(recreateCommands(v("stream", [{ id: "1-0", fields: { a: "1" } }]))).toBe("XADD user:1 1-0 a 1"));
  it("binary strings use hex escapes", () => expect(recreateCommands(v("string", { $bytes: "AP8=", len: 2 }))).toBe('SET user:1 "\\x00\\xff"'));
  it("json module", () => expect(recreateCommands(v("json", { a: 1 }))).toBe('JSON.SET user:1 $ "{\\"a\\":1}"'));
  it("unknown types yield nothing", () => expect(recreateCommands(v("unknown", 1))).toBe(""));
});

describe("helpers", () => {
  it("humanTtl", () => {
    expect(humanTtl(-1)).toBe("No expiry");
    expect(humanTtl(-2)).toBe("Key not found");
    expect(humanTtl(4500)).toBe("4.5 s");
    expect(humanTtl(90_061_000)).toBe("1d 1h 1m");
    expect(humanTtl(3_600_000)).toBe("1h");
  });
  it("accepts array-of-pairs hashes and array zsets", () => {
    expect(hashEntries([["a", 1]])).toEqual([["a", 1]]);
    expect(zsetEntries([["m", 2]])).toEqual([{ member: "m", score: 2 }]);
  });
});
