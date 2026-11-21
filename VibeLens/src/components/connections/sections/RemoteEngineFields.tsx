// Settings of the Trino and Spark (Livy) query engines inside the Iceberg "Query engine" section.
import { useId, useState } from "react";
import { Plus, X } from "lucide-react";
import type { HttpEngineAuth } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Button, Input } from "../../ui";
import { FormField, Notice, NumberField, SegmentedControl, invalidRing } from "../../ui/formKit";
import { SecretField } from "../SecretField";
import { useSecretStatus } from "../useSecretStatus";
import {
  DEFAULT_SESSION_START_TIMEOUT_SECS,
  blankHttpAuth,
  cleartextProblem,
  confFromRows,
  confRowError,
  confRows,
  engineSecretStored,
  validateRemoteEngine,
  type ConfRow,
  type RemoteEngine,
} from "../remoteEngineForm";
import { Group } from "./IcebergShared";
import type { SectionProps } from "./types";

const mono = (invalid: boolean) => cn("font-mono", invalid && invalidRing);

export function RemoteEngineFields({ engine: q, setEngine, draft, setEngineSecret, original, editing, profile, err }: SectionProps & { engine: RemoteEngine; setEngine: (q: RemoteEngine) => void }) {
  const uid = useId();
  const secrets = useSecretStatus(editing ? original?.id : null);
  const hasStored = engineSecretStored(original, profile, secrets.data?.hasEngineSecret);
  const live = validateRemoteEngine(q);
  // Required-field messages wait for a save or test; a URL that is wrong (or would leak a secret) shows at once.
  const urlErr = err("ice.engine.url") ?? (q.url.trim() ? live["ice.engine.url"] : undefined);
  const clear = cleartextProblem(q.url, q.auth);
  const trino = q.kind === "trino";
  const setAuth = (auth: HttpEngineAuth) => setEngine({ ...q, auth });

  return (
    <>
      <Group
        title={trino ? "Trino coordinator" : "Livy server"}
        description={
          trino
            ? "SQL runs on Trino through its client REST protocol. Tables are still browsed in the Iceberg catalog directly; name the Trino catalog that points at it."
            : "SQL runs in a Spark session that VibeLens opens on Apache Livy (also EMR on EKS's interactive endpoint) on the first query, reuses, and closes with the connection."
        }
      >
        <FormField
          label={trino ? "Coordinator URL" : "Livy URL"}
          htmlFor={`${uid}-url`}
          error={urlErr}
          hint="https:// with a certificate the OS trusts, or the CA file below. No user name, password, query or fragment in the URL."
        >
          <Input id={`${uid}-url`} value={q.url} spellCheck={false} autoComplete="off" placeholder={trino ? "https://trino.example.com:8443" : "https://livy.example.com:8998"} className={mono(!!urlErr)} onChange={(e) => setEngine({ ...q, url: e.target.value })} />
        </FormField>
        {clear?.level === "warn" && !urlErr && <Notice tone="warn">{clear.message}</Notice>}
        <div className={cn("grid gap-3", trino && "sm:grid-cols-2")}>
          <FormField
            label="Catalog"
            htmlFor={`${uid}-cat`}
            error={err("ice.engine.catalogName")}
            hint={trino ? "The Trino catalog for this Iceberg catalog, sent as X-Trino-Catalog. The default namespace is sent as the schema." : "The Spark catalog configured for this Iceberg catalog. The session starts with USE catalog.namespace."}
          >
            <Input id={`${uid}-cat`} value={q.catalog} spellCheck={false} autoComplete="off" placeholder={trino ? "iceberg" : "lake"} className={mono(!!err("ice.engine.catalogName"))} onChange={(e) => setEngine({ ...q, catalog: e.target.value })} />
          </FormField>
          {q.kind === "trino" && (
            <FormField label="User" htmlFor={`${uid}-user`} error={err("ice.engine.user")} hint="Sent as X-Trino-User.">
              <Input id={`${uid}-user`} value={q.user} spellCheck={false} autoComplete="off" placeholder="analyst" className={mono(!!err("ice.engine.user"))} onChange={(e) => setEngine({ ...q, user: e.target.value })} />
            </FormField>
          )}
        </div>
      </Group>

      <Group title="Sign-in">
        <SegmentedControl<HttpEngineAuth["kind"]>
          ariaLabel={`${trino ? "Trino" : "Livy"} authentication`}
          value={q.auth.kind}
          onChange={(k) => setAuth(blankHttpAuth(k, q.auth))}
          options={[
            { value: "none", label: "None" },
            { value: "basic", label: "User and password" },
            { value: "bearer", label: trino ? "JWT bearer token" : "Bearer token" },
          ]}
        />
        {q.auth.kind === "none" && <p className="text-[12px] leading-snug text-muted">{trino ? "The coordinator trusts the X-Trino-User header." : "The Livy server accepts anonymous requests."} Fine inside a private network; anything shared should ask for a credential.</p>}
        {q.auth.kind === "basic" && (
          <div className="grid gap-3 sm:grid-cols-2">
            <FormField label="User name" htmlFor={`${uid}-bu`} error={err("ice.engine.username")}>
              <Input id={`${uid}-bu`} value={q.auth.username} spellCheck={false} autoComplete="off" className={mono(!!err("ice.engine.username"))} onChange={(e) => setAuth({ kind: "basic", username: e.target.value })} />
            </FormField>
            <SecretField label="Password" edit={draft.engineSecret} onChange={setEngineSecret} hasStored={hasStored} hint="Sent with HTTP basic authentication over TLS. Saved to the OS keychain, never shown again." />
          </div>
        )}
        {q.auth.kind === "bearer" && (
          <SecretField label={trino ? "JWT" : "Bearer token"} edit={draft.engineSecret} onChange={setEngineSecret} hasStored={hasStored} hint="Sent as Authorization: Bearer over TLS. Saved to the OS keychain, never shown again." />
        )}
        {err("ice.engine.secret") && (
          <span role="alert" className="block text-[11px] text-danger">
            {err("ice.engine.secret")}
          </span>
        )}
        <FormField label="CA certificate" htmlFor={`${uid}-ca`} optional error={err("ice.engine.caCertPath")} hint="A PEM file to trust in addition to the OS roots, for a private CA. Certificates are always verified.">
          <Input id={`${uid}-ca`} value={q.caCertPath ?? ""} spellCheck={false} autoComplete="off" placeholder="/etc/ssl/private-ca.pem" className={mono(!!err("ice.engine.caCertPath"))} onChange={(e) => setEngine({ ...q, caCertPath: e.target.value || null })} />
        </FormField>
      </Group>

      {q.kind === "spark" && (
        <Group title="Spark session" description="Used when VibeLens creates its session. It never joins a session it did not create.">
          <div className="grid gap-3 sm:grid-cols-[1fr_minmax(0,11rem)]">
            <FormField label="Proxy user" htmlFor={`${uid}-proxy`} optional error={err("ice.engine.proxyUser")} hint="Livy's proxyUser: the user the session runs as, when the server allows impersonation.">
              <Input id={`${uid}-proxy`} value={q.proxyUser ?? ""} spellCheck={false} autoComplete="off" className={mono(!!err("ice.engine.proxyUser"))} onChange={(e) => setEngine({ ...q, proxyUser: e.target.value || null })} />
            </FormField>
            <FormField label="Start timeout" htmlFor={`${uid}-timeout`} optional error={err("ice.engine.timeout")} hint="Wait for a new session to be ready.">
              <NumberField
                id={`${uid}-timeout`}
                value={q.sessionStartTimeoutSecs}
                allowEmpty
                min={1}
                placeholder={String(DEFAULT_SESSION_START_TIMEOUT_SECS)}
                suffix="sec"
                invalid={!!err("ice.engine.timeout")}
                onChange={(v) => setEngine({ ...q, sessionStartTimeoutSecs: v })}
              />
            </FormField>
          </div>
          {/* Each row shows its own problem while typing (confRowError), so the field carries only the hint. */}
          <FormField label="Session configuration" optional hint="Spark settings for the session, such as spark.sql.catalog.lake. Stored in plain text: no secrets, and nothing that loads code (spark.jars, extraJavaOptions, …).">
            <ConfEditor key={`${uid}-conf`} initial={q.sessionConf} onChange={(sessionConf) => setEngine({ ...q, sessionConf })} />
          </FormField>
        </Group>
      )}
    </>
  );
}

