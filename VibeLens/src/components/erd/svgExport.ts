// Static SVG export of the diagram. Pure string building with resolved colours (CSS variables do
// not survive outside the app), mirroring the on-screen cards.
import { KEY_PATHS, LINK_PATHS, clusterHue } from "./glyphs";
import { METRICS, formatRows, layoutBounds, truncateToWidth, type ErdLayout, type Measure } from "./layout";
import { isCollapsed } from "./model";

export interface ExportPalette {
  bg: string;
  panel: string;
  panel2: string;
  border: string;
  fg: string;
  muted: string;
  accent: string;
  amber: string;
  dark: boolean;
  fontSans: string;
  fontMono: string;
}

export const esc = (s: string): string => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

export function hueColor(hue: number | null, palette: Pick<ExportPalette, "muted" | "dark">): string {
  if (hue === null) return palette.muted;
  return palette.dark ? `hsl(${hue} 62% 62%)` : `hsl(${hue} 58% 40%)`;
}

export function renderErdSvg(layout: ErdLayout, expanded: ReadonlySet<string>, p: ExportPalette, measure: Measure, title: string): string {
  const pad = 40;
  const b = layoutBounds(layout, pad);
  const w = Math.max(1, Math.ceil(b.w));
  const h = Math.max(1, Math.ceil(b.h));
  const out: string[] = [];
  out.push(`<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="${b.x} ${b.y} ${w} ${h}" font-family="${esc(p.fontSans)}">`);
  out.push(`<title>${esc(title)}</title>`);
  out.push(`<rect x="${b.x}" y="${b.y}" width="${w}" height="${h}" fill="${p.bg}"/>`);

  out.push(`<g fill="none" stroke="${p.muted}" stroke-width="1.4" stroke-linecap="round" opacity="0.8">`);
  for (const e of layout.edges) out.push(`<path d="${e.path}"/><path d="${e.many}"/><path d="${e.one}"/>`);
  out.push(`</g>`);

  for (const n of layout.nodes) {
    const t = n.table;
    const hue = hueColor(clusterHue(t.cluster), p);
    const collapsed = isCollapsed(t, expanded);
    out.push(`<g transform="translate(${n.x} ${n.y})">`);
    out.push(`<rect width="${n.w}" height="${n.h}" rx="9" fill="${p.panel}" stroke="${p.border}"/>`);
    out.push(`<path d="M0.5 ${METRICS.headerH} V10 a9.5 9.5 0 0 1 9.5 -9.5 H${n.w - 10} a9.5 9.5 0 0 1 9.5 9.5 V${METRICS.headerH} z" fill="${p.panel2}"/>`);
    out.push(`<rect x="0" y="9" width="3" height="${METRICS.headerH - 18}" rx="1.5" fill="${hue}"/>`);
    const meta = formatRows(t.rows);
    const metaW = meta ? measure(meta, "meta") : 0;
    const nameMax = n.w - 18 - 14 - (meta ? metaW + 10 : 0) - (collapsed ? measure(`${t.columns.length} columns`, "meta") + 24 : 0);
    out.push(`<text x="18" y="${METRICS.headerH / 2 + 4.5}" font-size="13" font-weight="600" fill="${p.fg}">${esc(truncateToWidth(t.name, nameMax, measure, "title"))}</text>`);
    if (meta) out.push(`<text x="${n.w - 14}" y="${METRICS.headerH / 2 + 4}" font-size="10.5" text-anchor="end" fill="${p.muted}" font-family="${esc(p.fontMono)}">${esc(meta)}</text>`);
    if (collapsed) {
      const label = `${t.columns.length} columns`;
      const x = n.w - 14 - (meta ? metaW + 10 : 0);
      out.push(`<text x="${x}" y="${METRICS.headerH / 2 + 4}" font-size="10.5" text-anchor="end" fill="${p.muted}">${esc(label)}</text>`);
    } else {
      out.push(`<line x1="0" x2="${n.w}" y1="${METRICS.headerH}" y2="${METRICS.headerH}" stroke="${p.border}"/>`);
      t.columns.forEach((c, i) => {
        const cy = METRICS.headerH + i * METRICS.rowH;
        const ty = cy + METRICS.rowH / 2 + 4;
        if (c.pk || c.fk) {
          const paths = c.pk ? KEY_PATHS : LINK_PATHS;
          out.push(
            `<g transform="translate(13 ${cy + 5.5}) scale(0.5)" fill="none" stroke="${c.pk ? p.amber : p.accent}" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">${paths.map((d) => `<path d="${d}"/>`).join("")}</g>`,
          );
        }
        const typeW = measure(c.type, "type");
        out.push(`<text x="${METRICS.nameX}" y="${ty}" font-size="12" fill="${p.fg}"${c.pk ? ' font-weight="600"' : ""}>${esc(truncateToWidth(c.name, n.w - METRICS.nameX - typeW - 28, measure, "name"))}</text>`);
        out.push(`<text x="${n.w - METRICS.padRight}" y="${ty - 0.5}" font-size="10.5" text-anchor="end" fill="${p.muted}" font-family="${esc(p.fontMono)}">${esc(c.type)}</text>`);
      });
    }
    out.push(`</g>`);
  }
  out.push(`</svg>`);
  return out.join("\n");
}
