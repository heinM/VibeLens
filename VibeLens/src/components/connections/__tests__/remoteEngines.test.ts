// Trino and Spark (Livy) query engines of an Iceberg connection: URL and cleartext rules, the Spark
// session configuration rules, validation, the engine secret and engine switching. The rules mirror
// docs/REMOTE_ENGINES_AND_REDSHIFT.md sections 1.4 and 2.4; Rust enforces them again.
import { describe, expect, it } from "vitest";
import { newProfile } from "../../../lib/defaults";
import type { ConnectionProfile, IcebergOptions, IcebergQueryEngine } from "../../../ipc/types";
import { cleanProfile, draftErrors, icebergSecretArgs, initialDraft, missingIcebergSecrets, reconcileDraft } from "../draft";
import { blankEngine, engineSummary, normalizeIceberg } from "../icebergForm";
import {
  MAX_SESSION_START_TIMEOUT_SECS,
  SPARK_ALLOWED_CONF,
  cleartextProblem,
  confFromRows,
  confRowError,
  confRowsError,
  engineEndpointChanged,
  engineSecretArg,
  engineSecretKind,
  engineSecretMissing,
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
  it("treats as loopback exactly what Rust does: localhost and literal loopback IPs (127.0.0.0/8, ::1)", () => {
    for (const h of ["localhost", "LOCALHOST", "127.0.0.1", "127.0.0.2", "127.255.255.254", "[::1]", "::1"]) expect(isLoopbackHost(h), h).toBe(true);
    for (const h of ["trino.localhost", "localhost.example.com", "10.0.0.5", "trino.internal", "127.0.0.2.example.com", "127.0.0.256", "128.0.0.1", "[::2]"]) expect(isLoopbackHost(h), h).toBe(false);
    // As URL gives hostnames: an IPv6 loopback written in full is normalised to [::1].
    expect(isLoopbackHost(new URL("http://[0:0:0:0:0:0:0:1]:8080").hostname)).toBe(true);
  });
  it("sends no credential over http:// to a *.localhost name (Rust resolves those like any other)", () => {
    expect(cleartextProblem("http://trino.localhost:8080", { kind: "bearer" })?.level).toBe("error");
    expect(cleartextProblem("http://127.0.0.9:8080", { kind: "bearer" })).toBeNull();
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

describe("Spark session configuration: Rust's allow-list", () => {
  const ok = (k: string, v: string) => sessionConfError(k, v, "lake");
  it("allows the catalog implementation only with an Iceberg Spark catalog class", () => {
    expect(ok("spark.sql.catalog.lake", "org.apache.iceberg.spark.SparkCatalog")).toBeNull();
    expect(ok("spark.sql.catalog.lake", " org.apache.iceberg.spark.SparkSessionCatalog ")).toBeNull();
    expect(ok("spark.sql.catalog.lake", "com.evil.Catalog")).toMatch(/must be org\.apache\.iceberg\.spark\.SparkCatalog/);
  });
  it("allows the listed catalog options, for the engine's own catalog only", () => {
    for (const o of ["type", "warehouse", "default-namespace", "cache-enabled", "cache.expiration-interval-ms"]) expect(ok(`spark.sql.catalog.lake.${o}`, "x"), o).toBeNull();
    expect(ok("spark.sql.catalog.lake.uri", "thrift://x")).toMatch(/set this on the cluster instead/);
    expect(ok("spark.sql.catalog.other.type", "glue")).toMatch(/set this on the cluster instead/);
    expect(ok("spark.sql.catalog.other", "org.apache.iceberg.spark.SparkCatalog")).toMatch(/set this on the cluster instead/);
    expect(sessionConfError("spark.sql.catalog.lake.type", "glue", "Lake")).toMatch(/cluster/); // case-sensitive
  });
  it("allows exactly the listed session settings", () => {
    expect([...SPARK_ALLOWED_CONF].sort()).toEqual(
      [
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
      ].sort(),
    );
    for (const k of SPARK_ALLOWED_CONF) expect(ok(k, "4"), k).toBeNull();
    expect(ok(" spark.sql.shuffle.partitions ", "8")).toBeNull(); // after trim
    expect(ok("spark.sql.Shuffle.partitions", "8")).toMatch(/cluster/); // case-sensitive
  });
  it("refuses everything else with 'set this on the cluster instead', code loading and credentials included", () => {
    for (const k of ["spark.jars", "spark.jars.packages", "spark.files", "spark.submit.pyFiles", "spark.driver.extraJavaOptions", "spark.executor.extraClassPath", "spark.sql.extensions", "spark.hadoop.fs.s3a.secret.key", "spark.sql.catalog.lake.client-secret"]) {
      expect(ok(k, "x"), k).toMatch(/set this on the cluster instead/);
    }
  });
  it("checks values: no control characters, at most 512 characters, no ${, not a credential", () => {
    expect(ok("spark.executor.memory", "4g\n")).toMatch(/control character/);
    expect(ok("spark.executor.memory", "x".repeat(512))).toBeNull();
    expect(ok("spark.executor.memory", "x".repeat(513))).toMatch(/512/);
    expect(ok("spark.sql.catalog.lake.warehouse", "${env:HOME}")).toMatch(/\$\{/);
    expect(ok("spark.sql.session.timeZone", "Bearer abc")).toMatch(/credential.*cluster instead/);
    expect(ok("spark.sql.session.timeZone", "bearer abc")).toMatch(/credential/);
  });
  it("ignores an empty row but refuses a value without a name", () => {
    expect(ok("", "")).toBeNull();
    expect(ok("", "x")).toMatch(/name/);
  });
  it("flags duplicates and nameless values per row, and over all rows", () => {
    const rows = [
      { key: "spark.executor.cores", value: "1" },
      { key: "spark.executor.cores", value: "2" },
      { key: " ", value: "" },
    ];
    expect(confRowError(rows, 0, "lake")).toBeNull();
    expect(confRowError(rows, 1, "lake")).toMatch(/twice/);
    expect(confRowsError(rows, "lake")).toMatch(/twice/);
    expect(confRowsError([{ key: "", value: "4" }], "lake")).toMatch(/name/);
    expect(confRowsError([{ key: "", value: "" }], "lake")).toBeNull();
    expect(confFromRows([{ key: " b ", value: "x" }, { key: "", value: "" }])).toEqual({ b: "x" });
  });
  it("feeds validation under ice.engine.sessionConf (Query engine section)", () => {
    const e = validateRemoteEngine(spark({ sessionConf: { "spark.jars": "s3://bucket/evil.jar" } }));
    expect(e["ice.engine.sessionConf"]).toMatch(/spark\.jars.*cluster instead/);
    expect(sectionOf("ice.engine.sessionConf")).toBe("queryEngine");
  });
});

describe("validateRemoteEngine", () => {
  it("accepts a complete Trino and a complete Spark engine", () => {
    expect(validateRemoteEngine(trino())).toEqual({});
    expect(validateRemoteEngine(spark({ proxyUser: "etl", sessionConf: { "spark.sql.catalog.lake": "org.apache.iceberg.spark.SparkCatalog", "spark.sql.catalog.lake.type": "glue" }, sessionStartTimeoutSecs: 600 }))).toEqual({});
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
  it("checks names as Rust's plain_name does (no control characters, at most 256 bytes), no more", () => {
    expect(validateRemoteEngine(trino({ catalog: "a.b" }))["ice.engine.catalogName"]).toBeUndefined();
    expect(validateRemoteEngine(trino({ catalog: "a\tb" }))["ice.engine.catalogName"]).toMatch(/control/);
    expect(validateRemoteEngine(trino({ user: "x".repeat(257) }))["ice.engine.user"]).toMatch(/256/);
    expect(validateRemoteEngine(spark({ proxyUser: "a b" }))["ice.engine.proxyUser"]).toBeUndefined();
    expect(validateRemoteEngine(spark({ proxyUser: "a\u0007" }))["ice.engine.proxyUser"]).toMatch(/control/);
  });
  it("bounds the session start timeout to 1-3600 seconds, as Rust does", () => {
    expect(MAX_SESSION_START_TIMEOUT_SECS).toBe(3600);
    expect(validateRemoteEngine(spark({ sessionStartTimeoutSecs: 3600 }))["ice.engine.timeout"]).toBeUndefined();
    expect(validateRemoteEngine(spark({ sessionStartTimeoutSecs: 3601 }))["ice.engine.timeout"]).toMatch(/1-3600/);
    expect(validateRemoteEngine(spark({ sessionStartTimeoutSecs: 0 }))["ice.engine.timeout"]).toBeDefined();
    expect(validateRemoteEngine(spark({ sessionStartTimeoutSecs: 1.5 }))["ice.engine.timeout"]).toBeDefined();
  });
  it("refuses a basic-auth user name with ':' (HTTP basic cannot carry it)", () => {
    expect(validateRemoteEngine(trino({ auth: { kind: "basic", username: "dom:bob" } }))["ice.engine.username"]).toMatch(/':'/);
    expect(validateRemoteEngine(trino({ auth: { kind: "basic", username: "bob" } }))["ice.engine.username"]).toBeUndefined();
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

describe("a typed engine secret never follows an engine or sign-in switch (reconcileDraft)", () => {
  const typed = (profile: ConnectionProfile) => {
    const d = initialDraft(null, "iceberg");
    d.profile = profile;
    d.engineSecret = { mode: "set", value: "trino-password" };
    return d;
  };
  it("clears it when the engine kind changes (Trino basic -> Spark basic)", () => {
    const d = reconcileDraft(typed(lake(trino({ auth: { kind: "basic", username: "bob" } }))), lake(spark({ auth: { kind: "basic", username: "bob" } })));
    expect(d.engineSecret).toEqual({ mode: "set", value: "" });
    expect(icebergSecretArgs(null, d).engineSecret).toBeUndefined();
  });
  it("clears it when the sign-in kind changes (basic -> bearer)", () => {
    const d = reconcileDraft(typed(lake(trino({ auth: { kind: "basic", username: "bob" } }))), lake(trino({ auth: { kind: "bearer" } })));
    expect(d.engineSecret.value).toBe("");
  });
  it("keeps it for any other edit (URL, catalog, user name)", () => {
    const before = typed(lake(trino({ auth: { kind: "basic", username: "bob" } })));
    const d = reconcileDraft(before, lake(trino({ url: "https://other:8443", catalog: "x", auth: { kind: "basic", username: "alice" } })));
    expect(d.engineSecret).toEqual({ mode: "set", value: "trino-password" });
  });
  it("forgets the Spark rows typed for another engine", () => {
    const before = { ...typed(lake(spark())), sessionConfRows: [{ key: "", value: "x" }] };
    expect(reconcileDraft(before, lake(trino())).sessionConfRows).toBeNull();
    expect(reconcileDraft(before, lake(spark({ url: "https://x" }))).sessionConfRows).toEqual([{ key: "", value: "x" }]);
  });
});

describe("Spark rows the saved map cannot hold block Save (draftErrors)", () => {
  it("refuses a value without a name and a name set twice, which the map would silently drop", () => {
    const d = initialDraft(null, "iceberg");
    d.profile = lake(spark());
    expect(draftErrors(d)).toEqual({});
    d.sessionConfRows = [{ key: "", value: "8" }];
    expect(draftErrors(d)["ice.engine.sessionConf"]).toMatch(/name/);
    d.sessionConfRows = [
      { key: "spark.executor.cores", value: "1" },
      { key: "spark.executor.cores", value: "2" },
    ];
    expect(draftErrors(d)["ice.engine.sessionConf"]).toMatch(/twice/);
    expect(firstErrorSection(draftErrors(d))).toBe("queryEngine");
    d.sessionConfRows = [{ key: "spark.executor.cores", value: "2" }, { key: "", value: "" }];
    expect(draftErrors(d)).toEqual({});
  });
  it("ignores rows once the engine is not Spark", () => {
    const d = initialDraft(null, "iceberg");
    d.profile = lake(trino());
    d.sessionConfRows = [{ key: "", value: "8" }];
    expect(draftErrors(d)).toEqual({});
  });
});

describe("a stored engine secret is bound to its endpoint (Rust deletes it when the endpoint moves)", () => {
  const saved = { ...lake(trino({ url: "https://trino.example.com:8443/base", auth: { kind: "basic", username: "bob" }, caCertPath: "/ca.pem" })), id: "p" };
  const edited = (over: Partial<Extract<RemoteEngine, { kind: "trino" }>>) => ({ ...lake(trino({ url: "https://trino.example.com:8443/base", auth: { kind: "basic", username: "bob" }, caCertPath: "/ca.pem", ...over })), id: "p" });
  const keep = { mode: "keep" as const, value: "" };

  it("is unchanged by edits that keep scheme, host, port, path and CA (incl. a trailing slash or default port spelled out)", () => {
    expect(engineEndpointChanged(saved, edited({}))).toBe(false);
    expect(engineEndpointChanged(saved, edited({ url: "https://TRINO.example.com:8443/base/", user: "alice" }))).toBe(false);
    const std = { ...lake(trino({ url: "https://h", auth: { kind: "bearer" } })), id: "p" };
    expect(engineEndpointChanged(std, { ...lake(trino({ url: "https://h:443", auth: { kind: "bearer" } })), id: "p" })).toBe(false);
    expect(engineSecretStored(saved, edited({}), true)).toBe(true);
    expect(engineSecretArg(saved, edited({}), keep)).toBeUndefined();
  });
  it("moves with the scheme, host, port, path prefix or CA path", () => {
    for (const over of [{ url: "http://localhost:8443/base" }, { url: "https://other.example.com:8443/base" }, { url: "https://trino.example.com:9443/base" }, { url: "https://trino.example.com:8443/other" }, { caCertPath: "/other.pem" }, { caCertPath: null }]) {
      expect(engineEndpointChanged(saved, edited(over)), JSON.stringify(over)).toBe(true);
    }
  });
  it("then asks for the secret again instead of showing it as stored, and deletes the old one if none is typed", () => {
    const moved = edited({ url: "https://other.example.com:8443/base" });
    expect(engineSecretStored(saved, moved, true)).toBe(false);
    expect(engineSecretMissing(saved, moved, keep, true)).toBe(true);
    expect(engineSecretArg(saved, moved, keep)).toBe("");
    expect(engineSecretArg(saved, moved, { mode: "set", value: "new" })).toBe("new");
    expect(engineSecretMissing(saved, moved, { mode: "set", value: "new" }, true)).toBe(false);
  });
  it("treats Trino -> Spark at the same URL as a new endpoint too", () => {
    const spk = { ...lake(spark({ url: "https://trino.example.com:8443/base", auth: { kind: "basic", username: "bob" }, caCertPath: "/ca.pem" })), id: "p" };
    expect(engineSecretStored(saved, spk, true)).toBe(false);
  });
});
