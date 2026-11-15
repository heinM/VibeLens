import { describe, expect, it } from "vitest";
import { analyzeAt } from "../analysis";
import { CAT, cur, labels, run, sorted } from "./fixtures";

const kindOf = (src: string) => {
  const { text, offset } = cur(src);
  return analyzeAt(text, offset)?.ctx.kind ?? null;
};

describe("cursor context", () => {
  it("statement start", () => {
    expect(kindOf("|")).toBe("statementStart");
    expect(kindOf("select 1; |")).toBe("statementStart");
    expect(kindOf("sel|")).toBe("statementStart");
  });
  it("relation after FROM/JOIN/UPDATE/INTO/TABLE", () => {
    expect(kindOf("select * from |")).toBe("relation");
    expect(kindOf("select * from a join |")).toBe("relation");
    expect(kindOf("update |")).toBe("relation");
    expect(kindOf("insert into |")).toBe("relation");
    expect(kindOf("delete from |")).toBe("relation");
    expect(kindOf("alter table |")).toBe("relation");
    expect(kindOf("drop table if exists |")).toBe("relation");
    expect(kindOf("select * from a, |")).toBe("relation");
    expect(kindOf("create index i on |")).toBe("relation");
  });
  it("is not fooled by FROM inside function calls or FOR UPDATE", () => {
    expect(kindOf("select extract(year from |) from t")).not.toBe("relation");
    expect(kindOf("select * from t for update |")).not.toBe("relation");
    expect(kindOf("select a is distinct from |")).not.toBe("relation");
  });
  it("qualified after a dot", () => {
    expect(kindOf("select o.| from shop.orders o")).toBe("qualified");
    expect(kindOf("select * from shop.|")).toBe("relation");
  });
  it("type after ::", () => {
    expect(kindOf("select x::|")).toBe("type");
  });
  it("nothing in strings and comments", () => {
    expect(kindOf("select 'abc|'")).toBeNull();
    expect(kindOf("select 1 -- foo|")).toBeNull();
    expect(kindOf("select /* foo| */ 1")).toBeNull();
  });
  it("expression clauses", () => {
    expect(kindOf("select | from shop.orders")).toBe("expression");
    expect(kindOf("select * from shop.orders where |")).toBe("expression");
    expect(kindOf("select * from shop.orders order by |")).toBe("expression");
    expect(kindOf("select count(|) from shop.orders")).toBe("expression");
  });
  it("after a relation: keywords", () => {
    expect(kindOf("select * from shop.orders |")).toBe("afterRelation");
    expect(kindOf("select * from shop.orders o |")).toBe("afterRelation");
  });
  it("column list", () => {
    expect(kindOf("insert into shop.orders (|)")).toBe("columnList");
    expect(kindOf("insert into shop.orders (id, |)")).toBe("columnList");
  });
  it("only looks at the current statement", () => {
    expect(kindOf("select 1 from a; select | from b")).toBe("expression");
  });
});

