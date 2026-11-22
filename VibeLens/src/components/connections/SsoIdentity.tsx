import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { CheckCircle2, ExternalLink, LogIn } from "lucide-react";
import { api, asError } from "../../ipc/commands";
import type { AwsIdentity, DeviceLoginStart, ErrorPayload } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { Button, ErrorBox, Input, Select, Spinner } from "../ui";
import { CopyIconButton, FormField, invalidRing } from "../ui/formKit";
import { openExternal } from "../ui/vgExternal";
import { COMMON_REGIONS, normalizeStartUrl, startUrlError } from "./awsHelpers";

type SsoIdentityValue = Extract<AwsIdentity, { kind: "sso" }>;

type Login =
  | { phase: "idle" }
  | { phase: "starting" }
  | { phase: "waiting"; start: DeviceLoginStart }
  | { phase: "failed"; error: ErrorPayload };

function useCountdown(expiresAt: string | null): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!expiresAt) return;
    setNow(Date.now());
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [expiresAt]);
  if (!expiresAt) return null;
  const end = Date.parse(expiresAt);
  if (Number.isNaN(end)) return null;
  return Math.max(0, Math.round((end - now) / 1000));
}

const mmss = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;

export function WaitingForBrowser({ start, onCancel }: { start: DeviceLoginStart; onCancel: () => void }) {
  const remaining = useCountdown(start.expiresAt);
  const link = start.verificationUriComplete ?? start.verificationUri;
  return (
    <div className="rounded-lg border border-accent/40 bg-accent/[0.05] p-4" aria-live="polite">
      <p className="text-[12px] text-muted">Confirm this code in your browser:</p>
      <div className="mt-2 flex items-center gap-2">
        <code className="selectable rounded-md border border-border bg-bg px-4 py-2 font-mono text-[28px] font-semibold leading-none tracking-[0.28em] text-fg">
          {start.userCode}
        </code>
        <CopyIconButton text={start.userCode} label="Copy code" />
      </div>
      <p className="mt-3 text-[12px] text-fg/90">
        We opened your browser. If nothing appeared,{" "}
        <button type="button" className="inline-flex items-center gap-1 text-accent underline-offset-2 hover:underline" onClick={() => void openExternal(link)}>
          open the verification page <ExternalLink className="size-3" />
        </button>
        .
      </p>
      <div className="mt-3 flex items-center gap-2 text-[12px] text-muted">
        <Spinner className="size-3.5" />
        <span>Waiting for you to approve the request</span>
        {remaining != null && (
          <span className={cn("ml-1 font-mono tabular-nums", remaining < 30 && "text-warn")}>{remaining > 0 ? `expires in ${mmss(remaining)}` : "code expired"}</span>
        )}
        <Button size="sm" variant="ghost" className="ml-auto" onClick={onCancel}>
          Cancel sign-in
        </Button>
      </div>
    </div>
  );
}

