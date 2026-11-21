// Trino and Spark (Livy) query engines of an Iceberg connection: URL and cleartext rules, the Spark
// session configuration rules, validation, the engine secret and engine switching. The rules mirror
// docs/REMOTE_ENGINES_AND_REDSHIFT.md sections 1.4 and 2.4; Rust enforces them again.
import { describe, expect, it } from "vitest";
import { newProfile } from "../../../lib/defaults";
import type { ConnectionProfile, IcebergOptions, IcebergQueryEngine } from "../../../ipc/types";
import { cleanProfile, icebergSecretArgs, initialDraft, missingIcebergSecrets } from "../draft";
import { blankEngine, engineSummary, normalizeIceberg } from "../icebergForm";
import {
  CODE_LOADING_CONF,
  cleartextProblem,
  confFromRows,
  confRowError,
  engineSecretKind,
  engineSecretStored,
  engineUrlError,
  isLoopbackHost,
  sessionConfError,
  validateRemoteEngine,
  type RemoteEngine,
} from "../remoteEngineForm";
import { firstErrorSection, sectionOf, validateProfile } from "../validation";

const trino = (over: Partial<Extract<RemoteEngine, { kind: "trino" }>> = {}): Extract<RemoteEngine, { kind: "trino" }> => ({
  kind: "trino",
  url: "https://trino.example.com:8443",
  catalog: "iceberg",
  user: "analyst",
  auth: { kind: "none" },
  caCertPath: null,
  ...over,
});
const spark = (over: Partial<Extract<RemoteEngine, { kind: "spark" }>> = {}): Extract<RemoteEngine, { kind: "spark" }> => ({
  kind: "spark",
  url: "https://livy.example.com:8998",
  catalog: "lake",
  auth: { kind: "none" },
  caCertPath: null,
  proxyUser: null,
  sessionConf: {},
  sessionStartTimeoutSecs: null,
  ...over,
});
const lake = (queryEngine: IcebergQueryEngine, over: Partial<IcebergOptions> = {}): ConnectionProfile => {
  const p = newProfile("iceberg");
  p.name = "lake";
  p.iceberg = { ...(p.iceberg as IcebergOptions), queryEngine, ...over };
  return p;
};

describe("engine URL", () => {
  it("accepts http(s) URLs with a host and a port", () => {
    expect(engineUrlError("https://trino.example.com:8443", "URL")).toBeNull();
    expect(engineUrlError("http://localhost:8080/", "URL")).toBeNull();
  });
  it("requires a value and an http(s) scheme", () => {
    expect(engineUrlError("  ", "Coordinator URL")).toMatch(/required/);
    expect(engineUrlError("trino.example.com", "URL")).toMatch(/full URL/);
    expect(engineUrlError("ftp://trino.example.com", "URL")).toMatch(/http/);
  });
  it("refuses credentials, a query string or a fragment in the URL", () => {
    expect(engineUrlError("https://bob:pw@trino.example.com", "URL")).toMatch(/user name or password/);
    expect(engineUrlError("https://bob@trino.example.com", "URL")).toMatch(/user name or password/);
    expect(engineUrlError("https://trino.example.com/?x=1", "URL")).toMatch(/query string/);
    expect(engineUrlError("https://trino.example.com/?", "URL")).toMatch(/query string/);
    expect(engineUrlError("https://trino.example.com/#top", "URL")).toMatch(/fragment/);
  });
});

describe("cleartext (R2)", () => {
  it("knows the loopback names the Rust side treats as this computer", () => {
    for (const h of ["localhost", "127.0.0.1", "[::1]", "trino.localhost", "LOCALHOST"]) expect(isLoopbackHost(h), h).toBe(true);
    for (const h of ["10.0.0.5", "trino.internal", "127.0.0.2.example.com"]) expect(isLoopbackHost(h), h).toBe(false);
  });
  it("is an error to send a password or token over http:// to another machine", () => {
    expect(cleartextProblem("http://trino.internal:8080", { kind: "basic", username: "u" })).toMatchObject({ level: "error", message: expect.stringMatching(/password.*unencrypted/) });
    expect(cleartextProblem("http://trino.internal:8080", { kind: "bearer" })).toMatchObject({ level: "error", message: expect.stringMatching(/token/) });
  });
  it("warns, without blocking, when no credential travels over http:// to another machine", () => {
    expect(cleartextProblem("http://trino.internal:8080", { kind: "none" })).toMatchObject({ level: "warn", message: expect.stringMatching(/not encrypted/) });
  });
  it("is fine over https, and over http to this computer even with a credential", () => {
    expect(cleartextProblem("https://trino.internal", { kind: "bearer" })).toBeNull();
    expect(cleartextProblem("http://localhost:8080", { kind: "basic", username: "u" })).toBeNull();
    expect(cleartextProblem("http://127.0.0.1:8998", { kind: "bearer" })).toBeNull();
  });
  it("feeds validation: the URL field carries the cleartext error, the warning is not an error", () => {
    expect(validateRemoteEngine(trino({ url: "http://trino.internal:8080", auth: { kind: "bearer" } }))["ice.engine.url"]).toMatch(/unencrypted/);
    expect(validateRemoteEngine(trino({ url: "http://trino.internal:8080" }))).toEqual({});
  });
});

