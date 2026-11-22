// MySQL connections: defaults, engine switching, the rules of crates/vibelens-core/src/mysql/validate.rs
// (TLS for passwords to other machines and for RDS IAM, database and user names, options), and the
// password binding of mysql/secret.rs (a changed endpoint means the password must be typed again).
import { describe, expect, it } from "vitest";
import { defaultPort, newProfile, usesHostPort } from "../../../lib/defaults";
import type { ConnectionProfile, TlsMode, TunnelConfig } from "../../../ipc/types";
import { cleanProfile, initialDraft, secretArgs, switchEngine, testPasswordArg } from "../draft";
import { mysqlDatabaseError, mysqlPasswordError, mysqlPasswordUnbound, validateMysql } from "../mysqlForm";
import { errorCountBySection, firstErrorSection, sectionOf, validateProfile } from "../validation";

const tunnel: TunnelConfig = { kind: "ssh", host: "bastion", port: 22, username: "ops", auth: { kind: "agent" }, knownHostFingerprint: null };
const my = (over: Partial<ConnectionProfile> = {}, tls: TlsMode = "verifyFull"): ConnectionProfile => ({
  ...newProfile("mysql"),
  name: "shop",
  host: "db.internal",
  database: "shop",
  auth: { kind: "password", username: "app" },
  ...over,
  tls: { mode: tls, caCertPath: null, useRdsBundle: false, ...(over.tls ?? {}) },
});
const iam = { kind: "rdsIam" as const, username: "app", aws: { identity: { kind: "profile" as const, profileName: "dev" }, region: "eu-west-1" } };

describe("MySQL defaults and engine switching", () => {
  it("a new MySQL profile: port 3306, password auth, Verify full, required database, default options", () => {
    const p = newProfile("mysql");
    expect(p).toMatchObject({ engine: "mysql", host: "localhost", port: 3306, database: "", auth: { kind: "password", username: "root" }, tunnel: null });
    expect(p.tls.mode).toBe("verifyFull");
    expect(p.mysql).toEqual({ connectTimeoutSecs: 30, statementTimeoutMs: null, idleDisconnectSecs: 300, maxPoolSize: 4 });
    expect(usesHostPort("mysql")).toBe(true);
    expect(defaultPort("mysql")).toBe(3306);
  });
  it("switching to MySQL keeps host, identity fields and the SSH tunnel", () => {
    const pg = { ...newProfile("postgres"), name: "x", host: "h", environment: "prod" as const, tunnel };
    const m = switchEngine(pg, "mysql");
    expect(m).toMatchObject({ engine: "mysql", name: "x", host: "h", environment: "prod", port: 3306, tunnel });
    expect(switchEngine(m, "postgres").port).toBe(5432);
  });
  it("cleaning trims the database, so a stray space never reaches Rust's check", () => {
    expect(cleanProfile(my({ database: "  shop " })).database).toBe("shop");
  });
  it("saves and tests the typed password like Postgres", () => {
    const d = initialDraft(null, "mysql");
    d.profile = my();
    d.password = { mode: "set", value: "s3cret" };
    expect(secretArgs(null, d).password).toBe("s3cret");
    expect(testPasswordArg(d)).toBe("s3cret");
  });
});

