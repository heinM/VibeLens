import { useId } from "react";
import type { AuthMethod, AwsTarget } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Input } from "../../ui";
import { FormField, Notice, SegmentedControl, Switch, invalidRing } from "../../ui/formKit";
import { AwsIdentityPicker } from "../AwsIdentityPicker";
import { defaultAwsTarget, regionFromArn, regionFromHost } from "../awsHelpers";
import { SecretField } from "../SecretField";
import { useSecretStatus } from "../useSecretStatus";
import type { SectionProps } from "./types";

type Kind = AuthMethod["kind"];

const PG_MODES: { value: Kind; label: string }[] = [
  { value: "none", label: "None" },
  { value: "password", label: "Password" },
  { value: "rdsIam", label: "RDS IAM" },
  { value: "secretsManager", label: "Secrets Manager" },
  { value: "accessToken", label: "Access token" },
];

/** Token commands people actually use; picked from a menu so nobody has to remember the flags. */
const TOKEN_COMMANDS: { label: string; command: string }[] = [
  { label: "Azure Database for PostgreSQL (Entra ID)", command: "az account get-access-token --resource-type oss-rdbms --query accessToken -o tsv" },
  { label: "OIDC via kubelogin (Keycloak / Entra)", command: "kubectl oidc-login get-token --oidc-issuer-url=https://idp.example.com/realms/bank --oidc-client-id=pgproxy | jq -r .status.token" },
  { label: "gcloud identity token", command: "gcloud auth print-identity-token" },
];
const VK_MODES: { value: Kind; label: string }[] = [
  { value: "none", label: "None" },
  { value: "password", label: "Password" },
  { value: "elastiCacheIam", label: "ElastiCache IAM" },
];

const awsOf = (a: AuthMethod): AwsTarget | null => ("aws" in a ? a.aws : null);
const userOf = (a: AuthMethod): string => ("username" in a ? a.username : "");