describe("Spark session configuration (design 2.4)", () => {
  it("refuses every setting that loads code into the cluster", () => {
    expect([...CODE_LOADING_CONF].sort()).toEqual(
      [
        "spark.driver.extraClassPath",
        "spark.driver.extraJavaOptions",
        "spark.executor.extraClassPath",
        "spark.executor.extraJavaOptions",
        "spark.files",
        "spark.jars",
        "spark.jars.packages",
        "spark.submit.pyFiles",
      ].sort(),
    );
    for (const k of CODE_LOADING_CONF) expect(sessionConfError(k, "x"), k).toMatch(/loads code/);
  });
  it("refuses keys and values that look like credentials, saying to configure them on the cluster", () => {
    for (const k of ["spark.hadoop.fs.s3a.secret.key", "spark.sql.catalog.lake.client-secret", "my.password", "x.token.value", "aws.credentials.provider", "fs.s3a.access.key", "ssl.trustStore.key"]) {
      expect(sessionConfError(k, "v"), k).toMatch(/credential.*cluster/);
    }
    expect(sessionConfError("spark.sql.catalog.lake.header.Authorization", "Bearer abc")).toMatch(/bearer token/);
    expect(sessionConfError("spark.sql.catalog.lake.header.x", "bearer abc")).toMatch(/bearer token/);
  });
  it("accepts ordinary catalog settings", () => {
    expect(sessionConfError("spark.sql.catalog.lake", "org.apache.iceberg.spark.SparkCatalog")).toBeNull();
    expect(sessionConfError("spark.sql.catalog.lake.type", "glue")).toBeNull();
    expect(sessionConfError("spark.sql.extensions", "org.apache.iceberg.spark.extensions.IcebergSparkSessionExtensions")).toBeNull();
    expect(sessionConfError("", "")).toBeNull();
  });
  it("needs a name for a value, and no spaces in names", () => {
    expect(sessionConfError("", "x")).toMatch(/name/);
    expect(sessionConfError("spark sql", "x")).toMatch(/spaces/);
  });
  it("flags duplicates per row and drops blank rows when saving", () => {
    const rows = [
      { key: "a", value: "1" },
      { key: "a", value: "2" },
      { key: " ", value: "" },
    ];
    expect(confRowError(rows, 0)).toBeNull();
    expect(confRowError(rows, 1)).toMatch(/twice/);
    expect(confFromRows([{ key: " b ", value: "x" }, { key: "", value: "" }])).toEqual({ b: "x" });
  });
  it("feeds validation under ice.engine.sessionConf (Query engine section)", () => {
    const e = validateRemoteEngine(spark({ sessionConf: { "spark.jars": "s3://bucket/evil.jar" } }));
    expect(e["ice.engine.sessionConf"]).toMatch(/spark\.jars.*loads code/);
    expect(sectionOf("ice.engine.sessionConf")).toBe("queryEngine");
  });
});

