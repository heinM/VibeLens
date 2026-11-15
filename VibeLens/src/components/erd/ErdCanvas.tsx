// The pan/zoom SVG surface of the ER diagram. The viewport lives in a ref and is written straight
// to the DOM (no React render per frame); React state only carries what changes how things are
// drawn (low-detail switch, zoom label). Hover/selection dimming is done in CSS from `data-*`
// attributes, so a hover re-renders only the cards whose flags changed.
import { forwardRef, memo, useCallback, useEffect, useImperativeHandle, useLayoutEffect, useRef, useState } from "react";
import { cn } from "../../lib/cn";
import { CHEVRON_DOWN, CHEVRON_RIGHT, KEY_PATHS, LINK_PATHS, clusterHue } from "./glyphs";
import {
  METRICS,
  clampScale,
  fitViewport,
  focusViewport,
  formatRows,
  layoutBounds,
  truncateToWidth,
  zoomAt,
  type ErdLayout,
  type Measure,
  type PlacedEdge,
  type PlacedNode,
  type Viewport,
} from "./layout";
import { hueColor } from "./svgExport";
import { isCollapsible } from "./model";
import "./erd.css";

export interface ErdCanvasHandle {
  /** Frame everything; `insetRight` reserves room for an overlaid side panel. */
  fit: (animate?: boolean, insetRight?: number) => void;
  zoomBy: (factor: number) => void;
  focusNode: (key: string, insetRight?: number) => void;
}

interface Props {
  layout: ErdLayout;
  measure: Measure;
  dark: boolean;
  /** Table whose relations are lit (hovered, else selected). */
  focusKey: string | null;
  related: ReadonlySet<string> | null;
  selected: string | null;
  onSelect: (key: string | null) => void;
  onHover: (key: string | null) => void;
  onOpen: (key: string) => void;
  onToggleExpand: (key: string) => void;
  onZoomChange: (pct: number) => void;
  className?: string;
}

const LOW_DETAIL = 0.5;
const LOD_FONT_MAX = 34; // keep in sync with .lod-name in erd.css
const ease = (t: number) => 1 - Math.pow(1 - t, 3);

