// Redshift connections: defaults, engine switching, the identifier and ARN checks (design 3.4), target /
// sign-in consistency, cleaning, and the fact that nothing host/port/password-shaped is asked for or saved.
import { describe, expect, it } from "vitest";
import { newProfile, newRedshiftOptions, usesHostPort } from "../../../lib/defaults";
import type { ConnectionProfile, RedshiftOptions } from "../../../ipc/types";
import { cleanProfile, icebergSecretArgs, initialDraft, secretArgs, switchEngine, testPasswordArg } from "../draft";
import { authAllowed, blankRedshiftAuth, blankTarget, clusterIdError, databaseError, dbUserError, isRedshiftDatabaseName, normalizeRedshift, requestedDatabaseError, redshiftSummary, validateRedshift, workgroupError } from "../redshiftForm";
import { errorCountBySection, firstErrorSection, sectionOf, validateProfile } from "../validation";

const aws = { identity: { kind: "profile", profileName: "dev" } as const, region: "eu-west-1" };
const opts = (over: Partial<RedshiftOptions> = {}): RedshiftOptions => ({ ...newRedshiftOptions(), target: { kind: "provisioned", clusterIdentifier: "analytics" }, aws, ...over });
const rs = (over: Partial<RedshiftOptions> = {}): ConnectionProfile => ({ ...newProfile("redshift"), name: "warehouse", redshift: opts(over) });

describe("Redshift defaults and engine switching", () => {
  it("a new Redshift profile has no host, port, password, TLS or tunnel, and IAM sign-in", () => {
    const p = newProfile("redshift");
    expect(p).toMatchObject({ engine: "redshift", host: "", port: 0, database: null, auth: { kind: "none" }, tunnel: null, iceberg: null });
    expect(p.tls.mode).toBe("disable");
    expect(p.redshift).toEqual({ target: { kind: "provisioned", clusterIdentifier: "" }, database: "dev", aws: { identity: { kind: "profile", profileName: "" }, region: "" }, auth: { kind: "iamIdentity" } });
    expect(usesHostPort("redshift")).toBe(false);
    expect(newProfile("postgres").redshift).toBeNull();
  });
  it("switching to Redshift keeps identity fields, drops the tunnel and the Iceberg options", () => {
    const pg = { ...newProfile("postgres"), name: "x", environment: "prod" as const, aiDataAccess: "none" as const, tunnel: { kind: "ssh" as const, host: "b", port: 22, username: "u", auth: { kind: "agent" as const }, knownHostFingerprint: null } };
    const r = switchEngine(pg, "redshift");
    expect(r).toMatchObject({ engine: "redshift", name: "x", environment: "prod", aiDataAccess: "none", tunnel: null, iceberg: null, port: 0 });
    expect(r.redshift?.auth.kind).toBe("iamIdentity");
    const back = switchEngine(r, "postgres");
    expect(back.redshift).toBeNull();
    expect(back.port).toBe(5432);
  });
  it("sends no password, SSH secret, catalog secret or engine secret on save or test", () => {
    const d = initialDraft(null, "redshift");
    d.profile = rs();
    d.password = { mode: "set", value: "ignored" };
    expect(secretArgs(null, d)).toEqual({ password: undefined, sshSecret: undefined });
    expect(icebergSecretArgs(null, d)).toEqual({ icebergSecret: undefined, s3Keys: undefined, engineSecret: undefined });
    expect(testPasswordArg(d)).toBeUndefined();
  });
});

