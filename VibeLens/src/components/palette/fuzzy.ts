// Pure fuzzy scorer for the command palette. No React, no DOM.
//
// Smith-Waterman style dynamic programme (the same family as fzf v2): every query character must
// appear in order in the target, and the score rewards what humans perceive as a "good" match:
//   - runs of consecutive characters,
//   - characters that start a word (after `_ - . / space :`), a camelCase hump or the string,
//   - matches near the start, shorter targets, exact-case characters,
//   - whole-string and prefix matches (large bonus).
// Gaps cost a little (opening a gap costs more than extending one).
//
// `fuzzyMatch` returns the score and the matched character positions (for highlighting), or null.
// `scoreTerms` splits a query on whitespace; every term must match the title or the secondary text.

export interface FuzzyResult {
  score: number;
  /** Indices into the target string (ascending), one per query character. */
  positions: number[];
}

const MATCH = 16;
const GAP_START = -3;
const GAP_EXT = -1;
const BONUS_START = 10;
const BONUS_BOUNDARY = 9;
const BONUS_CAMEL = 7;
const BONUS_DIGIT = 3;
const CONSECUTIVE = 5;
const PREFIX_BONUS = 40;
const EXACT_BONUS = 80;
const CASE_BONUS = 1;
const NEG = -1_000_000;

const MAX_QUERY = 32;
const MAX_TARGET = 160;

let bufM = new Int32Array(0);
let bufG = new Int32Array(0);
const bufBonus = new Int32Array(MAX_TARGET);

const isSep = (c: number) => c === 95 /* _ */ || c === 45 /* - */ || c === 46 /* . */ || c === 47 /* / */ || c === 32 /* space */ || c === 58 /* : */ || c === 92 /* \ */;
const isLower = (c: number) => c >= 97 && c <= 122;
const isUpper = (c: number) => c >= 65 && c <= 90;
const isDigit = (c: number) => c >= 48 && c <= 57;

/** Bonus for a match landing on each target position. */
function fillBonus(target: string, n: number): void {
  let prev = -1;
  for (let j = 0; j < n; j++) {
    const c = target.charCodeAt(j);
    let b = 0;
    if (j === 0) b = BONUS_START;
    else if (isSep(prev)) b = BONUS_BOUNDARY;
    else if (isLower(prev) && isUpper(c)) b = BONUS_CAMEL;
    else if (!isDigit(prev) && isDigit(c)) b = BONUS_DIGIT;
    bufBonus[j] = b;
    prev = c;
  }
}

/** Is `q` (already lowercase) an in-order subsequence of `t` (lowercase)? */
function isSubsequence(q: string, t: string): boolean {
  let qi = 0;
  for (let ti = 0; ti < t.length && qi < q.length; ti++) if (t.charCodeAt(ti) === q.charCodeAt(qi)) qi++;
  return qi === q.length;
}

/**
 * Fuzzy-match `query` against `target`. Case-insensitive; exact-case characters score slightly higher.
 * Returns null when the query is not an in-order subsequence of the target. An empty query matches
 * everything with score 0.
 */