describe("relation completion", () => {
  it("offers schemas and tables with schema in detail and rows in description", () => {
    const r = run("select * from |");
    const orders = r!.suggestions.find((s) => s.label === "orders")!;
    expect(orders.detail).toContain("shop");
    expect(orders.description).toBe("~1.3M rows");
    expect(orders.insertText).toBe("shop.orders");
    expect(labels(r)).toContain("shop");
    expect(labels(r)).toContain("audit_log");
    const audit = r!.suggestions.find((s) => s.label === "audit_log")!;
    expect(audit.insertText).toBe("audit_log"); // public needs no qualifier
  });
  it("excludes sequences and other non-relations", () => {
    expect(labels(run("select * from |"))).not.toContain("seq_thing");
  });
  it("schema-qualified: only that schema's tables, bare names", () => {
    const r = run("select * from shop.|");
    expect(labels(r).sort()).toEqual(["customers", "order_items", "orders", "products", "v_open_orders"]);
    expect(r!.suggestions.find((s) => s.label === "orders")!.insertText).toBe("orders");
    expect(labels(run("select * from nosuch.|"))).toEqual([]);
  });
  it("schema items re-trigger the list", () => {
    const s = run("select * from |")!.suggestions.find((x) => x.label === "shop" && x.kind === "schema")!;
    expect(s.insertText).toBe("shop.");
    expect(s.retrigger).toBe(true);
  });
  it("offers CTE names first", () => {
    const r = run("with recent as (select 1) select * from |");
    expect(sorted(r)[0]).toBe("recent");
  });
  it("tables already mentioned in the worksheet rank above the rest", () => {
    const r = run("select * from shop.products;\nselect * from |");
    expect(sorted(r)[0]).toBe("products");
  });
  it("replace range covers the typed prefix", () => {
    const { text, offset } = cur("select * from ord|");
    const r = run("select * from ord|")!;
    expect(text.slice(r.replaceStart, r.insertEnd)).toBe("ord");
    expect(offset).toBe(r.insertEnd);
  });
  it("quotes with a leading double quote", () => {
    const r = run('select * from "au|')!;
    expect(r.suggestions.find((s) => s.label === "audit_log")!.insertText).toBe('"audit_log"');
  });
  it("space after FROM triggers the list; a space elsewhere does not", () => {
    expect(labels(run("select * from |", CAT, " ")).length).toBeGreaterThan(0);
    expect(labels(run("select * from shop.orders where |", CAT, " "))).toEqual([]);
  });
  it("without a catalog still offers CTEs and does not throw", () => {
    expect(labels(run("with a as (select 1) select * from |", null))).toEqual(["a"]);
  });
});

