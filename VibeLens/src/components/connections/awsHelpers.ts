import type { AwsIdentity, AwsProfileSummary, AwsTarget } from "../../ipc/types";

export const COMMON_REGIONS = [
  "af-south-1",
  "ap-east-1",
  "ap-northeast-1",
  "ap-northeast-2",
  "ap-south-1",
  "ap-southeast-1",
  "ap-southeast-2",
  "ca-central-1",
  "eu-central-1",
  "eu-north-1",
  "eu-south-1",
  "eu-west-1",
  "eu-west-2",
  "eu-west-3",
  "me-south-1",
  "sa-east-1",
  "us-east-1",
  "us-east-2",
  "us-west-1",
  "us-west-2",
];

export const SECRET_ARN_RE = /^arn:aws[a-z-]*:secretsmanager:[a-z0-9-]+:\d{12}:secret:.+/;
export const REGION_RE = /^[a-z]{2}(-[a-z]+)+-\d$/;

export function regionFromArn(arn: string): string | null {
  const m = /^arn:aws[a-z-]*:[a-z0-9-]+:([a-z0-9-]+):/.exec(arn.trim());
  return m && REGION_RE.test(m[1]) ? m[1] : null;
}

/** `mydb.cluster-xyz.eu-west-1.rds.amazonaws.com` -> `eu-west-1`. */
export function regionFromHost(host: string): string | null {
  const m = /\.([a-z]{2}(?:-[a-z]+)+-\d)\.(?:rds|cache)\.amazonaws\.com(?:\.cn)?$/i.exec(host.trim());
  return m ? m[1].toLowerCase() : null;
}

export function isAwsHost(host: string): boolean {
  return /\.(rds|cache)\.amazonaws\.com(\.cn)?$/i.test(host.trim());
}

const ALIAS_RE = /^(d-[0-9a-f]{10}|[a-z0-9][a-z0-9-]{1,62})$/i;

/** Accepts a full start URL or a bare directory id / alias and returns the canonical start URL. */
export function normalizeStartUrl(input: string): string {
  const v = input.trim();
  if (ALIAS_RE.test(v)) return `https://${v.toLowerCase()}.awsapps.com/start`;
  return v;
}

export function startUrlError(input: string): string | null {
  const v = normalizeStartUrl(input);
  if (!v) return "Enter your IAM Identity Center start URL.";
  try {
    const u = new URL(v);
    if (u.protocol !== "https:" || !u.hostname.includes(".")) throw new Error("scheme");
  } catch {
    return "Use https://d-xxxxxxxxxx.awsapps.com/start or your organisation alias.";
  }
  return null;
}

export function awsTargetError(t: AwsTarget): Record<string, string> {
  const e: Record<string, string> = {};
  if (!t.region.trim()) e["aws.region"] = "Region is required.";
  else if (!REGION_RE.test(t.region.trim())) e["aws.region"] = "Region looks wrong, for example eu-west-1.";
  const id = t.identity;
  if (id.kind === "profile" && !id.profileName) e["aws.profile"] = "Choose an AWS profile.";
  if (id.kind === "sso") {
    const err = startUrlError(id.startUrl);
    if (err) e["aws.startUrl"] = err;
    if (!id.ssoRegion.trim()) e["aws.ssoRegion"] = "SSO region is required.";
    if (!err && !id.accountId) e["aws.account"] = "Sign in, then choose an account.";
    if (!err && id.accountId && !id.roleName) e["aws.role"] = "Choose a role.";
  }
  return e;
}

export const defaultAwsTarget = (region = ""): AwsTarget => ({ identity: { kind: "profile", profileName: "" }, region });

export function profileKindLabel(k: AwsProfileSummary["kind"]): string {
  switch (k) {
    case "sso":
      return "SSO";
    case "static":
      return "Access keys";
    case "assumeRole":
      return "Assume role";
    case "process":
      return "Credential process";
    default:
      return "Other";
  }
}

export function describeProfile(p: AwsProfileSummary): string {
  const parts = [profileKindLabel(p.kind)];
  if (p.kind === "sso" && p.ssoAccountId) parts.push(`account ${p.ssoAccountId}`);
  if (p.kind === "sso" && p.ssoRoleName) parts.push(p.ssoRoleName);
  if (p.region) parts.push(p.region);
  return parts.join(" · ");
}

export function emptyIdentity(kind: AwsIdentity["kind"], region = ""): AwsIdentity {
  switch (kind) {
    case "profile":
      return { kind: "profile", profileName: "" };
    case "sso":
      return { kind: "sso", startUrl: "", ssoRegion: region, accountId: "", roleName: "" };
    default:
      return { kind: "defaultChain" };
  }
}
