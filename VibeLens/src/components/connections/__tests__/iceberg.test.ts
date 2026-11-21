import { describe, expect, it } from "vitest";
import { newProfile } from "../../../lib/defaults";
import type { ConnectionProfile, IcebergOptions } from "../../../ipc/types";
import { cleanProfile, icebergSecretArgs, initialDraft, missingIcebergSecrets, switchEngine, testPasswordArg } from "../draft";
import {
  blankCatalog,
  blankEngine,
  blankStorage,
  cleanIceberg,
  headerRowError,
  headersFromRows,
  httpUrlError,
  normalizeIceberg,
  regionAfterArnEdit,
  secretKind,
  sqlFlavour,
  sqlitePathFromUri,
  sqliteUriFromPath,
  S3_TABLES_ARN_RE,
} from "../icebergForm";
import { errorCountBySection, firstErrorSection, sectionOf, validateProfile } from "../validation";

const ice = (over: Partial<IcebergOptions> = {}): ConnectionProfile => {
  const p = newProfile("iceberg");
  p.name = "lake";
  p.iceberg = { ...(p.iceberg as IcebergOptions), ...over };
  return p;
};
const aws = { identity: { kind: "profile", profileName: "dev" } as const, region: "eu-west-1" };

describe("validateProfile (iceberg)", () => {
  it("accepts the default local REST catalog once named, and ignores host/port/database", () => {
    const p = ice();
    expect(p.host).toBe("");
    expect(validateProfile(p, [])).toEqual({});
  });
  it("still requires a unique name", () => {
    expect(validateProfile({ ...ice(), name: " " }, []).name).toBeDefined();
    expect(validateProfile(ice(), [{ ...ice(), id: "x" }]).name).toMatch(/already/);
  });
  it("REST: URI, OAuth2 client id, token server and missing secret", () => {
    const rest = (uri: string, auth: IcebergOptions["catalog"] extends infer C ? C : never) => ice({ catalog: { kind: "rest", uri, warehouse: null, auth: auth as never, headers: {} } });
    expect(validateProfile(rest("catalog.example.com", { kind: "none" } as never), [])["ice.catalog.uri"]).toBeDefined();
    expect(validateProfile(rest("ftp://x", { kind: "none" } as never), [])["ice.catalog.uri"]).toMatch(/http/);
    const oauth = rest("https://c.example.com", { kind: "oAuth2", clientId: "", scope: null, serverUri: "nope" } as never);
    const e = validateProfile(oauth, [], { catalog: true, s3Keys: false });
    expect(Object.keys(e).sort()).toEqual(["ice.catalog.clientId", "ice.catalog.secret", "ice.catalog.serverUri"]);
    expect(validateProfile(rest("https://c.example.com", { kind: "bearer" } as never), [], { catalog: true, s3Keys: false })["ice.catalog.secret"]).toMatch(/token/);
  });
  it("REST: rejects bad and duplicate header names", () => {
    const p = ice({ catalog: { kind: "rest", uri: "http://x", warehouse: null, auth: { kind: "none" }, headers: { "bad name": "1" } } });
    expect(validateProfile(p, [])["ice.catalog.headers"]).toBeDefined();
    expect(headerRowError([{ name: "X-A", value: "1" }, { name: "x-a", value: "2" }], 1)).toMatch(/Duplicate/);
    expect(headerRowError([{ name: "", value: "" }], 0)).toBeNull();
    expect(headersFromRows([{ name: " X-A ", value: "1" }, { name: "", value: "z" }])).toEqual({ "X-A": "1" });
  });
  it("Glue: aws target, catalog id and s3 warehouse", () => {
    const bad = ice({ catalog: { kind: "glue", aws: { identity: { kind: "profile", profileName: "" }, region: "" }, catalogId: "12", warehouse: "/tmp/x" } });
    const e = validateProfile(bad, []);
    expect(e["ice.catalog.aws.region"]).toBeDefined();
    expect(e["ice.catalog.aws.profile"]).toBeDefined();
    expect(e["ice.catalog.catalogId"]).toBeDefined();
    expect(e["ice.catalog.warehouse"]).toMatch(/s3/);
    expect(validateProfile(ice({ catalog: { kind: "glue", aws, catalogId: "123456789012", warehouse: "s3://lake/wh" } }), [])).toEqual({});
    expect(validateProfile(ice({ catalog: { kind: "glue", aws, catalogId: null, warehouse: null } }), [])["ice.catalog.warehouse"]).toMatch(/required/);
  });
  it("S3 Tables: validates the table bucket ARN", () => {
    const arn = "arn:aws:s3tables:eu-west-1:123456789012:bucket/analytics";
    expect(S3_TABLES_ARN_RE.test(arn)).toBe(true);
    expect(S3_TABLES_ARN_RE.test("arn:aws:s3tables:eu-west-1:123456789012:bucket/Analytics")).toBe(false);
    expect(S3_TABLES_ARN_RE.test("arn:aws:s3:::analytics")).toBe(false);
    expect(S3_TABLES_ARN_RE.test(arn + "/table/x")).toBe(false);
    expect(validateProfile(ice({ catalog: { kind: "s3Tables", aws, tableBucketArn: arn } }), [])).toEqual({});
    expect(validateProfile(ice({ catalog: { kind: "s3Tables", aws, tableBucketArn: "" } }), [])["ice.catalog.tableBucketArn"]).toMatch(/required/);
    expect(validateProfile(ice({ catalog: { kind: "s3Tables", aws, tableBucketArn: "arn:aws:s3:::x" } }), [])["ice.catalog.tableBucketArn"]).toMatch(/Expected/);
  });
  it("SQL: sqlite or postgres, warehouse and catalog name", () => {
    const sql = (uri: string, warehouse = "/w", catalogName = "default") => ice({ catalog: { kind: "sql", uri, warehouse, catalogName } });
    expect(validateProfile(sql("sqlite:///data/c.db"), [])).toEqual({});
    expect(validateProfile(sql("postgres://u@h/db"), [])).toEqual({});
    expect(validateProfile(sql("mysql://u@h/db"), [])["ice.catalog.uri"]).toBeDefined();
    expect(validateProfile(sql("sqlite:"), [])["ice.catalog.uri"]).toMatch(/path/);
    expect(validateProfile(sql("sqlite::memory:"), [])).toEqual({});
    const e = validateProfile(sql("sqlite:///c.db", "", " "), []);
    expect(Object.keys(e).sort()).toEqual(["ice.catalog.catalogName", "ice.catalog.warehouse"]);
  });
  it("HMS: host:port and warehouse", () => {
    expect(validateProfile(ice({ catalog: { kind: "hms", address: "thrift://h:9083", warehouse: "s3://w" } }), [])["ice.catalog.address"]).toBeDefined();
    expect(validateProfile(ice({ catalog: { kind: "hms", address: "meta:9083", warehouse: "" } }), [])["ice.catalog.warehouse"]).toBeDefined();
    expect(validateProfile(ice({ catalog: { kind: "hms", address: "meta.internal:9083", warehouse: "s3://w" } }), [])).toEqual({});
  });
  it("storage: endpoint, region and keys", () => {
    const withStorage = (storage: IcebergOptions["storage"], m = false) => validateProfile(ice({ storage }), [], { catalog: false, s3Keys: m });
    expect(withStorage({ kind: "s3Static", endpoint: "minio:9000", region: "mars", pathStyle: true })).toMatchObject({ "ice.storage.endpoint": expect.any(String), "ice.storage.region": expect.any(String) });
    expect(withStorage({ kind: "s3Static", endpoint: "http://minio:9000", region: "us-east-1", pathStyle: true }, true)["ice.storage.keys"]).toBeDefined();
    expect(withStorage({ kind: "s3Static", endpoint: "http://minio:9000", region: "us-east-1", pathStyle: true })).toEqual({});
    // S3 storage next to a REST catalog has no identity to borrow
    expect(withStorage({ kind: "s3", aws: null, endpoint: null, region: null, pathStyle: false })["ice.storage.aws.region"]).toBeDefined();
    expect(withStorage({ kind: "s3", aws, endpoint: null, region: null, pathStyle: false })).toEqual({});
    expect(withStorage({ kind: "local" })).toEqual({});
  });
  it("Athena only with Glue or S3 Tables, with a workgroup and an s3 output", () => {
    const athena = { kind: "athena" as const, workgroup: "primary", outputLocation: null, aws: null };
    expect(validateProfile(ice({ queryEngine: athena }), [])["ice.engine.catalog"]).toBeDefined();
    const glue = { kind: "glue" as const, aws, catalogId: null, warehouse: "s3://w" };
    expect(validateProfile(ice({ catalog: glue, queryEngine: athena }), [])).toEqual({});
    const bad = validateProfile(ice({ catalog: glue, queryEngine: { ...athena, workgroup: " ", outputLocation: "/tmp" } }), []);
    expect(Object.keys(bad).sort()).toEqual(["ice.engine.output", "ice.engine.workgroup"]);
  });
  it("default namespace must be dot-separated levels", () => {
    expect(validateProfile(ice({ defaultNamespace: "lake.sales" }), [])).toEqual({});
    expect(validateProfile(ice({ defaultNamespace: "a..b" }), [])["ice.defaultNamespace"]).toBeDefined();
    expect(validateProfile(ice({ defaultNamespace: "my ns" }), [])["ice.defaultNamespace"]).toBeDefined();
  });
  it("maps iceberg errors to their sections", () => {
    expect(sectionOf("ice.catalog.uri")).toBe("catalog");
    expect(sectionOf("ice.catalog.aws.region")).toBe("catalog");
    expect(sectionOf("ice.storage.endpoint")).toBe("storage");
    expect(sectionOf("ice.engine.workgroup")).toBe("queryEngine");
    expect(sectionOf("ice.defaultNamespace")).toBe("general");
    const errs = { name: "x", "ice.catalog.uri": "y", "ice.engine.output": "z" };
    expect(errorCountBySection(errs)).toMatchObject({ general: 1, catalog: 1, queryEngine: 1, storage: 0 });
    expect(firstErrorSection({ "ice.engine.output": "z", "ice.catalog.uri": "y" })).toBe("catalog");
  });
});

