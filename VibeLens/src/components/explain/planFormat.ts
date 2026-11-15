// Number formatting for plan displays. Tabular, compact, and honest about precision.

export function formatMs(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return "–";
  if (ms === 0) return "0 ms";
  if (ms < 0.01) return "<0.01 ms";
  if (ms < 10) return `${ms.toFixed(2)} ms`;
  if (ms < 100) return `${ms.toFixed(1)} ms`;
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(2)} s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return `${m}m ${String(s).padStart(2, "0")}s`;
}

export function formatRows(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return "–";
  if (Math.abs(n) >= 1e9) return `${(n / 1e9).toFixed(1)}B`;
  if (Math.abs(n) >= 1e7) return `${(n / 1e6).toFixed(0)}M`;
  if (Math.abs(n) >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
  if (!Number.isInteger(n)) return n < 10 ? n.toFixed(1) : Math.round(n).toLocaleString("en-US");
  return n.toLocaleString("en-US");
}

export function formatCost(c: number | null | undefined): string {
  if (c === null || c === undefined || !Number.isFinite(c)) return "–";
  if (c >= 1e7) return `${(c / 1e6).toFixed(1)}M`;
  if (c >= 1000) return Math.round(c).toLocaleString("en-US");
  return c.toFixed(c < 10 ? 2 : 1);
}

/** Postgres pages are 8 kB. */
export function formatBlocks(blocks: number): string {
  const bytes = blocks * 8192;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} kB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(bytes < 10 * 1024 ** 2 ? 1 : 0)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

export function formatPercent(share: number): string {
  if (share <= 0) return "0%";
  if (share < 0.01) return "<1%";
  return `${Math.round(share * 100)}%`;
}

/** Renders any JSON value from a plan property as text. */
export function formatPropValue(v: unknown): string {
  if (v === null || v === undefined) return "null";
  if (typeof v === "string") return v;
  if (typeof v === "number") return Number.isInteger(v) ? v.toLocaleString("en-US").replace(/,/g, "") : String(v);
  if (typeof v === "boolean") return v ? "true" : "false";
  if (Array.isArray(v) && v.every((x) => typeof x === "string" || typeof x === "number")) return v.join(", ");
  return JSON.stringify(v, null, 2);
}

/** "402×", "1.2k×". */
export function formatFactor(f: number): string {
  if (f >= 1e6) return `${(f / 1e6).toFixed(1)}M×`;
  if (f >= 1000) return `${(f / 1000).toFixed(f >= 10_000 ? 0 : 1)}k×`;
  return `${Math.round(f)}×`;
}

/** Bar colour for a node's share of the plan: quiet when cheap, loud when it dominates. */
export function heatBg(share: number): string {
  if (share >= 0.3) return "bg-danger";
  if (share >= 0.1) return "bg-warn";
  return "bg-accent/55";
}

export function heatText(share: number): string {
  if (share >= 0.3) return "text-danger";
  if (share >= 0.1) return "text-warn";
  return "text-fg";
}
