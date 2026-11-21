// Pure helpers behind the Redshift part of the connection wizard: switching target and auth, the
// identifier checks (design 3.4: validated before anything is signed and sent to AWS), trimming and
// one-line summaries. No React in here. Rust validates again: it is the boundary.
import type { RedshiftAuth, RedshiftOptions, RedshiftTarget } from "../../ipc/types";
import { SECRET_ARN_RE, awsTargetError } from "./awsHelpers";

/** Provisioned cluster identifier: 1-63 lowercase letters, digits or hyphens; starts with a letter; no `--`; no trailing hyphen. */
export const CLUSTER_ID_RE = /^[a-z](?:[a-z0-9]|-(?!-))*$/;
/** Serverless workgroup name: 3-64 lowercase letters, digits or hyphens. */
export const WORKGROUP_RE = /^[a-z0-9-]{3,64}$/;
/** Database name: 1-64 characters, lowercase letters, digits, `_` or `$`, starting with a letter or `_`. */
export const DATABASE_RE = /^[a-z_][a-z0-9_$]{0,63}$/;
/** Database user for temporary credentials: 1-64 characters, a letter first, then letters, digits, `_ + . @ -`. */
export const DB_USER_RE = /^[A-Za-z][A-Za-z0-9_+.@-]{0,63}$/;

export function clusterIdError(v: string): string | null {
  const t = v.trim();
  if (!t) return "Cluster identifier is required.";
  if (t.length > 63 || !CLUSTER_ID_RE.test(t) || t.endsWith("-")) return "Use 1-63 lowercase letters, digits or hyphens, starting with a letter, with no -- and no hyphen at the end.";
  return null;
}

export function workgroupError(v: string): string | null {
  const t = v.trim();
  if (!t) return "Workgroup name is required.";
  if (!WORKGROUP_RE.test(t)) return "Use 3-64 lowercase letters, digits or hyphens.";
  return null;
}

export function databaseError(v: string): string | null {
  const t = v.trim();
  if (!t) return "Database is required (dev is Redshift's default).";
  if (!DATABASE_RE.test(t)) return "Use 1-64 lowercase letters, digits, _ or $, starting with a letter or _.";
  return null;
}

export function dbUserError(v: string): string | null {
  const t = v.trim();
  if (!t) return "Database user is required.";
  if (!DB_USER_RE.test(t)) return "Use 1-64 letters, digits or _ + . @ -, starting with a letter.";
  return null;
}

export function blankTarget(kind: RedshiftTarget["kind"], prev: RedshiftTarget): RedshiftTarget {
  if (prev.kind === kind) return prev;
  return kind === "provisioned" ? { kind, clusterIdentifier: "" } : { kind, workgroupName: "" };
}

export function blankRedshiftAuth(kind: RedshiftAuth["kind"], prev: RedshiftAuth): RedshiftAuth {
  if (prev.kind === kind) return prev;
  if (kind === "dbUser") return { kind, dbUser: "" };
  if (kind === "secret") return { kind, secretArn: "" };
  return { kind };
}

/** A database user only works with provisioned clusters (`GetClusterCredentials`). */
export const authAllowed = (auth: RedshiftAuth["kind"], target: RedshiftTarget["kind"]): boolean => auth !== "dbUser" || target === "provisioned";

/** Keep target and auth consistent: switching to serverless drops a database-user sign-in back to IAM. */
export function normalizeRedshift(o: RedshiftOptions): RedshiftOptions {
  if (!authAllowed(o.auth.kind, o.target.kind)) return { ...o, auth: { kind: "iamIdentity" } };
  return o;
}

