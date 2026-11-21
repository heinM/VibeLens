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
/** Rust's `MAX_SESSION_START_TIMEOUT_SECS`. */
export const MAX_SESSION_START_TIMEOUT_SECS = 3600;
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

/**
 * Same hosts the Rust side treats as this machine for the engines: `localhost` and literal loopback
 * addresses (all of 127.0.0.0/8, and ::1). Not `*.localhost` names: those are resolved like any other.
 * Takes a URL hostname (IPv6 in brackets, as `URL` gives it) or a bare address.
 */
export function isLoopbackHost(hostname: string): boolean {
  const h = hostname.toLowerCase();
  if (h === "localhost") return true;
  const v4 = /^127\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(h);
  if (v4) return v4.slice(1).every((o) => Number(o) <= 255);
  const v6 = h.startsWith("[") && h.endsWith("]") ? h.slice(1, -1) : h;
  return v6 === "::1" || /^(0{1,4}:){7}0{0,3}1$/.test(v6);
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
//
// An ALLOW-list, mirrored exactly from Rust: anything not listed (code loading, credentials, any other
// Spark setting) belongs on the cluster, not in a plain-text profile that can be edited by hand.

/** Implementations `spark.sql.catalog.<catalog>` may name. */
export const SPARK_CATALOG_IMPLS: readonly string[] = ["org.apache.iceberg.spark.SparkCatalog", "org.apache.iceberg.spark.SparkSessionCatalog"];
/** Options allowed as `spark.sql.catalog.<catalog>.<option>`. */
// `type` and `warehouse` are left out on purpose, as in Rust: a hand-edited profile could point the
// catalog at another store (`type=hadoop`, `warehouse=file:///`). They are set on the cluster.
export const SPARK_CATALOG_OPTIONS: readonly string[] = ["default-namespace", "cache-enabled", "cache.expiration-interval-ms"];
/** A Spark catalog name, as Rust's `is_spark_catalog_name`: it becomes part of `spark.sql.catalog.<name>` keys. */
export const SPARK_CATALOG_NAME_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
/** Session settings allowed whatever the catalog is. */
export const SPARK_ALLOWED_CONF: ReadonlySet<string> = new Set([
  "spark.sql.shuffle.partitions",
  "spark.sql.session.timeZone",
  "spark.sql.adaptive.enabled",
  "spark.sql.adaptive.coalescePartitions.enabled",
  "spark.executor.memory",
  "spark.executor.cores",
  "spark.executor.instances",
  "spark.driver.memory",
  "spark.driver.cores",
  "spark.dynamicAllocation.enabled",
  "spark.dynamicAllocation.minExecutors",
  "spark.dynamicAllocation.maxExecutors",
  "spark.dynamicAllocation.initialExecutors",
]);
export const MAX_SESSION_CONF_VALUE = 512;

const CREDENTIAL_KEY_WORDS = ["secret", "password", "token", "credential"];

/** True when a key names a credential: contains secret / password / token / credential, or ends in `.key` / `access.key`. */
export function looksLikeCredentialKey(key: string): boolean {
  const k = key.trim().toLowerCase();
  return CREDENTIAL_KEY_WORDS.some((w) => k.includes(w)) || k.endsWith(".key") || k.endsWith("access.key");
}

/** A value that is a credential (a bearer token) rather than a setting. */
export const looksLikeCredentialValue = (value: string): boolean => /^\s*bearer /i.test(value);

// eslint-disable-next-line no-control-regex
const CONTROL = /[\u0000-\u001f\u007f-\u009f]/;

/**
 * Why one session setting is refused, or null when it is allowed. `catalog` is the engine's catalog
 * field (the `<C>` of `spark.sql.catalog.<C>`). A row with neither name nor value is ignored by callers.
 */
export function sessionConfError(key: string, value: string, catalog: string): string | null {
  const k = key.trim();
  if (!k) return value.trim() ? "Give the setting a name, or remove the row." : null;
  const c = catalog.trim();
  const catKey = `spark.sql.catalog.${c}`;
  const isCatalogImpl = c !== "" && k === catKey;
  const isCatalogOption = c !== "" && k.startsWith(`${catKey}.`) && SPARK_CATALOG_OPTIONS.includes(k.slice(catKey.length + 1));
  if (!isCatalogImpl && !isCatalogOption && !SPARK_ALLOWED_CONF.has(k)) {
    return `${k} is not a setting VibeLens passes to a Spark session: set this on the cluster instead.`;
  }
  if (CONTROL.test(value)) return `The value of ${k} contains a control character.`;
  if (value.length > MAX_SESSION_CONF_VALUE) return `The value of ${k} is longer than ${MAX_SESSION_CONF_VALUE} characters.`;
  if (value.includes("${")) return `The value of ${k} can't contain \${ (variable substitution): set this on the cluster instead.`;
  if (looksLikeCredentialValue(value)) return `The value of ${k} looks like a credential, and the profile is saved as plain text: set this on the cluster instead.`;
  if (isCatalogImpl && !SPARK_CATALOG_IMPLS.includes(value.trim())) return `${k} must be ${SPARK_CATALOG_IMPLS.join(" or ")}.`;
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
export function confRowError(rows: ConfRow[], i: number, catalog: string): string | null {
  const r = rows[i];
  const e = sessionConfError(r.key, r.value, catalog);
  if (e) return e;
  if (r.key.trim() && rows.findIndex((x) => x.key.trim() === r.key.trim()) !== i) return `${r.key.trim()} is set twice.`;
  return null;
}

/**
 * The first problem among the editor's rows, including what the saved map cannot hold (a value with
 * no name, a name set twice): those rows would otherwise be dropped silently on save.
 */
export function confRowsError(rows: readonly ConfRow[], catalog: string): string | null {
  for (let i = 0; i < rows.length; i++) {
    const e = confRowError(rows as ConfRow[], i, catalog);
    if (e) return e;
  }
  return null;
}

// -- validation ------------------------------------------------------------------------------------

/** Rust's `plain_name`: not empty, no control characters, at most 256 bytes. Null when fine. */
export function plainNameError(value: string, what: string): string | null {
  if (!value.trim()) return `${what} is required.`;
  if (CONTROL.test(value)) return `${what} contains a control character.`;
  if (new TextEncoder().encode(value).length > 256) return `${what} is longer than 256 bytes.`;
  return null;
}

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
  else {
    const ce = plainNameError(q.catalog, "Catalog");
    if (ce) e["ice.engine.catalogName"] = ce;
    else if (q.kind === "spark" && !SPARK_CATALOG_NAME_RE.test(cat)) e["ice.engine.catalogName"] = "A Spark catalog name has only letters, digits and _ and does not start with a digit.";
  }
  if (q.kind === "trino") {
    const ue = q.user.trim() ? plainNameError(q.user, "User") : "User is required (sent as X-Trino-User).";
    if (ue) e["ice.engine.user"] = ue;
  }
  if (q.auth.kind === "basic") {
    const be = q.auth.username.trim() ? plainNameError(q.auth.username, "User name") : "User name is required for basic authentication.";
    if (be) e["ice.engine.username"] = be;
    else if (q.auth.username.includes(":")) e["ice.engine.username"] = "A user name can't contain ':' (HTTP basic authentication cannot carry it).";
  }
  if (secretMissing && q.auth.kind !== "none") e["ice.engine.secret"] = q.auth.kind === "basic" ? "Enter the password." : "Enter the bearer token.";
  if (q.caCertPath != null && q.caCertPath !== "" && !q.caCertPath.trim()) e["ice.engine.caCertPath"] = "Path is blank.";
  if (q.kind === "spark") {
    if (q.proxyUser != null && q.proxyUser.trim()) {
      const pe = plainNameError(q.proxyUser, "Proxy user");
      if (pe) e["ice.engine.proxyUser"] = pe;
    }
    const ce = confRowsError(confRows(q.sessionConf), q.catalog);
    if (ce) e["ice.engine.sessionConf"] = ce;
    const t = q.sessionStartTimeoutSecs;
    if (t != null && (!Number.isInteger(t) || t < 1 || t > MAX_SESSION_START_TIMEOUT_SECS)) e["ice.engine.timeout"] = `1-${MAX_SESSION_START_TIMEOUT_SECS} seconds, or leave empty for 300.`;
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

/**
 * The endpoint a stored engine secret is bound to, as Rust binds it: engine kind, scheme, host, port
 * (the scheme's default when absent), path prefix and CA path. Null when the URL does not parse.
 */
export function engineSecretBinding(p: Pick<ConnectionProfile, "engine" | "iceberg"> | null | undefined): string | null {
  if (!p || p.engine !== "iceberg") return null;
  const q = p.iceberg?.queryEngine;
  if (!isRemoteEngine(q)) return null;
  let u: URL;
  try {
    u = new URL(q.url.trim());
  } catch {
    return null;
  }
  const port = u.port || (u.protocol === "https:" ? "443" : u.protocol === "http:" ? "80" : "");
  const path = u.pathname.replace(/\/+$/, "");
  return JSON.stringify([q.kind, u.protocol, u.hostname.toLowerCase(), port, path, q.caCertPath?.trim() || null]);
}

/**
 * True when the saved profile had a secret-bearing engine and the edit moved its endpoint (engine,
 * scheme, host, port, path prefix or CA path). Rust then refuses to send the stored secret there and
 * deletes it unless a new one is typed, so the form asks for it again.
 */
export function engineEndpointChanged(original: ConnectionProfile | null, profile: ConnectionProfile): boolean {
  if (!engineSecretKind(original) || !engineSecretKind(profile)) return false;
  return engineSecretBinding(original) !== engineSecretBinding(profile);
}

/** `saveProfile`'s `engineSecret`: undefined keeps, "" deletes, a value stores. Stale secrets are deleted. */
export function engineSecretArg(original: ConnectionProfile | null, profile: ConnectionProfile, edit: SecretEdit): string | undefined {
  const kind = engineSecretKind(profile);
  const prevKind = engineSecretKind(original);
  if (!kind) return prevKind ? "" : undefined;
  const typed = edit.mode === "keep" ? undefined : edit.mode === "remove" ? "" : edit.value === "" ? undefined : edit.value;
  // Basic -> bearer, Trino -> Spark, or a new endpoint: the stored value was for something else.
  if (prevKind && (prevKind !== kind || engineEndpointChanged(original, profile)) && typed === undefined) return "";
  return typed;
}

/** Whether a stored secret can be shown as kept: same engine and auth, and the same endpoint, as when it was saved. */
export function engineSecretStored(original: ConnectionProfile | null, profile: ConnectionProfile, hasStored: boolean | undefined): boolean {
  const kind = engineSecretKind(profile);
  return !!kind && !!hasStored && engineSecretKind(original) === kind && !engineEndpointChanged(original, profile);
}

/** True when the engine needs a secret and none is typed or kept from the vault. */
export function engineSecretMissing(original: ConnectionProfile | null, profile: ConnectionProfile, edit: SecretEdit, hasStored: boolean | undefined): boolean {
  if (!engineSecretKind(profile)) return false;
  const kept = engineSecretStored(original, profile, hasStored) && edit.mode === "keep";
  return !kept && !(edit.mode === "set" && edit.value);
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
