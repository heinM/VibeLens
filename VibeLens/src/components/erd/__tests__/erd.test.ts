import { describe, expect, it } from "vitest";
import { COLLAPSE_THRESHOLD, ERD_MAX_TABLES, buildErdModel, clusterOf, collectEdges, neighbourhood, relationsOf } from "../model";
import { METRICS, anchorY, fitViewport, layoutBounds, layoutErd, packShelves, routeEdge, truncateToWidth, estimateMeasure, zoomAt, clampScale, MAX_SCALE, MIN_SCALE, formatRows } from "../layout";
import { esc, renderErdSvg, type ExportPalette } from "../svgExport";
import { col, fk, idCol, shopCatalog, table } from "../fixtures";

const rectsOverlap = (a: { x: number; y: number; w: number; h: number }, b: { x: number; y: number; w: number; h: number }) =>
  a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;

describe("buildErdModel", () => {
  it("draws only tables of the schema, with FK edges between them", () => {
    const cat = [...shopCatalog(), table("other", [idCol()], [], { schema: "public" }), table("v_orders", [idCol()], [], { kind: "view" })];
    const m = buildErdModel(cat, "shop");
    expect(m.tables.map((t) => t.name).sort()).toEqual(["audit_log", "customers", "order_items", "orders", "products"]);
    expect(m.edges).toHaveLength(3);
    expect(m.total).toBe(5);
    expect(m.hidden).toBe(0);
  });

  it("flags PK and FK columns and keeps ordinal order", () => {
    const m = buildErdModel(shopCatalog(), "shop");
    const items = m.tables.find((t) => t.name === "order_items")!;
    expect(items.columns.map((c) => [c.name, c.pk, c.fk])).toEqual([
      ["order_id", true, true],
      ["product_id", true, true],
      ["qty", false, false],
    ]);
  });

  it("ignores FKs that leave the schema or point at missing tables", () => {
    const cat = [table("a", [idCol(), col("x")], [fk(["x"], "b", ["id"], "elsewhere"), fk(["x"], "ghost")])];
    expect(collectEdges(cat, "shop")).toHaveLength(0);
  });

  it("keeps self references but excludes them from degree", () => {
    const cat = [table("emp", [idCol(), col("boss_id")], [fk(["boss_id"], "emp")]), table("dept", [idCol(), col("head")], [fk(["head"], "emp")])];
    const m = buildErdModel(cat, "shop");
    expect(m.edges.find((e) => e.self)).toBeDefined();
    expect(m.tables.find((t) => t.name === "emp")!.degree).toBe(1);
  });

  it("numbers clusters by size and marks lone tables -1", () => {
    const m = buildErdModel(shopCatalog(), "shop");
    expect(m.clusterCount).toBe(1);
    expect(m.tables.find((t) => t.name === "audit_log")!.cluster).toBe(-1);
    expect(m.tables.find((t) => t.name === "orders")!.cluster).toBe(0);
  });

  it("caps at the 150 most connected tables and says how many were hidden", () => {
    const hub = table("hub", [idCol()]);
    const spokes = Array.from({ length: 199 }, (_, i) => table(`t${String(i).padStart(3, "0")}`, [idCol(), col("hub_id")], i < 40 ? [fk(["hub_id"], "hub")] : []));
    const m = buildErdModel([hub, ...spokes], "shop");
    expect(m.tables).toHaveLength(ERD_MAX_TABLES);
    expect(m.capped).toBe(true);
    expect(m.hidden).toBe(50);
    expect(m.total).toBe(200);
    // hub (degree 40) and the 40 connected spokes all survive
    expect(m.tables.some((t) => t.name === "hub")).toBe(true);
    expect(m.tables.filter((t) => t.degree > 0)).toHaveLength(41);
    expect(m.edges).toHaveLength(40);
  });

  it("applies the filter before the cap", () => {
    const cat = [...shopCatalog()];
    const m = buildErdModel(cat, "shop", { filter: "ord" });
    expect(m.tables.map((t) => t.name).sort()).toEqual(["order_items", "orders"]);
    expect(m.hidden).toBe(3);
    expect(m.capped).toBe(false);
    expect(m.edges).toHaveLength(1);
  });

  it("neighbourhood and relationsOf follow edges in both directions", () => {
    const m = buildErdModel(shopCatalog(), "shop");
    expect([...neighbourhood(m, "shop.orders")].sort()).toEqual(["shop.customers", "shop.order_items", "shop.orders"]);
    const r = relationsOf(m, "shop.orders");
    expect(r.parents.map((e) => e.to)).toEqual(["shop.customers"]);
    expect(r.children.map((e) => e.from)).toEqual(["shop.order_items"]);
  });

  it("clusterOf ignores self loops", () => {
    const c = clusterOf(new Set(["a", "b"]), [{ id: "1", from: "a", to: "a", fromCols: [], toCols: [], name: "x", self: true }]);
    expect(c.count).toBe(0);
  });
});