export function AuthSection({ profile, editing, original, patch, setPassword, draft, err }: SectionProps) {
  const a = profile.auth;
  const uid = useId();
  const setAuth = (auth: AuthMethod) => patch((p) => ({ ...p, auth }));

  const changeKind = (kind: Kind) => {
    if (kind === a.kind) return;
    const aws = awsOf(a) ?? defaultAwsTarget(regionFromHost(profile.host) ?? "");
    const user = userOf(a);
    switch (kind) {
      case "none":
        return setAuth({ kind: "none" });
      case "password":
        return setAuth({ kind: "password", username: userOf(a) || (profile.engine === "postgres" ? "postgres" : "") });
      case "rdsIam":
        return setAuth({ kind: "rdsIam", username: user, aws });
      case "secretsManager":
        return setAuth({ kind: "secretsManager", secretArn: "", aws });
      case "elastiCacheIam":
        return setAuth({ kind: "elastiCacheIam", username: user, cacheName: "", serverless: false, aws });
      case "accessToken":
        return setAuth({ kind: "accessToken", username: a.kind === "password" ? "" : user, command: null });
    }
  };

  const secrets = useSecretStatus(editing ? original?.id : null);
  const hasStoredPassword = editing && original?.auth.kind === a.kind && (a.kind === "password" || (a.kind === "accessToken" && !(original.auth.kind === "accessToken" && original.auth.command))) && (secrets.data?.hasPassword ?? false);
  const tokenNeedsTls = a.kind === "accessToken" && profile.tls.mode === "disable";
  const modes = profile.engine === "postgres" ? PG_MODES : VK_MODES;
  const iamNeedsTls = (a.kind === "rdsIam" || a.kind === "elastiCacheIam") && profile.tls.mode === "disable";

  return (
    <div className="space-y-4">
      <FormField label="Authentication">
        <SegmentedControl ariaLabel="Authentication method" value={a.kind} onChange={changeKind} options={modes} />
      </FormField>

      {a.kind === "none" && (
        <p className="text-[12px] text-muted">
          Connect without credentials. Fine for local development; most hosted databases need one of the other methods.
        </p>
      )}

      {a.kind === "password" && (
        <div className="space-y-3">
          <FormField
            label={profile.engine === "valkey" ? "ACL username" : "Username"}
            htmlFor={`${uid}-user`}
            error={err("auth.username")}
            optional={profile.engine === "valkey"}
            hint={profile.engine === "valkey" ? "Leave empty to use the default user." : undefined}
          >
            <Input
              id={`${uid}-user`}
              value={a.username}
              autoComplete="off"
              spellCheck={false}
              className={cn("max-w-72 font-mono", err("auth.username") && invalidRing)}
              onChange={(e) => setAuth({ kind: "password", username: e.target.value })}
            />
          </FormField>
          <SecretField label="Password" edit={draft.password} onChange={setPassword} hasStored={hasStoredPassword} optional />
        </div>
      )}

      {a.kind === "rdsIam" && (
        <div className="space-y-3">
          <p className="text-[12px] leading-snug text-muted">
            VibeLens signs a short-lived (15 minute) token for each new connection. No password is stored anywhere.
          </p>
          <FormField label="Database user" htmlFor={`${uid}-user`} error={err("auth.username")} hint="A user granted the rds_iam role.">
            <Input
              id={`${uid}-user`}
              value={a.username}
              autoComplete="off"
              spellCheck={false}
              className={cn("max-w-72 font-mono", err("auth.username") && invalidRing)}
              onChange={(e) => setAuth({ ...a, username: e.target.value })}
            />
          </FormField>
          <AwsIdentityPicker idPrefix={`${uid}-aws`} target={a.aws} err={err} onChange={(aws) => setAuth({ ...a, aws })} />
        </div>
      )}

      {a.kind === "secretsManager" && (
        <div className="space-y-3">
          <p className="text-[12px] leading-snug text-muted">
            Username and password are read from the secret every time VibeLens opens a connection and are kept in memory only.
          </p>
          <FormField label="Secret ARN" htmlFor={`${uid}-arn`} error={err("auth.secretArn")} hint="arn:aws:secretsmanager:<region>:<account>:secret:<name>">
            <Input
              id={`${uid}-arn`}
              value={a.secretArn}
              spellCheck={false}
              autoComplete="off"
              placeholder="arn:aws:secretsmanager:eu-west-1:123456789012:secret:prod/orders-db-AbCdEf"
              className={cn("font-mono", err("auth.secretArn") && invalidRing)}
              onChange={(e) => {
                const secretArn = e.target.value;
                const next = regionFromArn(secretArn);
                const prev = regionFromArn(a.secretArn);
                const region = next && (!a.aws.region.trim() || a.aws.region === prev) ? next : a.aws.region;
                setAuth({ ...a, secretArn, aws: { ...a.aws, region } });
              }}
            />
          </FormField>
          <AwsIdentityPicker idPrefix={`${uid}-aws`} target={a.aws} err={err} onChange={(aws) => setAuth({ ...a, aws })} regionHint="Prefilled from the ARN. The secret and the database may live in different regions." />
        </div>
      )}

      {a.kind === "elastiCacheIam" && (
        <div className="space-y-3">
          <p className="text-[12px] leading-snug text-muted">
            VibeLens signs an auth token for each new connection. ElastiCache closes IAM connections after 12 hours; VibeLens reconnects with a fresh token.
          </p>
          <div className="grid gap-3 sm:grid-cols-2">
            <FormField label="ElastiCache user" htmlFor={`${uid}-user`} error={err("auth.username")}>
              <Input
                id={`${uid}-user`}
                value={a.username}
                autoComplete="off"
                spellCheck={false}
                className={cn("font-mono", err("auth.username") && invalidRing)}
                onChange={(e) => setAuth({ ...a, username: e.target.value })}
              />
            </FormField>
            <FormField label="Cache name" htmlFor={`${uid}-cache`} error={err("auth.cacheName")} hint="Replication group or serverless cache name.">
              <Input
                id={`${uid}-cache`}
                value={a.cacheName}
                autoComplete="off"
                spellCheck={false}
                className={cn("font-mono", err("auth.cacheName") && invalidRing)}
                onChange={(e) => setAuth({ ...a, cacheName: e.target.value })}
              />
            </FormField>
          </div>
          <Switch checked={a.serverless} onChange={(serverless) => setAuth({ ...a, serverless })} label="ElastiCache Serverless" description="Turn on if this endpoint belongs to a serverless cache." />
          <AwsIdentityPicker idPrefix={`${uid}-aws`} target={a.aws} err={err} onChange={(aws) => setAuth({ ...a, aws })} />
        </div>
      )}

      {a.kind === "accessToken" && (
        <div className="space-y-3">
          <p className="text-[12px] leading-snug text-muted">
            A bearer token is sent as the password. Use this for a capitec/pg-proxy with OIDC (Keycloak, Entra ID) or Microsoft Entra authentication on Azure Database for PostgreSQL.
            A JWT&rsquo;s expiry is honoured: an expired pasted token is refused with a clear message, and command output is reused until shortly before it expires.
          </p>
          <FormField
            label="Database user"
            htmlFor={`${uid}-user`}
            optional
            hint="pg-proxy ignores it (identity comes from the token). Azure needs the Entra user or group name. Empty sends “vibelens”."
          >
            <Input
              id={`${uid}-user`}
              value={a.username}
              autoComplete="off"
              spellCheck={false}
              className="max-w-80 font-mono"
              onChange={(e) => setAuth({ ...a, username: e.target.value })}
            />
          </FormField>
          <FormField label="Token source">
            <SegmentedControl
              ariaLabel="Token source"
              value={a.command == null ? "paste" : "command"}
              onChange={(v) => setAuth({ ...a, command: v === "paste" ? null : (a.command ?? "") })}
              options={[
                { value: "paste", label: "Paste a token" },
                { value: "command", label: "Run a command" },
              ]}
            />
          </FormField>
          {a.command == null ? (
            <SecretField
              label="Access token"
              edit={draft.password}
              onChange={setPassword}
              hasStored={hasStoredPassword}
              storedLabel="•••• token stored in keychain"
              hint="Tokens usually expire within an hour; a command is less work if your identity provider has a CLI."
            />
          ) : (
            <div className="space-y-2">
              <FormField
                label="Token command"
                htmlFor={`${uid}-cmd`}
                error={err("auth.command")}
                hint="Runs in your login shell for each new database session and must print the token (or JSON with accessToken / access_token) on stdout."
              >
                <Input
                  id={`${uid}-cmd`}
                  value={a.command}
                  spellCheck={false}
                  autoComplete="off"
                  placeholder="az account get-access-token --resource-type oss-rdbms --query accessToken -o tsv"
                  className={cn("font-mono", err("auth.command") && invalidRing)}
                  onChange={(e) => setAuth({ ...a, command: e.target.value })}
                />
              </FormField>
              <div className="flex flex-wrap gap-1.5">
                {TOKEN_COMMANDS.map((t) => (
                  <button
                    key={t.label}
                    type="button"
                    className="rounded-md border border-border px-2 py-0.5 text-[11.5px] text-muted hover:bg-panel-2 hover:text-fg"
                    onClick={() => setAuth({ ...a, command: t.command })}
                  >
                    {t.label}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>
      )}

      {tokenNeedsTls && (
        <Notice tone="warn">The token travels as a cleartext password. Turn TLS on in the TLS section (pg-proxy: TLS_ENABLED) unless this only ever runs on localhost.</Notice>
      )}

      {iamNeedsTls && <Notice tone="warn">IAM authentication requires TLS, but TLS is disabled for this connection. Change it in the TLS section.</Notice>}
    </div>
  );
}
