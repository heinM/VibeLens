import { describe, expect, it } from "vitest";
import { newProfile } from "../../../lib/defaults";
import type { ConnectionProfile } from "../../../ipc/types";
import { cleanProfile, initialDraft, secretArg, secretArgs, switchEngine } from "../draft";
import { normalizeStartUrl, regionFromArn, regionFromHost, startUrlError, SECRET_ARN_RE } from "../awsHelpers";
import { errorCountBySection, firstErrorSection, sectionOf, validateProfile } from "../validation";

const base = (over: Partial<ConnectionProfile> = {}): ConnectionProfile => ({ ...newProfile("postgres"), id: "p1", name: "orders", ...over });
const aws = { identity: { kind: "profile", profileName: "dev" } as const, region: "eu-west-1" };

describe("validateProfile", () => {
  it("accepts a default profile once named", () => {
    expect(validateProfile(base(), [])).toEqual({});
  });
  it("requires a unique name, case-insensitively, ignoring itself", () => {
    const other = base({ id: "p2", name: "Orders" });
    expect(validateProfile(base(), [other]).name).toMatch(/already/);
    expect(validateProfile(base({ id: "p2" }), [other])).toEqual({});
    expect(validateProfile(base({ name: "  " }), []).name).toMatch(/name/i);
  });
  it("validates host and port", () => {
    expect(validateProfile(base({ host: "" }), []).host).toBeDefined();
    expect(validateProfile(base({ host: "postgres://x" }), []).host).toBeDefined();
    expect(validateProfile(base({ port: 70000 }), []).port).toBeDefined();
    expect(validateProfile(base({ port: Number.NaN }), []).port).toBeDefined();
  });
  it("requires a database for postgres only", () => {
    expect(validateProfile(base({ database: "" }), []).database).toBeDefined();
    expect(validateProfile({ ...newProfile("valkey"), name: "c" }, [])).toEqual({});
  });
  it("validates Secrets Manager ARNs", () => {
    const arn = "arn:aws:secretsmanager:eu-west-1:123456789012:secret:prod/db-AbCdEf";
    expect(SECRET_ARN_RE.test(arn)).toBe(true);
    const ok = base({ auth: { kind: "secretsManager", secretArn: arn, aws } });
    expect(validateProfile(ok, [])).toEqual({});
    const bad = base({ auth: { kind: "secretsManager", secretArn: "arn:aws:s3:::bucket", aws } });
    expect(validateProfile(bad, [])["auth.secretArn"]).toBeDefined();
  });
  it("validates IAM identity completeness", () => {
    const p = base({ auth: { kind: "rdsIam", username: "app", aws: { identity: { kind: "profile", profileName: "" }, region: "" } } });
    const e = validateProfile(p, []);
    expect(e["aws.profile"]).toBeDefined();
    expect(e["aws.region"]).toBeDefined();
    const sso = base({
      auth: { kind: "rdsIam", username: "app", aws: { identity: { kind: "sso", startUrl: "https://d-1234567890.awsapps.com/start", ssoRegion: "eu-west-1", accountId: "", roleName: "" }, region: "eu-west-1" } },
    });
    expect(validateProfile(sso, [])["aws.account"]).toBeDefined();
    expect(validateProfile(base({ auth: { kind: "rdsIam", username: "app", aws: { identity: { kind: "defaultChain" }, region: "eu-west-1" } } }), [])).toEqual({});
  });
  it("validates ssh tunnels", () => {
    const t = { kind: "ssh" as const, host: "", port: 22, username: "", auth: { kind: "privateKey" as const, path: "" }, knownHostFingerprint: null };
    const e = validateProfile(base({ tunnel: t }), []);
    expect(Object.keys(e).sort()).toEqual(["tunnel.host", "tunnel.keyPath", "tunnel.username"]);
  });
  it("validates option ranges", () => {
    const p = base();
    p.pg.maxPoolSize = 0;
    p.pg.statementTimeoutMs = -5;
    const e = validateProfile(p, []);
    expect(e["pg.maxPoolSize"]).toBeDefined();
    expect(e["pg.statementTimeoutMs"]).toBeDefined();
  });
  it("maps errors to sections", () => {
    expect(sectionOf("auth.username")).toBe("auth");
    expect(sectionOf("aws.region")).toBe("auth");
    expect(sectionOf("tunnel.host")).toBe("network");
    expect(sectionOf("pg.maxPoolSize")).toBe("advanced");
    expect(sectionOf("name")).toBe("general");
    const errs = { host: "x", "tunnel.host": "y", "tunnel.port": "z" };
    expect(errorCountBySection(errs)).toMatchObject({ general: 1, network: 2 });
    expect(firstErrorSection(errs)).toBe("general");
    expect(firstErrorSection({})).toBeNull();
  });
});

