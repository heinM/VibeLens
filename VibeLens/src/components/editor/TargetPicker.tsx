import { FolderTree, Plug } from "lucide-react";
import type { ConnectionProfile } from "../../ipc/types";
import { useUIStore } from "../../store/useUIStore";
import { useWorkspaceStore, type WorkspaceTab } from "../../store/useWorkspaceStore";
import { useConnect, useConnectionStatus } from "../../hooks/useConnectionStatus";
import { useProfiles } from "../../hooks/useProfiles";
import { usePgDatabases } from "../../hooks/usePg";
import { useRsDatabases } from "../../hooks/useRedshift";
import { EnvBadge, Button } from "../ui";
import { Chip, MiniSelect, StatusDot } from "../ui/wsKit";
import { engineBadge } from "../grid/statsFormat";
import { DIALECT_ENGINE, dialectOf } from "../../lib/sqlIntel/dialect";
import { isSqlEngine } from "../../lib/engines";
import { envBg } from "../ui/env";
import { cn } from "../../lib/cn";

/**
 * Per-tab connection + database picker. Postgres, Iceberg and Redshift profiles are offered for SQL tabs
 * (Valkey has its own console). An Iceberg connection has no databases: the picker shows its default
 * namespace and the query engine instead. Redshift lists its databases through the Data API.
 */
export function TargetPicker({ tab }: { tab: WorkspaceTab }) {
  const updateTab = useWorkspaceStore((s) => s.updateTab);
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);
  const openWizard = useUIStore((s) => s.openWizard);
  const { data: profiles = [] } = useProfiles();
  const sqlProfiles = profiles.filter((p) => isSqlEngine(p.engine));
  const profile = sqlProfiles.find((p) => p.id === tab.profileId);
  const isIce = profile?.engine === "iceberg";
  const isRs = profile?.engine === "redshift";
  const status = useConnectionStatus(tab.profileId);
  const connect = useConnect();
  // Each engine is asked for its own databases; an Iceberg connection has none.
  const dbs = usePgDatabases(profile?.engine === "postgres" ? tab.profileId : null);
  const rsDbs = useRsDatabases(isRs ? tab.profileId : null);

  const currentDb = tab.database ?? (isRs ? profile?.redshift?.database : profile?.database) ?? "";
  const dbOptions = isRs ? [...(rsDbs.data ?? [])] : (dbs.data ?? []).filter((d) => d.allowConnections && !d.isTemplate).map((d) => d.name);
  if (currentDb && !dbOptions.includes(currentDb)) dbOptions.unshift(currentDb);

  const state = status?.state ?? "disconnected";

  return (
    <div className="flex min-w-0 items-center gap-1.5">
      <MiniSelect
        label="Connection for this tab"
        value={tab.profileId ?? ""}
        className="w-44 min-w-28 shrink"
        leading={profile ? <span className={cn("size-2 rounded-full", envBg[profile.environment])} /> : undefined}
        onChange={(id) => {
          if (id === "__new") return openWizard();
          updateTab(tab.id, { profileId: id || null, database: null });
          if (id) setActiveTarget(id, null);
        }}
      >
        <option value="">Choose connection…</option>
        {sqlProfiles.map((p) => (
          <option key={p.id} value={p.id}>
            {p.name}
          </option>
        ))}
        <option value="__new">New connection…</option>
      </MiniSelect>
      {profile && (
        <>
          {isIce ? (
            <IcebergTarget profile={profile} />
          ) : (
            <MiniSelect
              label="Database for this tab"
              value={currentDb}
              className="w-36 min-w-24 shrink"
              disabled={state !== "connected" && dbOptions.length <= 1}
              onChange={(db) => {
                updateTab(tab.id, { database: db || null });
                setActiveTarget(profile.id, db || null);
              }}
            >
              {dbOptions.length === 0 && <option value="">(default)</option>}
              {dbOptions.map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </MiniSelect>
          )}
          <EnvBadge env={profile.environment} />
          {state === "connected" ? (
            <StatusDot state="connected" />
          ) : (
            <Button size="sm" variant="secondary" loading={state === "connecting" || connect.isPending} onClick={() => connect.mutate(profile.id)} aria-label={`Connect to ${profile.name}`}>
              <Plug className="size-3.5" /> Connect
            </Button>
          )}
        </>
      )}
    </div>
  );
}

/** Default namespace and query engine of an Iceberg connection (read-only: set in the connection's settings). */
function IcebergTarget({ profile }: { profile: ConnectionProfile }) {
  const ns = profile.iceberg?.defaultNamespace ?? null;
  const dialect = dialectOf(profile);
  const engine = engineBadge(dialect ? DIALECT_ENGINE[dialect] : "datafusion");
  return (
    <div className="flex shrink-0 items-center gap-1.5">
      <span
        className="inline-flex h-7 max-w-40 shrink-0 items-center gap-1.5 rounded-md border border-border bg-panel-2 px-2 text-xs text-fg"
        title={ns ? `Default namespace: unqualified table names resolve in ${ns}. Change it in the connection's settings.` : "No default namespace: qualify tables as namespace.table. Set one in the connection's settings."}
        aria-label={ns ? `Default namespace ${ns}` : "No default namespace"}
      >
        <FolderTree className="size-3.5 shrink-0 text-muted" aria-hidden />
        <span className={ns ? "truncate font-mono" : "truncate text-muted"}>{ns ?? "no namespace"}</span>
      </span>
      <span className="hidden @[1280px]:inline-flex">
        <Chip tone={engine.tone} title={`${engine.title} Query engine of this connection.`}>
          {engine.label}
        </Chip>
      </span>
    </div>
  );
}
