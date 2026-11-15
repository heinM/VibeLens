// Metadata explorer for one Iceberg table or view (tab kind "icetable"; namespace levels are joined with "." in
// objectRef.schema). Sections: Overview, Schema, Partitioning & sort, Snapshots, Data preview, Maintenance.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Camera, Columns3, Copy, Eye, FileCode2, Layers, LayoutList, Loader2, Plug, RefreshCw, Table2, Table as TableIcon, Wrench } from "lucide-react";
import type { WorkspaceTab } from "../../store/useWorkspaceStore";
import { useConnect, useConnectionStatus } from "../../hooks/useConnectionStatus";
import { useProfile } from "../../hooks/useProfiles";
import { asError } from "../../ipc/commands";
import type { IcebergTableInfo } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { Button, EmptyState, EnvBadge, ErrorBox, IconButton } from "../ui";
import { Chip } from "../ui/wsKit";
import { copyToClipboard } from "../ui/clipboard";
import { showToast } from "../grid/toast";
import { openIcebergSqlTab } from "../sidebar/IcebergTree";
import { iceQueries } from "./iceQueries";
import { qualifiedName, selectAllSql } from "./iceSql";
import { countFields } from "./iceTableModel";
import { joinNamespace, namespaceLevels } from "./openIcebergTab";
import { FormatBadge } from "./icePrimitives";
import { IcebergGlyph } from "./IcebergGlyph";
import { LayoutPane } from "./LayoutPane";
import { MaintenancePane } from "./MaintenancePane";
import { OverviewPane } from "./OverviewPane";
import { PreviewPane } from "./PreviewPane";
import { SchemaPane } from "./SchemaPane";
import { SnapshotsPane } from "./SnapshotsPane";
import { useIcePreview } from "./useIcePreview";

export type PaneId = "overview" | "schema" | "layout" | "snapshots" | "preview" | "maintenance";

export function IcebergTableTab({ tab }: { tab: WorkspaceTab; active: boolean }) {
  const ref = tab.objectRef;
  if (!ref || !tab.profileId) {
    return <EmptyState title="Nothing to show">This tab is not linked to an Iceberg table. Open one from the navigator.</EmptyState>;
  }
  return <TableExplorer profileId={tab.profileId} namespace={namespaceLevels(ref.schema)} name={ref.name} />;
}

