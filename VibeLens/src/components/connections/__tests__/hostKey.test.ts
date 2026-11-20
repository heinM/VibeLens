import { describe, expect, it, vi } from "vitest";
import { newProfile } from "../../../lib/defaults";
import type { ConnectionProfile, ConnectionStatus, ErrorPayload, PresentedHostKey, TunnelConfig } from "../../../ipc/types";
import { connectTrustingHostKey, isHostKeyFingerprint, parseHostKeyFingerprint, presentedHostKey, withTrustedHostKey } from "../hostKey";
import { useHostKeyPrompt } from "../hostKeyPrompt";
import { sectionOf, validateProfile } from "../validation";

const FP = "SHA256:n0Tar3alK3yButTheRightShape0123456789+/abcd";
const OTHER_FP = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const key = (over: Partial<PresentedHostKey> = {}): PresentedHostKey => ({ host: "bastion.example", port: 22, fingerprint: FP, ...over });
const tunnel = (over: Partial<TunnelConfig> = {}): TunnelConfig => ({
  kind: "ssh",
  host: "bastion.example",
  port: 22,
  username: "ops",
  auth: { kind: "agent" },
  knownHostFingerprint: null,
  ...over,
});
const profile = (t: TunnelConfig | null): ConnectionProfile => ({ ...newProfile("postgres"), id: "p1", name: "orders", tunnel: t });
const unknownKey = (k: PresentedHostKey = key()): ErrorPayload => ({ code: "unknown_host_key", message: "not trusted yet", hostKey: k });

describe("host key fingerprints (review findings M1, M2)", () => {
  it("recognises the shape ssh-keygen prints, and nothing else", () => {
    expect(isHostKeyFingerprint(FP)).toBe(true);
    expect(isHostKeyFingerprint(OTHER_FP)).toBe(true);
    for (const bad of ["", "garbage", "SHA256:tooshort", `${FP}=`, ` ${FP}`, `${FP} `, "MD5:aa:bb:cc", FP.replace("SHA256:", "sha256:"), `${FP}A`]) {
      expect(isHostKeyFingerprint(bad), bad).toBe(false);
    }
  });

  it("finds the fingerprint in what people paste", () => {
    expect(parseHostKeyFingerprint(FP)).toBe(FP);
    expect(parseHostKeyFingerprint(`  ${FP}\n`)).toBe(FP);
    // A whole `ssh-keygen -lf` line.
    expect(parseHostKeyFingerprint(`256 ${FP} root@bastion (ED25519)`)).toBe(FP);
    for (const bad of ["", "hello", "SHA256:short", `256 ${FP} and ${OTHER_FP}`]) {
      expect(parseHostKeyFingerprint(bad), bad).toBeNull();
    }
  });

  it("reads the presented key only from an unknown_host_key error that carries a real one", () => {
    expect(presentedHostKey(unknownKey())).toEqual(key());
    expect(presentedHostKey(null)).toBeNull();
    expect(presentedHostKey({ code: "tunnel", message: "host key mismatch", hostKey: key() })).toBeNull();
    expect(presentedHostKey({ code: "unknown_host_key", message: "x" })).toBeNull();
    expect(presentedHostKey(unknownKey(key({ fingerprint: "garbage" })))).toBeNull();
    expect(presentedHostKey(unknownKey(key({ host: "" })))).toBeNull();
  });

  it("pins a key on the draft only when the draft still points at the host that presented it", () => {
    const draft = profile(tunnel());
    const trusted = withTrustedHostKey(draft, key());
    expect(trusted?.tunnel?.knownHostFingerprint).toBe(FP);
    expect(draft.tunnel?.knownHostFingerprint).toBeNull(); // the draft itself is not mutated
    expect({ ...trusted, tunnel: draft.tunnel }).toEqual(draft); // nothing else changes

    expect(withTrustedHostKey(profile(null), key())).toBeNull();
    expect(withTrustedHostKey(profile(tunnel({ host: "other.example" })), key())).toBeNull();
    expect(withTrustedHostKey(profile(tunnel({ port: 2222 })), key())).toBeNull();
    // A different key never replaces a pin; the user has to Forget first.
    expect(withTrustedHostKey(profile(tunnel({ knownHostFingerprint: OTHER_FP })), key())).toBeNull();
    expect(withTrustedHostKey(profile(tunnel({ knownHostFingerprint: FP })), key())?.tunnel?.knownHostFingerprint).toBe(FP);
    expect(withTrustedHostKey(draft, key({ fingerprint: "garbage" }))).toBeNull();
  });

  it("validates a pasted pin in the network section", () => {
    expect(validateProfile(profile(tunnel()), [])).toEqual({});
    expect(validateProfile(profile(tunnel({ knownHostFingerprint: FP })), [])).toEqual({});
    const errors = validateProfile(profile(tunnel({ knownHostFingerprint: "garbage" })), []);
    expect(errors["tunnel.knownHostFingerprint"]).toMatch(/SHA256/);
    expect(sectionOf("tunnel.knownHostFingerprint")).toBe("network");
  });
});

