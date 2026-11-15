// Pure helpers for the query history list: filtering, day grouping and relative times.
import type { HistoryEntry } from "../../store/useHistoryStore";

export type HistoryRow = { type: "day"; key: string; label: string; count: number } | { type: "entry"; entry: HistoryEntry };

const pad = (n: number) => String(n).padStart(2, "0");

/** Local calendar day, "2026-10-31". */
export const dayKey = (at: number): string => {
  const d = new Date(at);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
};

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

const startOfDay = (at: number): number => {
  const d = new Date(at);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
};

/** "Today", "Yesterday", a weekday for the last week, otherwise "12 Oct 2026". */
export function dayLabel(at: number, now: number): string {
  const days = Math.round((startOfDay(now) - startOfDay(at)) / 86_400_000);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  const d = new Date(at);
  if (days < 7) return WEEKDAYS[d.getDay()];
  return `${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}`;
}

/** "just now", "5m ago", "3h ago", "2d ago"; a clock time never needs more than a day of precision. */
export function timeAgo(at: number, now: number): string {
  const s = Math.max(0, Math.round((now - at) / 1000));
  if (s < 45) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.round(h / 24);
  return d < 60 ? `${d}d ago` : `${Math.round(d / 30)}mo ago`;
}

export function clockTime(at: number): string {
  const d = new Date(at);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** Whole statement on one line for previews and search. */
export const collapseSql = (sql: string): string => sql.replace(/\s+/g, " ").trim();

/** Every whitespace-separated word of the query must appear in the statement, connection, database or error. */
export function filterEntries(entries: readonly HistoryEntry[], query: string, onlyFailed: boolean): HistoryEntry[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0 && !onlyFailed) return entries.slice();
  return entries.filter((e) => {
    if (onlyFailed && e.ok) return false;
    if (words.length === 0) return true;
    const hay = `${e.sql}\n${e.profileName ?? ""}\n${e.database ?? ""}\n${e.error ?? ""}`.toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

/** Entries are expected newest first; a day header precedes each run of the same local day. */
export function groupByDay(entries: readonly HistoryEntry[], now: number): HistoryRow[] {
  const out: HistoryRow[] = [];
  let current: { key: string; header: Extract<HistoryRow, { type: "day" }> } | null = null;
  for (const e of entries) {
    const key = dayKey(e.at);
    if (!current || current.key !== key) {
      const header: Extract<HistoryRow, { type: "day" }> = { type: "day", key, label: dayLabel(e.at, now), count: 0 };
      out.push(header);
      current = { key, header };
    }
    current.header.count++;
    out.push({ type: "entry", entry: e });
  }
  return out;
}