describe("validateRemoteEngine", () => {
  it("accepts a complete Trino and a complete Spark engine", () => {
    expect(validateRemoteEngine(trino())).toEqual({});
    expect(validateRemoteEngine(spark({ proxyUser: "etl", sessionConf: { "spark.sql.catalog.lake.type": "glue" }, sessionStartTimeoutSecs: 600 }))).toEqual({});
  });
  it("requires the URL, the catalog and (Trino) the user", () => {
    const e = validateRemoteEngine(trino({ url: "", catalog: " ", user: "" }));
    expect(Object.keys(e).sort()).toEqual(["ice.engine.catalogName", "ice.engine.url", "ice.engine.user"]);
    expect(validateRemoteEngine(spark({ catalog: "" }))["ice.engine.catalogName"]).toMatch(/Spark catalog/);
  });
  it("requires the basic-auth user name and, when told, the secret", () => {
    expect(validateRemoteEngine(trino({ auth: { kind: "basic", username: "" } }))["ice.engine.username"]).toBeDefined();
    expect(validateRemoteEngine(trino({ auth: { kind: "basic", username: "u" } }), true)["ice.engine.secret"]).toMatch(/password/);
    expect(validateRemoteEngine(spark({ auth: { kind: "bearer" } }), true)["ice.engine.secret"]).toMatch(/token/);
    expect(validateRemoteEngine(spark(), true)["ice.engine.secret"]).toBeUndefined();
  });
  it("checks the catalog name, the timeout and the proxy user", () => {
    expect(validateRemoteEngine(trino({ catalog: "a.b" }))["ice.engine.catalogName"]).toBeDefined();
    expect(validateRemoteEngine(spark({ sessionStartTimeoutSecs: 0 }))["ice.engine.timeout"]).toBeDefined();
    expect(validateRemoteEngine(spark({ sessionStartTimeoutSecs: 1.5 }))["ice.engine.timeout"]).toBeDefined();
    expect(validateRemoteEngine(spark({ proxyUser: "a b" }))["ice.engine.proxyUser"]).toBeDefined();
  });
  it("has no switch to skip TLS verification (R4): the engine has no such field", () => {
    expect(Object.keys(trino()).sort()).toEqual(["auth", "caCertPath", "catalog", "kind", "url", "user"]);
    expect(Object.keys(spark()).sort()).toEqual(["auth", "caCertPath", "catalog", "kind", "proxyUser", "sessionConf", "sessionStartTimeoutSecs", "url"]);
  });
});

describe("Trino and Spark in an Iceberg connection", () => {
  it("are allowed with every catalog kind (Athena stays Glue / S3 Tables only)", () => {
    const rest = lake(trino());
    expect(rest.iceberg?.catalog.kind).toBe("rest");
    expect(normalizeIceberg(rest.iceberg as IcebergOptions).queryEngine.kind).toBe("trino");
    expect(normalizeIceberg(lake(spark()).iceberg as IcebergOptions).queryEngine.kind).toBe("spark");
    expect(normalizeIceberg(lake({ kind: "athena", workgroup: "primary", outputLocation: null, aws: null }).iceberg as IcebergOptions).queryEngine.kind).toBe("local");
    expect(validateProfile(rest, [], { catalog: false, s3Keys: false, engine: false })).toEqual({});
  });
  it("blankEngine builds empty Trino / Spark engines and carries catalog, auth and CA between them", () => {
    const t = blankEngine("trino", { kind: "local" });
    expect(t).toEqual({ kind: "trino", url: "", catalog: "", user: "", auth: { kind: "none" }, caCertPath: null });
    const s = blankEngine("spark", trino({ auth: { kind: "bearer" }, caCertPath: "/ca.pem" }));
    expect(s).toMatchObject({ kind: "spark", url: "", catalog: "iceberg", auth: { kind: "bearer" }, caCertPath: "/ca.pem", sessionConf: {}, proxyUser: null, sessionStartTimeoutSecs: null });
    expect(blankEngine("local", s)).toEqual({ kind: "local" });
  });
  it("puts engine errors in the Query engine section", () => {
    const e = validateProfile(lake(trino({ url: "" })), []);
    expect(e["ice.engine.url"]).toBeDefined();
    expect(firstErrorSection(e)).toBe("queryEngine");
  });
  it("summarises the engine for the section list", () => {
    expect(engineSummary(trino())).toBe("Trino · trino.example.com:8443");
    expect(engineSummary(spark({ url: "" }))).toBe("Spark");
  });
  it("cleans text fields before saving: trims, drops the trailing slash and blank optionals", () => {
    const c = cleanProfile(lake(spark({ url: " https://livy.example.com:8998/ ", catalog: " lake ", caCertPath: "  ", proxyUser: " etl ", sessionConf: { " spark.x ": " 1 " } })));
    expect(c.iceberg?.queryEngine).toEqual(spark({ url: "https://livy.example.com:8998", catalog: "lake", caCertPath: null, proxyUser: "etl", sessionConf: { "spark.x": "1" } }));
    const t = cleanProfile(lake(trino({ user: " u ", auth: { kind: "basic", username: " bob " } })));
    expect(t.iceberg?.queryEngine).toMatchObject({ user: "u", auth: { kind: "basic", username: "bob" } });
  });
});

