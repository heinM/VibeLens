import { useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Ellipsis, FlaskConical, Import, Plus } from "lucide-react";
import { useUIStore } from "../../store/useUIStore";
import { useWorkspaceStore } from "../../store/useWorkspaceStore";
import { useConnect, useConnectionStatuses, useDisconnect } from "../../hooks/useConnectionStatus";
import { PROFILES_KEY, useProfiles } from "../../hooks/useProfiles";
import { useTabActions } from "../../hooks/useTabActions";
import { api, asError } from "../../ipc/commands";
import type { ConnectionProfile } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { Button, EnvBadge, ErrorBox, IconButton } from "../ui";
import { useMenu, type MenuEntry } from "../ui/Menu";
import { SkeletonRows, StatusDot } from "../ui/wsKit";
import { EngineIcon } from "../ui/EngineIcon";
import { ENGINE_NAME } from "../../lib/engines";
import { redshiftSummary } from "../connections/redshiftForm";
import { demoProfiles } from "./demoProfiles";
import { SectionHeader } from "./TreeRow";

/** An empty worksheet bound to an Iceberg connection (the Postgres-only `newSqlTab` would drop the binding). */
function openIcebergBlankTab(profileId: string) {
  const tabs = useWorkspaceStore.getState().tabs;
  let n = 1;
  while (tabs.some((t) => t.title === `query-${n}.sql`)) n++;
  useWorkspaceStore.getState().openTab({ kind: "sql", title: `query-${n}.sql`, profileId, database: null, contents: "" });
}

function profileTitle(p: ConnectionProfile): string {
  if (p.engine === "redshift") return `${p.name} (${redshiftSummary(p.redshift).replace(/^Redshift/, ENGINE_NAME.redshift)})`;
  if (p.engine !== "iceberg" || !p.iceberg) return `${p.name} (${ENGINE_NAME[p.engine]}, ${p.host}:${p.port})`;
  const c = p.iceberg.catalog;
  const where = c.kind === "rest" || c.kind === "sql" ? c.uri : c.kind === "hms" ? c.address : c.kind === "s3Tables" ? c.tableBucketArn : `Glue ${c.aws.region}`;
  return `${p.name} (Apache Iceberg, ${where})`;
}

