import { describe, expect, it } from "vitest";
import { hoverAt } from "../hover";
import { CAT, cur } from "./fixtures";

const hover = (src: string) => {
  const { text, offset } = cur(src);
  return hoverAt(CAT, text, offset);
};

describe("hover", () => {
  it("table name shows its columns with PK/FK flags and row estimate", () => {
    const h = hover("select * from shop.ord|ers o")!;
    expect(h.markdown).toContain("**shop.orders**");
    expect(h.markdown).toContain("~1.3M rows");
    expect(h.markdown).toMatch(/customer_id\s+int8\s+FK → customers/);
    expect(h.markdown).toMatch(/id\s+int8\s+PK/);
  });
  it("shows the table comment", () => {
    expect(hover("select * from shop.custo|mers")!.markdown).toContain("People who buy things");
  });
  it("alias used as a qualifier resolves to its table", () => {
    const h = hover("select o|.id from shop.orders o")!;
    expect(h.markdown).toContain("shop.orders");
  });
  it("qualified column shows type, flags and comment", () => {
    const h = hover("select c.em|ail from shop.customers c")!;
    expect(h.markdown).toContain("customers.email");
    expect(h.markdown).toContain("`text`");
    expect(h.markdown).toContain("Login email");
    expect(h.markdown).toContain("not null");
  });
  it("bare column resolves through the scope", () => {
    const h = hover("select tot|al from shop.orders")!;
    expect(h.markdown).toContain("orders.total");
    expect(h.markdown).toContain("`numeric`");
  });
  it("ambiguous bare column mentions the other table", () => {
    const h = hover("select na|me from shop.customers c, shop.products p")!;
    expect(h.markdown).toContain("Also in");
  });
  it("schema part of a qualified name", () => {
    expect(hover("select * from sh|op.orders")!.markdown).toContain("schema");
  });
  it("hover range is the hovered token", () => {
    const { text, offset } = cur("select * from shop.ord|ers");
    const h = hoverAt(CAT, text, offset)!;
    expect(text.slice(h.start, h.end)).toBe("orders");
  });
  it("nothing in strings, comments or unknown names", () => {
    expect(hover("select 'ord|ers'")).toBeNull();
    expect(hover("select zzz|z from shop.orders")).toBeNull();
    expect(hover("-- ord|ers")).toBeNull();
  });
  it("works when the catalog is not loaded", () => {
    const { text, offset } = cur("select * from orders|");
    expect(hoverAt(null, text, offset)).toBeNull();
  });
});
