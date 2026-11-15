// Pure ER layout: node sizes, dagre ranking per connected cluster, shelf packing of clusters and
// lone tables, and FK edge geometry that attaches to the exact column rows.
// No React and no DOM; text measurement is injected so tests can run anywhere.
import dagre from "@dagrejs/dagre";
import { clusterOf, isCollapsed, type ErdEdge, type ErdModel, type ErdTable } from "./model";

export const METRICS = {
  headerH: 38,
  rowH: 22,
  padBottom: 8,
  minW: 220,
  maxW: 380,
  /** Left of column name: key/link icon slot. */
  nameX: 32,
  /** Right padding of the type label. */
  padRight: 14,
  clusterGap: 90,
  rankSep: 120,
  nodeSep: 34,
} as const;

export type TextKind = "title" | "name" | "type" | "meta";
export type Measure = (text: string, kind: TextKind) => number;

const CHAR_W: Record<TextKind, number> = { title: 7.6, name: 6.7, type: 6.3, meta: 6 };
/** Deterministic estimator used by tests and as a fallback before fonts are measurable. */
export const estimateMeasure: Measure = (text, kind) => text.length * CHAR_W[kind];

/** Cut `text` with an ellipsis so it fits in `maxPx`. */
export function truncateToWidth(text: string, maxPx: number, measure: Measure, kind: TextKind): string {
  if (measure(text, kind) <= maxPx) return text;
  let lo = 0;
  let hi = text.length;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (measure(text.slice(0, mid) + "…", kind) <= maxPx) lo = mid;
    else hi = mid - 1;
  }
  return lo <= 0 ? "…" : text.slice(0, lo) + "…";
}

export function formatRows(n: number | null): string {
  if (n === null || n < 0) return "";
  if (n < 1000) return `~${n}`;
  if (n < 1_000_000) return `~${(n / 1000).toFixed(n < 10_000 ? 1 : 0).replace(/\.0$/, "")}k`;
  if (n < 1_000_000_000) return `~${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0).replace(/\.0$/, "")}M`;
  return `~${(n / 1_000_000_000).toFixed(1).replace(/\.0$/, "")}B`;
}

export interface PlacedNode {
  key: string;
  table: ErdTable;
  x: number;
  y: number;
  w: number;
  h: number;
  collapsed: boolean;
}

export type Side = "l" | "r";

export interface PlacedEdge {
  id: string;
  edge: ErdEdge;
  /** Anchor on the FK holder (many side). */
  x1: number;
  y1: number;
  side1: Side;
  /** Anchor on the referenced table (one side). */
  x2: number;
  y2: number;
  side2: Side;
  /** SVG path data of the curve. */
  path: string;
  /** Crow's foot at the FK end. */
  many: string;
  /** Bar at the referenced end. */
  one: string;
}

export interface ErdLayout {
  nodes: PlacedNode[];
  byKey: Map<string, PlacedNode>;
  edges: PlacedEdge[];
  width: number;
  height: number;
}

export function nodeWidth(t: ErdTable, collapsed: boolean, measure: Measure): number {
  let w: number = METRICS.minW;
  const meta = formatRows(t.rows);
  const headerW = 18 + measure(t.name, "title") + 18 + (meta ? measure(meta, "meta") + 10 : 0) + (collapsed ? measure(`${t.columns.length} columns`, "meta") + 24 : 22);
  w = Math.max(w, headerW);
  if (!collapsed) {
    for (const c of t.columns) w = Math.max(w, METRICS.nameX + measure(c.name, "name") + 28 + measure(c.type, "type") + METRICS.padRight);
  }
  return Math.min(METRICS.maxW, Math.ceil(w));
}

export function nodeHeight(t: ErdTable, collapsed: boolean): number {
  return collapsed ? METRICS.headerH : METRICS.headerH + t.columns.length * METRICS.rowH + METRICS.padBottom;
}

/** Y of the centre of a column row (or of the header when collapsed / column unknown). */
export function anchorY(n: PlacedNode, column: string | undefined): number {
  if (n.collapsed || !column) return n.y + METRICS.headerH / 2;
  const i = n.table.columns.findIndex((c) => c.name === column);
  if (i < 0) return n.y + METRICS.headerH / 2;
  return n.y + METRICS.headerH + i * METRICS.rowH + METRICS.rowH / 2;
}

interface Box {
  id: string;
  w: number;
  h: number;
}

