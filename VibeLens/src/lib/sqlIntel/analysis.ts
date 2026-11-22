// Cursor-context detection: given SQL text and a cursor offset, decide what the user is typing
// (a relation name, a column after `alias.`, a join condition, …) and what is in scope there.
import {
  type Block,
  type Item,
  type Tree,
  buildTree,
  isBlock,
  isPunct,
  isQueryBlock,
  isTok,
  itemEnd,
  itemKw,
} from "./blocks";
import { CLAUSE_WORDS } from "./keywords";
import { type QueryScope, type Relation, scopeAt } from "./scope";
import { type Token, identValue, insideNonCode, isNameToken, tokenize } from "./tokenizer";

export type Clause = "select" | "from" | "join" | "where" | "on" | "group" | "order" | "having" | "limit" | "set" | "returning" | "values" | "conflict" | "other" | null;

export type Context =
  | { kind: "none" }
  | { kind: "statementStart" }
  /** After FROM/JOIN/UPDATE/INTO/TABLE…, optionally `schema.`. */
  | { kind: "relation"; qualifier: string[] | null }
  /** After `alias.` / `table.` / `schema.table.` in an expression. */
  | { kind: "qualified"; qualifier: string[] }
  | { kind: "expression"; clause: Clause }
  /** Right after `JOIN x ON` (or AND/OR within it). */
  | { kind: "on"; right: Relation | null; lefts: Relation[]; joinEligible: boolean; onText: string }
  /** Inside `INSERT INTO t (`, `REFERENCES t (`, `ON CONFLICT (`, `CREATE INDEX … ON t (`. */
  | { kind: "columnList"; table: string[] }
  | { kind: "type" }
  /** After a complete relation reference: WHERE / JOIN / ON / SET … */
  | { kind: "afterRelation"; clause: Clause };

export interface Analysis {
  ctx: Context;
  scope: QueryScope;
  tree: Tree;
  /** Text typed so far for the word being completed (includes an opening quote). */
  prefix: string;
  /** True when the word being completed starts with a double quote. */
  quoted: boolean;
  /** Offset where a completion's replacement starts. */
  replaceStart: number;
  /** Offset of the cursor. */
  insertEnd: number;
  /** End of the word under the cursor (for "replace" mode). */
  replaceEnd: number;
  /** Tokens of the statement (no comments). */
  toks: Token[];
}

// -- tokenisation cache ---------------------------------------------------------------------------

let lastSql: string | null = null;
let lastHash = false;
let lastTokens: Token[] = [];
/** `hashComments`: MySQL lexing (`#` comments, backtick identifiers). */
export function tokenizeCached(sql: string, hashComments = false): Token[] {
  if (sql !== lastSql || hashComments !== lastHash) {
    lastTokens = tokenize(sql, { mysql: hashComments });
    lastSql = sql;
    lastHash = hashComments;
  }
  return lastTokens;
}

/** Tokens (no comments) of the statement containing `offset`, plus its first-token index in the code list. */
export function statementTokens(all: Token[], offset: number): Token[] {
  const code = all.filter((t) => t.kind !== "comment");
  let a = 0;
  for (let i = 0; i < code.length; i++) {
    const t = code[i];
    if (t.start >= offset) break;
    if (t.kind === "punct" && t.text === ";" && t.end <= offset) a = i + 1;
  }
  let b = code.length;
  for (let i = a; i < code.length; i++) {
    const t = code[i];
    if (t.kind === "punct" && t.text === ";" && t.start >= offset) {
      b = i;
      break;
    }
  }
  return code.slice(a, b);
}

// -- helpers --------------------------------------------------------------------------------------

const CLAUSE_MAP: Record<string, Clause> = {
  select: "select",
  from: "from",
  join: "join",
  where: "where",
  on: "on",
  group: "group",
  order: "order",
  having: "having",
  limit: "limit",
  offset: "limit",
  set: "set",
  returning: "returning",
  values: "values",
  using: "from",
  window: "other",
  union: "other",
  intersect: "other",
  except: "other",
  fetch: "limit",
  into: "other",
  update: "other",
  delete: "other",
  insert: "other",
};

interface ClauseHit {
  word: string;
  /** Index into the scanned item list. */
  idx: number;
  items: Item[];
}

function scanClause(items: Item[], upTo: number): ClauseHit | null {
  for (let i = upTo - 1; i >= 0; i--) {
    const w = itemKw(items[i]);
    if (w && CLAUSE_WORDS.has(w)) {
      // `ON CONFLICT` is its own clause.
      if (w === "on" && itemKw(items[i + 1]) === "conflict" && i + 1 < upTo) return { word: "conflict", idx: i, items };
      // FOR UPDATE / ON UPDATE CASCADE / DO UPDATE SET are not statement-leading UPDATEs.
      if ((w === "update" || w === "delete") && ["for", "key", "do", "on", "share", "after", "before", "of", "instead"].includes(itemKw(items[i - 1]))) continue;
      // DISTINCT FROM, EXTRACT(x FROM …) handled by caller; `IS DISTINCT FROM`:
      if (w === "from" && itemKw(items[i - 1]) === "distinct") continue;
      return { word: w, idx: i, items };
    }
  }
  return null;
}

