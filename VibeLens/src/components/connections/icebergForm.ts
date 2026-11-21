// Pure helpers behind the Iceberg part of the connection wizard: catalog switching, defaults that
// keep the form self-consistent, secret bookkeeping and small parsers. No React in here.
import type { AwsTarget, IcebergCatalog, IcebergOptions, IcebergQueryEngine, IcebergStorage, RestAuth } from "../../ipc/types";
import { defaultAwsTarget, regionFromArn } from "./awsHelpers";
import { blankRemoteEngine, cleanRemoteEngine, isRemoteEngine, remoteEngineSummary } from "./remoteEngineForm";

export type CatalogKind = IcebergCatalog["kind"];
export type StorageKind = IcebergStorage["kind"];
export type SqlFlavour = "sqlite" | "postgres" | "other";

export const CATALOG_KINDS: { value: CatalogKind; label: string; short: string }[] = [
  { value: "rest", label: "REST", short: "REST" },
  { value: "glue", label: "AWS Glue", short: "Glue" },
  { value: "s3Tables", label: "S3 Tables", short: "S3 Tables" },
  { value: "sql", label: "SQL (JDBC)", short: "SQL" },
  { value: "hms", label: "Hive Metastore", short: "Hive Metastore" },
];

/** arn:aws:s3tables:<region>:<account>:bucket/<name> */
export const S3_TABLES_ARN_RE = /^arn:aws[a-z-]*:s3tables:[a-z0-9-]+:\d{12}:bucket\/[a-z0-9-]+$/;
const HEADER_NAME_RE = /^[A-Za-z0-9!#$%&'*+.^_`|~-]+$/;

/** Catalogs backed by AWS APIs; the only ones Athena can query. */
export const isAwsCatalog = (c: IcebergCatalog): c is Extract<IcebergCatalog, { kind: "glue" | "s3Tables" }> => c.kind === "glue" || c.kind === "s3Tables";

export const catalogAws = (c: IcebergCatalog): AwsTarget | null => (isAwsCatalog(c) ? c.aws : null);

/** What kind of value the single "iceberg secret" keychain slot holds for this catalog. */
export type SecretKind = "bearer" | "oauth2" | "sqlPassword";
export function secretKind(c: IcebergCatalog): SecretKind | null {
  if (c.kind === "rest") return c.auth.kind === "bearer" ? "bearer" : c.auth.kind === "oAuth2" ? "oauth2" : null;
  if (c.kind === "sql") return sqlFlavour(c.uri) === "postgres" ? "sqlPassword" : null;
  return null;
}
/** Bearer tokens and OAuth2 secrets must exist; a Postgres password may be empty (trust auth). */
export const secretRequired = (c: IcebergCatalog): boolean => {
  const k = secretKind(c);
  return k === "bearer" || k === "oauth2";
};

export function sqlFlavour(uri: string): SqlFlavour {
  const v = uri.trim().toLowerCase();
  if (v.startsWith("sqlite:")) return "sqlite";
  if (v.startsWith("postgres://") || v.startsWith("postgresql://")) return "postgres";
  return "other";
}

/** `/data/catalog.db` -> `sqlite:///data/catalog.db`; leaves values that already carry a scheme alone. */
export function sqliteUriFromPath(path: string): string {
  const p = path.trim();
  if (!p) return "sqlite:";
  if (/^sqlite:/i.test(p)) return p;
  return p.startsWith("/") ? `sqlite://${p}` : `sqlite:${p}`;
}
/** Inverse of `sqliteUriFromPath` for the path input. */
export function sqlitePathFromUri(uri: string): string {
  const v = uri.trim();
  if (/^sqlite:\/\//i.test(v)) return v.slice("sqlite://".length);
  if (/^sqlite:/i.test(v)) return v.slice("sqlite:".length);
  return v;
}
/** True when a postgres URL embeds a password (`user:secret@host`). */
export function urlHasInlinePassword(uri: string): boolean {
  return /^postgres(ql)?:\/\/[^/@:\s]+:[^/@\s]+@/i.test(uri.trim());
}

export function httpUrlError(v: string, what: string): string | null {
  const t = v.trim();
  if (!t) return `${what} is required.`;
  try {
    const u = new URL(t);
    if (u.protocol !== "http:" && u.protocol !== "https:") return `${what} must start with http:// or https://.`;
    if (!u.hostname) return `${what} needs a host name.`;
  } catch {
    return `${what} must be a full URL such as https://catalog.example.com.`;
  }
  return null;
}

export const isS3Url = (v: string): boolean => /^s3a?:\/\/[^/\s]+(\/.*)?$/i.test(v.trim());

export function blankCatalog(kind: CatalogKind, prev: IcebergCatalog, region = ""): IcebergCatalog {
  if (prev.kind === kind) return prev;
  const aws = catalogAws(prev) ?? defaultAwsTarget(region);
  const warehouse = "warehouse" in prev ? (prev.warehouse ?? "") : "";
  switch (kind) {
    case "rest":
      return { kind, uri: prev.kind === "sql" || prev.kind === "hms" ? "" : "http://localhost:8181", warehouse: null, auth: { kind: "none" }, headers: {} };
    case "glue":
      return { kind, aws, catalogId: null, warehouse: isS3Url(warehouse) ? warehouse : null };
    case "s3Tables":
      return { kind, aws, tableBucketArn: "" };
    case "sql":
      return { kind, uri: "", warehouse: warehouse && !isS3Url(warehouse) ? warehouse : "", catalogName: "default" };
    case "hms":
      return { kind, address: "", warehouse };
  }
}

export function blankRestAuth(kind: RestAuth["kind"]): RestAuth {
  if (kind === "oAuth2") return { kind, clientId: "", scope: null, serverUri: null };
  return { kind } as RestAuth;
}

export function blankStorage(kind: StorageKind, catalog: IcebergCatalog, prev: IcebergStorage): IcebergStorage {
  if (prev.kind === kind) return prev;
  const carry = prev.kind === "s3" || prev.kind === "s3Static" ? { endpoint: prev.endpoint, region: prev.region, pathStyle: prev.pathStyle } : { endpoint: null, region: null, pathStyle: false };
  switch (kind) {
    case "auto":
      return { kind };
    case "local":
      return { kind };
    case "s3":
      return { kind, aws: catalogAws(catalog) ? null : defaultAwsTarget(""), ...carry };
    case "s3Static":
      return { kind, ...carry };
  }
}

export function blankEngine(kind: IcebergQueryEngine["kind"], prev: IcebergQueryEngine): IcebergQueryEngine {
  if (prev.kind === kind) return prev;
  if (kind === "trino" || kind === "spark") return blankRemoteEngine(kind, prev);
  return kind === "local" ? { kind } : { kind, workgroup: "primary", outputLocation: null, aws: null };
}

/**
 * Keep the options internally consistent after any edit:
 *  - Athena only works with Glue / S3 Tables, so it falls back to Local otherwise (Trino and Spark
 *    work with every catalog kind: the user names the engine's catalog that points at this one);
 *  - an S3 storage block without its own identity needs one when the catalog has none to borrow.
 */
export function normalizeIceberg(o: IcebergOptions): IcebergOptions {
  let { storage, queryEngine } = o;
  if (queryEngine.kind === "athena" && !isAwsCatalog(o.catalog)) queryEngine = { kind: "local" };
  if (storage.kind === "s3" && storage.aws === null && !isAwsCatalog(o.catalog)) storage = { ...storage, aws: defaultAwsTarget("") };
  return storage === o.storage && queryEngine === o.queryEngine ? o : { ...o, storage, queryEngine };
}

/** Region typed into a field vs. the region implied by the previous ARN: only overwrite when it was ours. */
export function regionAfterArnEdit(currentRegion: string, prevArn: string, nextArn: string): string {
  const next = regionFromArn(nextArn);
  const prev = regionFromArn(prevArn);
  return next && (!currentRegion.trim() || currentRegion === prev) ? next : currentRegion;
}

/** Header rows <-> record. Blank names are dropped; later duplicates win. */
export interface HeaderRow {
  name: string;
  value: string;
}
export const headerRows = (h: Record<string, string>): HeaderRow[] => Object.entries(h).map(([name, value]) => ({ name, value }));
export function headersFromRows(rows: HeaderRow[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const r of rows) if (r.name.trim()) out[r.name.trim()] = r.value;
  return out;
}
export function headerRowError(rows: HeaderRow[], i: number): string | null {
  const r = rows[i];
  if (!r.name.trim()) return r.value.trim() ? "Give the header a name." : null;
  if (!HEADER_NAME_RE.test(r.name.trim())) return "Header names can't contain spaces or separators.";
  if (rows.findIndex((x) => x.name.trim().toLowerCase() === r.name.trim().toLowerCase()) !== i) return "Duplicate header.";
  return null;
}

/** One-line description used in the wizard's section list. */
export function catalogSummary(c: IcebergCatalog): string {
  switch (c.kind) {
    case "rest":
      return `REST · ${c.uri.replace(/^https?:\/\//, "") || "no URI"}`;
    case "glue":
      return `Glue${c.aws.region ? ` · ${c.aws.region}` : ""}`;
    case "s3Tables":
      return `S3 Tables${c.aws.region ? ` · ${c.aws.region}` : ""}`;
    case "sql":
      return `SQL · ${sqlFlavour(c.uri) === "other" ? "no database" : sqlFlavour(c.uri)}`;
    case "hms":
      return `Hive · ${c.address || "no address"}`;
  }
}
export function storageSummary(s: IcebergStorage): string {
  return { auto: "Automatic", s3: "Amazon S3", s3Static: "S3-compatible", local: "Local files" }[s.kind];
}
export function engineSummary(e: IcebergQueryEngine): string {
  if (isRemoteEngine(e)) return remoteEngineSummary(e);
  return e.kind === "local" ? "Local (DataFusion)" : `Athena · ${e.workgroup || "primary"}`;
}

/** Trim and null-out the optional text fields before persisting. */
export function cleanIceberg(o: IcebergOptions): IcebergOptions {
  const c = structuredClone(o);
  const nz = (v: string | null | undefined): string | null => (v && v.trim() ? v.trim() : null);
  c.defaultNamespace = nz(c.defaultNamespace);
  const cat = c.catalog;
  const trimAws = (a: AwsTarget) => {
    a.region = a.region.trim();
    if (a.identity.kind === "sso") a.identity.startUrl = a.identity.startUrl.trim();
  };
  switch (cat.kind) {
    case "rest":
      cat.uri = cat.uri.trim();
      cat.warehouse = nz(cat.warehouse);
      if (cat.auth.kind === "oAuth2") {
        cat.auth.clientId = cat.auth.clientId.trim();
        cat.auth.scope = nz(cat.auth.scope);
        cat.auth.serverUri = nz(cat.auth.serverUri);
      }
      cat.headers = headersFromRows(headerRows(cat.headers));
      break;
    case "glue":
      trimAws(cat.aws);
      cat.catalogId = nz(cat.catalogId);
      cat.warehouse = nz(cat.warehouse);
      break;
    case "s3Tables":
      trimAws(cat.aws);
      cat.tableBucketArn = cat.tableBucketArn.trim();
      break;
    case "sql":
      cat.uri = cat.uri.trim();
      cat.warehouse = cat.warehouse.trim();
      cat.catalogName = cat.catalogName.trim();
      break;
    case "hms":
      cat.address = cat.address.trim();
      cat.warehouse = cat.warehouse.trim();
      break;
  }
  const st = c.storage;
  if (st.kind === "s3" || st.kind === "s3Static") {
    st.endpoint = nz(st.endpoint);
    st.region = nz(st.region);
    if (st.kind === "s3" && st.aws) trimAws(st.aws);
  }
  if (c.queryEngine.kind === "athena") {
    c.queryEngine.workgroup = c.queryEngine.workgroup.trim();
    c.queryEngine.outputLocation = nz(c.queryEngine.outputLocation);
    if (c.queryEngine.aws) trimAws(c.queryEngine.aws);
  }
  if (isRemoteEngine(c.queryEngine)) c.queryEngine = cleanRemoteEngine(c.queryEngine);
  return c;
}
