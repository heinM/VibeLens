// Namespace tree built client-side from scanned Valkey keys, split on the profile's delimiter.
import type { KeyEntry, KeyType } from "../../ipc/types";

export interface KeyFolder {
  kind: "folder";
  id: string;
  name: string;
  /** Prefix including the trailing delimiter; usable as a SCAN pattern prefix. */
  prefix: string;
  /** Number of keys at or below this folder. */
  count: number;
  children: KeyNode[];
}
export interface KeyLeaf {
  kind: "key";
  id: string;
  name: string;
  key: string;
  keyType: KeyType | null;
}
export type KeyNode = KeyFolder | KeyLeaf;

export interface FlatKeyNode {
  node: KeyNode;
  depth: number;
}

const folderId = (prefix: string) => `f\u0001${prefix}`;
const leafId = (key: string) => `k\u0001${key}`;

/** Build the tree (folders first, then keys, each alphabetical). Duplicate keys are ignored. */
export function buildKeyTree(keys: readonly KeyEntry[], delimiter: string): KeyFolder {
  const root: KeyFolder = { kind: "folder", id: folderId(""), name: "", prefix: "", count: 0, children: [] };
  const folders = new Map<string, KeyFolder>([["", root]]);
  const seen = new Set<string>();

  for (const entry of keys) {
    if (seen.has(entry.key)) continue;
    seen.add(entry.key);
    const parts = delimiter ? entry.key.split(delimiter) : [entry.key];
    let parent = root;
    let prefix = "";
    parent.count++;
    for (let i = 0; i < parts.length - 1; i++) {
      prefix += parts[i] + delimiter;
      let f = folders.get(prefix);
      if (!f) {
        f = { kind: "folder", id: folderId(prefix), name: parts[i], prefix, count: 0, children: [] };
        folders.set(prefix, f);
        parent.children.push(f);
      }
      f.count++;
      parent = f;
    }
    const name = parts[parts.length - 1];
    parent.children.push({ kind: "key", id: leafId(entry.key), name, key: entry.key, keyType: entry.keyType });
  }

  const cmp = (a: KeyNode, b: KeyNode) => {
    if (a.kind !== b.kind) return a.kind === "folder" ? -1 : 1;
    return a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: "base" });
  };
  for (const f of folders.values()) f.children.sort(cmp);
  return root;
}

/** Depth-first flatten honouring the expanded set (root itself is not emitted). */
export function flattenKeyTree(root: KeyFolder, expanded: ReadonlySet<string>): FlatKeyNode[] {
  const out: FlatKeyNode[] = [];
  const walk = (folder: KeyFolder, depth: number) => {
    for (const child of folder.children) {
      out.push({ node: child, depth });
      if (child.kind === "folder" && expanded.has(child.id)) walk(child, depth + 1);
    }
  };
  walk(root, 0);
  return out;
}