export function fuzzyMatch(query: string, target: string): FuzzyResult | null {
  if (query.length === 0) return { score: 0, positions: [] };
  if (query.length > MAX_QUERY) query = query.slice(0, MAX_QUERY);
  const tgt = target.length > MAX_TARGET ? target.slice(0, MAX_TARGET) : target;
  const m = query.length;
  const n = tgt.length;
  if (m > n) return null;
  const q = query.toLowerCase();
  const t = tgt.toLowerCase();
  if (q.length !== m || t.length !== n) return null; // exotic case folding changed lengths: skip rather than misalign
  if (!isSubsequence(q, t)) return null;

  if (bufM.length < m * n) {
    bufM = new Int32Array(m * n);
    bufG = new Int32Array(m * n);
  }
  fillBonus(tgt, n);
  const M = bufM;
  const G = bufG;
  M.fill(NEG, 0, m * n);
  G.fill(NEG, 0, m * n);

  for (let i = 0; i < m; i++) {
    const qc = q.charCodeAt(i);
    const row = i * n;
    const prevRow = row - n;
    for (let j = i; j < n; j++) {
      if (t.charCodeAt(j) === qc) {
        const bonus = bufBonus[j];
        if (i === 0) {
          M[row + j] = MATCH + bonus * 2 - Math.min(j, 10);
        } else if (j > 0) {
          let best = NEG;
          const diag = M[prevRow + j - 1];
          if (diag > NEG / 2) best = diag + (bonus > CONSECUTIVE ? bonus : CONSECUTIVE);
          const gap = G[prevRow + j - 1];
          if (gap > NEG / 2 && gap + bonus > best) best = gap + bonus;
          if (best > NEG / 2) M[row + j] = best + MATCH;
        }
      }
      if (j > 0) {
        let g = NEG;
        const a = M[row + j - 1];
        if (a > NEG / 2) g = a + GAP_START;
        const b = G[row + j - 1];
        if (b > NEG / 2 && b + GAP_EXT > g) g = b + GAP_EXT;
        G[row + j] = g;
      }
    }
  }

  const last = (m - 1) * n;
  let bestJ = -1;
  let best = NEG;
  for (let j = m - 1; j < n; j++) {
    if (M[last + j] > best) {
      best = M[last + j];
      bestJ = j;
    }
  }
  if (bestJ < 0 || best <= NEG / 2) return null;

  // Walk back to recover the positions.
  const positions = new Array<number>(m);
  let j = bestJ;
  positions[m - 1] = j;
  for (let i = m - 1; i > 0; i--) {
    const row = i * n;
    const prevRow = row - n;
    const base = M[row + j] - MATCH;
    const bonus = bufBonus[j];
    const diag = M[prevRow + j - 1];
    if (j > 0 && diag > NEG / 2 && base === diag + (bonus > CONSECUTIVE ? bonus : CONSECUTIVE)) {
      j -= 1;
    } else {
      // Came out of a gap: find where the previous match ended.
      let c = j - 1;
      let found = -1;
      while (c >= 1) {
        const g = G[prevRow + c];
        const open = M[prevRow + c - 1];
        if (open > NEG / 2 && g === open + GAP_START) {
          found = c - 1;
          break;
        }
        c--;
      }
      j = found >= 0 ? found : Math.max(i - 1, 0);
    }
    positions[i - 1] = j;
  }

  let score = best;
  if (m === n && q === t) score += EXACT_BONUS;
  else if (t.startsWith(q)) score += PREFIX_BONUS;
  for (let i = 0; i < m; i++) if (tgt.charCodeAt(positions[i]) === query.charCodeAt(i)) score += CASE_BONUS;
  score -= Math.min(12, Math.floor((n - m) / 2));
  return { score, positions };
}

export interface TermsResult {
  score: number;
  /** Highlight positions in the title. */
  positions: number[];
}

/** Split a query into whitespace-separated terms (lowercase-insensitive; empties dropped). */
export function splitTerms(query: string): string[] {
  return query.split(/\s+/).filter(Boolean);
}

const EXTRA_WEIGHT = 0.55;
const CONTEXT_WEIGHT = 0.3;

/**
 * Every term must match the title, the `extra` text (qualified name, keywords) or the `context`
 * text (owning connection). Title matches count fully, extra at about half and context at under a
 * third; at least one term has to land in the title or extra, so typing part of a connection's name
 * doesn't surface every table it owns. Returns null if any term fails.
 */
export function scoreTerms(terms: readonly string[], title: string, extra: string, context = ""): TermsResult | null {
  if (terms.length === 0) return { score: 0, positions: [] };
  let total = 0;
  let primary = false;
  const marks = new Set<number>();
  for (const term of terms) {
    const a = fuzzyMatch(term, title);
    const b = extra ? fuzzyMatch(term, extra) : null;
    const best = a && (!b || a.score >= b.score * EXTRA_WEIGHT) ? a : null;
    if (best) {
      total += best.score;
      primary = true;
      for (const p of best.positions) marks.add(p);
      continue;
    }
    if (b) {
      total += b.score * EXTRA_WEIGHT;
      primary = true;
      continue;
    }
    const c = context ? fuzzyMatch(term, context) : null;
    if (!c) return null;
    total += c.score * CONTEXT_WEIGHT;
  }
  if (!primary) return null;
  return { score: total, positions: [...marks].sort((x, y) => x - y) };
}

/** Collapse sorted positions into [start, end) runs, for rendering highlighted spans. */
export function positionsToRanges(positions: readonly number[]): [number, number][] {
  const out: [number, number][] = [];
  for (const p of positions) {
    const last = out[out.length - 1];
    if (last && last[1] === p) last[1] = p + 1;
    else out.push([p, p + 1]);
  }
  return out;
}
