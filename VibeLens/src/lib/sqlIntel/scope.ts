// Scope analysis: which relations (tables, CTEs, derived tables, set-returning functions) a query
// block can see, how they are aliased, and what columns CTEs and subqueries expose.
import {
  type Block,
  type Item,
  type Tree,
  blockChainAt,
  blockEnd,
  blockStart,
  isBlock,
  isPunct,
  isQueryBlock,
  isTok,
  itemEnd,
  itemKw,
  itemStart,
} from "./blocks";
import { ALIAS_STOP_WORDS, SET_OPERATORS } from "./keywords";
import { type Token, identValue, isNameToken } from "./tokenizer";

/** The identifier an item denotes when it is a name token, else null. */
const nameOf = (it: Item | undefined): string | null => (isTok(it) && isNameToken(it.t) ? identValue(it.t) : null);

export type RelationRole = "from" | "join" | "update" | "into" | "using" | "delete";

export interface Relation {
  kind: "table" | "cte" | "derived" | "function";
  /** Name as written, unquoted: ["shop", "orders"]. For functions, the function name. */
  parts: string[];
  alias: string | null;
  /** Column aliases from `AS x(a, b)`. */
  aliasColumns: string[] | null;
  /** For `cte` and `derived`: the query block that defines the columns. */
  body: Block | null;
  role: RelationRole;
  /** Order of appearance within the arm. */
  order: number;
  /** Item index (within the arm) of the first name token. */
  itemIdx: number;
  nameStart: number;
  nameEnd: number;
  aliasStart: number;
  aliasEnd: number;
  /** Column list that followed the name (`INSERT INTO t (a, b)`), if any. */
  columnListBlock: Block | null;
}

export interface CteDef {
  name: string;
  columns: string[] | null;
  body: Block;
  nameStart: number;
  nameEnd: number;
}

/** Name this relation is referred to by in column qualifiers. */
export const exposedName = (r: Relation): string => r.alias ?? r.parts[r.parts.length - 1] ?? "";

// -- WITH header ----------------------------------------------------------------------------------

export interface WithHeader {
  ctes: CteDef[];
  /** Items after the header (the main statement). */
  rest: Item[];
  /** Item index in the original list where `rest` starts. */
  restFrom: number;
}

function namesInBlock(b: Block): string[] {
  return b.items.filter(isTok).filter((x) => isNameToken(x.t)).map((x) => identValue(x.t));
}

export function parseWithHeader(_tree: Tree, items: Item[]): WithHeader {
  if (itemKw(items[0]) !== "with") return { ctes: [], rest: items, restFrom: 0 };
  const ctes: CteDef[] = [];
  let k = 1;
  if (itemKw(items[k]) === "recursive") k++;
  for (;;) {
    const nameItem = items[k];
    if (!isTok(nameItem) || !isNameToken(nameItem.t)) break;
    k++;
    let columns: string[] | null = null;
    const colItem = items[k];
    if (isBlock(colItem) && itemKw(items[k + 1]) === "as") {
      columns = namesInBlock(colItem.b);
      k++;
    }
    if (itemKw(items[k]) !== "as") break;
    k++;
    if (itemKw(items[k]) === "not") k++;
    if (itemKw(items[k]) === "materialized") k++;
    const bodyItem = items[k];
    if (!isBlock(bodyItem)) break;
    k++;
    ctes.push({ name: identValue(nameItem.t), columns, body: bodyItem.b, nameStart: nameItem.t.start, nameEnd: nameItem.t.end });
    if (isPunct(items[k], ",")) k++;
    else break;
  }
  return { ctes, rest: items.slice(k), restFrom: k };
}

// -- Arms (UNION/INTERSECT/EXCEPT) ----------------------------------------------------------------

export interface Arm {
  items: Item[];
  /** Offset from which this arm owns the cursor (end of the preceding set operator, or block start). */
  from: number;
}

export function splitArms(_tree: Tree, items: Item[], blockFrom: number): Arm[] {
  const arms: Arm[] = [];
  let cur: Item[] = [];
  let from = blockFrom;
  for (const it of items) {
    if (isTok(it) && SET_OPERATORS.has(itemKw(it))) {
      arms.push({ items: cur, from });
      cur = [];
      from = it.t.end;
      continue;
    }
    cur.push(it);
  }
  arms.push({ items: cur, from });
  return arms;
}

// -- Relations ------------------------------------------------------------------------------------