function TableExplorer({ profileId, namespace, name }: { profileId: string; namespace: string[]; name: string }) {
  const qc = useQueryClient();
  const profile = useProfile(profileId);
  const status = useConnectionStatus(profileId);
  const connect = useConnect();
  const connected = status?.state === "connected";
  const [pane, setPane] = useState<PaneId>("overview");
  const [visited, setVisited] = useState<ReadonlySet<PaneId>>(() => new Set<PaneId>(["overview"]));

  const q = useQuery({ ...iceQueries.describe(profileId, namespace, name), enabled: connected });
  const info = q.data;
  const sql = useMemo(() => selectAllSql(namespace, name, 100), [namespace, name]);
  const preview = useIcePreview(profileId, sql);

  const go = useCallback((p: PaneId) => {
    setPane(p);
    setVisited((v) => (v.has(p) ? v : new Set(v).add(p)));
  }, []);

  // The preview reads the *current* snapshot: run it on first visit, and again when the table moves on.
  const snapshotKey = info?.currentSnapshotId ?? "none";
  const ranFor = useRef<string | null>(null);
  useEffect(() => {
    if (!info || !visited.has("preview")) return;
    const key = `${profileId}|${snapshotKey}`;
    if (ranFor.current === key) return;
    ranFor.current = key;
    void preview.run();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [info, visited, snapshotKey, profileId]);

  const refresh = async () => {
    const key = ["ice", profileId, "describe", joinNamespace(namespace), name] as const;
    const before = snapshotKey;
    await qc.invalidateQueries({ queryKey: key });
    const after = qc.getQueryData<IcebergTableInfo>(key)?.currentSnapshotId ?? "none";
    // When the table moved on, the effect above re-runs the preview; otherwise re-run it here.
    if (visited.has("preview") && after === before) void preview.run();
  };

  const openSql = (title: string, contents: string) => openIcebergSqlTab(profileId, title, contents);

  if (!profile) return <EmptyState title="Connection not found">The connection for this table was deleted. Close the tab.</EmptyState>;

  if (!connected) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
        <IcebergGlyph className="size-6 text-muted" />
        <div className="text-sm font-medium text-fg">{profile.name} is not connected</div>
        {connect.error && <ErrorBox error={asError(connect.error)} />}
        <Button variant="primary" loading={connect.isPending || status?.state === "connecting"} onClick={() => connect.mutate(profileId)}>
          <Plug className="size-4" /> Connect
        </Button>
      </div>
    );
  }

  const view = info?.kind === "view";
  const nav: { id: PaneId; label: string; icon: typeof Table2; count?: number | string; disabled?: boolean }[] = [
    { id: "overview", label: "Overview", icon: LayoutList },
    { id: "schema", label: "Schema", icon: Columns3, count: info ? countFields(info.fields) : undefined },
    { id: "layout", label: "Partitioning & sort", icon: Layers, count: info ? (info.partitionSpec.length || undefined) : undefined, disabled: view },
    { id: "snapshots", label: "Snapshots", icon: Camera, count: info && !view ? info.snapshots.length : undefined, disabled: view },
    { id: "preview", label: "Data preview", icon: TableIcon },
    { id: "maintenance", label: "Maintenance", icon: Wrench, disabled: view },
  ];
  const activePane = nav.find((n) => n.id === pane)?.disabled ? "overview" : pane;

  let body: React.ReactNode;
  if (q.isPending) {
    body = (
      <div className="flex h-full items-center justify-center gap-2 text-xs text-muted">
        <Loader2 className="size-4 animate-spin" /> Reading table metadata…
      </div>
    );
  } else if (q.isError || !info) {
    body = (
      <div className="mx-auto max-w-xl space-y-3 p-6">
        <ErrorBox error={asError(q.error)} />
        <Button onClick={() => void q.refetch()}>
          <RefreshCw className="size-3.5" /> Try again
        </Button>
      </div>
    );
  } else if (activePane === "preview") {
    body = <PreviewPane info={info} state={preview.state} onRun={() => void preview.run()} onOpenSql={() => openSql(name, sql)} />;
  } else {
    body = (
      <div className="mx-auto w-full max-w-5xl p-5 @[720px]:px-7 @[720px]:py-6">
        {activePane === "overview" && <OverviewPane info={info} onGo={go} />}
        {activePane === "schema" && <SchemaPane info={info} />}
        {activePane === "layout" && <LayoutPane info={info} />}
        {activePane === "snapshots" && <SnapshotsPane info={info} onQueryAsOf={(id, text) => openSql(`${name}@${id.slice(-6)}`, text)} />}
        {activePane === "maintenance" && <MaintenancePane profile={profile} info={info} onExpired={() => undefined} />}
      </div>
    );
  }

  const qualified = qualifiedName(namespace, name);
  return (
    <div className="@container flex h-full min-h-0 flex-col bg-bg">
      <div className="flex h-10 shrink-0 items-center gap-2 border-b border-border bg-panel px-3">
        {view ? <Eye className="size-4 shrink-0 text-emerald-500" /> : <Table2 className="size-4 shrink-0 text-sky-500" />}
        <div className="flex min-w-0 items-baseline gap-0.5 font-mono text-[13px]">
          {namespace.length > 0 && <span className="shrink-0 text-muted">{namespace.join(".")}.</span>}
          <span className="truncate font-semibold text-fg">{name}</span>
        </div>
        {info && <FormatBadge version={info.formatVersion} />}
        <Chip>{view ? "view" : "table"}</Chip>
        {profile.environment !== "dev" && <EnvBadge env={profile.environment} />}
        <div className="ml-auto flex shrink-0 items-center gap-1">
          <IconButton label="Copy qualified name" onClick={() => void copyToClipboard(qualified).then((ok) => ok && showToast({ title: "Copied name", detail: qualified, tone: "ok", durationMs: 2000 }))}>
            <Copy className="size-3.5" />
          </IconButton>
          <Button size="sm" onClick={() => openSql(name, sql)}>
            <FileCode2 className="size-3.5" /> Query
          </Button>
          <IconButton label="Refresh metadata" onClick={() => void refresh()} disabled={q.isFetching}>
            <RefreshCw className={cn("size-3.5", q.isFetching && "animate-spin")} />
          </IconButton>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col @[720px]:flex-row">
        <nav aria-label="Table sections" className="flex shrink-0 gap-0.5 overflow-x-auto border-b border-border bg-panel/60 p-1.5 @[720px]:w-52 @[720px]:flex-col @[720px]:overflow-visible @[720px]:border-b-0 @[720px]:border-r @[720px]:p-2">
          {nav.map(({ id, label, icon: Icon, count, disabled }) => {
            const on = activePane === id;
            return (
              <button
                key={id}
                type="button"
                disabled={disabled}
                aria-current={on ? "page" : undefined}
                title={disabled ? (view ? "Views don't have this" : undefined) : undefined}
                onClick={() => go(id)}
                className={cn(
                  "flex h-8 shrink-0 items-center gap-2 whitespace-nowrap rounded-md px-2 text-left text-[13px] outline-none focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-40",
                  on ? "bg-panel-2 text-fg" : "text-muted hover:bg-panel-2/60 hover:text-fg",
                )}
              >
                <Icon className={cn("size-3.5 shrink-0", on && "text-accent")} />
                <span className="min-w-0 flex-1 truncate">{label}</span>
                {count !== undefined && <span className="rounded bg-bg px-1 text-[11px] tabular-nums text-muted">{count}</span>}
              </button>
            );
          })}
        </nav>
        <div className={cn("min-h-0 min-w-0 flex-1", activePane === "preview" && info ? "overflow-hidden" : "overflow-auto")} role="region" aria-label={nav.find((n) => n.id === activePane)?.label}>
          {body}
        </div>
      </div>
    </div>
  );
}
