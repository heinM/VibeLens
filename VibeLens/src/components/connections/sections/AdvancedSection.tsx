import { useId } from "react";
import { cn } from "../../../lib/cn";
import { Input } from "../../ui";
import type { PgProxyMode } from "../../../ipc/types";
import { FormField, Notice, NumberField, SegmentedControl, Switch, invalidRing } from "../../ui/formKit";

const PROXY_MODES: { value: PgProxyMode; label: string }[] = [
  { value: "blocking", label: "Blocking" },
  { value: "nonBlocking", label: "Non-blocking" },
  { value: "shunt", label: "Shunt" },
];
import type { SectionProps } from "./types";

export function AdvancedSection({ profile, patch, err }: SectionProps) {
  const uid = useId();
  const pg = profile.pg;
  const vk = profile.valkey;
  const setPg = (n: Partial<typeof pg>) => patch((p) => ({ ...p, pg: { ...p.pg, ...n } }));
  const setVk = (n: Partial<typeof vk>) => patch((p) => ({ ...p, valkey: { ...p.valkey, ...n } }));

  if (profile.engine === "postgres") {
    const px = pg.pgproxy ?? null;
    return (
      <div className="space-y-5">
      <div className="space-y-3 rounded-md border border-border p-3">
        <Switch
          checked={!!px}
          onChange={(on) => setPg({ pgproxy: on ? { mode: "blocking", healthUrl: null } : null })}
          label="Connects through PGProxy"
          description="capitec/pg-proxy in front of the database: AI query analysis, blocking, data masking, OIDC auth."
        />
        {px && (
          <div className="space-y-3 pl-1">
            <FormField label="Proxy mode" hint="Match the proxy's PROXY_MODE.">
              <SegmentedControl ariaLabel="PGProxy mode" value={px.mode} onChange={(mode) => setPg({ pgproxy: { ...px, mode } })} options={PROXY_MODES} />
            </FormField>
            {px.mode === "shunt" && (
              <p className="text-[12px] leading-snug text-muted">
                Shunt mode refuses BEGIN, COMMIT and ROLLBACK. VibeLens then enforces read-only runs (agent, read-only tabs) with the session&rsquo;s default_transaction_read_only instead of a transaction,
                refuses CALL and DO in them, and resets sessions without ROLLBACK. Scripts with their own transactions will be rejected by the proxy.
              </p>
            )}
            <FormField
              label="Health URL"
              htmlFor={`${uid}-hu`}
              optional
              error={err("pg.pgproxy.healthUrl")}
              hint="The proxy's HEALTH_CHECK_ADDR (port 8080 by default). Test connection then checks GET /ready, so a proxy whose database is down isn't mistaken for a bad login."
            >
              <Input
                id={`${uid}-hu`}
                value={px.healthUrl ?? ""}
                spellCheck={false}
                placeholder="http://pgproxy.internal:8080"
                className={cn("font-mono", err("pg.pgproxy.healthUrl") && invalidRing)}
                onChange={(e) => setPg({ pgproxy: { ...px, healthUrl: e.target.value } })}
              />
            </FormField>
            <p className="text-[12px] leading-snug text-muted">
              Queries the proxy blocks come back as policy errors with its reason and hint. For OIDC login choose <span className="font-medium">Access token</span> under Authentication.
            </p>
          </div>
        )}
      </div>
      <div className="grid gap-4 sm:grid-cols-2">
        <FormField
          label="Connect timeout"
          htmlFor={`${uid}-ct`}
          error={err("pg.connectTimeoutSecs")}
          hint="Raise to 60 or more for Aurora Serverless v2 that scales to zero: a cold start can take 15 seconds or more."
        >
          <NumberField id={`${uid}-ct`} value={pg.connectTimeoutSecs} min={1} max={600} suffix="s" invalid={!!err("pg.connectTimeoutSecs")} onChange={(v) => setPg({ connectTimeoutSecs: v ?? Number.NaN })} />
        </FormField>
        <FormField label="Statement timeout" htmlFor={`${uid}-st`} error={err("pg.statementTimeoutMs")} hint="Cancels queries that run longer. Empty means no limit.">
          <NumberField id={`${uid}-st`} allowEmpty value={pg.statementTimeoutMs} min={1} suffix="ms" placeholder="None" invalid={!!err("pg.statementTimeoutMs")} onChange={(v) => setPg({ statementTimeoutMs: v })} />
        </FormField>
        <FormField
          label="Release idle sessions"
          htmlFor={`${uid}-id`}
          error={err("pg.idleDisconnectSecs")}
          hint="Closes the pooled server sessions after this much idle time so serverless databases can pause. The connection stays open and the next query reconnects. 0 keeps them open."
        >
          <NumberField id={`${uid}-id`} value={pg.idleDisconnectSecs} min={0} suffix="s" invalid={!!err("pg.idleDisconnectSecs")} onChange={(v) => setPg({ idleDisconnectSecs: v ?? Number.NaN })} />
        </FormField>
        <FormField label="Pool size" htmlFor={`${uid}-ps`} error={err("pg.maxPoolSize")} hint="Maximum simultaneous connections.">
          <NumberField id={`${uid}-ps`} value={pg.maxPoolSize} min={1} max={64} invalid={!!err("pg.maxPoolSize")} onChange={(v) => setPg({ maxPoolSize: v ?? Number.NaN })} />
        </FormField>
        <FormField label="Application name" htmlFor={`${uid}-an`} error={err("pg.applicationName")} hint="Shown in pg_stat_activity, handy for DBAs.">
          <Input id={`${uid}-an`} value={pg.applicationName} spellCheck={false} className={cn(err("pg.applicationName") && invalidRing)} onChange={(e) => setPg({ applicationName: e.target.value })} />
        </FormField>
      </div>
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <Switch checked={vk.cluster} onChange={(cluster) => setVk({ cluster })} label="Cluster mode" description="Connect to a Valkey/Redis Cluster. Only database 0 is available." />
      {vk.cluster && profile.tunnel && (
        <Notice tone="warn">Cluster mode over an SSH tunnel isn&rsquo;t supported in V1. Use a route that reaches the nodes directly, such as a VPN, or connect to a non-cluster replica.</Notice>
      )}
      <div className="grid gap-4 sm:grid-cols-2">
        <FormField label="Key delimiter" htmlFor={`${uid}-kd`} error={err("valkey.keyDelimiter")} hint="Groups keys into folders in the key browser.">
          <Input id={`${uid}-kd`} value={vk.keyDelimiter} spellCheck={false} className={cn("max-w-32 font-mono", err("valkey.keyDelimiter") && invalidRing)} onChange={(e) => setVk({ keyDelimiter: e.target.value })} />
        </FormField>
        <FormField label="SCAN count" htmlFor={`${uid}-sc`} error={err("valkey.scanCount")} hint="Keys requested per SCAN call.">
          <NumberField id={`${uid}-sc`} value={vk.scanCount} min={1} max={100000} invalid={!!err("valkey.scanCount")} onChange={(v) => setVk({ scanCount: v ?? Number.NaN })} />
        </FormField>
        <FormField label="Connect timeout" htmlFor={`${uid}-ct`} error={err("valkey.connectTimeoutSecs")}>
          <NumberField id={`${uid}-ct`} value={vk.connectTimeoutSecs} min={1} max={600} suffix="s" invalid={!!err("valkey.connectTimeoutSecs")} onChange={(v) => setVk({ connectTimeoutSecs: v ?? Number.NaN })} />
        </FormField>
        <FormField label="Idle disconnect" htmlFor={`${uid}-id`} error={err("valkey.idleDisconnectSecs")} hint="Closes the connection and tunnel after this much idle time.">
          <NumberField id={`${uid}-id`} value={vk.idleDisconnectSecs} min={0} suffix="s" invalid={!!err("valkey.idleDisconnectSecs")} onChange={(v) => setVk({ idleDisconnectSecs: v ?? Number.NaN })} />
        </FormField>
      </div>
    </div>
  );
}