/** Key/value rows for the Spark session configuration. Rows are local so blanks and duplicates survive typing. */
function ConfEditor({ initial, onChange }: { initial: Record<string, string>; onChange: (c: Record<string, string>) => void }) {
  const [rows, setRowsState] = useState<ConfRow[]>(() => confRows(initial));
  const setRows = (next: ConfRow[]) => {
    setRowsState(next);
    // Duplicates collapse in the saved map; keep the first so the row error and the saved value agree.
    const firstWins = next.filter((r, i) => next.findIndex((x) => x.key.trim() === r.key.trim()) === i);
    onChange(confFromRows(firstWins));
  };
  const set = (i: number, patch: Partial<ConfRow>) => setRows(rows.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div className="space-y-1.5">
      {rows.length === 0 && <p className="text-[12px] text-muted">None. The cluster's own defaults apply.</p>}
      {rows.map((r, i) => {
        const rowErr = confRowError(rows, i);
        return (
          <div key={i} className="space-y-0.5">
            <div className="grid grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)_auto] items-center gap-1.5">
              <Input aria-label={`Setting ${i + 1} name`} value={r.key} spellCheck={false} autoComplete="off" placeholder="spark.sql.catalog.lake" className={cn("font-mono", rowErr && "!border-danger/70")} onChange={(e) => set(i, { key: e.target.value })} />
              <Input aria-label={`Setting ${i + 1} value`} value={r.value} spellCheck={false} autoComplete="off" placeholder="org.apache.iceberg.spark.SparkCatalog" className={cn("font-mono", rowErr && "!border-danger/70")} onChange={(e) => set(i, { value: e.target.value })} />
              <button type="button" aria-label={`Remove setting ${i + 1}`} onClick={() => setRows(rows.filter((_, j) => j !== i))} className="grid size-7 place-items-center rounded-md text-muted hover:bg-panel-2 hover:text-fg">
                <X className="size-3.5" />
              </button>
            </div>
            {rowErr && (
              <span role="alert" className="block text-[11px] leading-snug text-danger">
                {rowErr}
              </span>
            )}
          </div>
        );
      })}
      <Button size="sm" variant="ghost" onClick={() => setRows([...rows, { key: "", value: "" }])}>
        <Plus className="size-3.5" /> Add setting
      </Button>
    </div>
  );
}
