import { describe, expect, it } from "vitest";
import { isSsoExpired, ssoLoginsFor } from "../ssoReauth";

describe("isSsoExpired", () => {
  it("matches the backend text wherever it appears", () => {
    expect(isSsoExpired({ message: "SSO session expired — sign in again" })).toBe(true);
    expect(isSsoExpired("[auth] authentication failed: SSO session expired — sign in again")).toBe(true);
    expect(isSsoExpired({ message: "password authentication failed" })).toBe(false);
    expect(isSsoExpired(null)).toBe(false);
    expect(isSsoExpired("")).toBe(false);
  });
});

describe("ssoLoginsFor", () => {
  const sso = { kind: "sso", startUrl: "https://d-1.awsapps.com/start", ssoRegion: "eu-west-1", accountId: "1", roleName: "r" };

  it("finds an inline Identity Center identity at any depth, once", () => {
    const profile = {
      engine: "postgres",
      auth: { kind: "rdsIam", username: "u", aws: { identity: sso, region: "af-south-1" } },
      iceberg: { storage: { kind: "s3", aws: { identity: sso, region: "af-south-1" } } },
    };
    expect(ssoLoginsFor(profile, undefined, undefined)).toEqual([{ startUrl: sso.startUrl, region: "eu-west-1" }]);
  });

  it("resolves a named AWS profile through a session the app already holds", () => {
    const profile = { auth: { kind: "rdsIam", aws: { identity: { kind: "profile", profileName: "dev" }, region: "af-south-1" } } };
    const awsProfiles = [{ name: "dev", kind: "sso" as const, region: "af-south-1", ssoStartUrl: "https://d-2.awsapps.com/start", ssoAccountId: null, ssoRoleName: null }];
    expect(ssoLoginsFor(profile, awsProfiles, [])).toEqual([]);
    expect(ssoLoginsFor(profile, awsProfiles, [{ startUrl: "https://d-2.awsapps.com/start", region: "us-east-1", expiresAt: null }])).toEqual([
      { startUrl: "https://d-2.awsapps.com/start", region: "us-east-1" },
    ]);
  });

  it("returns nothing for password, default-chain and static profiles", () => {
    expect(ssoLoginsFor({ auth: { kind: "password" } }, [], [])).toEqual([]);
    expect(ssoLoginsFor({ auth: { aws: { identity: { kind: "defaultChain" } } } }, [], [])).toEqual([]);
    const awsProfiles = [{ name: "keys", kind: "static" as const, region: null, ssoStartUrl: null, ssoAccountId: null, ssoRoleName: null }];
    expect(ssoLoginsFor({ aws: { identity: { kind: "profile", profileName: "keys" } } }, awsProfiles, [])).toEqual([]);
  });

  it("ignores blank values and unrelated objects that happen to have a kind", () => {
    expect(ssoLoginsFor({ a: { kind: "sso", startUrl: " ", ssoRegion: "eu-west-1" }, b: { kind: "sso" }, c: null }, [], [])).toEqual([]);
  });
});
