import { describe, expect, it } from "vitest";
import { buildKeyTree, flattenKeyTree } from "../keyTree";
import type { KeyEntry } from "../../../ipc/types";

const k = (key: string): KeyEntry => ({ key, keyType: null });

describe("buildKeyTree", () => {
  const tree = buildKeyTree([k("user:1"), k("user:2"), k("user:profile:9"), k("config"), k("a"), k("user:1")], ":");

  it("counts unique keys per folder", () => {
    expect(tree.count).toBe(5);
    const user = tree.children.find((c) => c.kind === "folder" && c.name === "user");
    expect(user?.kind === "folder" && user.count).toBe(3);
  });
  it("sorts folders before keys, then naturally", () => {
    expect(tree.children.map((c) => c.name)).toEqual(["user", "a", "config"]);
  });
  it("flattens only expanded folders", () => {
    expect(flattenKeyTree(tree, new Set()).map((n) => n.node.name)).toEqual(["user", "a", "config"]);
    const user = tree.children[0];
    const flat = flattenKeyTree(tree, new Set([user.id]));
    expect(flat.map((n) => `${n.depth}:${n.node.name}`)).toEqual(["0:user", "1:profile", "1:1", "1:2", "0:a", "0:config"]);
  });
  it("a key can coexist with a same-named folder", () => {
    const t = buildKeyTree([k("a"), k("a:b")], ":");
    expect(t.children).toHaveLength(2);
  });
  it("treats an empty delimiter as flat", () => {
    const t = buildKeyTree([k("a:b")], "");
    expect(t.children[0].kind).toBe("key");
  });
  it("keeps keys with empty trailing segments", () => {
    const t = buildKeyTree([k("a:")], ":");
    const f = t.children[0];
    expect(f.kind === "folder" && f.children[0].name).toBe("");
  });
});
