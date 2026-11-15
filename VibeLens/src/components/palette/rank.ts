// Ranking for the palette: fuzzy score + recency boost, no React.
import { scoreTerms, splitTerms } from "./fuzzy";

export interface Rankable {
  id: string;
  title: string;
  /** Secondary searchable text (qualified name, connection, keywords). Not highlighted. */
  search: string;
  /** Owning connection etc. Matches only alongside a title/search hit. */
  context?: string;
}

export interface Ranked<T extends Rankable> {
  item: T;
  score: number;
  /** Highlight positions in `title`. */
  positions: number[];
}

/** Up to this many points for the most recent pick; fades linearly down the MRU list. */
export const RECENCY_BOOST = 28;

export function recencyBoost(rank: number | undefined, listLength: number): number {
  if (rank === undefined) return 0;
  return Math.round(RECENCY_BOOST * (1 - rank / Math.max(listLength, 1)));
}

/**
 * Filter and rank items for a query. With an empty query every item is returned in input order
 * (the caller groups and caps them). Ties break on recency, then title length, then input order.
 */
export function rankItems<T extends Rankable>(items: readonly T[], query: string, mru: ReadonlyMap<string, number> = new Map(), limit = Infinity): Ranked<T>[] {
  const terms = splitTerms(query);
  if (terms.length === 0) return items.map((item) => ({ item, score: 0, positions: [] }));
  const out: Ranked<T>[] = [];
  for (const item of items) {
    const r = scoreTerms(terms, item.title, item.search, item.context);
    if (!r) continue;
    out.push({ item, score: r.score + recencyBoost(mru.get(item.id), mru.size), positions: r.positions });
  }
  out.sort((a, b) => b.score - a.score || a.item.title.length - b.item.title.length);
  return out.length > limit ? out.slice(0, limit) : out;
}