export function SsoIdentity({
  value,
  onChange,
  err,
  idPrefix,
}: {
  value: SsoIdentityValue;
  onChange: (v: SsoIdentityValue) => void;
  err: (field: string) => string | undefined;
  idPrefix: string;
}) {
  const qc = useQueryClient();
  const [login, setLogin] = useState<Login>({ phase: "idle" });
  const [signedInUrl, setSignedInUrl] = useState<string | null>(null);
  const attempt = useRef(0);
  useEffect(
    () => () => {
      attempt.current += 1; // ignore late results after unmount
    },
    [],
  );

  const startUrl = normalizeStartUrl(value.startUrl);
  const urlProblem = startUrlError(value.startUrl);

  const sessions = useQuery({ queryKey: ["aws-sso-sessions"], queryFn: () => api.awsSsoSessions(), refetchOnMount: "always" });
  const session = sessions.data?.find((s) => s.startUrl === startUrl);
  const expired = !!session?.expiresAt && Date.parse(session.expiresAt) < Date.now();
  const signedIn = !urlProblem && ((!!session && !expired) || signedInUrl === startUrl);

  const accounts = useQuery({
    queryKey: ["aws-sso-accounts", startUrl, signedInUrl],
    queryFn: () => api.awsSsoListAccounts(startUrl),
    enabled: signedIn,
  });
  const roles = useQuery({
    queryKey: ["aws-sso-roles", startUrl, value.accountId],
    queryFn: () => api.awsSsoListRoles(startUrl, value.accountId),
    enabled: signedIn && !!value.accountId,
  });

  // One role: just pick it.
  useEffect(() => {
    const list = roles.data;
    if (list && list.length === 1 && !value.roleName) onChange({ ...value, roleName: list[0].roleName });
  }, [roles.data, value, onChange]);

  const signIn = useCallback(async () => {
    if (urlProblem || !value.ssoRegion.trim()) return;
    const mine = ++attempt.current;
    setLogin({ phase: "starting" });
    try {
      const start = await api.awsSsoStart(startUrl, value.ssoRegion.trim());
      if (mine !== attempt.current) return;
      setLogin({ phase: "waiting", start });
      await api.awsSsoComplete(start.loginId);
      if (mine !== attempt.current) return;
      setSignedInUrl(startUrl);
      setLogin({ phase: "idle" });
      onChange({ ...value, startUrl });
      void qc.invalidateQueries({ queryKey: ["aws-sso-sessions"] });
      void qc.invalidateQueries({ queryKey: ["aws-sso-accounts"] });
    } catch (e) {
      if (mine !== attempt.current) return;
      setLogin({ phase: "failed", error: asError(e) });
    }
  }, [urlProblem, value, startUrl, onChange, qc]);

  const cancel = () => {
    attempt.current += 1;
    setLogin({ phase: "idle" });
  };

  const accountOptions = accounts.data ?? [];
  const hasSavedAccount = value.accountId && !accountOptions.some((a) => a.accountId === value.accountId);
  const roleOptions = roles.data ?? [];
  const hasSavedRole = value.roleName && !roleOptions.some((r) => r.roleName === value.roleName);
  const busy = login.phase === "starting" || login.phase === "waiting";
  const urlId = `${idPrefix}-sso-url`;

  return (
    <div className="space-y-3">
      {sessions.data && sessions.data.length > 0 && (
        <div>
          <div className="mb-1 text-[11px] text-muted">Signed-in sessions on this machine</div>
          <div className="flex flex-wrap gap-1.5">
            {sessions.data.map((s) => {
              const dead = !!s.expiresAt && Date.parse(s.expiresAt) < Date.now();
              const on = s.startUrl === startUrl;
              return (
                <button
                  key={s.startUrl}
                  type="button"
                  onClick={() => onChange({ ...value, startUrl: s.startUrl, ssoRegion: s.region, accountId: on ? value.accountId : "", roleName: on ? value.roleName : "" })}
                  className={cn(
                    "inline-flex h-7 max-w-full items-center gap-1.5 rounded-md border px-2 text-[12px] outline-none focus-visible:ring-2 focus-visible:ring-accent/60",
                    on ? "border-accent/70 bg-accent/10 text-fg" : "border-border bg-bg text-muted hover:text-fg",
                  )}
                  title={s.startUrl}
                >
                  <CheckCircle2 className={cn("size-3.5 shrink-0", dead ? "text-warn" : "text-ok")} />
                  <span className="truncate font-mono">{s.startUrl.replace(/^https:\/\//, "").replace(/\/start\/?$/, "")}</span>
                  {dead && <span className="text-warn">expired</span>}
                </button>
              );
            })}
          </div>
        </div>
      )}

      <div className="grid gap-3 sm:grid-cols-[1fr_180px]">
        <FormField
          label="Start URL"
          htmlFor={urlId}
          error={err("aws.startUrl") ?? (value.startUrl && urlProblem ? urlProblem : undefined)}
          hint="https://d-xxxxxxxxxx.awsapps.com/start, or just your organisation alias."
        >
          <Input
            id={urlId}
            value={value.startUrl}
            spellCheck={false}
            placeholder="https://d-xxxxxxxxxx.awsapps.com/start"
            className={cn("font-mono", err("aws.startUrl") && invalidRing)}
            onChange={(e) => onChange({ ...value, startUrl: e.target.value, accountId: "", roleName: "" })}
          />
        </FormField>
        <FormField label="SSO region" htmlFor={`${idPrefix}-sso-region`} error={err("aws.ssoRegion")} hint="Where Identity Center is enabled.">
          <Input
            id={`${idPrefix}-sso-region`}
            list={`${idPrefix}-regions`}
            value={value.ssoRegion}
            spellCheck={false}
            placeholder="eu-west-1"
            className={cn("font-mono", err("aws.ssoRegion") && invalidRing)}
            onChange={(e) => onChange({ ...value, ssoRegion: e.target.value })}
          />
          <datalist id={`${idPrefix}-regions`}>
            {COMMON_REGIONS.map((r) => (
              <option key={r} value={r} />
            ))}
          </datalist>
        </FormField>
      </div>

      {login.phase === "waiting" ? (
        <WaitingForBrowser start={login.start} onCancel={cancel} />
      ) : (
        <div className="flex flex-wrap items-center gap-2">
          <Button onClick={() => void signIn()} disabled={!!urlProblem || !value.ssoRegion.trim() || busy} loading={login.phase === "starting"}>
            <LogIn className="size-3.5" />
            {signedIn ? "Sign in again" : "Sign in with IAM Identity Center"}
          </Button>
          {signedIn && !accounts.isError && (
            <span className="inline-flex items-center gap-1 text-[12px] text-ok">
              <CheckCircle2 className="size-3.5" /> Signed in
            </span>
          )}
          {expired && signedInUrl !== startUrl && <span className="text-[12px] text-warn">Your session expired. Sign in again to list accounts.</span>}
        </div>
      )}
      {login.phase === "failed" && <ErrorBox error={login.error} />}

      {signedIn && (
        <div className="grid gap-3 sm:grid-cols-2">
          <FormField label="Account" htmlFor={`${idPrefix}-sso-account`} error={err("aws.account")}>
            {accounts.isPending ? (
              <div className="flex h-8 items-center gap-2 text-[12px] text-muted">
                <Spinner className="size-3.5" /> Loading accounts
              </div>
            ) : accounts.isError ? (
              <ErrorBox error={asError(accounts.error)} />
            ) : (
              <Select
                id={`${idPrefix}-sso-account`}
                value={value.accountId}
                className={err("aws.account") ? invalidRing : undefined}
                onChange={(e) => onChange({ ...value, accountId: e.target.value, roleName: "" })}
              >
                <option value="">Choose an account</option>
                {hasSavedAccount && <option value={value.accountId}>{value.accountId}</option>}
                {accountOptions.map((a) => (
                  <option key={a.accountId} value={a.accountId}>
                    {a.accountName ? `${a.accountName} (${a.accountId})` : a.accountId}
                  </option>
                ))}
              </Select>
            )}
          </FormField>
          <FormField label="Role" htmlFor={`${idPrefix}-sso-role`} error={err("aws.role")}>
            {roles.isFetching && value.accountId ? (
              <div className="flex h-8 items-center gap-2 text-[12px] text-muted">
                <Spinner className="size-3.5" /> Loading roles
              </div>
            ) : roles.isError ? (
              <ErrorBox error={asError(roles.error)} />
            ) : (
              <Select
                id={`${idPrefix}-sso-role`}
                value={value.roleName}
                disabled={!value.accountId}
                className={err("aws.role") ? invalidRing : undefined}
                onChange={(e) => onChange({ ...value, roleName: e.target.value })}
              >
                <option value="">{value.accountId ? "Choose a role" : "Choose an account first"}</option>
                {hasSavedRole && <option value={value.roleName}>{value.roleName}</option>}
                {roleOptions.map((r) => (
                  <option key={r.roleName} value={r.roleName}>
                    {r.roleName}
                  </option>
                ))}
              </Select>
            )}
          </FormField>
        </div>
      )}
      {!signedIn && (value.accountId || value.roleName) && (
        <p className="text-[12px] text-muted">
          Saved identity: account <span className="font-mono">{value.accountId}</span>, role <span className="font-mono">{value.roleName}</span>. Sign in to change it.
        </p>
      )}
    </div>
  );
}