function parseRelationAt(tree: Tree, items: Item[], j: number, role: RelationRole, order: number): { rel: Relation; next: number } | null {
  let k = j;
  while (itemKw(items[k]) === "only" || itemKw(items[k]) === "lateral") k++;
  const first = items[k];
  if (!first) return null;
  const rel: Relation = {
    kind: "table",
    parts: [],
    alias: null,
    aliasColumns: null,
    body: null,
    role,
    order,
    itemIdx: k,
    nameStart: itemStart(tree, first),
    nameEnd: itemEnd(tree, first),
    aliasStart: -1,
    aliasEnd: -1,
    columnListBlock: null,
  };
  if (isBlock(first)) {
    rel.kind = "derived";
    rel.body = first.b;
    k++;
  } else if (isTok(first) && isNameToken(first.t)) {
    rel.parts.push(identValue(first.t));
    k++;
    while (isPunct(items[k], ".") && nameOf(items[k + 1]) !== null) {
      const part = items[k + 1] as { t: Token; i: number };
      rel.parts.push(identValue(part.t));
      rel.nameEnd = part.t.end;
      k += 2;
    }
    const after = items[k];
    if (isBlock(after) && !isQueryBlock(after.b)) {
      if (role === "into") {
        rel.columnListBlock = after.b;
      } else {
        rel.kind = "function";
        rel.nameEnd = itemEnd(tree, after);
      }
      k++;
    }
  } else return null;

  // Optional alias: AS x | x, followed by an optional column alias list.
  let sawAs = false;
  if (itemKw(items[k]) === "as") {
    sawAs = true;
    k++;
  }
  const a = items[k];
  if (isTok(a) && isNameToken(a.t) && (sawAs || a.t.kind === "quoted" || !ALIAS_STOP_WORDS.has(itemKw(a)))) {
    rel.alias = identValue(a.t);
    rel.aliasStart = a.t.start;
    rel.aliasEnd = a.t.end;
    k++;
    const cols = items[k];
    if (isBlock(cols) && !isQueryBlock(cols.b)) {
      rel.aliasColumns = namesInBlock(cols.b);
      k++;
    }
  }
  return { rel, next: k };
}

/** Relations introduced by one SELECT/UPDATE/DELETE/INSERT arm, in order of appearance. */
export function collectRelations(tree: Tree, arm: Item[]): Relation[] {
  const out: Relation[] = [];
  const first = itemKw(arm[0]);
  const writeStmt = first === "insert" || first === "merge";
  let k = 0;
  while (k < arm.length) {
    const it = arm[k];
    const w = itemKw(it);
    let role: RelationRole | null = null;
    if (w === "from") role = first === "delete" ? "delete" : "from";
    else if (w === "join") role = "join";
    else if (w === "update" && k === 0) role = "update";
    else if (w === "into" && writeStmt) role = "into";
    else if (w === "using" && (first === "delete" || first === "merge") && !isBlock(arm[k + 1])) role = "using";
    if (!role) {
      k++;
      continue;
    }
    let next = k + 1;
    let parsed = parseRelationAt(tree, arm, next, role, out.length);
    while (parsed) {
      out.push(parsed.rel);
      next = parsed.next;
      // `FROM a, b, c` — a comma straight after a relation continues the list.
      if ((role === "from" || role === "delete" || role === "using") && isPunct(arm[next], ",")) {
        parsed = parseRelationAt(tree, arm, next + 1, role === "delete" ? "from" : role, out.length);
        continue;
      }
      break;
    }
    k = Math.max(next, k + 1);
  }
  return out;
}

// -- Output columns of a query block ----------------------------------------------------------------

export type OutputColumn = { kind: "name"; name: string } | { kind: "star"; qualifier: string | null };

function trailingCastStrip(seg: Item[]): Item[] {
  for (let i = seg.length - 1; i >= 0; i--) {
    if (isPunct(seg[i], "::")) return seg.slice(0, i);
  }
  return seg;
}

function segmentName(seg0: Item[]): OutputColumn | null {
  if (seg0.length === 0) return null;
  // Explicit alias: … AS name
  const n = seg0.length;
  if (n >= 2 && itemKw(seg0[n - 2]) === "as" && nameOf(seg0[n - 1]) !== null) {
    return { kind: "name", name: nameOf(seg0[n - 1]) as string };
  }
  const last = seg0[n - 1];
  // `*` and `t.*`
  if (isPunct(last, "*")) {
    if (n >= 3 && isPunct(seg0[n - 2], ".") && nameOf(seg0[n - 3]) !== null) {
      return { kind: "star", qualifier: nameOf(seg0[n - 3]) };
    }
    if (n === 1) return { kind: "star", qualifier: null };
  }
  // Implicit alias: `expr name` where the last token is a name and the one before is not an operator/dot.
  if (n >= 2 && isTok(last) && isNameToken(last.t) && !ALIAS_STOP_WORDS.has(itemKw(last)) && itemKw(last) !== "end") {
    const before = seg0[n - 2];
    const beforeIsOperand = isBlock(before) || (isTok(before) && (isNameToken(before.t) || before.t.kind === "number" || before.t.kind === "string" || before.t.text === ")"));
    const beforeIsDot = isPunct(before, ".");
    const beforeIsKeywordOp = ["not", "and", "or", "is", "in", "like", "ilike", "between", "when", "then", "else", "case", "distinct", "interval", "date", "timestamp"].includes(itemKw(before));
    if (beforeIsOperand && !beforeIsDot && !beforeIsKeywordOp) return { kind: "name", name: identValue(last.t) };
  }
  const seg = trailingCastStrip(seg0);
  const l = seg[seg.length - 1];
  if (!l) return null;
  if (isTok(l) && isNameToken(l.t)) {
    if (itemKw(l) === "end") return { kind: "name", name: "case" };
    return { kind: "name", name: identValue(l.t) };
  }
  // Function call: name(args) → column is named after the function.
  if (isBlock(l) && seg.length >= 2 && nameOf(seg[seg.length - 2]) !== null) {
    return { kind: "name", name: nameOf(seg[seg.length - 2]) as string };
  }
  return null;
}