describe("column completion", () => {
  it("columns after alias. even when FROM comes later", () => {
    const r = run("SELECT o.| FROM shop.orders o");
    expect(labels(r)).toEqual(expect.arrayContaining(["id", "customer_id", "status", "total", "created_at"]));
    expect(labels(r)).not.toContain("email");
  });
  it("shows type in detail and PK/FK/nullable in documentation", () => {
    const r = run("SELECT o.| FROM shop.orders o")!;
    const cid = r.suggestions.find((s) => s.label === "customer_id")!;
    expect(cid.detail).toContain("int8");
    expect(cid.documentation).toContain("Foreign key → shop.customers.id");
    expect(cid.documentation).toContain("not null");
    const id = r.suggestions.find((s) => s.label === "id")!;
    expect(id.detail).toBe("int8 · PK");
    expect(cid.detail).toBe("int8 · FK");
    expect(id.documentation).toContain("Primary key");
    expect(r.suggestions.find((s) => s.label === "status")!.documentation).toContain("nullable");
  });
  it("resolves AS aliases and unaliased tables", () => {
    expect(labels(run("select c.| from shop.customers as c"))).toContain("email");
    expect(labels(run("select customers.| from shop.customers"))).toContain("email");
  });
  it("resolves schema.table. and a table not yet in FROM", () => {
    expect(labels(run("select shop.orders.| from shop.orders"))).toContain("total");
    expect(labels(run("select orders.|"))).toContain("total");
  });
  it("handles joins: each alias gets its own columns", () => {
    const sql = "select c.|, o.id from shop.orders o join shop.customers c on o.customer_id = c.id";
    expect(labels(run(sql))).toContain("email");
    expect(labels(run(sql))).not.toContain("total");
  });
  it("handles comma joins and implicit aliases", () => {
    expect(labels(run("select x.| from shop.orders o, shop.customers x"))).toContain("email");
    expect(labels(run("select p.| from shop.products p"))).toContain("sku");
  });
  it("CTE columns come from its select list", () => {
    const r = run("with t as (select id, total as amount, count(*) n from shop.orders group by 1, 2) select t.| from t");
    expect(labels(r).sort()).toEqual(["amount", "id", "n"]);
  });
  it("CTE with explicit column list", () => {
    expect(labels(run("with t(a, b) as (select 1, 2) select t.| from t")).sort()).toEqual(["a", "b"]);
  });
  it("CTE star expands from the catalog and keeps types", () => {
    const r = run("with t as (select * from shop.customers) select t.| from t")!;
    expect(labels(r)).toContain("email");
    expect(r.suggestions.find((s) => s.label === "email")!.detail).toContain("text");
  });
  it("derived tables", () => {
    expect(labels(run("select s.| from (select customer_id, sum(total) as spent from shop.orders group by 1) s")).sort()).toEqual(["customer_id", "spent"]);
  });
  it("later CTEs can see earlier ones", () => {
    expect(labels(run("with a as (select id from shop.orders), b as (select a.| from a) select 1"))).toEqual(["id"]);
  });
  it("subquery scope: inner tables win, outer are correlated", () => {
    const sql = "select * from shop.customers c where exists (select 1 from shop.orders o where o.customer_id = c.| )";
    expect(labels(run(sql))).toContain("email");
    const inner = run("select * from shop.customers c where exists (select 1 from shop.orders o where |)")!;
    const idx = sorted(inner);
    expect(idx.indexOf("total")).toBeLessThan(idx.indexOf("email"));
  });
  it("UNION arms have separate scopes", () => {
    const r = run("select a.id from shop.orders a union select | from shop.customers b");
    expect(labels(r)).toContain("email");
    expect(labels(r)).not.toContain("total");
  });
  it("UPDATE ... SET offers the target's columns", () => {
    expect(labels(run("update shop.orders set |"))).toContain("status");
  });
  it("INSERT column list and RETURNING", () => {
    expect(labels(run("insert into shop.orders (|)"))).toEqual(expect.arrayContaining(["customer_id", "total"]));
    expect(labels(run("insert into shop.orders (id) values (1) returning |"))).toContain("created_at");
  });
  it("ON CONFLICT (…) uses the insert target", () => {
    expect(labels(run("insert into shop.orders (id) values (1) on conflict (|)"))).toContain("id");
  });
  it("quotes identifiers that need it", () => {
    const r = run("select a.| from audit_log a")!;
    expect(r.suggestions.find((s) => s.label === "Weird Col")!.insertText).toBe('"Weird Col"');
    const r2 = run("select o.| from shop.orders o")!;
    expect(r2.suggestions.find((s) => s.label === "order")!.insertText).toBe('"order"'); // reserved word
    expect(r2.suggestions.find((s) => s.label === "status")!.insertText).toBe("status");
  });
  it("unqualified: columns of in-scope tables first, then functions and keywords", () => {
    const r = run("select | from shop.orders o join shop.customers c on o.customer_id = c.id");
    const order = sorted(r);
    const firstFn = order.indexOf("count");
    const firstKw = order.indexOf("DISTINCT");
    expect(order.indexOf("total")).toBeLessThan(firstFn);
    expect(order.indexOf("email")).toBeLessThan(firstFn);
    expect(firstFn).toBeLessThan(firstKw);
  });
  it("ambiguous columns are qualified with their alias on insert", () => {
    const r = run("select | from shop.orders o join shop.customers c on 1=1")!;
    expect(r.suggestions.filter((s) => s.label === "id").map((s) => s.insertText).sort()).toEqual(["c.id", "o.id"]);
    expect(r.suggestions.find((s) => s.label === "email")!.insertText).toBe("email");
  });
  it("offers aliases that re-trigger the list", () => {
    const a = run("select | from shop.orders o")!.suggestions.find((s) => s.kind === "alias" && s.label === "o")!;
    expect(a.insertText).toBe("o.");
    expect(a.retrigger).toBe(true);
  });
  it("select list offers *", () => {
    expect(labels(run("select | from shop.orders"))).toContain("*");
  });
  it("qualified star completion for alias.* is not required but alias. lists columns once", () => {
    const r = run("select o.| from shop.orders o, shop.orders o2")!;
    expect(r.suggestions.filter((s) => s.label === "id")).toHaveLength(1);
  });
  it("is fast on a large catalog", () => {
    const meta = {
      ...CAT.meta,
      tables: Array.from({ length: 5000 }, (_, i) => ({
        schema: i % 2 ? "public" : "s" + (i % 17),
        name: "table_" + i,
        kind: "table" as const,
        comment: null,
        estimatedRows: i,
        columns: Array.from({ length: 30 }, (_, c) => ({ name: "c" + c, dataType: "int4", nullable: true, default: null, isPrimaryKey: c === 0, ordinal: c, comment: null })),
        indexes: [],
        foreignKeys: [],
      })),
    };
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    return import("../catalog").then(({ indexCatalog }) => {
      const cat = indexCatalog(meta);
      run("select * from |", cat); // warm
      const t0 = performance.now();
      const r = run("select * from |", cat)!;
      const t1 = performance.now();
      run("select t.| from public.table_1 t", cat);
      const t2 = performance.now();
      expect(r.suggestions.length).toBeGreaterThan(5000);
      expect(t1 - t0).toBeLessThan(50);
      expect(t2 - t1).toBeLessThan(50);
    });
  });
});