describe("layoutErd", () => {
  const model = () => buildErdModel(shopCatalog(), "shop");

  it("places every table without overlaps", () => {
    const l = layoutErd(model());
    expect(l.nodes).toHaveLength(5);
    for (let i = 0; i < l.nodes.length; i++) for (let j = i + 1; j < l.nodes.length; j++) expect(rectsOverlap(l.nodes[i], l.nodes[j])).toBe(false);
  });

  it("lays parents left of children (rankdir LR)", () => {
    const l = layoutErd(model());
    const x = (n: string) => l.byKey.get(`shop.${n}`)!.x;
    expect(x("customers")).toBeLessThan(x("orders"));
    expect(x("orders")).toBeLessThan(x("order_items"));
    expect(x("products")).toBeLessThan(x("order_items"));
  });

  it("sizes cards from their columns, collapses wide tables and expands on request", () => {
    const wide = table("wide", [idCol(), ...Array.from({ length: COLLAPSE_THRESHOLD + 3 }, (_, i) => col(`c${i}`, "text"))]);
    const m = buildErdModel([wide, ...shopCatalog()], "shop");
    const collapsed = layoutErd(m).byKey.get("shop.wide")!;
    expect(collapsed.collapsed).toBe(true);
    expect(collapsed.h).toBe(METRICS.headerH);
    const open = layoutErd(m, new Set(["shop.wide"])).byKey.get("shop.wide")!;
    expect(open.collapsed).toBe(false);
    expect(open.h).toBe(METRICS.headerH + (COLLAPSE_THRESHOLD + 4) * METRICS.rowH + METRICS.padBottom);
    const exactly = table("twelve", Array.from({ length: COLLAPSE_THRESHOLD }, (_, i) => col(`c${i}`)));
    expect(layoutErd(buildErdModel([exactly], "shop")).byKey.get("shop.twelve")!.collapsed).toBe(false);
  });

  it("attaches edges to the FK and PK column rows", () => {
    const l = layoutErd(model());
    const e = l.edges.find((x) => x.edge.from === "shop.orders")!;
    const orders = l.byKey.get("shop.orders")!;
    const customers = l.byKey.get("shop.customers")!;
    expect(e.y1).toBe(anchorY(orders, "customer_id"));
    expect(e.y2).toBe(anchorY(customers, "id"));
    // customers sits left, so the FK leaves orders' left edge and lands on customers' right edge
    expect(e.side1).toBe("l");
    expect(e.x1).toBe(orders.x);
    expect(e.side2).toBe("r");
    expect(e.x2).toBe(customers.x + customers.w);
    expect(e.path.startsWith(`M${e.x1},${e.y1} C`)).toBe(true);
    expect(e.path.endsWith(` ${e.x2},${e.y2}`)).toBe(true);
  });

  it("anchors collapsed tables at the header", () => {
    const wide = table("wide", [idCol(), ...Array.from({ length: 14 }, (_, i) => col(`c${i}`))]);
    const kid = table("kid", [idCol(), col("wide_id")], [fk(["wide_id"], "wide")]);
    const l = layoutErd(buildErdModel([wide, kid], "shop"));
    const e = l.edges[0];
    expect(e.y2).toBe(l.byKey.get("shop.wide")!.y + METRICS.headerH / 2);
  });

  it("routes self references as a loop on the right edge", () => {
    const emp = table("emp", [idCol(), col("boss_id")], [fk(["boss_id"], "emp")]);
    const l = layoutErd(buildErdModel([emp], "shop"));
    const e = l.edges[0];
    const n = l.byKey.get("shop.emp")!;
    expect(e.side1).toBe("r");
    expect(e.side2).toBe("r");
    expect(e.x1).toBe(n.x + n.w);
    expect(e.y1).not.toBe(e.y2);
  });

  it("routeEdge handles same-rank tables by looping out on the right", () => {
    const a = layoutErd(buildErdModel([table("a", [idCol(), col("b_id")], [fk(["b_id"], "b")]), table("b", [idCol()])], "shop"));
    const na = a.byKey.get("shop.a")!;
    const nb = { ...a.byKey.get("shop.b")!, x: na.x + 10, y: na.y + 300 };
    const e = routeEdge(a.edges[0].edge, na, nb);
    expect(e.side1).toBe("r");
    expect(e.side2).toBe("r");
  });

  it("packs lone tables into rows instead of one long column", () => {
    const lone = Array.from({ length: 40 }, (_, i) => table(`solo${i}`, [idCol(), col("name", "text")]));
    const l = layoutErd(buildErdModel(lone, "shop"));
    const b = layoutBounds(l);
    expect(b.w).toBeGreaterThan(b.h * 0.8); // wide, not a tall strip
    expect(new Set(l.nodes.map((n) => n.y)).size).toBeGreaterThan(1);
    for (let i = 0; i < l.nodes.length; i++) for (let j = i + 1; j < l.nodes.length; j++) expect(rectsOverlap(l.nodes[i], l.nodes[j])).toBe(false);
  });

  it("lays out 150 connected tables quickly (hub, 29 mid-level tables, 4 children each, plus cross links)", () => {
    const tables = [table("hub", [idCol()])];
    for (let m = 0; m < 29; m++) {
      tables.push(table(`mid${m}`, [idCol(), col("hub_id")], [fk(["hub_id"], "hub")]));
      for (let c = 0; c < 4; c++) tables.push(table(`leaf${m}_${c}`, [idCol(), col("mid_id"), col("hub_id")], [fk(["mid_id"], `mid${m}`), fk(["hub_id"], "hub")]));
    }
    const m = buildErdModel(tables, "shop");
    expect(m.tables).toHaveLength(146);
    const t0 = performance.now();
    const l = layoutErd(m);
    const dt = performance.now() - t0;
    expect(l.nodes).toHaveLength(146);
    expect(l.edges).toHaveLength(29 + 29 * 4 * 2);
    expect(dt).toBeLessThan(4000); // ~100 ms on an idle machine; generous because CI shares CPUs
  });

  it("is deterministic", () => {
    const a = layoutErd(model()).nodes.map((n) => [n.key, n.x, n.y]);
    const b = layoutErd(model()).nodes.map((n) => [n.key, n.x, n.y]);
    expect(a).toEqual(b);
  });
});

