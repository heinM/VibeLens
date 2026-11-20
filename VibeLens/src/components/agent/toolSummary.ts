// Presentation helpers for agent tool calls. Argument shapes come from the Rust agent, so
// everything here is defensive: unknown shapes fall back to compact JSON.
import type { ApprovalTarget, Environment } from "../../ipc/types";

export type ToolIconName = "list" | "schema" | "table" | "sql" | "key" | "files" | "file" | "write" | "other";

export function toolIconName(tool: string): ToolIconName {
  switch (tool) {
    case "list_connections":
      return "list";
    case "get_schema":
      return "schema";
    case "describe_table":
      return "table";
    case "run_sql":
      return "sql";
    case "valkey_scan":
    case "valkey_get":
    case "valkey_command":
      return "key";
    case "list_files":
      return "files";
    case "read_file":
      return "file";
    case "write_file":
      return "write";
    default:
      return "other";
  }
}

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);

function pick(args: unknown, ...keys: string[]): string | null {
  if (!isRecord(args)) return typeof args === "string" ? args : null;
  for (const k of keys) {
    const v = args[k];
    if (typeof v === "string" && v.trim()) return v;
    if (typeof v === "number") return String(v);
    if (Array.isArray(v) && v.every((x) => typeof x === "string" || typeof x === "number")) return v.join(" ");
  }
  return null;
}

const oneLine = (s: string, max: number) => {
  const t = s.replace(/\s+/g, " ").trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
};

export function compactJson(v: unknown, max = 100): string {
  try {
    return oneLine(JSON.stringify(v) ?? "", max);
  } catch {
    return "";
  }
}

/** One short line describing what a tool call does. */
export function summarizeToolArgs(tool: string, args: unknown): string {
  let s: string | null = null;
  switch (tool) {
    case "run_sql":
      s = pick(args, "sql", "query", "statement");
      break;
    case "describe_table": {
      const schema = pick(args, "schema");
      const table = pick(args, "table", "name");
      s = table ? (schema ? `${schema}.${table}` : table) : null;
      break;
    }
    case "valkey_scan":
      s = pick(args, "pattern", "match");
      break;
    case "valkey_get":
      s = pick(args, "key");
      break;
    case "valkey_command":
      s = pick(args, "argv", "command", "args");
      break;
    case "list_files":
    case "read_file":
    case "write_file":
      s = pick(args, "path", "dir", "directory");
      break;
    case "get_schema":
      s = pick(args, "schema");
      break;
    default:
      break;
  }
  return oneLine(s ?? compactJson(args), 120);
}

/** True when text clearly concerns production (for the red approval treatment). */
export const mentionsProd = (s: string | null | undefined): boolean => !!s && /\bprod(uction)?\b/i.test(s);

/** What the approval card's fixed header says about where a call runs. */
export interface TargetSummary {
  connection: string;
  environment: Environment;
  /** "database shop" (Postgres) or "db 3" (Valkey); null when the engine has neither. */
  where: string | null;
  /** Set when the statement runs somewhere other than the connection's default database. */
  warning: string | null;
}

/**
 * Turns the backend-resolved target into the card's header text. The model picks the database
 * (and the Valkey db number), so this is the user's only reliable view of where a call lands.
 */
export function describeTarget(target: ApprovalTarget | null | undefined): TargetSummary | null {
  if (!target) return null;
  let where: string | null = null;
  let warning: string | null = null;
  if (target.database != null) {
    where = `database ${target.database}`;
    if (target.defaultDatabase != null && target.defaultDatabase !== target.database) {
      warning = `Not this connection's default database (${target.defaultDatabase}).`;
    }
  } else if (target.valkeyDb != null) {
    where = `db ${target.valkeyDb}`;
  }
  return { connection: target.connection, environment: target.environment, where, warning };
}

export type PreviewKind = "sql" | "diff" | "text";

export function previewKind(tool: string, preview: string | null): PreviewKind {
  if (!preview) return "text";
  if (tool === "run_sql") return "sql";
  if (tool === "write_file" || /^(---|\+\+\+|@@|diff )/m.test(preview)) return "diff";
  return "text";
}

export function formatElapsed(ms: number | null): string {
  if (ms == null) return "";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(ms < 10_000 ? 1 : 0)} s`;
}

export function formatTokens(n: number): string {
  return n.toLocaleString("en-US");
}
