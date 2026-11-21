import { newProfile, usesHostPort } from "../../lib/defaults";
import type { AuthMethod, ConnectionProfile, EngineKind } from "../../ipc/types";
import { cleanIceberg, secretKind, secretRequired } from "./icebergForm";
import { cleanRedshift, normalizeRedshift } from "./redshiftForm";
import { engineSecretArg, engineSecretMissing } from "./remoteEngineForm";

/** How a write-only secret (password, SSH passphrase) is treated on save. */
export interface SecretEdit {
  mode: "keep" | "set" | "remove";
  value: string;
}

/** Static S3 keys are saved and deleted as a pair. */
export interface S3KeysEdit {
  mode: "keep" | "set" | "remove";
  accessKeyId: string;
  secretAccessKey: string;
}

export interface Draft {
  profile: ConnectionProfile;
  password: SecretEdit;
  sshSecret: SecretEdit;
  /** REST bearer token / OAuth2 client secret / SQL catalog (Postgres) password. */
  icebergSecret: SecretEdit;
  s3Keys: S3KeysEdit;
  /** Trino / Spark (Livy) password or bearer token (`saveProfile`'s `engineSecret`). */
  engineSecret: SecretEdit;
}

export const keepSecret = (): SecretEdit => ({ mode: "keep", value: "" });
export const newSecret = (): SecretEdit => ({ mode: "set", value: "" });

/** undefined = keep, "" = delete, otherwise store. */
export function secretArg(edit: SecretEdit): string | undefined {
  if (edit.mode === "keep") return undefined;
  if (edit.mode === "remove") return "";
  return edit.value === "" ? undefined : edit.value;
}

export const keepS3Keys = (): S3KeysEdit => ({ mode: "keep", accessKeyId: "", secretAccessKey: "" });
export const newS3Keys = (): S3KeysEdit => ({ mode: "set", accessKeyId: "", secretAccessKey: "" });

export function initialDraft(existing: ConnectionProfile | null, engine: EngineKind = "postgres"): Draft {
  if (existing) return { profile: structuredClone(existing), password: keepSecret(), sshSecret: keepSecret(), icebergSecret: keepSecret(), s3Keys: keepS3Keys(), engineSecret: keepSecret() };
  return { profile: newProfile(engine), password: newSecret(), sshSecret: newSecret(), icebergSecret: newSecret(), s3Keys: newS3Keys(), engineSecret: newSecret() };
}

/** Switching engine on a new connection resets port, auth, TLS and options but keeps identity fields. */
export function switchEngine(p: ConnectionProfile, engine: EngineKind): ConnectionProfile {
  if (p.engine === engine) return p;
  const fresh = newProfile(engine);
  return {
    ...fresh,
    id: p.id,
    name: p.name,
    host: p.host,
    environment: p.environment,
    aiDataAccess: p.aiDataAccess,
    color: p.color,
    // Iceberg and Redshift have no SSH tunnel option: catalogs, object stores and AWS APIs are reached directly.
    tunnel: usesHostPort(engine) ? p.tunnel : null,
  };
}

/** Mirrors `AuthMethod::uses_stored_password`: the keychain password slot holds a password or a pasted access token. */
export const usesPassword = (a: AuthMethod) => a.kind === "password" || (a.kind === "accessToken" && !a.command?.trim());

export interface SecretArgs {
  password: string | undefined;
  sshSecret: string | undefined;
}

/**
 * Translate the draft into the `password` / `sshSecret` arguments for `saveProfile`.
 * When an edit switches away from a mode that used a stored secret, the stale secret is deleted.
 */
export function secretArgs(original: ConnectionProfile | null, draft: Draft): SecretArgs {
  const p = draft.profile;
  let password = usesPassword(p.auth) ? secretArg(draft.password) : undefined;
  if (!usesPassword(p.auth) && original && usesPassword(original.auth)) password = "";
  // Password -> pasted token (or back): the stored secret means something else now.
  if (usesPassword(p.auth) && original && usesPassword(original.auth) && original.auth.kind !== p.auth.kind && password === undefined) password = "";

  const tunnelNeedsSecret = !!p.tunnel && p.tunnel.auth.kind !== "agent";
  let sshSecret = tunnelNeedsSecret ? secretArg(draft.sshSecret) : undefined;
  const hadSecret = !!original?.tunnel && original.tunnel.auth.kind !== "agent";
  if (!tunnelNeedsSecret && hadSecret) sshSecret = "";
  // Passphrase -> password (or back): the old secret means something else now.
  if (tunnelNeedsSecret && hadSecret && original?.tunnel?.auth.kind !== p.tunnel?.auth.kind && sshSecret === undefined) sshSecret = "";
  return { password, sshSecret };
}

/**
 * What the test button passes as `password`. Only the typed or removed state matters.
 * `test_connection` has a single secret slot, so for Iceberg it carries the catalog secret.
 */
export function testPasswordArg(draft: Draft): string | undefined {
  if (draft.profile.engine === "iceberg") return secretArg(draft.icebergSecret);
  return usesPassword(draft.profile.auth) ? secretArg(draft.password) : undefined;
}

export interface IcebergSecretArgs {
  icebergSecret: string | undefined;
  /** Trino / Spark password or token: undefined keeps, "" deletes. */
  engineSecret: string | undefined;
  /** `{ "", "" }` deletes the stored pair; undefined keeps it. */
  s3Keys: { accessKeyId: string; secretAccessKey: string } | undefined;
}

