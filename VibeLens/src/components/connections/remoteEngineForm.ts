// Pure helpers behind the Trino and Spark (Livy) query engines of an Iceberg connection: blank
// values, URL and cleartext checks, the Spark session configuration rules and the bookkeeping of the
// engine secret (password or bearer token in the vault). No React in here.
//
// These checks mirror the Rust rules (docs/REMOTE_ENGINES_AND_REDSHIFT.md, sections 1.4 and 2.4) so
// the user sees a problem while typing. Rust validates again on save and on use: it is the boundary.
import type { ConnectionProfile, HttpEngineAuth, IcebergQueryEngine } from "../../ipc/types";
import type { SecretEdit } from "./draft";

export type RemoteEngine = Extract<IcebergQueryEngine, { kind: "trino" | "spark" }>;
export type RemoteEngineKind = RemoteEngine["kind"];

export const isRemoteEngine = (q: IcebergQueryEngine | null | undefined): q is RemoteEngine => q?.kind === "trino" || q?.kind === "spark";

export const DEFAULT_SESSION_START_TIMEOUT_SECS = 300;
export const ENGINE_NAME: Record<RemoteEngineKind, string> = { trino: "Trino", spark: "Spark (Livy)" };

export function blankRemoteEngine(kind: RemoteEngineKind, prev: IcebergQueryEngine): RemoteEngine {
  // Carry what means the same thing on both (endpoint aside: a Trino coordinator is not a Livy server).
  const carry = isRemoteEngine(prev) ? { catalog: prev.catalog, auth: prev.auth, caCertPath: prev.caCertPath } : { catalog: "", auth: { kind: "none" } as HttpEngineAuth, caCertPath: null };
  if (kind === "trino") return { kind, url: "", user: "", ...carry };
  return { kind, url: "", ...carry, proxyUser: null, sessionConf: {}, sessionStartTimeoutSecs: null };
}

export function blankHttpAuth(kind: HttpEngineAuth["kind"], prev: HttpEngineAuth): HttpEngineAuth {
  if (prev.kind === kind) return prev;
  return kind === "basic" ? { kind, username: "" } : { kind };
}

// -- URLs and cleartext (R2) -------------------------------------------------------------------------

/** Same hosts the Rust side treats as this machine (see the LLM provider check in agent/store.rs). */
export function isLoopbackHost(hostname: string): boolean {
  const h = hostname.toLowerCase();
  return h === "localhost" || h === "127.0.0.1" || h === "[::1]" || h === "::1" || h.endsWith(".localhost");
}

/** An engine base URL: http(s), a host, and no credentials, query or fragment. Null when fine. */
export function engineUrlError(v: string, what: string): string | null {
  const t = v.trim();
  if (!t) return `${what} is required.`;
  let u: URL;
  try {
    u = new URL(t);
  } catch {
    return `${what} must be a full URL such as https://trino.example.com:8443.`;
  }
  if (u.protocol !== "http:" && u.protocol !== "https:") return `${what} must start with http:// or https://.`;
  if (!u.hostname) return `${what} needs a host name.`;
  if (u.username || u.password) return `${what} can't contain a user name or password. Use the sign-in settings below; secrets are kept in the OS keychain.`;
  if (u.search || t.includes("?")) return `${what} can't have a query string (?…).`;
  if (u.hash || t.includes("#")) return `${what} can't have a fragment (#…).`;
  return null;
}

export type CleartextProblem = { level: "error" | "warn"; message: string };

/**
 * What sending this engine's traffic over the URL would expose. A credential over http:// to another
 * machine is refused (Rust refuses it too); no credential over http:// to another machine is allowed but
 * the user is told that queries and results travel unencrypted.
 */
export function cleartextProblem(url: string, auth: HttpEngineAuth): CleartextProblem | null {
  let u: URL;
  try {
    u = new URL(url.trim());
  } catch {
    return null;
  }
  if (u.protocol !== "http:" || isLoopbackHost(u.hostname)) return null;
  if (auth.kind !== "none")
    return {
      level: "error",
      message: `A ${auth.kind === "basic" ? "password" : "token"} would be sent unencrypted to ${u.hostname}. Use an https:// URL, or http:// only to this computer (localhost).`,
    };
  return { level: "warn", message: `Queries and results to ${u.hostname} are not encrypted over http://. Use https:// unless the network path is private and trusted.` };
}