/** Field errors keyed `rs.*` (the wizard's Warehouse section). */
export function validateRedshift(o: RedshiftOptions): Record<string, string> {
  const e: Record<string, string> = {};
  const t = o.target;
  const te = t.kind === "provisioned" ? clusterIdError(t.clusterIdentifier) : workgroupError(t.workgroupName);
  if (te) e[t.kind === "provisioned" ? "rs.clusterIdentifier" : "rs.workgroupName"] = te;
  const de = databaseError(o.database);
  if (de) e["rs.database"] = de;
  for (const [k, v] of Object.entries(awsTargetError(o.aws))) e[`rs.${k}`] = v;
  const a = o.auth;
  if (a.kind === "dbUser") {
    if (t.kind !== "provisioned") e["rs.dbUser"] = "A database user works with provisioned clusters only. Serverless uses your IAM identity or a secret.";
    else {
      const ue = dbUserError(a.dbUser);
      if (ue) e["rs.dbUser"] = ue;
    }
  }
  if (a.kind === "secret") {
    const arn = a.secretArn.trim();
    const m = REDSHIFT_SECRET_ARN_RE.exec(arn);
    if (!arn) e["rs.secretArn"] = "Secret ARN is required.";
    else if (!SECRET_ARN_RE.test(arn) || !m) e["rs.secretArn"] = "Expected arn:aws:secretsmanager:<region>:<12-digit account>:secret:<name>, the name using letters, digits and / _ + = . @ -.";
    else if (o.aws.region.trim() && m[1] !== o.aws.region.trim()) e["rs.secretArn"] = `The secret is in ${m[1]}, but this connection's AWS region is ${o.aws.region.trim()}. Use a secret in the same region.`;
  }
  return e;
}

/** A Secrets Manager ARN whose name part is limited to what Rust accepts. Group 1 is the region. */
export const REDSHIFT_SECRET_ARN_RE = /^arn:aws[a-z-]*:secretsmanager:([a-z0-9-]+):\d{12}:secret:[A-Za-z0-9/_+=.@-]+$/;

/** A database requested for a Redshift run (target picker, history): ASCII letters, digits, `_`, `$`. */
export const isRedshiftDatabaseName = (v: string): boolean => /^[A-Za-z0-9_$]+$/.test(v);

/** Why a tab's requested Redshift database can't be sent (null = fine; null database = the connection's own). */
export function requestedDatabaseError(db: string | null | undefined): string | null {
  if (db == null) return null;
  return isRedshiftDatabaseName(db) ? null : `"${db}" is not a Redshift database name VibeLens will send (ASCII letters, digits, _ and $ only). Choose another database for this tab.`;
}

export function cleanRedshift(o: RedshiftOptions): RedshiftOptions {
  const c = structuredClone(o);
  if (c.target.kind === "provisioned") c.target.clusterIdentifier = c.target.clusterIdentifier.trim();
  else c.target.workgroupName = c.target.workgroupName.trim();
  c.database = c.database.trim();
  c.aws.region = c.aws.region.trim();
  if (c.aws.identity.kind === "sso") c.aws.identity.startUrl = c.aws.identity.startUrl.trim();
  if (c.auth.kind === "dbUser") c.auth.dbUser = c.auth.dbUser.trim();
  if (c.auth.kind === "secret") c.auth.secretArn = c.auth.secretArn.trim();
  return c;
}

/** The cluster or workgroup name, for lists and titles. */
export const redshiftTargetName = (t: RedshiftTarget): string => (t.kind === "provisioned" ? t.clusterIdentifier : t.workgroupName);

/** `Redshift · analytics · dev · eu-west-1`, the shape the approval card uses (design 3.4, R7). */
export function redshiftSummary(o: RedshiftOptions | null | undefined): string {
  if (!o) return "Amazon Redshift";
  return ["Redshift", redshiftTargetName(o.target) || (o.target.kind === "serverless" ? "no workgroup" : "no cluster"), o.database || null, o.aws.region || null].filter(Boolean).join(" · ");
}

export function redshiftAuthLabel(a: RedshiftAuth): string {
  return a.kind === "iamIdentity" ? "IAM identity" : a.kind === "dbUser" ? `Database user ${a.dbUser || "…"}` : "Secrets Manager";
}