function nameChainBefore(items: Item[], endIdx: number): { parts: string[]; startIdx: number } | null {
  // items[endIdx] is the last name; walk back over `.name` pairs.
  const parts: string[] = [];
  let i = endIdx;
  for (;;) {
    const it = items[i];
    if (!isTok(it) || !isNameToken(it.t)) break;
    parts.unshift(identValue(it.t));
    if (isPunct(items[i - 1], ".") && i - 2 >= 0 && isTok(items[i - 2]) && isNameToken((items[i - 2] as { t: Token }).t)) i -= 2;
    else {
      return { parts, startIdx: i };
    }
  }
  return parts.length ? { parts, startIdx: i + 1 } : null;
}

/** `INSERT INTO t (` / `REFERENCES t (` / `ON CONFLICT (` / `CREATE INDEX … ON t (` owner table. */
function columnListOwner(_tree: Tree, chain: Block[], scope: QueryScope): string[] | null {
  const b = chain[chain.length - 1];
  if (!b.parent || isQueryBlock(b)) return null;
  const parent = b.parent;
  const j = parent.items.findIndex((it) => isBlock(it) && it.b === b);
  if (j <= 0) return null;
  const prevItem = parent.items[j - 1];
  // ON CONFLICT (…)
  if (itemKw(prevItem) === "conflict" && itemKw(parent.items[j - 2]) === "on") {
    const into = scope.relations.find((r) => r.role === "into");
    return into ? into.parts : null;
  }
  let chainInfo = nameChainBefore(parent.items, j - 1);
  if (!chainInfo) return null;
  let startIdx = chainInfo.startIdx;
  // optional `AS alias` between table and column list: INSERT INTO t AS x (…)
  if (itemKw(parent.items[startIdx - 1]) === "as" && parent.items[startIdx - 2]) {
    const c2 = nameChainBefore(parent.items, startIdx - 2);
    if (c2) {
      chainInfo = c2;
      startIdx = c2.startIdx;
    }
  }
  const lead = itemKw(parent.items[startIdx - 1]);
  const first = itemKw(parent.items[0]);
  if (lead === "into" || lead === "references") return chainInfo.parts;
  if (lead === "on" && first === "create") return chainInfo.parts;
  return null;
}

function expectsRelation(before: Item[], inQueryBlock: boolean): boolean {
  const n = before.length;
  if (n === 0) return false;
  const prev = before[n - 1];
  const w = itemKw(prev);
  const prev2 = itemKw(before[n - 2]);
  const first = itemKw(before[0]);
  switch (w) {
    case "from":
      return inQueryBlock && prev2 !== "distinct";
    case "join":
    case "references":
    case "only":
    case "truncate":
      return true;
    case "into":
      return true;
    case "update":
      return n === 1 || !["for", "key", "do", "on", "share", "after", "before", "of", "instead"].includes(prev2);
    case "table":
      return ["alter", "drop", "truncate", "lock", "comment"].includes(first) && first !== "create";
    case "exists":
      return first === "drop" && before.some((it) => itemKw(it) === "table" || itemKw(it) === "view");
    case "using":
      return first === "delete" || first === "merge";
    case "on":
      return first === "create" && before.some((it) => itemKw(it) === "index");
    default:
      return false;
  }
}

// -- the entry point ------------------------------------------------------------------------------

