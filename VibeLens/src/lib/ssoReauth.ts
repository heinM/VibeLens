// Deciding when an error is an expired IAM Identity Center session, and which session the
// connection uses, so the navigator can offer "Sign in again" next to the error.
import type { AwsProfileSummary, SsoSessionInfo } from "../ipc/types";

/**
 * True for the backend's own expired-session error (`CoreError::Auth` with the text from
 * aws/sso.rs `SESSION_EXPIRED`). The same words inside another kind of error, such as text a
 * database server sent back, do not count: a server must not be able to raise a sign-in prompt.
 */
export function isSsoExpired(error: { code?: string; message: string } | string | null | undefined): boolean {
  if (!error) return false;
  if (typeof error === "string") return /^(\[auth\]\s*)?authentication failed: sso session expired/i.test(error.trim());
  if (error.code !== undefined && error.code !== "auth") return false;
  return /^(authentication failed: )?sso session expired/i.test(error.message.trim());
}

export interface SsoLogin {
  startUrl: string;
  region: string;
}

type Found = { kind: "sso"; startUrl: string; ssoRegion: string } | { kind: "profile"; profileName: string };

/** Every AWS identity anywhere in a connection profile (auth, catalog, storage, query engine). */
function identities(node: unknown, out: Found[], depth = 0): Found[] {
  if (depth > 8 || node === null || typeof node !== "object") return out;
  if (Array.isArray(node)) {
    for (const v of node) identities(v, out, depth + 1);
    return out;
  }
  const o = node as Record<string, unknown>;
  if (o.kind === "sso" && typeof o.startUrl === "string" && typeof o.ssoRegion === "string") {
    out.push({ kind: "sso", startUrl: o.startUrl, ssoRegion: o.ssoRegion });
  } else if (o.kind === "profile" && typeof o.profileName === "string") {
    out.push({ kind: "profile", profileName: o.profileName });
  }
  for (const v of Object.values(o)) identities(v, out, depth + 1);
  return out;
}

/**
 * The Identity Center sessions a connection signs in with. A named AWS profile resolves through
 * `~/.aws/config`; its SSO region is taken from the session this app already holds for that start
 * URL, so a profile the app never signed in to yields nothing (the connection form handles that).
 */
export function ssoLoginsFor(profile: unknown, awsProfiles: AwsProfileSummary[] | undefined, sessions: SsoSessionInfo[] | undefined): SsoLogin[] {
  const logins: SsoLogin[] = [];
  const add = (startUrl: string, region: string) => {
    const url = startUrl.trim();
    const r = region.trim();
    if (url && r && !logins.some((l) => l.startUrl === url)) logins.push({ startUrl: url, region: r });
  };
  for (const id of identities(profile, [])) {
    if (id.kind === "sso") {
      add(id.startUrl, id.ssoRegion);
      continue;
    }
    const url = awsProfiles?.find((p) => p.name === id.profileName && p.kind === "sso")?.ssoStartUrl;
    const session = url ? sessions?.find((s) => s.startUrl === url) : undefined;
    if (url && session) add(url, session.region);
  }
  return logins;
}