export const ErdCanvas = forwardRef<ErdCanvasHandle, Props>(function ErdCanvas(
  { layout, measure, dark, focusKey, related, selected, onSelect, onHover, onOpen, onToggleExpand, onZoomChange, className },
  ref,
) {
  const rootRef = useRef<HTMLDivElement>(null);
  const worldRef = useRef<SVGGElement>(null);
  const vp = useRef<Viewport>({ tx: 0, ty: 0, scale: 1 });
  const size = useRef({ w: 0, h: 0 });
  const anim = useRef<number | null>(null);
  const drag = useRef<{ id: number; x: number; y: number; tx: number; ty: number; moved: boolean; captured: boolean } | null>(null);
  const justDragged = useRef(false);
  const [low, setLow] = useState(false);
  const lastPct = useRef(-1);

  const apply = useCallback(
    (v: Viewport) => {
      vp.current = v;
      const g = worldRef.current;
      const root = rootRef.current;
      if (g) {
        g.setAttribute("transform", `translate(${v.tx} ${v.ty}) scale(${v.scale})`);
        g.style.setProperty("--inv", String(1 / v.scale));
      }
      if (root) {
        // The dotted background follows the world. At low zoom use a sparser grid so it never turns to grey noise.
        const step = 24 * v.scale * (v.scale < 0.4 ? 4 : v.scale < 0.7 ? 2 : 1);
        root.style.backgroundSize = `${step}px ${step}px`;
        root.style.backgroundPosition = `${v.tx}px ${v.ty}px`;
      }
      const isLow = v.scale < LOW_DETAIL;
      setLow((p) => (p === isLow ? p : isLow));
      const pct = Math.round(v.scale * 100);
      if (pct !== lastPct.current) {
        lastPct.current = pct;
        onZoomChange(pct);
      }
    },
    [onZoomChange],
  );

  const cancelAnim = () => {
    if (anim.current !== null) cancelAnimationFrame(anim.current);
    anim.current = null;
  };

  const animateTo = useCallback(
    (to: Viewport, animate = true) => {
      cancelAnim();
      const from = vp.current;
      if (!animate || window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) {
        apply(to);
        return;
      }
      const t0 = performance.now();
      const dur = 260;
      const step = (now: number) => {
        const k = Math.min(1, (now - t0) / dur);
        const e = ease(k);
        // Interpolate scale geometrically so zooms feel even.
        const scale = from.scale * Math.pow(to.scale / from.scale, e);
        apply({ scale, tx: from.tx + (to.tx - from.tx) * e, ty: from.ty + (to.ty - from.ty) * e });
        anim.current = k < 1 ? requestAnimationFrame(step) : null;
      };
      anim.current = requestAnimationFrame(step);
    },
    [apply],
  );

  useImperativeHandle(
    ref,
    () => ({
      fit: (animate = true, insetRight = 0) => animateTo(fitViewport(layoutBounds(layout), size.current.w - insetRight, size.current.h, 76), animate),
      zoomBy: (f) => animateTo(zoomAt(vp.current, f, size.current.w / 2, size.current.h / 2)),
      focusNode: (key, insetRight = 0) => {
        const n = layout.byKey.get(key);
        if (!n) return;
        const s = Math.min(1, Math.max(vp.current.scale, 0.85));
        const v = focusViewport(n, size.current.w - insetRight, size.current.h, s);
        animateTo(v);
      },
    }),
    [animateTo, layout],
  );

  // Track container size.
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const measureSize = () => {
      const r = el.getBoundingClientRect();
      size.current = { w: r.width, h: r.height };
    };
    measureSize();
    const ro = new ResizeObserver(measureSize);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // Wheel and pinch. Needs non-passive listeners to be able to preventDefault.
  useEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const rel = (e: { clientX: number; clientY: number }) => {
      const r = el.getBoundingClientRect();
      return { x: e.clientX - r.left, y: e.clientY - r.top };
    };
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      cancelAnim();
      const p = rel(e);
      const v = vp.current;
      const lineScale = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1;
      if (e.shiftKey || (Math.abs(e.deltaX) > Math.abs(e.deltaY) && !e.ctrlKey)) {
        // Sideways scroll pans.
        apply({ ...v, tx: v.tx - (e.deltaX || e.deltaY) * lineScale, ty: v.ty });
        return;
      }
      // ctrlKey is how browsers report a trackpad pinch; it comes with small deltas, so boost it.
      const k = e.ctrlKey ? 0.01 : 0.0016;
      const factor = Math.exp(-e.deltaY * lineScale * k);
      apply(zoomAt(v, factor, p.x, p.y));
    };
    let gestureStart = 1;
    const onGestureStart = (e: Event) => {
      e.preventDefault();
      gestureStart = vp.current.scale;
    };
    const onGestureChange = (e: Event) => {
      e.preventDefault();
      const g = e as unknown as { scale: number; clientX: number; clientY: number };
      const p = rel(g);
      const target = clampScale(gestureStart * g.scale);
      apply(zoomAt(vp.current, target / vp.current.scale, p.x, p.y));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    // Safari (WKWebView) reports pinch as gesture* events instead of ctrl+wheel.
    el.addEventListener("gesturestart", onGestureStart);
    el.addEventListener("gesturechange", onGestureChange);
    return () => {
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("gesturestart", onGestureStart);
      el.removeEventListener("gesturechange", onGestureChange);
    };
  }, [apply]);

  useEffect(() => () => cancelAnim(), []);

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0 && e.button !== 1) return;
    if ((e.target as Element).closest("[data-erd-ui]")) return;
    cancelAnim();
    drag.current = { id: e.pointerId, x: e.clientX, y: e.clientY, tx: vp.current.tx, ty: vp.current.ty, moved: false, captured: false };
    justDragged.current = false;
  };
  const onPointerMove = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    const dx = e.clientX - d.x;
    const dy = e.clientY - d.y;
    if (!d.moved) {
      if (Math.hypot(dx, dy) < 4) return;
      d.moved = true;
      // Capture only once it is a real drag, so plain clicks still reach the cards.
      try {
        rootRef.current?.setPointerCapture(e.pointerId);
        d.captured = true;
      } catch {
        /* pointer already gone */
      }
      onHover(null);
    }
    apply({ ...vp.current, tx: d.tx + dx, ty: d.ty + dy });
  };
  const endDrag = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    if (d.captured) {
      try {
        rootRef.current?.releasePointerCapture(e.pointerId);
      } catch {
        /* already released */
      }
    }
    justDragged.current = d.moved;
    drag.current = null;
    if (d.moved) window.setTimeout(() => (justDragged.current = false), 0);
  };

  const dragging = () => drag.current?.moved === true || justDragged.current;

  return (
    <div
      ref={rootRef}
      className={cn("vg-erd relative h-full w-full overflow-hidden outline-none", className)}
      data-focus={focusKey ? "" : undefined}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={endDrag}
      onPointerCancel={endDrag}
      onClick={(e) => {
        if (dragging()) return;
        if (e.target === e.currentTarget || (e.target as Element).tagName === "svg") onSelect(null);
      }}
      style={{ cursor: drag.current?.moved ? "grabbing" : "grab" }}
    >
      <svg width="100%" height="100%" role="img" aria-label="Entity relationship diagram">
        <g ref={worldRef}>
          <g>
            {layout.edges.map((e) => (
              <EdgeView key={e.id} e={e} hl={!!focusKey && (e.edge.from === focusKey || e.edge.to === focusKey)} />
            ))}
          </g>
          <g>
            {layout.nodes.map((n) => (
              <NodeView
                key={n.key}
                n={n}
                measure={measure}
                dark={dark}
                low={low}
                sel={selected === n.key}
                rel={!!related?.has(n.key)}
                onClick={(k) => {
                  if (!dragging()) onSelect(k);
                }}
                onHover={onHover}
                onOpen={onOpen}
                onToggleExpand={onToggleExpand}
              />
            ))}
          </g>
        </g>
      </svg>
    </div>
  );
});

