import type { AwsTarget, ConnectionProfile, IcebergOptions } from "../../ipc/types";
import { REGION_RE, SECRET_ARN_RE, awsTargetError } from "./awsHelpers";
import { isHostKeyFingerprint } from "./hostKey";
import { S3_TABLES_ARN_RE, headerRowError, headerRows, httpUrlError, isAwsCatalog, isS3Url, sqlFlavour } from "./icebergForm";

export type SectionId = "general" | "auth" | "network" | "tls" | "ai" | "advanced" | "catalog" | "storage" | "queryEngine";
export type FieldErrors = Record<string, string>;

export function sectionOf(field: string): SectionId {
  if (field.startsWith("ice.catalog")) return "catalog";
  if (field.startsWith("ice.storage")) return "storage";
  if (field.startsWith("ice.engine")) return "queryEngine";
  if (field.startsWith("auth") || field.startsWith("aws")) return "auth";
  if (field.startsWith("tunnel")) return "network";
  if (field.startsWith("tls")) return "tls";
  if (field.startsWith("pg.") || field.startsWith("valkey.")) return "advanced";
  return "general";
}

const intIn = (v: number, min: number, max: number) => Number.isInteger(v) && v >= min && v <= max;

/** Which write-only Iceberg secrets are required but neither typed nor stored. */
export interface MissingSecrets {
  catalog: boolean;
  s3Keys: boolean;
}

const prefixed = (errs: FieldErrors, prefix: string): FieldErrors => Object.fromEntries(Object.entries(errs).map(([k, v]) => [`${prefix}${k}`, v]));
const awsErrors = (t: AwsTarget, prefix: string) => prefixed(awsTargetError(t), prefix);