export function ProfileList() {
  const { data: profiles, isLoading, error } = useProfiles();
  const { data: statuses } = useConnectionStatuses();
  const activeProfileId = useUIStore((s) => s.activeProfileId);
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);
  const openWizard = useUIStore((s) => s.openWizard);
  const connect = useConnect();
  const disconnect = useDisconnect();
  const { newSqlTab, newValkeyConsole } = useTabActions();
  const { menuNode, openAt, openFrom } = useMenu();
  const qc = useQueryClient();
  const [demoBusy, setDemoBusy] = useState(false);
  const [demoError, setDemoError] = useState<ReturnType<typeof asError> | null>(null);

  /** Creates the two Docker demo connections (see scripts/dev-mac.sh) and refreshes the list. */
  const addDemoConnections = async () => {
    setDemoBusy(true);
    setDemoError(null);
    try {
      for (const d of demoProfiles()) await api.saveProfile(d.profile, d.password);
    } catch (e) {
      setDemoError(asError(e));
    } finally {
      setDemoBusy(false);
      await qc.invalidateQueries({ queryKey: PROFILES_KEY });
    }
  };

  const stateOf = (id: string) => statuses?.find((s) => s.profileId === id)?.state ?? "disconnected";

  const menuFor = (p: ConnectionProfile): MenuEntry[] => {
    const st = stateOf(p.id);
    return [
      st === "connected" || st === "connecting"
        ? { label: "Disconnect", onSelect: () => disconnect.mutate(p.id) }
        : { label: "Connect", onSelect: () => connect.mutate(p.id) },
      "separator",
      p.engine === "postgres"
        ? { label: "New SQL tab", onSelect: () => newSqlTab({ profileId: p.id, database: null }) }
        : p.engine === "iceberg"
          ? { label: "New SQL tab", onSelect: () => openIcebergBlankTab(p.id) }
          : p.engine === "redshift"
            ? { label: "New SQL tab", onSelect: () => newSqlTab({ profileId: p.id, database: null }) }
            : { label: "New Valkey console", onSelect: () => newValkeyConsole({ profileId: p.id }) },
      "separator",
      { label: "Edit connection…", onSelect: () => openWizard(p.id) },
    ];
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-panel">
      <SectionHeader
        title="Connections"
        count={profiles?.length}
        actions={
          <>
            <IconButton label="Import connections" onClick={() => useUIStore.getState().setImportOpen(true)}>
              <Import className="size-4" />
            </IconButton>
            <IconButton label="New connection" onClick={() => openWizard()}>
              <Plus className="size-4" />
            </IconButton>
          </>
        }
      />
      <div role="listbox" aria-label="Connections" className="min-h-0 flex-1 overflow-y-auto py-1">
        {isLoading && <SkeletonRows rows={3} />}
        {error && (
          <div className="p-2">
            <ErrorBox error={asError(error)} />
          </div>
        )}
        {profiles && profiles.length === 0 && (
          <div className="flex flex-col items-start gap-2 px-3 py-3">
            <Button variant="primary" size="sm" onClick={() => openWizard()}>
              <Plus className="size-3.5" /> New connection
            </Button>
            <Button size="sm" onClick={() => useUIStore.getState().setImportOpen(true)}>
              <Import className="size-3.5" /> Import from DataGrip, DBeaver, …
            </Button>
            <div className="mt-1 w-full border-t border-border pt-3">
              <Button size="sm" loading={demoBusy} onClick={() => void addDemoConnections()}>
                <FlaskConical className="size-3.5" /> Add local demo connections
              </Button>
              <p className="mt-2 text-[11px] leading-relaxed text-muted">
                Postgres on port 55432 and Valkey on 56379, matching the Docker databases <span className="font-mono">scripts/dev-mac.sh</span> starts.
              </p>
              {demoError && (
                <div className="mt-2">
                  <ErrorBox error={demoError} />
                </div>
              )}
            </div>
          </div>
        )}
        {profiles?.map((p) => {
          const selected = p.id === activeProfileId;
          const st = stateOf(p.id);
          return (
            <div
              key={p.id}
              role="option"
              aria-selected={selected}
              tabIndex={0}
              title={profileTitle(p)}
              onClick={() => setActiveTarget(p.id, null)}
              onDoubleClick={() => st === "disconnected" || st === "error" ? connect.mutate(p.id) : undefined}
              onContextMenu={(e) => openAt(e, menuFor(p))}
              onKeyDown={(e) => {
                if (e.key === "Enter") setActiveTarget(p.id, null);
                if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) openFrom(e.currentTarget, menuFor(p));
              }}
              className={cn(
                "group relative flex h-8 cursor-default items-center gap-2 pl-3 pr-1 outline-none",
                selected ? "bg-accent/15" : "hover:bg-panel-2",
              )}
            >
              {selected && <span aria-hidden className="absolute inset-y-1 left-0 w-0.5 rounded-r bg-accent" />}
              <EngineIcon engine={p.engine} />
              <span className="min-w-0 flex-1 truncate text-[13px]">{p.name || "(unnamed)"}</span>
              <EnvBadge env={p.environment} />
              <StatusDot state={st} />
              <button
                type="button"
                aria-label={`Actions for ${p.name}`}
                aria-haspopup="menu"
                onClick={(e) => {
                  e.stopPropagation();
                  openFrom(e.currentTarget, menuFor(p), "right");
                }}
                className="flex size-6 shrink-0 items-center justify-center rounded text-muted opacity-0 hover:bg-border hover:text-fg focus-visible:opacity-100 group-hover:opacity-100"
              >
                <Ellipsis className="size-4" />
              </button>
            </div>
          );
        })}
      </div>
      {menuNode}
    </div>
  );
}