/** Returns null when the cursor is in a string, comment or number: nothing to complete there. */
export function analyzeAt(sql: string, offset: number, hashComments = false): Analysis | null {
  const all = tokenizeCached(sql, hashComments);
  if (insideNonCode(all, offset)) return null;
  const toks = statementTokens(all, offset);
  const tree = buildTree(toks);

  // The word being completed.
  let prefixTok: Token | null = null;
  for (const t of toks) {
    if (t.start >= offset) break;
    if ((t.kind === "word" && offset <= t.end) || (t.kind === "quoted" && (offset < t.end || !t.closed))) prefixTok = t;
  }
  const boundary = prefixTok ? prefixTok.start : offset;
  const prefix = prefixTok ? sql.slice(prefixTok.start, offset) : "";
  const quoted = prefixTok?.kind === "quoted";
  const replaceEnd = prefixTok ? Math.max(offset, prefixTok.end) : offset;

  const scope = scopeAt(tree, offset);
  const chain = scope.chain;
  // Innermost block (may be a function-call paren inside the query block).
  const innermost = (() => {
    let b = tree.root;
    for (const c of chain) b = c;
    return b;
  })();
  const before = innermost.items.filter((it) => itemEnd(tree, it) <= boundary);
  const inQuery = isQueryBlock(innermost);

  const result = (ctx: Context): Analysis => ({ ctx, scope, tree, prefix, quoted, replaceStart: boundary, insertEnd: offset, replaceEnd, toks });

  const n = before.length;
  const prev = before[n - 1];

  // Cast target: x::|
  if (isPunct(prev, "::")) return result({ kind: "type" });

  // Qualified: a.b.|
  if (isPunct(prev, ".")) {
    const info = nameChainBefore(before, n - 2);
    if (!info) return result({ kind: "none" });
    const relationExpected = expectsRelation(before.slice(0, info.startIdx), inQuery) || (isPunct(before[info.startIdx - 1], ",") && commaContinuesRelationList(before.slice(0, info.startIdx - 1), inQuery));
    return result(relationExpected ? { kind: "relation", qualifier: info.parts } : { kind: "qualified", qualifier: info.parts });
  }

  // Column list of INSERT/REFERENCES/CONFLICT/CREATE INDEX.
  const owner = columnListOwner(tree, chain, scope);
  if (owner) return result({ kind: "columnList", table: owner });

  // CREATE TABLE x ( col <type> …
  if (!inQuery && itemKw(innermost.parent?.items[0]) === "create" && innermost.parent?.items.some((it) => itemKw(it) === "table")) {
    const pn = isTok(prev) && isNameToken(prev.t);
    const startOfDef = n === 1 || isPunct(before[n - 2], ",");
    return result(pn && startOfDef ? { kind: "type" } : { kind: "none" });
  }
  if (isTok(prev) && prev.t.text === ";") return result({ kind: "statementStart" });

  if (expectsRelation(before, inQuery)) return result({ kind: "relation", qualifier: null });
  if (isPunct(prev, ",") && commaContinuesRelationList(before.slice(0, n - 1), inQuery)) return result({ kind: "relation", qualifier: null });

  // Which clause are we in? Function-call parens defer to the enclosing level.
  let hit: ClauseHit | null = scanClause(before, n);
  if (!hit) {
    for (let level = chain.length - 1; level > 0 && !hit && !isQueryBlock(chain[level]); level--) {
      const blk = chain[level];
      const parent = blk.parent;
      if (!parent) break;
      const at = parent.items.findIndex((it) => isBlock(it) && it.b === blk);
      hit = scanClause(parent.items, at);
    }
  }

  if (!hit) {
    if (n === 0) return result({ kind: "statementStart" });
    if (["explain", "analyze", "verbose"].includes(itemKw(before[0])) && n <= 4) return result({ kind: "statementStart" });
    return result({ kind: "expression", clause: null });
  }

  const clause = CLAUSE_MAP[hit.word] ?? "other";
  const hitItems = hit.items;
  const atSameLevel = hitItems === before || hitItems === innermost.items;

  // After a complete relation reference.
  if ((hit.word === "from" || hit.word === "join" || hit.word === "update" || hit.word === "using" || hit.word === "into") && atSameLevel) {
    const prevIsName = isTok(prev) && isNameToken(prev.t) && !CLAUSE_WORDS.has(itemKw(prev));
    const prevIsBlock = isBlock(prev);
    const prevIsKwModifier = ["left", "right", "inner", "full", "cross", "natural", "outer"].includes(itemKw(prev));
    if ((prevIsName || prevIsBlock || prevIsKwModifier) && !isPunct(prev, ",")) {
      return result({ kind: "afterRelation", clause: hit.word === "into" ? ("values" as Clause) : clause });
    }
  }

  if (hit.word === "on" && atSameLevel) {
    // The relation introduced by the JOIN this ON belongs to is the last one that starts before it.
    const onItem = hit.items[hit.idx];
    const onStart = isTok(onItem) ? onItem.t.start : 0;
    const rels = scope.relations.filter((r) => r.nameStart < onStart);
    const right = rels.length ? rels.reduce((a, b) => (b.nameStart > a.nameStart ? b : a)) : null;
    const lefts = right ? rels.filter((r) => r !== right && r.nameStart < right.nameStart) : [];
    const pw = itemKw(prev);
    const joinEligible = right !== null && right.role === "join" && (hit.idx === n - 1 || pw === "and" || pw === "or");
    const onEnd = isTok(onItem) ? onItem.t.end : boundary;
    return result({ kind: "on", right, lefts, joinEligible, onText: sql.slice(onEnd, boundary) });
  }

  return result({ kind: "expression", clause });
}

function commaContinuesRelationList(beforeComma: Item[], inQuery: boolean): boolean {
  const hit = scanClause(beforeComma, beforeComma.length);
  if (hit && (hit.word === "from" || hit.word === "using") && inQuery) return true;
  // DROP TABLE a, | / TRUNCATE a, |
  const first = itemKw(beforeComma[0]);
  if ((first === "drop" || first === "truncate" || first === "lock") && !hit) return true;
  return false;
}