/** Validation for the Iceberg-only parts of a profile (catalog, storage, query engine, namespace). */
export function validateIceberg(o: IcebergOptions, missing: MissingSecrets = { catalog: false, s3Keys: false }): FieldErrors {
  const e: FieldErrors = {};
  const ns = (o.defaultNamespace ?? "").trim();
  if (ns && (/\s/.test(ns) || ns.split(".").some((l) => l === ""))) e["ice.defaultNamespace"] = "Use dot-separated levels without spaces, for example shop or lake.sales.";

  const c = o.catalog;
  switch (c.kind) {
    case "rest": {
      const u = httpUrlError(c.uri, "Catalog URI");
      if (u) e["ice.catalog.uri"] = u;
      if (c.auth.kind === "oAuth2") {
        if (!c.auth.clientId.trim()) e["ice.catalog.clientId"] = "Client ID is required.";
        if (c.auth.serverUri?.trim()) {
          const s = httpUrlError(c.auth.serverUri, "Token server URI");
          if (s) e["ice.catalog.serverUri"] = s;
        }
      }
      if (missing.catalog) e["ice.catalog.secret"] = c.auth.kind === "bearer" ? "Enter the bearer token." : "Enter the client secret.";
      const rows = headerRows(c.headers);
      for (let i = 0; i < rows.length; i++) {
        const he = headerRowError(rows, i);
        if (he) {
          e["ice.catalog.headers"] = he;
          break;
        }
      }
      break;
    }
    case "glue": {
      Object.assign(e, awsErrors(c.aws, "ice.catalog."));
      if (c.catalogId && c.catalogId.trim() && !/^\d{12}$/.test(c.catalogId.trim())) e["ice.catalog.catalogId"] = "A catalog ID is a 12-digit AWS account number.";
      if (!c.warehouse?.trim()) e["ice.catalog.warehouse"] = "Warehouse is required, for example s3://my-lake/warehouse.";
      else if (!isS3Url(c.warehouse)) e["ice.catalog.warehouse"] = "Use an s3:// path, for example s3://my-lake/warehouse.";
      break;
    }
    case "s3Tables": {
      Object.assign(e, awsErrors(c.aws, "ice.catalog."));
      if (!c.tableBucketArn.trim()) e["ice.catalog.tableBucketArn"] = "Table bucket ARN is required.";
      else if (!S3_TABLES_ARN_RE.test(c.tableBucketArn.trim())) e["ice.catalog.tableBucketArn"] = "Expected arn:aws:s3tables:<region>:<12-digit account>:bucket/<name>.";
      break;
    }
    case "sql": {
      if (!c.uri.trim()) e["ice.catalog.uri"] = "Database is required.";
      else if (sqlFlavour(c.uri) === "other") e["ice.catalog.uri"] = "Use a SQLite file (sqlite:///path/catalog.db) or a PostgreSQL URL (postgres://host/db).";
      else if (sqlFlavour(c.uri) === "sqlite" && !/^sqlite:\/*[^/\s]/i.test(c.uri.trim()) && c.uri.trim().toLowerCase() !== "sqlite::memory:") e["ice.catalog.uri"] = "Add the path of the SQLite file.";
      if (!c.warehouse.trim()) e["ice.catalog.warehouse"] = "Warehouse is required: where table data and metadata are written, such as s3://bucket/path or /data/warehouse.";
      if (!c.catalogName.trim()) e["ice.catalog.catalogName"] = "Catalog name is required (default is fine).";
      break;
    }
    case "hms": {
      if (!c.address.trim()) e["ice.catalog.address"] = "Metastore address is required.";
      else if (!/^[^\s:/]+:\d{1,5}$/.test(c.address.trim())) e["ice.catalog.address"] = "Use host:port, for example metastore.internal:9083.";
      if (!c.warehouse.trim()) e["ice.catalog.warehouse"] = "Warehouse is required.";
      break;
    }
  }

  const s = o.storage;
  if (s.kind === "s3" || s.kind === "s3Static") {
    if (s.endpoint?.trim()) {
      const u = httpUrlError(s.endpoint, "Endpoint");
      if (u) e["ice.storage.endpoint"] = u;
    }
    if (s.region?.trim() && !REGION_RE.test(s.region.trim())) e["ice.storage.region"] = "Region looks wrong, for example eu-west-1.";
  }
  if (s.kind === "s3") {
    if (s.aws) Object.assign(e, awsErrors(s.aws, "ice.storage."));
    else if (!isAwsCatalog(c)) e["ice.storage.aws.region"] = "Choose the AWS identity used to read the bucket.";
  }
  if (s.kind === "s3Static" && missing.s3Keys) e["ice.storage.keys"] = "Enter both the access key ID and the secret access key.";

  const q = o.queryEngine;
  if (q.kind === "athena") {
    if (!isAwsCatalog(c)) e["ice.engine.catalog"] = "Athena queries Glue and S3 Tables catalogs only.";
    if (!q.workgroup.trim()) e["ice.engine.workgroup"] = "Workgroup is required (primary is the default).";
    if (q.outputLocation?.trim() && !isS3Url(q.outputLocation)) e["ice.engine.output"] = "Use an s3:// path, for example s3://my-athena-results/vibelens/.";
    if (q.aws) Object.assign(e, awsErrors(q.aws, "ice.engine."));
  }
  return e;
}

export function validateProfile(p: ConnectionProfile, others: ConnectionProfile[], missing?: MissingSecrets): FieldErrors {
  const e: FieldErrors = {};
  const name = p.name.trim();
  if (!name) e.name = "Give the connection a name.";
  else if (others.some((o) => o.id !== p.id && o.name.trim().toLowerCase() === name.toLowerCase()))
    e.name = "Another connection already uses this name.";

  if (p.engine === "iceberg") {
    if (!p.iceberg) e["ice.catalog.uri"] = "Catalog settings are missing.";
    else Object.assign(e, validateIceberg(p.iceberg, missing));
    return e;
  }

  if (!p.host.trim()) e.host = "Host is required.";
  else if (/\s/.test(p.host.trim()) || /^[a-z]+:\/\//i.test(p.host.trim())) e.host = "Enter a hostname or IP address only, without a scheme.";
  if (!intIn(p.port, 1, 65535)) e.port = "Port must be 1-65535.";
  if (p.engine === "postgres" && !(p.database ?? "").trim()) e.database = "Database is required.";

  const a = p.auth;
  switch (a.kind) {
    case "password":
      if (p.engine === "postgres" && !a.username.trim()) e["auth.username"] = "Username is required.";
      break;
    case "rdsIam":
      if (!a.username.trim()) e["auth.username"] = "Database user is required.";
      Object.assign(e, awsTargetError(a.aws));
      break;
    case "secretsManager":
      if (!a.secretArn.trim()) e["auth.secretArn"] = "Secret ARN is required.";
      else if (!SECRET_ARN_RE.test(a.secretArn.trim())) e["auth.secretArn"] = "Expected arn:aws:secretsmanager:<region>:<12-digit account>:secret:<name>.";
      Object.assign(e, awsTargetError(a.aws));
      break;
    case "accessToken":
      if (a.command != null && /[\r\n]/.test(a.command)) e["auth.command"] = "Use a single command line (chain steps with && or a script).";
      break;
    case "elastiCacheIam":
      if (!a.username.trim()) e["auth.username"] = "ElastiCache user is required.";
      if (!a.cacheName.trim()) e["auth.cacheName"] = "Cache name is required.";
      Object.assign(e, awsTargetError(a.aws));
      break;
    default:
      break;
  }

  const t = p.tunnel;
  if (t) {
    if (!t.host.trim()) e["tunnel.host"] = "SSH host is required.";
    if (!intIn(t.port, 1, 65535)) e["tunnel.port"] = "Port must be 1-65535.";
    if (!t.username.trim()) e["tunnel.username"] = "SSH username is required.";
    if (t.auth.kind === "privateKey" && !t.auth.path.trim()) e["tunnel.keyPath"] = "Path to the private key file is required.";
    if (t.knownHostFingerprint != null && !isHostKeyFingerprint(t.knownHostFingerprint))
      e["tunnel.knownHostFingerprint"] = "A host key fingerprint looks like SHA256: followed by 43 characters.";
  }

  if (p.tls.mode !== "disable" && p.tls.caCertPath && /^\s*$/.test(p.tls.caCertPath)) e["tls.caCertPath"] = "Path is blank.";

  if (p.engine === "postgres") {
    if (!intIn(p.pg.connectTimeoutSecs, 1, 600)) e["pg.connectTimeoutSecs"] = "1-600 seconds.";
    if (p.pg.statementTimeoutMs != null && !intIn(p.pg.statementTimeoutMs, 1, 86_400_000)) e["pg.statementTimeoutMs"] = "Positive milliseconds, or leave empty for none.";
    if (!intIn(p.pg.idleDisconnectSecs, 0, 86_400)) e["pg.idleDisconnectSecs"] = "0-86400 seconds.";
    if (!intIn(p.pg.maxPoolSize, 1, 64)) e["pg.maxPoolSize"] = "1-64 connections.";
    if (!p.pg.applicationName.trim()) e["pg.applicationName"] = "Application name can't be empty.";
    const h = p.pg.pgproxy?.healthUrl?.trim();
    if (h) {
      try {
        const u = new URL(h);
        if (u.protocol !== "http:" && u.protocol !== "https:") throw new Error("scheme");
      } catch {
        e["pg.pgproxy.healthUrl"] = "A full URL such as http://pgproxy.internal:8080, or leave empty.";
      }
    }
  } else {
    if (!p.valkey.keyDelimiter) e["valkey.keyDelimiter"] = "Key delimiter can't be empty.";
    if (!intIn(p.valkey.scanCount, 1, 100_000)) e["valkey.scanCount"] = "1-100000.";
    if (!intIn(p.valkey.connectTimeoutSecs, 1, 600)) e["valkey.connectTimeoutSecs"] = "1-600 seconds.";
    if (!intIn(p.valkey.idleDisconnectSecs, 0, 86_400)) e["valkey.idleDisconnectSecs"] = "0-86400 seconds.";
  }
  return e;
}

export function errorCountBySection(errors: FieldErrors): Record<SectionId, number> {
  const out: Record<SectionId, number> = { general: 0, auth: 0, network: 0, tls: 0, ai: 0, advanced: 0, catalog: 0, storage: 0, queryEngine: 0 };
  for (const k of Object.keys(errors)) out[sectionOf(k)] += 1;
  return out;
}

export function firstErrorSection(errors: FieldErrors): SectionId | null {
  const order: SectionId[] = ["general", "auth", "catalog", "storage", "queryEngine", "network", "tls", "ai", "advanced"];
  const counts = errorCountBySection(errors);
  return order.find((s) => counts[s] > 0) ?? null;
}
