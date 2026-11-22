import { Database, Server, Warehouse, Zap } from "lucide-react";
import type { Environment } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Input } from "../../ui";
import { IcebergGlyph } from "../../iceberg/IcebergGlyph";
import { FormField, Notice, NumberField, RadioCards, SegmentedControl, invalidRing } from "../../ui/formKit";
import { usesHostPort } from "../../../lib/defaults";
import { switchEngine } from "../draft";
import type { SectionProps } from "./types";

const ENV_DOT: Record<Environment, string> = {
  dev: "bg-env-dev",
  test: "bg-env-test",
  staging: "bg-env-staging",
  prod: "bg-env-prod",
};

const SWATCHES = ["#5eead4", "#60a5fa", "#a78bfa", "#f472b6", "#fb923c", "#facc15", "#4ade80", "#94a3b8"];

export function GeneralSection({ profile, editing, patch, patchIceberg, err }: SectionProps) {
  const ice = profile.iceberg;
  return (
    <div className="space-y-5">
      <FormField label="Engine" hint={editing ? "The engine can't be changed after a connection is created." : undefined}>
        <RadioCards
          ariaLabel="Engine"
          columns={2}
          value={profile.engine}
          onChange={(engine) => patch((p) => switchEngine(p, engine))}
          options={[
            { value: "postgres", title: "PostgreSQL", description: "Aurora, RDS and self-hosted.", icon: <Database className="size-4" />, disabled: editing && profile.engine !== "postgres" },
            { value: "valkey", title: "Valkey / Redis", description: "ElastiCache, MemoryDB and self-hosted.", icon: <Zap className="size-4" />, disabled: editing && profile.engine !== "valkey" },
            { value: "iceberg", title: "Apache Iceberg", description: "Lakehouse tables on S3, Glue, S3 Tables or a REST catalog.", icon: <IcebergGlyph />, disabled: editing && profile.engine !== "iceberg" },
            { value: "mysql", title: "MySQL", description: "MySQL 8, Aurora MySQL, RDS MySQL and self-hosted.", icon: <Server className="size-4" />, disabled: editing && profile.engine !== "mysql" },
            { value: "redshift", title: "Amazon Redshift", description: "Provisioned or Serverless, signed in with IAM through the Data API.", icon: <Warehouse className="size-4" />, disabled: editing && profile.engine !== "redshift" },
          ]}
        />
      </FormField>

      <FormField label="Name" htmlFor="wz-name" error={err("name")} hint="Shown in the sidebar and in agent context.">
        <Input
          id="wz-name"
          data-autofocus
          value={profile.name}
          autoComplete="off"
          placeholder={{ postgres: "orders-db (prod)", mysql: "shop-db (prod)", iceberg: "lakehouse (prod)", redshift: "warehouse (prod)", valkey: "session-cache" }[profile.engine]}
          className={err("name") ? invalidRing : undefined}
          onChange={(e) => patch((p) => ({ ...p, name: e.target.value }))}
        />
      </FormField>

      {usesHostPort(profile.engine) && (
      <div className="grid grid-cols-[1fr_110px] gap-3">
        <FormField label="Host" htmlFor="wz-host" error={err("host")}>
          <Input
            id="wz-host"
            value={profile.host}
            spellCheck={false}
            autoComplete="off"
            placeholder={profile.engine === "valkey" ? "my-cache.xxxxxx.use1.cache.amazonaws.com" : "mydb.cluster-xyz.eu-west-1.rds.amazonaws.com"}
            className={cn("font-mono", err("host") && invalidRing)}
            onChange={(e) => patch((p) => ({ ...p, host: e.target.value }))}
          />
        </FormField>
        <FormField label="Port" htmlFor="wz-port" error={err("port")}>
          <NumberField id="wz-port" value={profile.port} min={1} max={65535} invalid={!!err("port")} onChange={(v) => patch((p) => ({ ...p, port: v ?? Number.NaN }))} />
        </FormField>
      </div>
      )}

      {profile.engine === "iceberg" && ice && (
        <FormField label="Default namespace" htmlFor="wz-ns" optional error={err("ice.defaultNamespace")} hint="Unqualified table names in SQL resolve here. Use dots for nested namespaces, such as lake.sales.">
          <Input
            id="wz-ns"
            value={ice.defaultNamespace ?? ""}
            spellCheck={false}
            autoComplete="off"
            placeholder="shop"
            className={cn("max-w-72 font-mono", err("ice.defaultNamespace") && invalidRing)}
            onChange={(e) => patchIceberg((o) => ({ ...o, defaultNamespace: e.target.value || null }))}
          />
        </FormField>
      )}

      {(profile.engine === "postgres" || profile.engine === "mysql") && (
        <FormField
          label={profile.engine === "mysql" ? "Default database" : "Database"}
          htmlFor="wz-db"
          error={err("database")}
          hint={
            profile.engine === "mysql"
              ? "Required. Unqualified names in SQL resolve here, and the agent works here unless told otherwise. You can browse the others from the sidebar."
              : "The database opened first. You can browse the others from the sidebar."
          }
        >
          <Input
            id="wz-db"
            value={profile.database ?? ""}
            spellCheck={false}
            autoComplete="off"
            className={cn("max-w-72 font-mono", err("database") && invalidRing)}
            onChange={(e) => patch((p) => ({ ...p, database: e.target.value }))}
          />
        </FormField>
      )}

      <FormField label="Environment" hint="Production connections ask for typed confirmation before any write.">
        <SegmentedControl
          ariaLabel="Environment"
          value={profile.environment}
          onChange={(environment) => patch((p) => ({ ...p, environment }))}
          options={(["dev", "test", "staging", "prod"] as Environment[]).map((e) => ({
            value: e,
            label: (
              <>
                <span className={cn("size-2 rounded-full", ENV_DOT[e])} />
                {e}
              </>
            ),
          }))}
        />
      </FormField>
      {profile.environment === "prod" && (
        <Notice tone="warn">
          {profile.engine === "iceberg"
            ? "Production: expiring snapshots and writes ask for the connection name. Give the catalog credentials read-only access for anything the AI agent touches."
            : profile.engine === "redshift"
              ? "Production: writes ask for the connection name. Each statement commits on its own, so there is no transaction to roll back. Give the IAM identity a read-only database role for anything the AI agent touches."
              : "Production: use a least-privilege database role for anything the AI agent touches. The agent\u2019s read-only check is a safety net, not a security boundary."}
        </Notice>
      )}

      <FormField label="Colour" optional hint="Tints this connection in the sidebar and tabs. Defaults to the environment colour.">
        <div className="flex flex-wrap items-center gap-1.5" role="radiogroup" aria-label="Connection colour">
          <button
            type="button"
            role="radio"
            aria-checked={profile.color == null}
            onClick={() => patch((p) => ({ ...p, color: null }))}
            className={cn("h-6 rounded-md border px-2 text-[11px] outline-none focus-visible:ring-2 focus-visible:ring-accent/60", profile.color == null ? "border-accent/70 text-fg" : "border-border text-muted hover:text-fg")}
          >
            Auto
          </button>
          {SWATCHES.map((c) => (
            <button
              key={c}
              type="button"
              role="radio"
              aria-checked={profile.color === c}
              aria-label={c}
              onClick={() => patch((p) => ({ ...p, color: c }))}
              className={cn("size-6 rounded-md border outline-none focus-visible:ring-2 focus-visible:ring-accent/60", profile.color === c ? "border-fg ring-1 ring-fg/60" : "border-transparent")}
              style={{ background: c }}
            />
          ))}
        </div>
      </FormField>
    </div>
  );
}
