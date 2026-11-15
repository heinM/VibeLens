import { useCallback, useState } from "react";
import { Group, Panel, Separator, useDefaultLayout } from "react-resizable-panels";
import { useHistoryStore } from "../../store/useHistoryStore";
import { cn } from "../../lib/cn";
import { HistoryPanel } from "../history/HistoryPanel";
import { layoutStorage, panelStyle, readBool, writeBool } from "../layout/storage";
import { Explorer } from "./Explorer";
import { ProfileList } from "./ProfileList";
import { SectionHeader } from "./TreeRow";
import { WorkspaceFiles } from "./WorkspaceFiles";

/** A boolean remembered in localStorage (never throws). */
function usePersistedBool(key: string, fallback: boolean): [boolean, () => void] {
  const [value, setValue] = useState(() => readBool(key, fallback));
  const toggle = useCallback(() => {
    const next = !value;
    setValue(next);
    writeBool(key, next);
  }, [key, value]);
  return [value, toggle];
}

/** Workspace files and query history share the lower panel; either can fold away and the other takes the space. */
function LowerSections() {
  const [filesCollapsed, toggleFiles] = usePersistedBool("vg.sidebar.filesCollapsed", false);
  const [historyCollapsed, toggleHistory] = usePersistedBool("vg.sidebar.historyCollapsed", false);
  const historyCount = useHistoryStore((s) => s.entries.length);
  const grow = (collapsed: boolean) => (collapsed ? { flex: "0 0 auto" } : { flex: "1 1 0" });
  return (
    <div className="flex h-full min-h-0 flex-col bg-panel">
      <section aria-label="Workspace files" className="flex min-h-0 flex-col overflow-hidden" style={grow(filesCollapsed)}>
        <WorkspaceFiles collapsed={filesCollapsed} onToggleCollapsed={toggleFiles} />
      </section>
      <section aria-label="History" className={cn("flex min-h-0 flex-col overflow-hidden border-t border-border", historyCollapsed && "bg-panel")} style={grow(historyCollapsed)}>
        <SectionHeader title="History" count={historyCount} collapsed={historyCollapsed} onToggleCollapsed={toggleHistory} />
        {!historyCollapsed && (
          <div className="min-h-0 flex-1 overflow-hidden">
            <HistoryPanel />
          </div>
        )}
      </section>
    </div>
  );
}

/** Left column: connections, the object/key explorer for the focused connection, workspace files and history. */
export function Sidebar() {
  const layout = useDefaultLayout({ id: "vg-sidebar", panelIds: ["profiles", "explorer", "files"], storage: layoutStorage });
  return (
    <aside aria-label="Navigator" className="h-full min-w-0 bg-panel">
      <Group orientation="vertical" defaultLayout={layout.defaultLayout} onLayoutChanged={layout.onLayoutChanged} className="h-full">
        <Panel style={panelStyle} id="profiles" defaultSize="26%" minSize={96}>
          <ProfileList />
        </Panel>
        <Separator className="vg-sep" />
        <Panel style={panelStyle} id="explorer" defaultSize="38%" minSize={120}>
          <Explorer />
        </Panel>
        <Separator className="vg-sep" />
        <Panel style={panelStyle} id="files" defaultSize="36%" minSize={72}>
          <LowerSections />
        </Panel>
      </Group>
    </aside>
  );
}