// -- Spark session configuration (R1, design 2.4) ---------------------------------------------------

/** Settings that load code into the cluster from a profile file that can be edited by hand. */
export const CODE_LOADING_CONF: ReadonlySet<string> = new Set([
  "spark.jars",
  "spark.jars.packages",
  "spark.files",
  "spark.submit.pyFiles",
  "spark.driver.extraJavaOptions",
  "spark.executor.extraJavaOptions",
  "spark.driver.extraClassPath",
  "spark.executor.extraClassPath",
]);

const CREDENTIAL_KEY_WORDS = ["secret", "password", "token", "credential"];

/** True when a key names a credential: contains secret / password / token / credential, or ends in `.key` / `access.key`. */
export function looksLikeCredentialKey(key: string): boolean {
  const k = key.trim().toLowerCase();
  return CREDENTIAL_KEY_WORDS.some((w) => k.includes(w)) || k.endsWith(".key") || k.endsWith("access.key");
}

/** Why one session setting is refused, or null when it is fine. Empty rows are ignored by the caller. */
export function sessionConfError(key: string, value: string): string | null {
  const k = key.trim();
  if (!k) return value.trim() ? "Give the setting a name, such as spark.sql.catalog.lake." : null;
  if (/\s/.test(k)) return `"${k}": a setting name can't contain spaces.`;
  if (CODE_LOADING_CONF.has(k)) return `${k} is refused: it loads code into the cluster from a profile file that can be edited by hand. Configure it on the Livy server or the cluster instead.`;
  if (looksLikeCredentialKey(k)) return `${k} looks like a credential. Secrets can't be stored in session settings (they are saved in plain text): configure it on the cluster instead.`;
  if (/^\s*bearer\s/i.test(value)) return `The value of ${k} looks like a bearer token. Secrets can't be stored in session settings: configure it on the cluster instead.`;
  return null;
}

export interface ConfRow {
  key: string;
  value: string;
}
export const confRows = (c: Record<string, string>): ConfRow[] => Object.entries(c).map(([key, value]) => ({ key, value }));
/** Blank names are dropped; later duplicates win (as for REST headers). */
export function confFromRows(rows: ConfRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const r of rows) if (r.key.trim()) out[r.key.trim()] = r.value;
  return out;
}
/** Row-level problem for the editor (adds duplicate detection to `sessionConfError`). */
export function confRowError(rows: ConfRow[], i: number): string | null {
  const r = rows[i];
  const e = sessionConfError(r.key, r.value);
  if (e) return e;
  if (r.key.trim() && rows.findIndex((x) => x.key.trim() === r.key.trim()) !== i) return `${r.key.trim()} is set twice.`;
  return null;
}

// -- validation ------------------------------------------------------------------------------------