describe("connectTrustingHostKey", () => {
  const status = { profileId: "p1", state: "connected" } as ConnectionStatus;
  const deps = (connect: (id: string) => Promise<ConnectionStatus>, answer = true) => ({
    connect: vi.fn(connect),
    ask: vi.fn(async () => answer),
    trust: vi.fn(async () => undefined),
  });

  it("connects without asking anything when the key is already pinned", async () => {
    const d = deps(async () => status);
    await expect(connectTrustingHostKey("p1", d)).resolves.toBe(status);
    expect(d.connect).toHaveBeenCalledTimes(1);
    expect(d.ask).not.toHaveBeenCalled();
    expect(d.trust).not.toHaveBeenCalled();
  });

  it("passes every other failure through untouched", async () => {
    const mismatch: ErrorPayload = { code: "tunnel", message: "host key mismatch" };
    const d = deps(async () => Promise.reject(mismatch));
    await expect(connectTrustingHostKey("p1", d)).rejects.toBe(mismatch);
    expect(d.ask).not.toHaveBeenCalled();
    expect(d.trust).not.toHaveBeenCalled();
  });

  it("stores nothing and does not reconnect when the user does not trust the key", async () => {
    const err = unknownKey();
    const d = deps(async () => Promise.reject(err), false);
    await expect(connectTrustingHostKey("p1", d)).rejects.toBe(err);
    expect(d.ask).toHaveBeenCalledWith("p1", key());
    expect(d.trust).not.toHaveBeenCalled();
    expect(d.connect).toHaveBeenCalledTimes(1);
  });

  it("pins exactly the presented key, then connects again", async () => {
    let calls = 0;
    const d = deps(async () => (++calls === 1 ? Promise.reject(unknownKey()) : status));
    await expect(connectTrustingHostKey("p1", d)).resolves.toBe(status);
    expect(d.trust).toHaveBeenCalledTimes(1);
    expect(d.trust).toHaveBeenCalledWith("p1", key());
    expect(d.connect).toHaveBeenCalledTimes(2);
    // Trust is stored before the second attempt.
    expect(d.trust.mock.invocationCallOrder[0]).toBeLessThan(d.connect.mock.invocationCallOrder[1]);
  });

  it("asks once per attempt: a second unknown key is reported, not looped on", async () => {
    const second = unknownKey(key({ fingerprint: OTHER_FP }));
    let calls = 0;
    const d = deps(async () => Promise.reject(++calls === 1 ? unknownKey() : second));
    await expect(connectTrustingHostKey("p1", d)).rejects.toBe(second);
    expect(d.ask).toHaveBeenCalledTimes(1);
    expect(d.connect).toHaveBeenCalledTimes(2);
  });

  it("does not reconnect when the pin could not be stored", async () => {
    const refused: ErrorPayload = { code: "invalid", message: "this connection's SSH tunnel now points at other:22" };
    const d = deps(async () => Promise.reject(unknownKey()));
    d.trust.mockRejectedValueOnce(refused);
    await expect(connectTrustingHostKey("p1", d)).rejects.toBe(refused);
    expect(d.connect).toHaveBeenCalledTimes(1);
  });
});

describe("useHostKeyPrompt", () => {
  const prompt = () => useHostKeyPrompt.getState();

  it("resolves with the user's answer and closes", async () => {
    const asked = prompt().ask("p1", key());
    expect(prompt().pending?.key).toEqual(key());
    prompt().answer(true);
    await expect(asked).resolves.toBe(true);
    expect(prompt().pending).toBeNull();

    const again = prompt().ask("p1", key());
    prompt().answer(false);
    await expect(again).resolves.toBe(false);
  });

  it("lets a second request about the same key join the open question", async () => {
    const a = prompt().ask("p1", key());
    const b = prompt().ask("p1", key());
    prompt().answer(true);
    await expect(Promise.all([a, b])).resolves.toEqual([true, true]);
  });

  it("never carries a yes over to a different key or connection", async () => {
    const first = prompt().ask("p1", key());
    const otherKey = prompt().ask("p1", key({ fingerprint: OTHER_FP }));
    await expect(first).resolves.toBe(false); // replaced, so not trusted
    const otherProfile = prompt().ask("p2", key({ fingerprint: OTHER_FP }));
    await expect(otherKey).resolves.toBe(false);
    expect(prompt().pending?.profileId).toBe("p2");
    prompt().answer(true);
    await expect(otherProfile).resolves.toBe(true);
  });
});
