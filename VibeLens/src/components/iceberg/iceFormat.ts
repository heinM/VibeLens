// Formatting helpers for the Iceberg UI. Pure and Safari-16 safe.

/** 1536 -> "1.5 KB". Takes a number or the string form Iceberg summaries use. */
export function humanBytes(v: number | string | null | undefined): string {
  if (v === null || v === undefined || v === "") return "–";
  const n = typeof v === "string" ? Number(v) : v;
  if (!Number.isFinite(n) || n < 0) return "–";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB", "PB"];
  let x = n / 1024;
  let i = 0;
  while (x >= 1024 && i < units.length - 1) {
    x /= 1024;
    i++;
  }
  return `${x >= 100 ? x.toFixed(0) : x >= 10 ? x.toFixed(1) : x.toFixed(2)} ${units[i]}`;
}

/** Exact count with thousands separators: 1234567 -> "1,234,567". */
export function groupDigits(v: number | string | null | undefined): string {
  if (v === null || v === undefined || v === "") return "–";
  const n = typeof v === "string" ? Number(v) : v;
  if (!Number.isFinite(n)) return String(v);
  return Math.round(n).toLocaleString("en-US");
}

/** Short exact-ish count for tree rows: 3000 -> "3k", 1250000 -> "1.3M". */
export function compactNumber(n: number | null | undefined): string {
  if (n === null || n === undefined || n < 0) return "";
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0).replace(/\.0$/, "")}k`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(n < 10_000_000 ? 1 : 0).replace(/\.0$/, "")}M`;
  return `${(n / 1_000_000_000).toFixed(1).replace(/\.0$/, "")}B`;
}

/** Signed delta for the snapshot timeline: +1,500 / −3,500. */
export function signed(n: number, sign: "+" | "−"): string {
  return `${sign}${groupDigits(n)}`;
}

/** Summary value as an integer, or null when absent or not numeric. */
export function summaryInt(summary: Record<string, string>, key: string): number | null {
  const v = summary[key];
  if (v === undefined || v === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

const pad = (n: number) => String(n).padStart(2, "0");
/** 2026-10-31 14:03:07 in the viewer's local time. */
export function formatDateTime(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "–";
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export function relativeTime(ms: number, now = Date.now()): string {
  const s = Math.round((now - ms) / 1000);
  if (s < 0) return "in the future";
  if (s < 45) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h} h ago`;
  const d = Math.round(h / 24);
  if (d < 45) return `${d} day${d === 1 ? "" : "s"} ago`;
  const mo = Math.round(d / 30);
  if (mo < 18) return `${mo} months ago`;
  return `${Math.round(d / 365)} years ago`;
}

/** Value for <input type="datetime-local">: local time without seconds. */
export function toLocalInput(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
export function fromLocalInput(v: string): number | null {
  if (!v) return null;
  const t = new Date(v).getTime();
  return Number.isFinite(t) ? t : null;
}

export const DAY_MS = 86_400_000;
