// SSH host keys on the UI side. VibeLens never trusts a host key silently: with no pin, Rust stops
// after the key exchange (before any authentication) and reports the key the server presented.
// The user checks it and trusts it, which is the only way a pin gets written.
import type { ConnectionProfile, ConnectionStatus, ErrorPayload, PresentedHostKey } from "../../ipc/types";
import { asError } from "../../ipc/commands";

const FINGERPRINT_RE = /^SHA256:[A-Za-z0-9+/]{43}$/;

/** `SHA256:` plus the 43 unpadded base64 characters of a 32-byte digest. Mirrors `is_host_key_fingerprint` in Rust. */
export function isHostKeyFingerprint(s: string): boolean {
  return FINGERPRINT_RE.test(s);
}

/**
 * The fingerprint in pasted text: the bare fingerprint, or a whole `ssh-keygen -lf` line
 * (`256 SHA256:… comment (ED25519)`). Null when there is none, or more than one to choose from.
 */
export function parseHostKeyFingerprint(pasted: string): string | null {
  const found = new Set(pasted.split(/\s+/).filter(isHostKeyFingerprint));
  return found.size === 1 ? [...found][0] : null;
}

/** The key an `unknown_host_key` error carries; null for every other error, and for a malformed one. */
export function presentedHostKey(e: ErrorPayload | null | undefined): PresentedHostKey | null {
  const key = e?.code === "unknown_host_key" ? e.hostKey : null;
  if (!key || !key.host || !Number.isInteger(key.port) || !isHostKeyFingerprint(key.fingerprint)) return null;
  return key;
}

/**
 * The draft with `key` pinned, or null when the key does not belong on it: no tunnel, a tunnel
 * that points at another host or port than the one that presented the key, or a different pin
 * already in place (only Forget removes a pin). Same rules as `trust_host_key` in Rust.
 */
export function withTrustedHostKey(profile: ConnectionProfile, key: PresentedHostKey): ConnectionProfile | null {
  const t = profile.tunnel;
  if (!t || t.host !== key.host || t.port !== key.port || !isHostKeyFingerprint(key.fingerprint)) return null;
  const pinned = t.knownHostFingerprint?.trim();
  if (pinned && pinned !== key.fingerprint) return null;
  return { ...profile, tunnel: { ...t, knownHostFingerprint: key.fingerprint } };
}

export interface TrustDeps {
  connect: (profileId: string) => Promise<ConnectionStatus>;
  /** Shows the key to the user; resolves true only when they chose to trust it. */
  ask: (profileId: string, key: PresentedHostKey) => Promise<boolean>;
  /** Pins the key on the saved profile. */
  trust: (profileId: string, key: PresentedHostKey) => Promise<unknown>;
}

/**
 * Connects; when the SSH host's key is not pinned, asks the user once and, if they trust it, pins
 * exactly that key and connects again. Anything else (another failure, a "no", a refused pin, a
 * second unknown key) is rejected as it is, so the caller shows it like any connection error.
 */
export async function connectTrustingHostKey(profileId: string, deps: TrustDeps): Promise<ConnectionStatus> {
  try {
    return await deps.connect(profileId);
  } catch (e) {
    const key = presentedHostKey(asError(e));
    if (!key || !(await deps.ask(profileId, key))) throw e;
    await deps.trust(profileId, key);
    return deps.connect(profileId);
  }
}
