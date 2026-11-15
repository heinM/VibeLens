import { describe, expect, it } from "vitest";
import { newProfile } from "../../../lib/defaults";
import type { ConnectionProfile, ImportCandidate, ImportReport } from "../../../ipc/types";
import { cleanProfile, initialDraft, secretArgs, testPasswordArg, usesPassword } from "../draft";
import { validateProfile } from "../validation";
import { endpointOf, importName, initialSelection } from "../ImportDialog";

const base = (over: Partial<ConnectionProfile> = {}): ConnectionProfile => ({ ...newProfile("postgres"), id: "p1", name: "pgproxy", ...over });

describe("access-token auth", () => {
  it("a pasted token uses the keychain password slot; a command does not", () => {
    expect(usesPassword({ kind: "accessToken", username: "", command: null })).toBe(true);
    expect(usesPassword({ kind: "accessToken", username: "", command: "  " })).toBe(true);
    expect(usesPassword({ kind: "accessToken", username: "", command: "az account get-access-token" })).toBe(false);
  });

  it("switching password -> pasted token without typing one deletes the old password", () => {
    const original = base({ auth: { kind: "password", username: "app" } });
    const d = initialDraft(original);
    d.profile = { ...original, auth: { kind: "accessToken", username: "", command: null } };
    expect(secretArgs(original, d).password).toBe("");
  });

  it("a typed token is what Test sends", () => {
    const d = initialDraft(null);
    d.profile = base({ auth: { kind: "accessToken", username: "", command: null } });
    d.password = { mode: "set", value: "eyJ.a.b" };
    expect(testPasswordArg(d)).toBe("eyJ.a.b");
    d.profile = base({ auth: { kind: "accessToken", username: "", command: "get-token" } });
    expect(testPasswordArg(d)).toBeUndefined();
  });

  it("validates and cleans", () => {
    expect(validateProfile(base({ auth: { kind: "accessToken", username: "", command: null } }), [])).toEqual({});
    expect(validateProfile(base({ auth: { kind: "accessToken", username: "", command: "a\nb" } }), [])["auth.command"]).toBeDefined();
    const c = cleanProfile(base({ auth: { kind: "accessToken", username: " u ", command: "   " } }));
    expect(c.auth).toEqual({ kind: "accessToken", username: "u", command: null });
  });
});

describe("PGProxy options", () => {
  it("validates the health URL and trims it", () => {
    const p = (healthUrl: string | null) => base({ pg: { ...newProfile("postgres").pg, pgproxy: { mode: "shunt", healthUrl } } });
    expect(validateProfile(p("http://pgproxy:8080"), [])).toEqual({});
    expect(validateProfile(p(null), [])).toEqual({});
    expect(validateProfile(p("pgproxy:8080"), [])["pg.pgproxy.healthUrl"]).toBeDefined();
    expect(cleanProfile(p(" http://pgproxy:8080/ ")).pg.pgproxy?.healthUrl).toBe("http://pgproxy:8080");
    expect(cleanProfile(p("  ")).pg.pgproxy?.healthUrl).toBeNull();
  });
});

describe("import dialog helpers", () => {
  const cand = (over: Partial<ImportCandidate> = {}): ImportCandidate => ({
    source: "dBeaver",
    sourceFile: "f",
    group: "Prod",
    profile: { ...base({ id: crypto.randomUUID(), name: "orders", host: "db", port: 5432, database: "app" }) },
    warnings: [],
    needsSecret: true,
    duplicateOf: null,
    ...over,
  });
  it("names, endpoints and default selection", () => {
    const a = cand();
    const dup = cand({ duplicateOf: "existing" });
    expect(importName(a, false)).toBe("orders");
    expect(importName(a, true)).toBe("Prod / orders");
    expect(importName(cand({ group: null }), true)).toBe("orders");
    expect(endpointOf(a)).toBe("db:5432/app");
    const r: ImportReport = { source: "dBeaver", path: "f", candidates: [a, dup], skipped: [], error: null };
    expect([...initialSelection([r])]).toEqual([a.profile.id]);
  });
});