describe("iceberg form helpers", () => {
  it("classifies SQL catalog URIs and converts sqlite paths", () => {
    expect(sqlFlavour("sqlite:///x.db")).toBe("sqlite");
    expect(sqlFlavour("POSTGRES://h/db")).toBe("postgres");
    expect(sqlFlavour("postgresql://h/db")).toBe("postgres");
    expect(sqlFlavour("")).toBe("other");
    expect(sqliteUriFromPath("/data/c.db")).toBe("sqlite:///data/c.db");
    expect(sqliteUriFromPath("c.db")).toBe("sqlite:c.db");
    expect(sqliteUriFromPath("sqlite:///x.db")).toBe("sqlite:///x.db");
    expect(sqlitePathFromUri("sqlite:///data/c.db")).toBe("/data/c.db");
    expect(sqlitePathFromUri(sqliteUriFromPath("/data/c.db"))).toBe("/data/c.db");
    expect(sqlitePathFromUri("sqlite:c.db")).toBe("c.db");
  });
  it("httpUrlError accepts http(s) only", () => {
    expect(httpUrlError("https://a.example.com/x", "URI")).toBeNull();
    expect(httpUrlError("localhost:8181", "URI")).not.toBeNull();
    expect(httpUrlError("", "URI")).toMatch(/required/);
  });
  it("prefills the region from the ARN without overwriting a region the user chose", () => {
    const arn = "arn:aws:s3tables:eu-west-1:123456789012:bucket/a";
    expect(regionAfterArnEdit("", "", arn)).toBe("eu-west-1");
    expect(regionAfterArnEdit("eu-west-1", arn, "arn:aws:s3tables:us-east-2:123456789012:bucket/a")).toBe("us-east-2");
    expect(regionAfterArnEdit("ap-south-1", arn, "arn:aws:s3tables:us-east-2:123456789012:bucket/a")).toBe("ap-south-1");
    expect(regionAfterArnEdit("eu-west-1", "", "arn:aws:s3tab")).toBe("eu-west-1");
  });
  it("switching catalog keeps the AWS identity and drops what doesn't apply", () => {
    const glue = blankCatalog("glue", { kind: "rest", uri: "http://x", warehouse: null, auth: { kind: "none" }, headers: {} }, "eu-west-1");
    expect(glue).toMatchObject({ kind: "glue", aws: { region: "eu-west-1" }, warehouse: null });
    const tables = blankCatalog("s3Tables", { ...(glue as object), kind: "glue", aws, catalogId: null, warehouse: "s3://w" } as never);
    expect(tables).toMatchObject({ kind: "s3Tables", aws: { region: "eu-west-1" }, tableBucketArn: "" });
    expect(blankCatalog("rest", tables)).toMatchObject({ kind: "rest", uri: "http://localhost:8181" });
    const sql = blankCatalog("sql", { kind: "glue", aws, catalogId: null, warehouse: "s3://w" });
    expect(sql).toMatchObject({ kind: "sql", warehouse: "", catalogName: "default" });
    expect(blankCatalog("rest", sql)).toMatchObject({ uri: "" });
  });
  it("normalizeIceberg falls back from Athena when the catalog has no AWS identity", () => {
    const o = ice({ queryEngine: { kind: "athena", workgroup: "primary", outputLocation: null, aws: null } }).iceberg as IcebergOptions;
    expect(normalizeIceberg(o).queryEngine.kind).toBe("local");
    const glue = { ...o, catalog: { kind: "glue" as const, aws, catalogId: null, warehouse: "s3://w" } };
    expect(normalizeIceberg(glue)).toBe(glue);
  });
  it("normalizeIceberg gives S3 storage its own identity when the catalog has none to share", () => {
    const o = ice({ storage: { kind: "s3", aws: null, endpoint: null, region: null, pathStyle: false } }).iceberg as IcebergOptions;
    const n = normalizeIceberg(o);
    expect(n.storage.kind === "s3" && n.storage.aws).toBeTruthy();
  });
  it("storage and engine switches carry shared fields", () => {
    const cat = newProfile("iceberg").iceberg!.catalog;
    const s3 = blankStorage("s3", cat, { kind: "s3Static", endpoint: "http://m:9000", region: "us-east-1", pathStyle: true });
    expect(s3).toMatchObject({ kind: "s3", endpoint: "http://m:9000", pathStyle: true });
    expect(blankStorage("auto", cat, s3)).toEqual({ kind: "auto" });
    expect(blankEngine("athena", { kind: "local" })).toMatchObject({ kind: "athena", workgroup: "primary", aws: null });
  });
  it("secretKind says what the single keychain slot holds", () => {
    expect(secretKind({ kind: "rest", uri: "", warehouse: null, auth: { kind: "bearer" }, headers: {} })).toBe("bearer");
    expect(secretKind({ kind: "rest", uri: "", warehouse: null, auth: { kind: "oAuth2", clientId: "c", scope: null, serverUri: null }, headers: {} })).toBe("oauth2");
    expect(secretKind({ kind: "sql", uri: "postgres://h/db", warehouse: "w", catalogName: "c" })).toBe("sqlPassword");
    expect(secretKind({ kind: "sql", uri: "sqlite:///x", warehouse: "w", catalogName: "c" })).toBeNull();
    expect(secretKind({ kind: "glue", aws, catalogId: null, warehouse: null })).toBeNull();
  });
  it("cleanProfile trims and nulls blank optional iceberg fields without mutating the input", () => {
    const p = ice({
      defaultNamespace: "  ",
      catalog: { kind: "rest", uri: " http://x ", warehouse: " ", auth: { kind: "oAuth2", clientId: " id ", scope: "", serverUri: " " }, headers: {} },
      storage: { kind: "s3Static", endpoint: " ", region: " us-east-1 ", pathStyle: true },
    });
    const c = cleanProfile(p).iceberg as IcebergOptions;
    expect(c.defaultNamespace).toBeNull();
    expect(c.catalog).toMatchObject({ uri: "http://x", warehouse: null, auth: { clientId: "id", scope: null, serverUri: null } });
    expect(c.storage).toMatchObject({ endpoint: null, region: "us-east-1" });
    expect((p.iceberg as IcebergOptions).defaultNamespace).toBe("  ");
    expect(cleanIceberg(p.iceberg as IcebergOptions).storage).toMatchObject({ endpoint: null });
  });
});

