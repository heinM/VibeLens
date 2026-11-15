// "Copy as command": turn an inspected ValkeyValue back into commands that recreate it, plus small
// display helpers shared by the inspector.
import type { ValkeyValue } from "../../ipc/types";
import { formatArgv, type ArgvPart } from "./argv";

export function isBytesValue(v: unknown): v is { $bytes: string; len: number } {
  return typeof v === "object" && v !== null && !Array.isArray(v) && typeof (v as Record<string, unknown>).$bytes === "string";
}

/** A single element as a command argument (text stays text, binary stays binary). */
export function toArg(v: unknown): ArgvPart {
  if (typeof v === "string") return v;
  if (isBytesValue(v)) return { b64: v.$bytes };
  if (v === null || v === undefined) return "";
  if (typeof v === "number" || typeof v === "boolean" || typeof v === "bigint") return String(v);
  return JSON.stringify(v);
}

/** Text shown for an element in tables ("‹bytes n›" for binary). */
export function elementText(v: unknown): string {
  if (typeof v === "string") return v;
  if (isBytesValue(v)) return `‹bytes ${v.len}›`;
  if (v === null || v === undefined) return "";
  if (typeof v === "object") {
    try {
      return JSON.stringify(v);
    } catch {
      return String(v);
    }
  }
  return String(v);
}

/** Entries of a hash value: object, or an array of [field, value] pairs. */
export function hashEntries(value: unknown): [string, unknown][] {
  if (Array.isArray(value)) {
    return value.flatMap((e): [string, unknown][] => (Array.isArray(e) && e.length >= 2 ? [[String(e[0]), e[1]]] : []));
  }
  if (value && typeof value === "object" && !isBytesValue(value)) return Object.entries(value as Record<string, unknown>);
  return [];
}

export interface ZsetEntry {
  member: unknown;
  score: unknown;
}
export function zsetEntries(value: unknown): ZsetEntry[] {
  if (!Array.isArray(value)) return [];
  return value.map((e): ZsetEntry => {
    if (Array.isArray(e)) return { member: e[0], score: e[1] };
    if (e && typeof e === "object") {
      const o = e as Record<string, unknown>;
      return { member: o.member, score: o.score };
    }
    return { member: e, score: "" };
  });
}

export interface StreamEntry {
  id: string;
  fields: [string, unknown][];
}
export function streamEntries(value: unknown): StreamEntry[] {
  if (!Array.isArray(value)) return [];
  return value.map((e): StreamEntry => {
    if (Array.isArray(e)) return { id: String(e[0]), fields: hashEntries(e[1]) };
    const o = (e ?? {}) as Record<string, unknown>;
    return { id: String(o.id ?? ""), fields: hashEntries(o.fields) };
  });
}

/** argv lists that recreate the value (and its TTL). Empty when the type cannot be expressed. */
export function recreateArgv(v: ValkeyValue): ArgvPart[][] {
  const out: ArgvPart[][] = [];
  const key: ArgvPart = v.key;
  switch (v.keyType) {
    case "string":
      out.push(["SET", key, toArg(v.value)]);
      break;
    case "hash": {
      const flat = hashEntries(v.value).flatMap(([f, x]) => [f as ArgvPart, toArg(x)]);
      if (flat.length) out.push(["HSET", key, ...flat]);
      break;
    }
    case "list":
      if (Array.isArray(v.value) && v.value.length) out.push(["RPUSH", key, ...v.value.map(toArg)]);
      break;
    case "set":
      if (Array.isArray(v.value) && v.value.length) out.push(["SADD", key, ...v.value.map(toArg)]);
      break;
    case "zset": {
      const flat = zsetEntries(v.value).flatMap((e) => [toArg(e.score), toArg(e.member)]);
      if (flat.length) out.push(["ZADD", key, ...flat]);
      break;
    }
    case "stream":
      for (const e of streamEntries(v.value)) {
        if (e.fields.length) out.push(["XADD", key, e.id, ...e.fields.flatMap(([f, x]) => [f as ArgvPart, toArg(x)])]);
      }
      break;
    case "json":
      out.push(["JSON.SET", key, "$", JSON.stringify(v.value)]);
      break;
    default:
      break;
  }
  if (out.length > 0 && v.ttlMs > 0) out.push(["PEXPIRE", key, String(v.ttlMs)]);
  return out;
}

export function recreateCommands(v: ValkeyValue): string {
  return recreateArgv(v).map(formatArgv).join("\n");
}

/** 90061000 -> "1d 1h 1m", 4500 -> "4.5s". */
export function humanTtl(ttlMs: number): string {
  if (ttlMs === -1) return "No expiry";
  if (ttlMs < 0) return "Key not found";
  if (ttlMs < 1000) return `${ttlMs} ms`;
  const s = Math.floor(ttlMs / 1000);
  if (s < 60) return ttlMs < 10_000 ? `${(ttlMs / 1000).toFixed(1)} s` : `${s} s`;
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  const parts: string[] = [];
  if (d) parts.push(`${d}d`);
  if (h) parts.push(`${h}h`);
  if (m) parts.push(`${m}m`);
  if (!d && !h && sec) parts.push(`${sec}s`);
  return parts.slice(0, 3).join(" ");
}

export function humanBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
