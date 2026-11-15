import { useEffect } from "react";
import { useUIStore } from "../../store/useUIStore";
import { useWorkspaceStore } from "../../store/useWorkspaceStore";
import { cn } from "../../lib/cn";
import { ValkeyConsole } from "../valkey/ValkeyConsole";
import { TableTab } from "../table/TableTab";
import { ErdTab } from "../erd/ErdTab";
import { IcebergTableTab } from "../iceberg/IcebergTableTab";
import { EmptyWorkspace } from "./EmptyWorkspace";
import { SqlTabPane } from "./SqlTabPane";
import { TabStrip } from "./TabStrip";

/**
 * Tab strip + one pane per open tab. Every pane stays mounted (hidden with `visibility`) so result
 * sets, scroll positions, editor undo history and console logs survive tab switches.
 */
export function EditorArea() {
  const tabs = useWorkspaceStore((s) => s.tabs);
  const activeTabId = useWorkspaceStore((s) => s.activeTabId);
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);
  const active = tabs.find((t) => t.id === activeTabId);

  // The navigator and status bar follow the focused tab's connection.
  useEffect(() => {
    if (active?.profileId) setActiveTarget(active.profileId, active.kind === "valkey" ? null : active.database);
  }, [active?.id, active?.profileId, active?.database, active?.kind, setActiveTarget]);

  if (tabs.length === 0) return <EmptyWorkspace />;

  return (
    <div className="flex h-full min-h-0 flex-col bg-bg">
      <TabStrip />
      <div className="relative min-h-0 flex-1">
        {tabs.map((t) => {
          const isActive = t.id === activeTabId;
          return (
            <div key={t.id} className={cn("absolute inset-0", isActive ? "z-10" : "pointer-events-none invisible z-0")} aria-hidden={!isActive}>
              {t.kind === "sql" ? (
                <SqlTabPane tab={t} active={isActive} />
              ) : t.kind === "valkey" ? (
                <ValkeyConsole tab={t} active={isActive} />
              ) : t.kind === "table" ? (
                <TableTab tab={t} active={isActive} />
              ) : t.kind === "icetable" ? (
                <IcebergTableTab tab={t} active={isActive} />
              ) : (
                <ErdTab tab={t} active={isActive} />
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