/** The output column list of a query block (first arm for UNIONs). `RETURNING` lists count too. */
export function outputColumns(tree: Tree, body: Block): OutputColumn[] {
  const header = parseWithHeader(tree, body.items);
  const arms = splitArms(tree, header.rest, 0);
  const arm = arms[0]?.items ?? [];
  let k = arm.findIndex((it) => itemKw(it) === "select" || itemKw(it) === "returning");
  if (k < 0) return [];
  k++;
  if (itemKw(arm[k]) === "distinct") {
    k++;
    if (itemKw(arm[k]) === "on") k += 2;
  } else if (itemKw(arm[k]) === "all") k++;
  const out: OutputColumn[] = [];
  let seg: Item[] = [];
  const flush = () => {
    const c = segmentName(seg);
    if (c) out.push(c);
    seg = [];
  };
  for (; k < arm.length; k++) {
    const it = arm[k];
    const w = itemKw(it);
    if (w === "from" || w === "into" || (w === "returning" && itemKw(arm[0]) !== "returning")) break;
    if (isPunct(it, ",")) flush();
    else seg.push(it);
  }
  flush();
  return out;
}

// -- Scope at a cursor ----------------------------------------------------------------------------

export interface QueryScope {
  /** The query block the cursor is directly in (root for top-level statements). */
  block: Block;
  /** Blocks from the root to the innermost block containing the cursor. */
  chain: Block[];
  /** Relations visible in the cursor's own SELECT/UPDATE/… (resolved: table refs naming a CTE become `cte`). */
  relations: Relation[];
  /** Relations of enclosing queries (correlated subqueries), innermost first. */
  outer: Relation[];
  /** Every CTE visible at the cursor, innermost definition first. */
  ctes: CteDef[];
  /** Items of the arm the cursor is in. */
  arm: Item[];
}

export function resolveCtes(rels: Relation[], ctes: CteDef[]): Relation[] {
  return rels.map((r) => {
    if (r.kind !== "table" || r.parts.length !== 1) return r;
    const c = ctes.find((x) => x.name.toLowerCase() === r.parts[0].toLowerCase());
    return c ? { ...r, kind: "cte" as const, body: c.body } : r;
  });
}

function armAt(tree: Tree, block: Block, offset: number): { arm: Item[]; header: WithHeader } {
  const header = parseWithHeader(tree, block.items);
  const arms = splitArms(tree, header.rest, block.open < 0 ? 0 : tree.toks[block.open].end);
  let chosen = arms[0];
  for (const a of arms) if (a.from <= offset) chosen = a;
  return { arm: chosen.items, header };
}

export function scopeAt(tree: Tree, offset: number): QueryScope {
  const chain = blockChainAt(tree, offset);
  // Innermost query-ish block (function-call parens pass through to their enclosing query).
  let qi = chain.length - 1;
  while (qi > 0 && !isQueryBlock(chain[qi])) qi--;
  const block = chain[qi];

  const ctes: CteDef[] = [];
  const perLevel: { arm: Item[] }[] = [];
  for (let i = qi; i >= 0; i--) {
    const b = chain[i];
    if (!isQueryBlock(b)) continue;
    const { arm, header } = armAt(tree, b, offset);
    ctes.push(...header.ctes);
    perLevel.push({ arm });
  }
  const relations = resolveCtes(collectRelations(tree, perLevel[0].arm), ctes);
  const outer: Relation[] = [];
  for (let i = 1; i < perLevel.length; i++) outer.push(...resolveCtes(collectRelations(tree, perLevel[i].arm), ctes));
  return { block, chain, relations, outer, ctes, arm: perLevel[0].arm };
}

/** Offset range of a block's interior, for hover/ownership tests. */
export const blockRange = (tree: Tree, b: Block): [number, number] => [blockStart(tree, b), blockEnd(tree, b)];

/** Relations of the first arm of a CTE/derived-table body (used to expand `SELECT *`). */
export function bodyRelations(tree: Tree, body: Block, outerCtes: CteDef[]): Relation[] {
  const header = parseWithHeader(tree, body.items);
  const arms = splitArms(tree, header.rest, 0);
  return resolveCtes(collectRelations(tree, arms[0]?.items ?? []), [...header.ctes, ...outerCtes]);
}