describe("MySQL validation mirrors Rust", () => {
  it("a password to another machine needs Verify full; the message names the risk", () => {
    for (const tls of ["disable", "require"] as const) {
      const e = validateMysql(my({}, tls));
      expect(e["tls.mode"]).toMatch(/reusable credential/);
      expect(sectionOf("tls.mode")).toBe("tls");
    }
    expect(validateMysql(my())["tls.mode"]).toBeUndefined();
  });
  it("an SSH tunnel does not lift the rule, even to a loopback host behind it", () => {
    expect(validateMysql(my({ tunnel }, "disable"))["tls.mode"]).toMatch(/SSH tunnel/);
    expect(validateMysql(my({ tunnel, host: "127.0.0.1" }, "require"))["tls.mode"]).toBeTruthy();
  });
  it("this machine (no tunnel) may use any TLS mode with a password", () => {
    for (const host of ["localhost", "127.0.0.1", "127.9.9.9", "::1"])
      for (const tls of ["disable", "require", "verifyFull"] as const) expect(validateMysql(my({ host }, tls))["tls.mode"], `${host} ${tls}`).toBeUndefined();
    expect(validateMysql(my({ host: "db.localhost" }, "disable"))["tls.mode"]).toBeTruthy();
  });
  it("RDS IAM always needs Verify full, on this machine too, and an AWS region", () => {
    expect(validateMysql(my({ auth: iam, host: "localhost" }, "require"))["tls.mode"]).toMatch(/cleartext plugin/);
    expect(validateMysql(my({ auth: iam }))["tls.mode"]).toBeUndefined();
    const noRegion = { ...iam, aws: { ...iam.aws, region: "" } };
    expect(Object.keys(validateProfile(my({ auth: noRegion }), [])).some((k) => sectionOf(k) === "auth")).toBe(true);
  });
  it("only password and RDS IAM sign-in", () => {
    expect(validateMysql(my({ auth: { kind: "none" } }))["auth.kind"]).toBeTruthy();
    expect(validateMysql(my({ auth: { kind: "secretsManager", secretArn: "arn", aws: iam.aws } }))["auth.kind"]).toBeTruthy();
  });
  it("database: required, at most 64 characters, no control characters or outer spaces", () => {
    for (const ok of ["shop", "a".repeat(64), "my-db", "ünïcode", "a b"]) expect(mysqlDatabaseError(ok), ok).toBeNull();
    for (const bad of ["", "   ", null, "a".repeat(65), "bad\u0000name", " shop", "shop "]) expect(mysqlDatabaseError(bad), String(bad)).not.toBeNull();
    expect(validateMysql(my({ database: null })).database).toMatch(/required/);
  });
  it("user name: required, at most 32 characters", () => {
    expect(validateMysql(my({ auth: { kind: "password", username: "" } }))["auth.username"]).toBeTruthy();
    expect(validateMysql(my({ auth: { kind: "password", username: "u".repeat(33) } }))["auth.username"]).toMatch(/32/);
    expect(validateMysql(my({ auth: { kind: "password", username: "u".repeat(32) } }))["auth.username"]).toBeUndefined();
  });
  it("options: pool 1-32, connect timeout 1-600, statement timeout positive or empty; errors land in Advanced", () => {
    const e = validateMysql(my({ mysql: { connectTimeoutSecs: 0, statementTimeoutMs: 0, idleDisconnectSecs: -1, maxPoolSize: 33 } }));
    expect(Object.keys(e).sort()).toEqual(["my.connectTimeoutSecs", "my.idleDisconnectSecs", "my.maxPoolSize", "my.statementTimeoutMs"]);
    expect(errorCountBySection(e).advanced).toBe(4);
    expect(validateMysql(my({ mysql: { connectTimeoutSecs: 600, statementTimeoutMs: null, idleDisconnectSecs: 0, maxPoolSize: 32 } }))).toEqual({});
  });
  it("validateProfile applies the MySQL rules and the shared host/port rules", () => {
    const e = validateProfile(my({ host: "", port: 0 }, "disable"), []);
    expect(e.host).toBeTruthy();
    expect(e.port).toBeTruthy();
    expect(validateProfile(my(), [])).toEqual({});
    expect(firstErrorSection(validateProfile(my({}, "disable"), []))).toBe("tls");
  });
});

describe("MySQL password bound to its endpoint", () => {
  const saved = my();
  const keep = { mode: "keep" as const, value: "" };
  it("an edit of host, port, user, TLS, CA, RDS bundle or tunnel unbinds the saved password", () => {
    const edits: Partial<ConnectionProfile>[] = [
      { host: "other.internal" },
      { port: 3307 },
      { auth: { kind: "password", username: "root" } },
      { tls: { mode: "require", caCertPath: null, useRdsBundle: false } },
      { tls: { mode: "verifyFull", caCertPath: "/ca.pem", useRdsBundle: false } },
      { tls: { mode: "verifyFull", caCertPath: null, useRdsBundle: true } },
      { tunnel },
    ];
    for (const e of edits) {
      const p = { ...saved, ...e };
      expect(mysqlPasswordUnbound(saved, p), JSON.stringify(e)).toBe(true);
      expect(mysqlPasswordError(saved, p, keep, true), JSON.stringify(e)).toMatch(/Enter it again/);
      expect(mysqlPasswordError(saved, p, { mode: "set", value: "pw" }, true)).toBeNull();
    }
  });
  it("cosmetic edits keep it; a new connection needs one typed", () => {
    expect(mysqlPasswordUnbound(saved, { ...saved, name: "x", color: "#fff", environment: "prod", aiDataAccess: "results", database: "other" })).toBe(false);
    expect(mysqlPasswordError(saved, { ...saved, name: "x" }, keep, true)).toBeNull();
    expect(mysqlPasswordError(null, my(), { mode: "set", value: "" }, false)).toMatch(/Enter the password/);
    expect(mysqlPasswordError(null, my({ auth: iam }), { mode: "set", value: "" }, false)).toBeNull();
    expect(mysqlPasswordError(null, newProfile("postgres"), { mode: "set", value: "" }, false)).toBeNull();
  });
});