describe("iceberg secrets", () => {
  const rest = (auth: "none" | "bearer" | "oauth2", extra: Partial<IcebergOptions> = {}) =>
    ice({ catalog: { kind: "rest", uri: "http://x", warehouse: null, auth: auth === "none" ? { kind: "none" } : auth === "bearer" ? { kind: "bearer" } : { kind: "oAuth2", clientId: "c", scope: null, serverUri: null }, headers: {} }, ...extra });

  it("a new connection stores the typed token", () => {
    const d = initialDraft(null, "iceberg");
    d.profile = rest("bearer");
    d.icebergSecret.value = "tok";
    expect(icebergSecretArgs(null, d)).toEqual({ icebergSecret: "tok", s3Keys: undefined });
    expect(testPasswordArg(d)).toBe("tok");
  });
  it("editing keeps stored secrets unless replaced or removed", () => {
    const original = { ...rest("bearer"), id: "p" };
    const d = initialDraft(original);
    expect(icebergSecretArgs(original, d).icebergSecret).toBeUndefined();
    d.icebergSecret = { mode: "remove", value: "" };
    expect(icebergSecretArgs(original, d).icebergSecret).toBe("");
    d.icebergSecret = { mode: "set", value: "new" };
    expect(icebergSecretArgs(original, d).icebergSecret).toBe("new");
  });
  it("deletes the stale secret when the catalog stops needing one, or when its meaning changes", () => {
    const original = { ...rest("bearer"), id: "p" };
    const none = initialDraft(original);
    none.profile = { ...rest("none"), id: "p" };
    expect(icebergSecretArgs(original, none).icebergSecret).toBe("");
    const oauth = initialDraft(original);
    oauth.profile = { ...rest("oauth2"), id: "p" };
    oauth.icebergSecret = { mode: "keep", value: "" };
    expect(icebergSecretArgs(original, oauth).icebergSecret).toBe("");
    oauth.icebergSecret = { mode: "set", value: "s" };
    expect(icebergSecretArgs(original, oauth).icebergSecret).toBe("s");
  });
  it("sends S3 key pairs only when both halves are present; deletes them when static keys are dropped", () => {
    const d = initialDraft(null, "iceberg");
    d.profile = ice({ storage: { kind: "s3Static", endpoint: null, region: null, pathStyle: false } });
    d.s3Keys = { mode: "set", accessKeyId: "AK", secretAccessKey: "" };
    expect(icebergSecretArgs(null, d).s3Keys).toBeUndefined();
    d.s3Keys = { mode: "set", accessKeyId: "AK", secretAccessKey: "SK" };
    expect(icebergSecretArgs(null, d).s3Keys).toEqual({ accessKeyId: "AK", secretAccessKey: "SK" });
    const original = { ...ice({ storage: { kind: "s3Static", endpoint: null, region: null, pathStyle: false } }), id: "p" };
    const e = initialDraft(original);
    e.profile = { ...ice({ storage: { kind: "auto" } }), id: "p" };
    expect(icebergSecretArgs(original, e).s3Keys).toEqual({ accessKeyId: "", secretAccessKey: "" });
  });
  it("reports required secrets that are neither typed nor stored", () => {
    const d = initialDraft(null, "iceberg");
    d.profile = rest("bearer", { storage: { kind: "s3Static", endpoint: null, region: null, pathStyle: false } });
    expect(missingIcebergSecrets(null, d, undefined)).toEqual({ catalog: true, s3Keys: true, engine: false });
    d.icebergSecret.value = "t";
    d.s3Keys = { mode: "set", accessKeyId: "a", secretAccessKey: "b" };
    expect(missingIcebergSecrets(null, d, undefined)).toEqual({ catalog: false, s3Keys: false, engine: false });
    const original = { ...d.profile, id: "p" };
    const e = initialDraft(original);
    expect(missingIcebergSecrets(original, e, { hasIcebergSecret: true, hasS3Keys: true })).toEqual({ catalog: false, s3Keys: false, engine: false });
    expect(missingIcebergSecrets(original, e, { hasIcebergSecret: false, hasS3Keys: true })).toEqual({ catalog: true, s3Keys: false, engine: false });
    // an optional Postgres password is never "missing"
    const sql = initialDraft(null, "iceberg");
    sql.profile = ice({ catalog: { kind: "sql", uri: "postgres://u@h/db", warehouse: "w", catalogName: "c" } });
    sql.icebergSecret.value = "";
    expect(missingIcebergSecrets(null, sql, undefined).catalog).toBe(false);
  });
  it("switching to iceberg resets host/port and drops the SSH tunnel", () => {
    const pg = { ...newProfile("postgres"), name: "x", environment: "prod" as const, tunnel: { kind: "ssh" as const, host: "b", port: 22, username: "u", auth: { kind: "agent" as const }, knownHostFingerprint: null } };
    const v = switchEngine(pg, "iceberg");
    expect(v).toMatchObject({ engine: "iceberg", name: "x", environment: "prod", port: 0, tunnel: null });
    expect(v.iceberg?.catalog.kind).toBe("rest");
    expect(switchEngine(v, "postgres").iceberg).toBeNull();
  });
});
