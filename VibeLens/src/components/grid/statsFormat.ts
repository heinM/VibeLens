// Formatting for the engine statistics shown in the results status line (Iceberg: DataFusion, Athena,
// Trino, Spark; Redshift). Only Athena reports a cost; the others leave `costUsdEstimate` null.
// Pure, so the rounding rules and the wording of the tooltips are unit-tested.
import type { StatementStats } from "../../hooks/useQueryRunner";

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB"] as const;

/** Binary bytes with three significant digits: `0 B`, `219 KB`, `1.50 MB`, `12.3 GB`. */
export function humanBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  let unit = 0;
  let v = n;
  while (v >= 1024 && unit < UNITS.length - 1) {
    v /= 1024;
    unit++;
  }
  const digits = unit === 0 ? 0 : v >= 100 ? 0 : v >= 10 ? 1 : 2;
  let text = v.toFixed(digits);
  // 1023.96 KB rounds to "1024 KB": say "1.00 MB" instead.
  if (Number(text) >= 1024 && unit < UNITS.length - 1) {
    unit++;
    text = (Number(text) / 1024).toFixed(2);
  }
  return `${text} ${UNITS[unit]}`;
}

/** Below this the estimate is shown as a bound: four decimals would print 0.0000. */
export const COST_FLOOR_USD = 0.0001;

/**
 * An Athena cost estimate as the status line shows it: `≈ $0.0123`, `< $0.0001` for the 10 MB minimum
 * on a small query, two decimals from one dollar up. Null when the engine gave none.
 */
export function formatCostUsd(usd: number | null | undefined): string | null {
  if (usd === null || usd === undefined || !Number.isFinite(usd) || usd < 0) return null;
  if (usd === 0) return "no charge"; // the engine reported a free statement (Athena DDL)
  if (usd < COST_FLOOR_USD) return `< $${COST_FLOOR_USD.toFixed(4)}`;
  const four = Math.round(usd * 1e4) / 1e4;
  if (four < 1) return `≈ $${four.toFixed(4)}`;
  return `≈ $${usd.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

export interface EngineBadge {
  label: string;
  tone: "muted" | "accent";
  title: string;
}

/** The badge for the `engine` string of a stats event. */
export function engineBadge(engine: string): EngineBadge {
  switch (engine.toLowerCase()) {
    case "datafusion":
      return { label: "DataFusion · local", tone: "muted", title: "Ran on the embedded DataFusion engine on this computer. Nothing is billed." };
    case "athena":
      return { label: "Athena", tone: "accent", title: "Ran on Amazon Athena." };
    case "trino":
      return { label: "Trino", tone: "accent", title: "Ran on your Trino cluster." };
    case "spark":
      return { label: "Spark", tone: "accent", title: "Ran in a Spark session on Apache Livy." };
    case "redshift":
      return { label: "Redshift", tone: "accent", title: "Ran on Amazon Redshift through the Data API." };
    default:
      return { label: engine, tone: "muted", title: `Ran on ${engine}.` };
  }
}

export const MIN_BILLED_BYTES = 10 * 1024 * 1024;

/** Tooltip of the cost figure: it is an estimate, how it is made, and why the bill can differ. */
export function costTooltip(bytesScanned: number | null | undefined): string {
  const basis =
    bytesScanned === null || bytesScanned === undefined
      ? ""
      : bytesScanned < MIN_BILLED_BYTES
        ? ` This query scanned ${humanBytes(bytesScanned)}, so the 10 MB minimum applies.`
        : ` Based on ${humanBytes(bytesScanned)} scanned.`;
  return `Estimate only: US$5 per TB scanned (Athena's list price) with a 10 MB minimum per query.${basis} The actual bill depends on your region and any discounts or commitments.`;
}

/** Tooltip of the data-scanned figure. */
export function scannedTooltip(engine: string, bytes: number): string {
  const exact = `${bytes.toLocaleString("en-US")} bytes`;
  switch (engine.toLowerCase()) {
    case "athena":
      return `Data scanned by Athena for this query (${exact}). Athena bills by data scanned.`;
    case "trino":
      return `Data processed by Trino for this query (${exact}).`;
    default:
      return `Data read from storage by this query (${exact}).`;
  }
}

/** First characters of an engine query id, enough to recognise it; the full id goes in the tooltip and the clipboard. */
export function shortQueryId(id: string, keep = 8): string {
  return id.length > keep + 1 ? `${id.slice(0, keep)}…` : id;
}

export interface StatsView {
  engine: EngineBadge;
  scanned: { text: string; title: string } | null;
  cost: { text: string; title: string } | null;
  queryId: { short: string; full: string } | null;
}

/** Everything the status line needs, derived from the stats event. */
export function statsView(stats: StatementStats): StatsView {
  return {
    engine: engineBadge(stats.engine),
    scanned: stats.bytesScanned === null ? null : { text: `${humanBytes(stats.bytesScanned)} scanned`, title: scannedTooltip(stats.engine, stats.bytesScanned) },
    cost: (() => {
      const text = formatCostUsd(stats.costUsdEstimate);
      return text === null ? null : { text, title: costTooltip(stats.bytesScanned) };
    })(),
    queryId: stats.engineQueryId ? { short: shortQueryId(stats.engineQueryId), full: stats.engineQueryId } : null,
  };
}