/** Row-by-row ("shelf") packing: tallest first, wrapping at `targetW`. Returns top-left offsets. */
export function packShelves(boxes: readonly Box[], targetW: number, gap: number): { pos: Map<string, { x: number; y: number }>; width: number; height: number } {
  const sorted = [...boxes].sort((a, b) => b.h - a.h || b.w - a.w || a.id.localeCompare(b.id));
  const pos = new Map<string, { x: number; y: number }>();
  let x = 0;
  let y = 0;
  let rowH = 0;
  let width = 0;
  for (const b of sorted) {
    if (x > 0 && x + b.w > targetW) {
      x = 0;
      y += rowH + gap;
      rowH = 0;
    }
    pos.set(b.id, { x, y });
    x += b.w + gap;
    rowH = Math.max(rowH, b.h);
    width = Math.max(width, x - gap);
  }
  return { pos, width, height: sorted.length ? y + rowH : 0 };
}

export function layoutErd(model: Pick<ErdModel, "tables" | "edges">, expanded: ReadonlySet<string> = new Set(), measure: Measure = estimateMeasure): ErdLayout {
  const sizes = new Map<string, { w: number; h: number; collapsed: boolean; table: ErdTable }>();
  for (const t of model.tables) {
    const collapsed = isCollapsed(t, expanded);
    sizes.set(t.key, { w: nodeWidth(t, collapsed, measure), h: nodeHeight(t, collapsed), collapsed, table: t });
  }
  const keys = new Set(sizes.keys());
  const { members } = clusterOf(keys, model.edges);

  // Each cluster is laid out on its own with dagre (LR: referenced tables left, referencing right).
  const local = new Map<string, { x: number; y: number }>(); // position inside its cluster box
  const boxes: Box[] = [];
  const clusterOfKey = new Map<string, string>();
  const inCluster = new Set<string>();
  members.forEach((group, ci) => {
    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: "LR", ranksep: METRICS.rankSep, nodesep: METRICS.nodeSep, marginx: 0, marginy: 0, ranker: "network-simplex" });
    g.setDefaultEdgeLabel(() => ({}));
    for (const k of group) {
      const s = sizes.get(k) as { w: number; h: number };
      g.setNode(k, { width: s.w, height: s.h });
    }
    const seen = new Set<string>();
    for (const e of model.edges) {
      if (e.self || !group.includes(e.from) || !group.includes(e.to)) continue;
      const id = `${e.to}>${e.from}`;
      if (seen.has(id)) continue;
      seen.add(id);
      g.setEdge(e.to, e.from); // parent -> child
    }
    dagre.layout(g);
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (const k of group) {
      const n = g.node(k);
      const s = sizes.get(k) as { w: number; h: number };
      const x = n.x - s.w / 2;
      const y = n.y - s.h / 2;
      local.set(k, { x, y });
      minX = Math.min(minX, x);
      minY = Math.min(minY, y);
      maxX = Math.max(maxX, x + s.w);
      maxY = Math.max(maxY, y + s.h);
    }
    for (const k of group) {
      const p = local.get(k) as { x: number; y: number };
      local.set(k, { x: p.x - minX, y: p.y - minY });
      clusterOfKey.set(k, `c${ci}`);
      inCluster.add(k);
    }
    boxes.push({ id: `c${ci}`, w: maxX - minX, h: maxY - minY });
  });
  // Lone tables are their own boxes.
  for (const k of keys) {
    if (inCluster.has(k)) continue;
    const s = sizes.get(k) as { w: number; h: number };
    clusterOfKey.set(k, `t:${k}`);
    local.set(k, { x: 0, y: 0 });
    boxes.push({ id: `t:${k}`, w: s.w, h: s.h });
  }

  const area = boxes.reduce((a, b) => a + (b.w + METRICS.clusterGap) * (b.h + METRICS.clusterGap), 0);
  const widest = boxes.reduce((a, b) => Math.max(a, b.w), 0);
  const targetW = Math.max(widest, Math.sqrt(area) * 1.6, 1400);
  const packed = packShelves(boxes, targetW, METRICS.clusterGap);

  const nodes: PlacedNode[] = [];
  const byKey = new Map<string, PlacedNode>();
  for (const t of model.tables) {
    const s = sizes.get(t.key) as { w: number; h: number; collapsed: boolean };
    const origin = packed.pos.get(clusterOfKey.get(t.key) as string) ?? { x: 0, y: 0 };
    const p = local.get(t.key) ?? { x: 0, y: 0 };
    const n: PlacedNode = { key: t.key, table: t, x: Math.round(origin.x + p.x), y: Math.round(origin.y + p.y), w: s.w, h: s.h, collapsed: s.collapsed };
    nodes.push(n);
    byKey.set(t.key, n);
  }

  const edges: PlacedEdge[] = [];
  for (const e of model.edges) {
    const a = byKey.get(e.from);
    const b = byKey.get(e.to);
    if (!a || !b) continue;
    edges.push(routeEdge(e, a, b));
  }
  return { nodes, byKey, edges, width: packed.width, height: packed.height };
}