describe("engine secret (saveProfile engineSecret: undefined keeps, \"\" deletes)", () => {
  const basic = (over: Partial<Extract<RemoteEngine, { kind: "trino" }>> = {}) => lake(trino({ auth: { kind: "basic", username: "bob" }, ...over }));

  it("names what the vault slot holds; none when the engine needs no secret", () => {
    expect(engineSecretKind(basic())).toBe("trino:basic");
    expect(engineSecretKind(lake(spark({ auth: { kind: "bearer" } })))).toBe("spark:bearer");
    expect(engineSecretKind(lake(trino()))).toBeNull();
    expect(engineSecretKind(lake({ kind: "local" }))).toBeNull();
    expect(engineSecretKind(newProfile("postgres"))).toBeNull();
  });
  it("a new connection stores the typed secret", () => {
    const d = initialDraft(null, "iceberg");
    d.profile = basic();
    d.engineSecret = { mode: "set", value: "pw" };
    expect(icebergSecretArgs(null, d).engineSecret).toBe("pw");
    expect(missingIcebergSecrets(null, d, undefined).engine).toBe(false);
    d.engineSecret = { mode: "set", value: "" };
    expect(icebergSecretArgs(null, d).engineSecret).toBeUndefined();
    expect(missingIcebergSecrets(null, d, undefined).engine).toBe(true);
  });
  it("editing keeps the stored secret unless it is replaced or removed", () => {
    const original = { ...basic(), id: "p" };
    const d = initialDraft(original);
    expect(d.engineSecret.mode).toBe("keep");
    expect(icebergSecretArgs(original, d).engineSecret).toBeUndefined();
    expect(missingIcebergSecrets(original, d, { hasIcebergSecret: false, hasS3Keys: false, hasEngineSecret: true }).engine).toBe(false);
    d.engineSecret = { mode: "remove", value: "" };
    expect(icebergSecretArgs(original, d).engineSecret).toBe("");
    d.engineSecret = { mode: "set", value: "new" };
    expect(icebergSecretArgs(original, d).engineSecret).toBe("new");
  });
  it("is missing when nothing is stored (or the backend does not report it) and nothing is typed", () => {
    const original = { ...basic(), id: "p" };
    const d = initialDraft(original);
    expect(missingIcebergSecrets(original, d, { hasIcebergSecret: false, hasS3Keys: false, hasEngineSecret: false }).engine).toBe(true);
    expect(missingIcebergSecrets(original, d, { hasIcebergSecret: false, hasS3Keys: false }).engine).toBe(true);
    expect(validateProfile(d.profile, [], missingIcebergSecrets(original, d, undefined))["ice.engine.secret"]).toMatch(/password/);
  });
  it("deletes the stored secret when the engine stops needing one", () => {
    const original = { ...basic(), id: "p" };
    for (const next of [lake(trino()), lake({ kind: "local" }), lake({ kind: "athena", workgroup: "primary", outputLocation: null, aws: null })]) {
      const d = initialDraft(original);
      d.profile = { ...next, id: "p" };
      expect(icebergSecretArgs(original, d).engineSecret).toBe("");
    }
  });
  it("deletes a kept secret whose meaning changed (basic -> bearer, Trino -> Spark), and stores a new one when typed", () => {
    const original = { ...basic(), id: "p" };
    const bearer = initialDraft(original);
    bearer.profile = { ...lake(trino({ auth: { kind: "bearer" } })), id: "p" };
    expect(icebergSecretArgs(original, bearer).engineSecret).toBe("");
    expect(engineSecretStored(original, bearer.profile, true)).toBe(false);
    expect(missingIcebergSecrets(original, bearer, { hasIcebergSecret: false, hasS3Keys: false, hasEngineSecret: true }).engine).toBe(true);
    bearer.engineSecret = { mode: "set", value: "jwt" };
    expect(icebergSecretArgs(original, bearer).engineSecret).toBe("jwt");
    const toSpark = initialDraft(original);
    toSpark.profile = { ...lake(spark({ auth: { kind: "basic", username: "bob" } })), id: "p" };
    expect(icebergSecretArgs(original, toSpark).engineSecret).toBe("");
  });
  it("leaves the slot alone for engines that never had a secret", () => {
    const original = { ...lake(trino()), id: "p" };
    const d = initialDraft(original);
    d.profile = { ...lake({ kind: "local" }), id: "p" };
    expect(icebergSecretArgs(original, d).engineSecret).toBeUndefined();
    const pg = initialDraft(null, "postgres");
    expect(icebergSecretArgs(null, pg).engineSecret).toBeUndefined();
  });
  it("does not touch the catalog secret slot (icebergSecret stays separate)", () => {
    const d = initialDraft(null, "iceberg");
    d.profile = lake(trino({ auth: { kind: "bearer" } }), { catalog: { kind: "rest", uri: "http://x", warehouse: null, auth: { kind: "bearer" }, headers: {} } });
    d.icebergSecret = { mode: "set", value: "catalog-token" };
    d.engineSecret = { mode: "set", value: "engine-token" };
    expect(icebergSecretArgs(null, d)).toMatchObject({ icebergSecret: "catalog-token", engineSecret: "engine-token" });
  });
});
