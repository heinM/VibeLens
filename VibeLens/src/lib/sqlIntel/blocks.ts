// Parenthesis tree over a token list. Scope analysis works on "items" (a token, or a whole nested
// parenthesised block) so that clause keywords inside subqueries and function arguments never leak
// into the enclosing query's analysis.
import type { Token } from "./tokenizer";
import { kw } from "./tokenizer";

export type Item = { t: Token; i: number } | { b: Block };

export interface Block {
  /** Index of the `(` token in the token list, -1 for the root. */
  open: number;
  /** Index of the matching `)`; for an unclosed block, the token count. */
  close: number;
  closed: boolean;
  parent: Block | null;
  items: Item[];
}

export interface Tree {
  root: Block;
  toks: Token[];
}

export function buildTree(toks: Token[]): Tree {
  const root: Block = { open: -1, close: toks.length, closed: false, parent: null, items: [] };
  let cur = root;
  for (let i = 0; i < toks.length; i++) {
    const t = toks[i];
    if (t.kind === "punct" && t.text === "(") {
      const b: Block = { open: i, close: toks.length, closed: false, parent: cur, items: [] };
      cur.items.push({ b });
      cur = b;
    } else if (t.kind === "punct" && t.text === ")" && cur.parent) {
      cur.close = i;
      cur.closed = true;
      cur = cur.parent;
    } else cur.items.push({ t, i });
  }
  return { root, toks };
}

export const isTok = (it: Item | undefined): it is { t: Token; i: number } => !!it && "t" in it;
export const isBlock = (it: Item | undefined): it is { b: Block } => !!it && "b" in it;

/** Lower-cased keyword of an item ("" for blocks and non-words). */
export const itemKw = (it: Item | undefined): string => (isTok(it) ? kw(it.t) : "");
export const isPunct = (it: Item | undefined, ch: string): boolean => isTok(it) && it.t.kind === "punct" && it.t.text === ch;

export function blockStart(tree: Tree, b: Block): number {
  return b.open < 0 ? 0 : tree.toks[b.open].start;
}
export function blockEnd(tree: Tree, b: Block): number {
  if (b.closed) return tree.toks[b.close].end;
  return tree.toks.length ? tree.toks[tree.toks.length - 1].end : 0;
}
export const itemStart = (tree: Tree, it: Item): number => (isTok(it) ? it.t.start : blockStart(tree, it.b));
export const itemEnd = (tree: Tree, it: Item): number => (isTok(it) ? it.t.end : blockEnd(tree, it.b));

/** `(SELECT …)`, `(WITH …)`, `(VALUES …)`, `(TABLE x)`: a nested query rather than a list or call. */
export function isQueryBlock(b: Block): boolean {
  if (b.parent === null) return true;
  const k = itemKw(b.items[0]);
  return k === "select" || k === "with" || k === "values" || k === "table";
}

/** Blocks from the root down to the innermost block that contains `offset` (cursor semantics). */
export function blockChainAt(tree: Tree, offset: number): Block[] {
  const chain: Block[] = [tree.root];
  let cur = tree.root;
  for (;;) {
    let next: Block | null = null;
    for (const it of cur.items) {
      if (!isBlock(it)) continue;
      const b = it.b;
      const open = tree.toks[b.open];
      if (offset >= open.end && (!b.closed || offset <= tree.toks[b.close].start)) {
        next = b;
        break;
      }
    }
    if (!next) return chain;
    chain.push(next);
    cur = next;
  }
}
