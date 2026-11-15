import { describe, expect, it } from "vitest";
import { fuzzyMatch, positionsToRanges, scoreTerms, splitTerms } from "../fuzzy";
import { rankItems, type Rankable } from "../rank";
import { mruRanks, parseMru, pushMru, readMru, recordMru, MRU_LIMIT, type MruStorage } from "../mru";

const score = (q: string, t: string) => fuzzyMatch(q, t)?.score ?? -Infinity;

describe("fuzzyMatch", () => {
  it("rejects non-subsequences and over-long queries", () => {
    expect(fuzzyMatch("xyz", "orders")).toBeNull();
    expect(fuzzyMatch("sredro", "orders")).toBeNull();
    expect(fuzzyMatch("ordersx", "orders")).toBeNull();
  });

  it("matches everything for an empty query", () => {
    expect(fuzzyMatch("", "anything")).toEqual({ score: 0, positions: [] });
  });

  it("returns ascending positions, one per query char, pointing at the right chars", () => {
    const r = fuzzyMatch("oit", "order_items");
    expect(r).not.toBeNull();
    const p = r!.positions;
    expect(p).toHaveLength(3);
    expect([...p].sort((a, b) => a - b)).toEqual(p);
    expect(p.map((i) => "order_items"[i]).join("")).toBe("oit");
  });

  it("prefers word-boundary matches: 'oi' picks order_items over doily", () => {
    expect(score("oi", "order_items")).toBeGreaterThan(score("oi", "doily"));
    expect(fuzzyMatch("oi", "order_items")!.positions).toEqual([0, 6]);
  });

  it("prefers prefix over mid-word", () => {
    expect(score("ord", "orders")).toBeGreaterThan(score("ord", "records"));
    expect(score("ord", "orders")).toBeGreaterThan(score("ord", "sales_orders"));
  });

  it("prefers exact over prefix over substring", () => {
    expect(score("orders", "orders")).toBeGreaterThan(score("orders", "orders_archive"));
    expect(score("orders", "orders_archive")).toBeGreaterThan(score("orders", "old_orders"));
  });

  it("prefers shorter targets on equal matches", () => {
    expect(score("order", "orders")).toBeGreaterThan(score("order", "order_items_history"));
  });

  it("recognises camelCase humps and snake_case segments", () => {
    const camel = fuzzyMatch("gcn", "getCustomerName")!;
    expect(camel.positions).toEqual([0, 3, 11]);
    const snake = fuzzyMatch("gcn", "get_customer_name")!;
    expect(snake.positions).toEqual([0, 4, 13]);
    expect(camel.score).toBeGreaterThan(score("gcn", "gxxcxxnxxxxxxxx"));
  });

  it("favours consecutive runs over scattered letters", () => {
    expect(score("cust", "customers")).toBeGreaterThan(score("cust", "c_u_s_t"));
  });

  it("is case-insensitive but rewards exact case", () => {
    expect(fuzzyMatch("ORD", "orders")).not.toBeNull();
    expect(score("Ord", "Orders")).toBeGreaterThan(score("Ord", "orders"));
  });

  it("handles repeated characters without mis-assigning positions", () => {
    const r = fuzzyMatch("aaa", "banana")!;
    expect(r.positions.map((i) => "banana"[i]).join("")).toBe("aaa");
    expect(new Set(r.positions).size).toBe(3);
  });

  it("copes with long targets and 1-char queries", () => {
    const long = "x".repeat(300) + "y";
    expect(fuzzyMatch("y", long)).toBeNull(); // beyond the 160-char window
    expect(fuzzyMatch("x", long)).not.toBeNull();
  });
});