describe("join conditions on ON", () => {
  it("suggests the FK condition first: left.fk = right.pk", () => {
    const r = run("select * from shop.orders o join shop.customers c on |")!;
    const top = [...r.suggestions].sort((a, b) => (a.sortText < b.sortText ? -1 : 1))[0];
    expect(top.kind).toBe("join");
    expect(top.label).toBe("o.customer_id = c.id");
    expect(top.preselect).toBe(true);
    expect(top.detail).toContain("orders_customer_id_fkey");
  });
  it("works in the reverse direction (referenced table on the left)", () => {
    const r = run("select * from shop.customers c join shop.orders o on |")!;
    expect(labels(r)).toContain("c.id = o.customer_id");
  });
  it("uses table names when there are no aliases", () => {
    const r = run("select * from shop.orders join shop.customers on |")!;
    expect(labels(r)).toContain("orders.customer_id = customers.id");
  });
  it("finds the FK against any earlier relation, not only the previous one", () => {
    const r = run("select * from shop.order_items oi join shop.orders o on oi.order_id = o.id join shop.customers c on |")!;
    expect(labels(r)).toContain("o.customer_id = c.id");
  });
  it("offers it again after AND only when it is not already written", () => {
    expect(labels(run("select * from shop.orders o join shop.customers c on o.customer_id = c.id and |")!).filter((l) => l.includes("customer_id = c.id"))).toEqual([]);
    expect(labels(run("select * from shop.orders o join shop.customers c on o.status = 'x' and |")!)).toContain("o.customer_id = c.id");
  });
  it("self-referencing FK", () => {
    const r = run("select * from public.accounts a join public.accounts p on |")!;
    expect(labels(r)).toEqual(expect.arrayContaining(["a.id = p.parent_id", "a.parent_id = p.id"]));
  });
  it("falls back to naming convention when there is no FK", () => {
    const r = run("select * from public.accounts a join public.invoices i on |")!;
    expect(r.suggestions.find((s) => s.kind === "join")!.label).toBe("a.id = i.account_id");
    expect(r.suggestions.find((s) => s.kind === "join")!.detail).toBe("by naming");
  });
  it("still offers columns while typing a qualified name in ON", () => {
    expect(labels(run("select * from shop.orders o join shop.customers c on o.|"))).toContain("customer_id");
  });
  it("no join suggestions when the right side is a CTE", () => {
    const r = run("with x as (select 1) select * from shop.orders o join x on |")!;
    expect(r.suggestions.some((s) => s.kind === "join")).toBe(false);
  });
});

describe("snippets, keywords, types", () => {
  it("offers the snippets at statement start", () => {
    const r = run("|")!;
    const sn = r.suggestions.filter((s) => s.kind === "snippet").map((s) => s.label);
    expect(sn).toEqual(expect.arrayContaining(["sel*", "selc", "ins", "upd", "cte"]));
  });
  it("the update snippet has a WHERE placeholder", () => {
    const upd = run("|")!.suggestions.find((s) => s.label === "upd")!;
    expect(upd.insertText).toMatch(/WHERE \$\{\d:condition\}/);
    expect(upd.isSnippet).toBe(true);
  });
  it("a fully typed snippet trigger outranks the keyword", () => {
    expect(sorted(run("ins|"))[0]).toBe("ins");
    expect(sorted(run("sel|"))[0]).toBe("sel*");
    expect(sorted(run("selec|"))[0]).toBe("SELECT");
  });
  it("types after ::", () => {
    expect(labels(run("select x::|"))).toEqual(expect.arrayContaining(["text", "jsonb", "uuid"]));
  });
  it("keywords after a relation need two typed characters", () => {
    expect(labels(run("select * from shop.orders |"))).toEqual([]);
    expect(labels(run("select * from shop.orders wh|"))).toContain("WHERE");
    expect(labels(run("select * from shop.orders o join shop.customers c |"))).toEqual([]);
    expect(labels(run("select * from shop.orders o join shop.customers c on|"))).toContain("ON");
  });
});