const EdgeView = memo(function EdgeView({ e, hl }: { e: PlacedEdge; hl: boolean }) {
  const label = `${e.edge.from}.${e.edge.fromCols.join(", ")} references ${e.edge.to}.${e.edge.toCols.join(", ")}`;
  return (
    <g className="vg-edge" data-hl={hl ? "" : undefined} data-self={e.edge.self ? "" : undefined}>
      <path className="line" d={e.path} />
      <path d={e.many} />
      <path d={e.one} />
      <path className="hit" d={e.path}>
        <title>{`${e.edge.name}\n${label}`}</title>
      </path>
    </g>
  );
});

interface NodeProps {
  n: PlacedNode;
  measure: Measure;
  dark: boolean;
  low: boolean;
  sel: boolean;
  rel: boolean;
  onClick: (key: string) => void;
  onHover: (key: string | null) => void;
  onOpen: (key: string) => void;
  onToggleExpand: (key: string) => void;
}

const NodeView = memo(function NodeView({ n, measure, dark, low, sel, rel, onClick, onHover, onOpen, onToggleExpand }: NodeProps) {
  const t = n.table;
  const hue = hueColor(clusterHue(t.cluster), { muted: "var(--vg-muted)", dark });
  const collapsible = isCollapsible(t);
  const meta = formatRows(t.rows);
  const metaW = meta ? measure(meta, "meta") : 0;
  const toggleLabel = n.collapsed ? `${t.columns.length} columns` : "Hide columns";
  const toggleW = collapsible ? measure(toggleLabel, "meta") + 30 : 0;
  const nameMax = n.w - 18 - 14 - (meta ? metaW + 10 : 0) - (toggleW ? toggleW + 8 : 0);

  return (
    <g
      className="vg-node"
      transform={`translate(${n.x} ${n.y})`}
      data-sel={sel ? "" : undefined}
      data-rel={rel ? "" : undefined}
      data-key={n.key}
      onClick={() => onClick(n.key)}
      onDoubleClick={() => onOpen(n.key)}
      onPointerEnter={() => onHover(n.key)}
      onPointerLeave={() => onHover(null)}
    >
      <rect className="shadow" x={0} y={3} width={n.w} height={n.h} rx={10} opacity={0.9} />
      <rect className="card" width={n.w} height={n.h} rx={10} />
      <path className="head" d={`M0.5 ${METRICS.headerH} V10.5 a10 10 0 0 1 10 -10 H${n.w - 10.5} a10 10 0 0 1 10 10 V${METRICS.headerH} z`} />
      <rect x={0} y={9} width={3} height={METRICS.headerH - 18} rx={1.5} fill={hue} />
      {low ? (
        <>
          <text className="lod-name" x={16} y={METRICS.headerH / 2} dominantBaseline="central">
            {truncateToWidth(t.name, ((n.w - 28) * 13) / LOD_FONT_MAX, measure, "title")}
          </text>
          {!n.collapsed &&
            t.columns.map((c, i) => (
              <rect
                key={c.name}
                className={c.pk ? "lod-bar pk" : c.fk ? "lod-bar fk" : "lod-bar"}
                x={16}
                y={METRICS.headerH + i * METRICS.rowH + 8}
                width={Math.max(24, Math.min(n.w - 120, measure(c.name, "name") * 1.1))}
                height={6}
                rx={3}
              />
            ))}
        </>
      ) : (
        <>
          <text className="t-name" x={18} y={METRICS.headerH / 2 + 4.5} fontSize={13}>
            {truncateToWidth(t.name, nameMax, measure, "title")}
          </text>
          {meta && (
            <text className="t-muted mono" x={n.w - 14} y={METRICS.headerH / 2 + 4} fontSize={10.5} textAnchor="end">
              {meta}
            </text>
          )}
          {collapsible && (
            <g
              className="toggle"
              data-erd-ui
              transform={`translate(${n.w - 14 - (meta ? metaW + 10 : 0) - toggleW} ${METRICS.headerH / 2 - 10})`}
              onClick={(e) => {
                e.stopPropagation();
                onToggleExpand(n.key);
              }}
              onDoubleClick={(e) => e.stopPropagation()}
            >
              <title>{n.collapsed ? "Show all columns" : "Collapse to name only"}</title>
              <rect width={toggleW} height={20} rx={6} />
              <g transform="translate(6 3) scale(0.58)" fill="none" strokeWidth={2.6} strokeLinecap="round" strokeLinejoin="round" stroke="var(--vg-muted)">
                <path d={n.collapsed ? CHEVRON_RIGHT : CHEVRON_DOWN} />
              </g>
              <text className="t-muted" x={22} y={14} fontSize={10.5}>
                {toggleLabel}
              </text>
            </g>
          )}
          {!n.collapsed && (
            <>
              <line className="divider" x1={0} x2={n.w} y1={METRICS.headerH} y2={METRICS.headerH} />
              {t.columns.map((c, i) => {
                const cy = METRICS.headerH + i * METRICS.rowH;
                const typeW = measure(c.type, "type");
                return (
                  <g key={c.name}>
                    {(c.pk || c.fk) && (
                      <g
                        transform={`translate(13 ${cy + 5.5}) scale(0.5)`}
                        className={c.pk ? "ico-key" : "ico-link"}
                        fill="none"
                        strokeWidth={2.4}
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        stroke="inherit"
                      >
                        {(c.pk ? KEY_PATHS : LINK_PATHS).map((d) => (
                          <path key={d} d={d} className={c.pk ? "ico-key" : "ico-link"} />
                        ))}
                      </g>
                    )}
                    <text className={cn("t-col", c.pk && "pk")} x={METRICS.nameX} y={cy + METRICS.rowH / 2 + 4} fontSize={12}>
                      {truncateToWidth(c.name, n.w - METRICS.nameX - typeW - 28, measure, "name")}
                    </text>
                    <text className="t-muted mono" x={n.w - METRICS.padRight} y={cy + METRICS.rowH / 2 + 3.5} fontSize={10.5} textAnchor="end">
                      {c.type}
                    </text>
                  </g>
                );
              })}
            </>
          )}
        </>
      )}
    </g>
  );
});