describe("Redshift identifiers", () => {
  it("cluster identifier: lowercase, starts with a letter, no -- and no trailing hyphen, at most 63", () => {
    for (const ok of ["a", "analytics", "analytics-cluster-1", "a".repeat(63)]) expect(clusterIdError(ok), ok).toBeNull();
    for (const bad of ["", "Analytics", "1cluster", "my--cluster", "cluster-", "my_cluster", "a".repeat(64), "my.cluster"]) expect(clusterIdError(bad), bad).not.toBeNull();
  });
  it("workgroup name: 3-64 lowercase letters, digits or hyphens", () => {
    for (const ok of ["abc", "analytics-wg", "1wg", "a".repeat(64)]) expect(workgroupError(ok), ok).toBeNull();
    for (const bad of ["", "ab", "Analytics", "wg_1", "a".repeat(65)]) expect(workgroupError(bad), bad).not.toBeNull();
  });
  it("database: 1-64 lowercase letters, digits, _ or $, starting with a letter or _", () => {
    for (const ok of ["dev", "_tmp", "sales$2026", "a".repeat(64)]) expect(databaseError(ok), ok).toBeNull();
    for (const bad of ["", "Dev", "1db", "my-db", "a".repeat(65), "db;drop"]) expect(databaseError(bad), bad).not.toBeNull();
  });
  it("database user: a letter first, then letters, digits or _ + . @ -", () => {
    for (const ok of ["analyst", "IAM_user", "jane.doe@example.com", "a+b-c"]) expect(dbUserError(ok), ok).toBeNull();
    for (const bad of ["", "1user", "a:b", "a/b", "a b", "a".repeat(65)]) expect(dbUserError(bad), bad).not.toBeNull();
  });
});

describe("validateRedshift / validateProfile", () => {
  it("accepts a complete provisioned and serverless profile, ignoring host and port", () => {
    expect(validateProfile(rs(), [])).toEqual({});
    expect(validateProfile(rs({ target: { kind: "serverless", workgroupName: "analytics" } }), [])).toEqual({});
  });
  it("still requires a unique name", () => {
    expect(validateProfile({ ...rs(), name: "" }, []).name).toBeDefined();
    expect(validateProfile(rs(), [{ ...rs(), id: "other" }]).name).toMatch(/already/);
  });
  it("reports target, database and AWS identity problems in the Warehouse section", () => {
    const e = validateProfile(rs({ target: { kind: "provisioned", clusterIdentifier: "" }, database: "", aws: { identity: { kind: "profile", profileName: "" }, region: "" } }), []);
    expect(Object.keys(e).sort()).toEqual(["rs.aws.profile", "rs.aws.region", "rs.clusterIdentifier", "rs.database"]);
    expect(Object.keys(e).every((k) => sectionOf(k) === "warehouse")).toBe(true);
    expect(firstErrorSection(e)).toBe("warehouse");
    expect(errorCountBySection(e).warehouse).toBe(4);
    expect(validateProfile(rs({ target: { kind: "serverless", workgroupName: "x" } }), [])["rs.workgroupName"]).toBeDefined();
  });
  it("validates the sign-in: database user (provisioned only) and the Secrets Manager ARN shape", () => {
    expect(validateRedshift(opts({ auth: { kind: "dbUser", dbUser: "" } }))["rs.dbUser"]).toMatch(/required/);
    expect(validateRedshift(opts({ auth: { kind: "dbUser", dbUser: "analyst" } }))).toEqual({});
    expect(validateRedshift(opts({ target: { kind: "serverless", workgroupName: "wg1" }, auth: { kind: "dbUser", dbUser: "analyst" } }))["rs.dbUser"]).toMatch(/provisioned/);
    expect(validateRedshift(opts({ auth: { kind: "secret", secretArn: "" } }))["rs.secretArn"]).toMatch(/required/);
    expect(validateRedshift(opts({ auth: { kind: "secret", secretArn: "arn:aws:s3:::bucket" } }))["rs.secretArn"]).toMatch(/secretsmanager/);
    expect(validateRedshift(opts({ auth: { kind: "secret", secretArn: "arn:aws:secretsmanager:eu-west-1:123456789012:secret:rs-AbCdEf" } }))).toEqual({});
  });
  it("does not run the Postgres or Valkey checks on a Redshift profile", () => {
    const p = rs();
    p.valkey.keyDelimiter = "";
    p.pg.applicationName = "";
    expect(validateProfile(p, [])).toEqual({});
  });
});