describe("packShelves", () => {
  it("wraps at the target width", () => {
    const r = packShelves(
      [
        { id: "a", w: 100, h: 50 },
        { id: "b", w: 100, h: 40 },
        { id: "c", w: 100, h: 30 },
      ],
      250,
      10,
    );
    expect(r.pos.get("a")).toEqual({ x: 0, y: 0 });
    expect(r.pos.get("b")).toEqual({ x: 110, y: 0 });
    expect(r.pos.get("c")).toEqual({ x: 0, y: 60 });
    expect(r.height).toBe(90);
  });
});

describe("viewport maths", () => {
  it("fit centres the diagram and never zooms in past 1", () => {
    const v = fitViewport({ x: 0, y: 0, w: 200, h: 100 }, 1000, 800);
    expect(v.scale).toBe(1);
    expect(v.tx).toBe(400);
    expect(v.ty).toBe(350);
    const small = fitViewport({ x: 0, y: 0, w: 4000, h: 2000 }, 1000, 800, 40);
    expect(small.scale).toBeCloseTo(920 / 4000, 5);
  });

  it("zoomAt keeps the point under the cursor fixed and clamps", () => {
    const v0 = { tx: 30, ty: 40, scale: 1 };
    const px = 300;
    const py = 200;
    const world = { x: (px - v0.tx) / v0.scale, y: (py - v0.ty) / v0.scale };
    const v1 = zoomAt(v0, 1.5, px, py);
    expect(world.x * v1.scale + v1.tx).toBeCloseTo(px, 6);
    expect(world.y * v1.scale + v1.ty).toBeCloseTo(py, 6);
    expect(zoomAt(v0, 1000, 0, 0).scale).toBe(MAX_SCALE);
    expect(clampScale(0.0001)).toBe(MIN_SCALE);
  });
});

describe("helpers", () => {
  it("truncateToWidth shortens with an ellipsis and leaves short text alone", () => {
    expect(truncateToWidth("short", 500, estimateMeasure, "name")).toBe("short");
    const t = truncateToWidth("a_very_long_column_name_indeed", 80, estimateMeasure, "name");
    expect(t.endsWith("…")).toBe(true);
    expect(estimateMeasure(t, "name")).toBeLessThanOrEqual(80);
  });

  it("formatRows is compact", () => {
    expect(formatRows(null)).toBe("");
    expect(formatRows(980)).toBe("~980");
    expect(formatRows(1250000)).toBe("~1.3M");
    expect(formatRows(4200)).toBe("~4.2k");
  });
});

describe("renderErdSvg", () => {
  const palette: ExportPalette = { bg: "#000", panel: "#111", panel2: "#222", border: "#333", fg: "#eee", muted: "#888", accent: "#5ee", amber: "#fb0", dark: true, fontSans: "Inter, sans-serif", fontMono: "monospace" };

  it("emits well-formed SVG with every table and edge, and escapes names", () => {
    const cat = [table("a<b>&\"c", [idCol()]), ...shopCatalog()];
    const m = buildErdModel(cat, "shop");
    const l = layoutErd(m);
    const svg = renderErdSvg(l, new Set(), palette, estimateMeasure, "shop diagram");
    expect(svg.startsWith("<svg ")).toBe(true);
    expect(svg.endsWith("</svg>")).toBe(true);
    expect(svg).toContain("customers");
    expect(svg).toContain("a&lt;b&gt;&amp;&quot;c");
    expect(svg).not.toContain("a<b>");
    expect((svg.match(/<g transform="translate\(/g) ?? []).length).toBeGreaterThanOrEqual(6);
    // balanced tags
    expect((svg.match(/<g[ >]/g) ?? []).length).toBe((svg.match(/<\/g>/g) ?? []).length);
    expect(esc("&<>\"")).toBe("&amp;&lt;&gt;&quot;");
  });
});