/** Geometry of one FK edge: from the FK column row of `a` to the referenced column row of `b`. */
export function routeEdge(edge: ErdEdge, a: PlacedNode, b: PlacedNode): PlacedEdge {
  const y1 = anchorY(a, edge.fromCols[0]);
  const y2 = anchorY(b, edge.toCols[0]);
  const gap = 28;
  let side1: Side;
  let side2: Side;
  if (edge.self) {
    side1 = "r";
    side2 = "r";
  } else if (b.x + b.w + gap <= a.x) {
    side1 = "l";
    side2 = "r"; // referenced table sits to the left of the FK holder
  } else if (a.x + a.w + gap <= b.x) {
    side1 = "r";
    side2 = "l";
  } else {
    side1 = "r";
    side2 = "r"; // same rank: loop out on the right
  }
  const x1 = side1 === "l" ? a.x : a.x + a.w;
  const x2 = side2 === "l" ? b.x : b.x + b.w;
  const s1 = side1 === "l" ? -1 : 1;
  const s2 = side2 === "l" ? -1 : 1;

  let path: string;
  if (side1 === side2) {
    // Both anchors on the same side: bulge outwards past the nearer card edge.
    const out = (s1 > 0 ? Math.max(x1, x2) : Math.min(x1, x2)) + s1 * (edge.self ? 46 : 64);
    path = `M${x1},${y1} C${out},${y1} ${out},${y2} ${x2},${y2}`;
  } else {
    const d = Math.max(48, Math.abs(x2 - x1) * 0.5);
    path = `M${x1},${y1} C${x1 + s1 * d},${y1} ${x2 + s2 * d},${y2} ${x2},${y2}`;
  }
  // Crow's foot: apex 11px out from the card, feet on the card edge.
  const ax = x1 + s1 * 11;
  const many = `M${ax},${y1} L${x1},${y1 - 5} M${ax},${y1} L${x1},${y1} M${ax},${y1} L${x1},${y1 + 5}`;
  const bx = x2 + s2 * 8;
  const one = `M${bx},${y2 - 5} L${bx},${y2 + 5}`;
  return { id: edge.id, edge, x1, y1, side1, x2, y2, side2, path, many, one };
}

export interface Bounds {
  x: number;
  y: number;
  w: number;
  h: number;
}

export function layoutBounds(l: Pick<ErdLayout, "nodes">, pad = 0): Bounds {
  if (l.nodes.length === 0) return { x: 0, y: 0, w: 0, h: 0 };
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const n of l.nodes) {
    minX = Math.min(minX, n.x);
    minY = Math.min(minY, n.y);
    maxX = Math.max(maxX, n.x + n.w);
    maxY = Math.max(maxY, n.y + n.h);
  }
  return { x: minX - pad, y: minY - pad, w: maxX - minX + pad * 2, h: maxY - minY + pad * 2 };
}

export interface Viewport {
  /** Screen-space translation of the world origin. */
  tx: number;
  ty: number;
  scale: number;
}

export const MIN_SCALE = 0.05;
export const MAX_SCALE = 2.5;

export const clampScale = (s: number) => Math.min(MAX_SCALE, Math.max(MIN_SCALE, s));

/** Viewport that frames `b` in a `w` x `h` container with `margin` px around (never zooms in beyond `maxScale`). */
export function fitViewport(b: Bounds, w: number, h: number, margin = 48, maxScale = 1): Viewport {
  if (b.w <= 0 || b.h <= 0 || w <= 0 || h <= 0) return { tx: 0, ty: 0, scale: 1 };
  const scale = clampScale(Math.min((w - margin * 2) / b.w, (h - margin * 2) / b.h, maxScale));
  return { scale, tx: (w - b.w * scale) / 2 - b.x * scale, ty: (h - b.h * scale) / 2 - b.y * scale };
}

/** Zoom by `factor` keeping the world point under the screen point (px, py) fixed. */
export function zoomAt(v: Viewport, factor: number, px: number, py: number): Viewport {
  const scale = clampScale(v.scale * factor);
  const k = scale / v.scale;
  return { scale, tx: px - (px - v.tx) * k, ty: py - (py - v.ty) * k };
}

/** Viewport centred on a node at (at most) `scale`. */
export function focusViewport(n: Pick<PlacedNode, "x" | "y" | "w" | "h">, w: number, h: number, scale: number): Viewport {
  const s = clampScale(scale);
  return { scale: s, tx: w / 2 - (n.x + n.w / 2) * s, ty: h / 2 - (n.y + n.h / 2) * s };
}