describe("scoreTerms", () => {
  it("requires every whitespace-separated term to match", () => {
    expect(scoreTerms(splitTerms("ord shop"), "orders", "shop.orders Demo Postgres")).not.toBeNull();
    expect(scoreTerms(splitTerms("ord nope"), "orders", "shop.orders Demo Postgres")).toBeNull();
  });

  it("matches a qualified schema.table query through the extra text", () => {
    const r = scoreTerms(["shop.ord"], "orders", "shop.orders");
    expect(r).not.toBeNull();
    expect(r!.positions).toEqual([]); // nothing to highlight in the bare title
  });

  it("does not let the connection name alone match a table", () => {
    expect(scoreTerms(["ord"], "carts", "shop.carts", "Orders DB")).toBeNull();
    expect(scoreTerms(["carts", "ord"], "carts", "shop.carts", "Orders DB")).not.toBeNull();
  });

  it("weights title matches above extra-only matches", () => {
    const a = scoreTerms(["demo"], "demo_table", "x")!;
    const b = scoreTerms(["demo"], "other", "Demo Postgres")!;
    expect(a.score).toBeGreaterThan(b.score);
  });
});

describe("positionsToRanges", () => {
  it("merges adjacent positions", () => {
    expect(positionsToRanges([0, 1, 2, 5, 7, 8])).toEqual([[0, 3], [5, 6], [7, 9]]);
    expect(positionsToRanges([])).toEqual([]);
  });
});

describe("rankItems", () => {
  const mk = (id: string, title: string, search = title): Rankable => ({ id, title, search });
  const items = [mk("1", "order_items"), mk("2", "orders"), mk("3", "customers"), mk("4", "old_records")];

  it("filters and orders by score", () => {
    const r = rankItems(items, "ord");
    expect(r.map((x) => x.item.id)).toEqual(["2", "1"].concat(r.length > 2 ? ["4"] : []));
    expect(r[0].item.title).toBe("orders");
  });

  it("returns everything in input order for an empty query", () => {
    expect(rankItems(items, "").map((x) => x.item.id)).toEqual(["1", "2", "3", "4"]);
  });

  it("puts a recent pick first when scores are close", () => {
    const mru = mruRanks(["1"]);
    const r = rankItems(items, "order", mru);
    expect(r[0].item.id).toBe("1");
  });

  it("honours the limit", () => {
    expect(rankItems(items, "o", new Map(), 2)).toHaveLength(2);
  });

  it("filters 5,000 items well inside a frame budget", () => {
    const names = ["orders", "customers", "order_items", "products", "payments", "shipments", "invoices", "audit_log"];
    const big: Rankable[] = Array.from({ length: 5000 }, (_, i) => {
      const t = `${names[i % names.length]}_${i}`;
      return mk(String(i), t, `public.${t} Demo Postgres table`);
    });
    rankItems(big, "ord"); // warm up the JIT
    const t0 = performance.now();
    for (let k = 0; k < 5; k++) rankItems(big, "ordit");
    const per = (performance.now() - t0) / 5;
    expect(per).toBeLessThan(60); // generous CI bound; ~5-10 ms locally
  });
});

describe("mru", () => {
  const memStore = (init?: string): MruStorage & { data: Map<string, string> } => {
    const data = new Map<string, string>();
    if (init) data.set("vg.palette.mru", init);
    return { data, getItem: (k) => data.get(k) ?? null, setItem: (k, v) => void data.set(k, v) };
  };

  it("pushMru moves to front, dedupes and trims", () => {
    expect(pushMru(["a", "b", "c"], "c")).toEqual(["c", "a", "b"]);
    expect(pushMru(["a", "b"], "z", 2)).toEqual(["z", "a"]);
  });

  it("parseMru ignores garbage", () => {
    expect(parseMru(null)).toEqual([]);
    expect(parseMru("{not json")).toEqual([]);
    expect(parseMru('{"a":1}')).toEqual([]);
    expect(parseMru('["a", 3, "b"]')).toEqual(["a", "b"]);
    expect(parseMru(JSON.stringify(Array.from({ length: 100 }, (_, i) => String(i))))).toHaveLength(MRU_LIMIT);
  });

  it("round-trips through storage", () => {
    const s = memStore();
    recordMru("x", s);
    recordMru("y", s);
    expect(readMru(s)).toEqual(["y", "x"]);
  });

  it("survives storage that throws", () => {
    const bad: MruStorage = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(() => recordMru("q", bad)).not.toThrow();
    expect(Array.isArray(readMru(bad))).toBe(true);
  });

  it("mruRanks gives 0 to the most recent", () => {
    expect(mruRanks(["a", "b"]).get("a")).toBe(0);
    expect(mruRanks(["a", "b"]).get("b")).toBe(1);
  });
});