describe("target and sign-in consistency", () => {
  it("blankTarget / blankRedshiftAuth start empty and keep the current value when unchanged", () => {
    const t = { kind: "provisioned" as const, clusterIdentifier: "c" };
    expect(blankTarget("provisioned", t)).toBe(t);
    expect(blankTarget("serverless", t)).toEqual({ kind: "serverless", workgroupName: "" });
    expect(blankRedshiftAuth("dbUser", { kind: "iamIdentity" })).toEqual({ kind: "dbUser", dbUser: "" });
    expect(blankRedshiftAuth("secret", { kind: "iamIdentity" })).toEqual({ kind: "secret", secretArn: "" });
    expect(blankRedshiftAuth("iamIdentity", { kind: "secret", secretArn: "x" })).toEqual({ kind: "iamIdentity" });
  });
  it("a database user is offered for provisioned clusters only; going serverless falls back to IAM", () => {
    expect(authAllowed("dbUser", "provisioned")).toBe(true);
    expect(authAllowed("dbUser", "serverless")).toBe(false);
    expect(authAllowed("secret", "serverless")).toBe(true);
    const o = opts({ target: { kind: "serverless", workgroupName: "wg1" }, auth: { kind: "dbUser", dbUser: "a" } });
    expect(normalizeRedshift(o).auth).toEqual({ kind: "iamIdentity" });
    const fine = opts();
    expect(normalizeRedshift(fine)).toBe(fine);
  });
  it("cleanProfile trims Redshift fields without mutating the draft", () => {
    const p = rs({ target: { kind: "provisioned", clusterIdentifier: " analytics " }, database: " dev ", aws: { ...aws, region: " eu-west-1 " }, auth: { kind: "secret", secretArn: " arn:aws:secretsmanager:eu-west-1:123456789012:secret:x " } });
    const c = cleanProfile(p);
    expect(c.redshift).toEqual({ target: { kind: "provisioned", clusterIdentifier: "analytics" }, database: "dev", aws, auth: { kind: "secret", secretArn: "arn:aws:secretsmanager:eu-west-1:123456789012:secret:x" } });
    expect(p.redshift?.database).toBe(" dev ");
  });
  it("summarises the target the way the approval card names it", () => {
    expect(redshiftSummary(opts())).toBe("Redshift · analytics · dev · eu-west-1");
    expect(redshiftSummary(opts({ target: { kind: "serverless", workgroupName: "" } }))).toBe("Redshift · no workgroup · dev · eu-west-1");
    expect(redshiftSummary(null)).toBe("Amazon Redshift");
  });
});

describe("parity with Rust's Redshift checks", () => {
  const arn = (region: string, name: string) => `arn:aws:secretsmanager:${region}:123456789012:secret:${name}`;
  it("limits the secret name to [A-Za-z0-9/_+=.@-]", () => {
    expect(validateRedshift(opts({ auth: { kind: "secret", secretArn: arn("eu-west-1", "team/rs_ro+1=a.b@c-AbCdEf") } }))).toEqual({});
    for (const bad of ["rs ro", "rs;ro", "rs*", "rs:ro", "ré"]) {
      expect(validateRedshift(opts({ auth: { kind: "secret", secretArn: arn("eu-west-1", bad) } }))["rs.secretArn"], bad).toMatch(/Expected arn/);
    }
  });
  it("requires the secret's region to be the connection's AWS region", () => {
    expect(validateRedshift(opts({ auth: { kind: "secret", secretArn: arn("us-east-1", "rs") } }))["rs.secretArn"]).toMatch(/us-east-1.*eu-west-1/);
    expect(validateRedshift(opts({ aws: { ...aws, region: "us-east-1" }, auth: { kind: "secret", secretArn: arn("us-east-1", "rs") } }))).toEqual({});
  });
  it("only sends a requested database name of ASCII letters, digits, _ and $", () => {
    for (const ok of ["dev", "Sales_2026", "a$b"]) expect(isRedshiftDatabaseName(ok), ok).toBe(true);
    for (const bad of ["", "my-db", "a b", "db;drop", "dév"]) expect(isRedshiftDatabaseName(bad), bad).toBe(false);
    expect(requestedDatabaseError(null)).toBeNull();
    expect(requestedDatabaseError("dev")).toBeNull();
    expect(requestedDatabaseError("my-db")).toMatch(/ASCII letters/);
  });
});