/**
 * Translate the draft into the `icebergSecret` / `s3Keys` arguments for `saveProfile`.
 * Secrets that the current catalog or storage no longer uses are deleted so stale keychain entries don't linger.
 */
export function icebergSecretArgs(original: ConnectionProfile | null, draft: Draft): IcebergSecretArgs {
  const o = draft.profile.iceberg;
  const prev = original?.iceberg ?? null;
  // Leaving Iceberg (or its remote engine) deletes a stored engine secret.
  const engineSecret = engineSecretArg(original, draft.profile, draft.engineSecret);
  if (draft.profile.engine !== "iceberg" || !o) return { icebergSecret: undefined, s3Keys: undefined, engineSecret };

  const kind = secretKind(o.catalog);
  const prevKind = prev ? secretKind(prev.catalog) : null;
  let icebergSecret = kind ? secretArg(draft.icebergSecret) : undefined;
  if (!kind && prevKind) icebergSecret = "";
  // Bearer token -> OAuth2 secret (or back): the stored value means something else now.
  if (kind && prevKind && kind !== prevKind && icebergSecret === undefined) icebergSecret = "";

  const wantsKeys = o.storage.kind === "s3Static";
  const hadKeys = prev?.storage.kind === "s3Static";
  const k = draft.s3Keys;
  let s3Keys: IcebergSecretArgs["s3Keys"];
  if (wantsKeys) {
    if (k.mode === "remove") s3Keys = { accessKeyId: "", secretAccessKey: "" };
    else if (k.mode === "set" && k.accessKeyId && k.secretAccessKey) s3Keys = { accessKeyId: k.accessKeyId, secretAccessKey: k.secretAccessKey };
  } else if (hadKeys) {
    s3Keys = { accessKeyId: "", secretAccessKey: "" };
  }
  return { icebergSecret, s3Keys, engineSecret };
}

/**
 * Which required Iceberg secrets are still missing, given what is stored in the keychain.
 * Feeds `validateProfile` so the wizard can flag an empty token before a test or save.
 */
export function missingIcebergSecrets(
  original: ConnectionProfile | null,
  draft: Draft,
  stored: { hasIcebergSecret: boolean; hasS3Keys: boolean; hasEngineSecret?: boolean } | undefined,
): { catalog: boolean; s3Keys: boolean; engine: boolean } {
  const o = draft.profile.iceberg;
  if (draft.profile.engine !== "iceberg" || !o) return { catalog: false, s3Keys: false, engine: false };
  const kind = secretKind(o.catalog);
  const prevKind = original?.iceberg ? secretKind(original.iceberg.catalog) : null;
  const kept = !!stored?.hasIcebergSecret && prevKind === kind && draft.icebergSecret.mode === "keep";
  const catalog = secretRequired(o.catalog) && !kept && !draft.icebergSecret.value;

  const k = draft.s3Keys;
  const keysKept = !!stored?.hasS3Keys && original?.iceberg?.storage.kind === "s3Static" && k.mode === "keep";
  const s3Keys = o.storage.kind === "s3Static" && !keysKept && !(k.accessKeyId && k.secretAccessKey);
  const engine = engineSecretMissing(original, draft.profile, draft.engineSecret, stored?.hasEngineSecret);
  return { catalog, s3Keys, engine };
}

/** Trims text fields so we never persist stray whitespace. */
export function cleanProfile(p: ConnectionProfile): ConnectionProfile {
  const c = structuredClone(p);
  c.name = c.name.trim();
  c.host = c.host.trim();
  if (c.database != null) c.database = c.database.trim();
  c.tls.caCertPath = c.tls.caCertPath?.trim() ? c.tls.caCertPath.trim() : null;
  if (c.tunnel) {
    c.tunnel.host = c.tunnel.host.trim();
    c.tunnel.username = c.tunnel.username.trim();
    if (c.tunnel.auth.kind === "privateKey") c.tunnel.auth = { kind: "privateKey", path: c.tunnel.auth.path.trim() };
  }
  if (c.iceberg) c.iceberg = cleanIceberg(c.iceberg);
  if (c.redshift) c.redshift = cleanRedshift(normalizeRedshift(c.redshift));
  const a = c.auth;
  if (a.kind === "secretsManager") a.secretArn = a.secretArn.trim();
  if (a.kind === "rdsIam" || a.kind === "elastiCacheIam") a.username = a.username.trim();
  if (a.kind === "password") a.username = a.username.trim();
  if (a.kind === "accessToken") {
    a.username = a.username.trim();
    a.command = a.command?.trim() ? a.command.trim() : null;
  }
  if (c.pg.pgproxy) c.pg.pgproxy = { ...c.pg.pgproxy, healthUrl: c.pg.pgproxy.healthUrl?.trim() ? c.pg.pgproxy.healthUrl.trim().replace(/\/+$/, "") : null };
  if (a.kind === "elastiCacheIam") a.cacheName = a.cacheName.trim();
  if ("aws" in a) {
    a.aws.region = a.aws.region.trim();
    if (a.aws.identity.kind === "sso") a.aws.identity.startUrl = a.aws.identity.startUrl.trim();
  }
  return c;
}