describe("aws helpers", () => {
  it("extracts regions", () => {
    expect(regionFromArn("arn:aws:secretsmanager:eu-west-1:123456789012:secret:x")).toBe("eu-west-1");
    expect(regionFromArn("nonsense")).toBeNull();
    expect(regionFromHost("mydb.cluster-abc.af-south-1.rds.amazonaws.com")).toBe("af-south-1");
    expect(regionFromHost("my-cache.abc.use1.cache.amazonaws.com")).toBeNull();
    expect(regionFromHost("localhost")).toBeNull();
  });
  it("normalises and validates SSO start URLs", () => {
    expect(normalizeStartUrl("d-1234567890")).toBe("https://d-1234567890.awsapps.com/start");
    expect(normalizeStartUrl("my-org")).toBe("https://my-org.awsapps.com/start");
    expect(normalizeStartUrl(" https://d-1234567890.awsapps.com/start ")).toBe("https://d-1234567890.awsapps.com/start");
    expect(startUrlError("https://d-1234567890.awsapps.com/start")).toBeNull();
    expect(startUrlError("my-org")).toBeNull();
    expect(startUrlError("")).not.toBeNull();
    expect(startUrlError("http://insecure.example.com/start")).not.toBeNull();
    expect(startUrlError("https://awsapps.com")).toBeNull(); // has a dot; the backend decides
  });
});

describe("secrets and drafts", () => {
  it("maps secret edits to save arguments", () => {
    expect(secretArg({ mode: "keep", value: "" })).toBeUndefined();
    expect(secretArg({ mode: "remove", value: "" })).toBe("");
    expect(secretArg({ mode: "set", value: "" })).toBeUndefined();
    expect(secretArg({ mode: "set", value: "pw" })).toBe("pw");
  });
  it("new drafts store a typed password; edits keep by default", () => {
    const d = initialDraft(null);
    d.password.value = "pw";
    expect(secretArgs(null, d).password).toBe("pw");
    const e = initialDraft(base());
    expect(secretArgs(base(), e).password).toBeUndefined();
  });
  it("deletes stale secrets when switching away from them", () => {
    const original = base({ auth: { kind: "password", username: "u" }, tunnel: { kind: "ssh", host: "b", port: 22, username: "u", auth: { kind: "password" }, knownHostFingerprint: null } });
    const d = initialDraft(original);
    d.profile.auth = { kind: "none" };
    d.profile.tunnel = null;
    expect(secretArgs(original, d)).toEqual({ password: "", sshSecret: "" });
  });
  it("drops an old SSH secret when the auth kind changes and nothing new is typed", () => {
    const original = base({ tunnel: { kind: "ssh", host: "b", port: 22, username: "u", auth: { kind: "privateKey", path: "/k" }, knownHostFingerprint: null } });
    const d = initialDraft(original);
    if (d.profile.tunnel) d.profile.tunnel.auth = { kind: "password" };
    d.sshSecret = { mode: "set", value: "" };
    expect(secretArgs(original, d).sshSecret).toBe("");
    d.sshSecret = { mode: "set", value: "new" };
    expect(secretArgs(original, d).sshSecret).toBe("new");
  });
  it("does not send secrets that the current mode doesn't use", () => {
    const d = initialDraft(null);
    d.password.value = "leftover";
    d.profile.auth = { kind: "none" };
    expect(secretArgs(null, d).password).toBeUndefined();
  });
  it("switching engine resets port, auth and tls but keeps identity fields", () => {
    const p = { ...base({ name: "x", host: "h", environment: "prod", aiDataAccess: "none" }), port: 1234 };
    const v = switchEngine(p, "valkey");
    expect(v).toMatchObject({ engine: "valkey", name: "x", host: "h", environment: "prod", aiDataAccess: "none", port: 6379, database: null, auth: { kind: "none" } });
    expect(v.tls.mode).toBe("disable");
  });
  it("cleanProfile trims text fields without mutating the input", () => {
    const p = base({ name: "  a  ", host: " h " });
    const c = cleanProfile(p);
    expect(c.name).toBe("a");
    expect(c.host).toBe("h");
    expect(p.name).toBe("  a  ");
  });
});