/** Deliberately loose (Rust has the final say): one name, no whitespace, quotes, backticks or dots. */
const CATALOG_RE = /^[^\s"'`.]+$/;

/**
 * Field errors of a Trino or Spark engine, keyed `ice.engine.*` (the wizard's Query engine section).
 * `secretMissing`: basic / bearer auth with neither a typed nor a stored secret.
 */
export function validateRemoteEngine(q: RemoteEngine, secretMissing = false): Record<string, string> {
  const e: Record<string, string> = {};
  const what = q.kind === "trino" ? "Coordinator URL" : "Livy URL";
  const u = engineUrlError(q.url, what);
  if (u) e["ice.engine.url"] = u;
  else {
    const c = cleartextProblem(q.url, q.auth);
    if (c?.level === "error") e["ice.engine.url"] = c.message;
  }
  const cat = q.catalog.trim();
  if (!cat) e["ice.engine.catalogName"] = q.kind === "trino" ? "Catalog is required: the Trino catalog that points at this Iceberg catalog." : "Catalog is required: the Spark catalog configured for this Iceberg catalog.";
  else if (!CATALOG_RE.test(cat)) e["ice.engine.catalogName"] = "Use the catalog name only: no spaces, quotes or dots.";
  if (q.kind === "trino" && !q.user.trim()) e["ice.engine.user"] = "User is required (sent as X-Trino-User).";
  if (q.auth.kind === "basic" && !q.auth.username.trim()) e["ice.engine.username"] = "User name is required for basic authentication.";
  if (secretMissing && q.auth.kind !== "none") e["ice.engine.secret"] = q.auth.kind === "basic" ? "Enter the password." : "Enter the bearer token.";
  if (q.caCertPath != null && q.caCertPath !== "" && !q.caCertPath.trim()) e["ice.engine.caCertPath"] = "Path is blank.";
  if (q.kind === "spark") {
    if (q.proxyUser != null && /\s/.test(q.proxyUser.trim())) e["ice.engine.proxyUser"] = "A proxy user can't contain spaces.";
    const rows = confRows(q.sessionConf);
    for (let i = 0; i < rows.length; i++) {
      const ce = confRowError(rows, i);
      if (ce) {
        e["ice.engine.sessionConf"] = ce;
        break;
      }
    }
    const t = q.sessionStartTimeoutSecs;
    if (t != null && (!Number.isInteger(t) || t < 1 || t > 86_400)) e["ice.engine.timeout"] = "1-86400 seconds, or leave empty for 300.";
  }
  return e;
}

// -- the engine secret ------------------------------------------------------------------------------

/**
 * What the engine-secret vault slot holds for this query engine: `trino:basic`, `spark:bearer`, …
 * Null when the engine needs none. A change of kind means the stored value means something else.
 */
export function engineSecretKind(p: Pick<ConnectionProfile, "engine" | "iceberg"> | null | undefined): string | null {
  if (!p || p.engine !== "iceberg") return null;
  const q = p.iceberg?.queryEngine;
  if (!isRemoteEngine(q) || q.auth.kind === "none") return null;
  return `${q.kind}:${q.auth.kind}`;
}

/** `saveProfile`'s `engineSecret`: undefined keeps, "" deletes, a value stores. Stale secrets are deleted. */
export function engineSecretArg(original: ConnectionProfile | null, profile: ConnectionProfile, edit: SecretEdit): string | undefined {
  const kind = engineSecretKind(profile);
  const prevKind = engineSecretKind(original);
  if (!kind) return prevKind ? "" : undefined;
  const typed = edit.mode === "keep" ? undefined : edit.mode === "remove" ? "" : edit.value === "" ? undefined : edit.value;
  // Basic -> bearer, or Trino -> Spark: the stored value was for something else.
  if (prevKind && prevKind !== kind && typed === undefined) return "";
  return typed;
}

/** True when the engine needs a secret and none is typed or kept from the vault. */
export function engineSecretMissing(original: ConnectionProfile | null, profile: ConnectionProfile, edit: SecretEdit, hasStored: boolean | undefined): boolean {
  const kind = engineSecretKind(profile);
  if (!kind) return false;
  const kept = !!hasStored && engineSecretKind(original) === kind && edit.mode === "keep";
  return !kept && !(edit.mode === "set" && edit.value);
}

/** Whether a stored secret can be shown as kept for this engine (same engine and auth as when it was saved). */
export function engineSecretStored(original: ConnectionProfile | null, profile: ConnectionProfile, hasStored: boolean | undefined): boolean {
  const kind = engineSecretKind(profile);
  return !!kind && !!hasStored && engineSecretKind(original) === kind;
}

/** Trim and null-out optional text before persisting. */
export function cleanRemoteEngine(q: RemoteEngine): RemoteEngine {
  const nz = (v: string | null | undefined): string | null => (v && v.trim() ? v.trim() : null);
  const auth: HttpEngineAuth = q.auth.kind === "basic" ? { kind: "basic", username: q.auth.username.trim() } : q.auth;
  const base = { url: q.url.trim().replace(/\/+$/, ""), catalog: q.catalog.trim(), auth, caCertPath: nz(q.caCertPath) };
  if (q.kind === "trino") return { ...q, ...base, user: q.user.trim() };
  const sessionConf: Record<string, string> = {};
  for (const [k, v] of Object.entries(q.sessionConf)) if (k.trim()) sessionConf[k.trim()] = v.trim();
  return { ...q, ...base, proxyUser: nz(q.proxyUser), sessionConf };
}

export function remoteEngineSummary(q: RemoteEngine): string {
  const host = (() => {
    try {
      return new URL(q.url.trim()).host;
    } catch {
      return "";
    }
  })();
  return `${q.kind === "trino" ? "Trino" : "Spark"}${host ? ` · ${host}` : ""}`;
}
