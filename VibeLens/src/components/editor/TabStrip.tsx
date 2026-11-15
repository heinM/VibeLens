import { useEffect, useRef, useState } from "react";
import { FileCode2, Network, Plus, SquareTerminal, Table2, X } from "lucide-react";
import { cn } from "../../lib/cn";
import { useWorkspaceStore, type WorkspaceTab } from "../../store/useWorkspaceStore";
import { useProfiles } from "../../hooks/useProfiles";
import { useTabActions } from "../../hooks/useTabActions";
import { ConfirmDialog, IconButton } from "../ui";
import { useMenu } from "../ui/Menu";
import { envBg } from "../ui/env";
import { MOD } from "../ui/platform";
import "./tabStrip.css";

/** What a tab shows as its name: tables read `schema.name` outside `public`; diagrams name their schema. */
export function tabLabel(t: WorkspaceTab): string {
  if (t.kind === "table" && t.objectRef) return t.objectRef.schema === "public" ? t.objectRef.name : `${t.objectRef.schema}.${t.objectRef.name}`;
  if (t.kind === "erd") return t.objectRef?.schema ? `${t.objectRef.schema} diagram` : t.title;
  return t.title;
}

const KIND_NOUN: Record<WorkspaceTab["kind"], string> = { sql: "Worksheet", valkey: "Valkey console", table: "Table", erd: "Diagram", icetable: "Iceberg table" };

const tabIcon = (kind: WorkspaceTab["kind"]) => (kind === "sql" ? FileCode2 : kind === "table" || kind === "icetable" ? Table2 : kind === "erd" ? Network : SquareTerminal);

export function TabStrip() {
  const tabs = useWorkspaceStore((s) => s.tabs);
  const activeTabId = useWorkspaceStore((s) => s.activeTabId);
  const setActive = useWorkspaceStore((s) => s.setActive);
  const closeTab = useWorkspaceStore((s) => s.closeTab);
  const { data: profiles = [] } = useProfiles();
  const { newSqlTab, newValkeyConsole } = useTabActions();
  const { menuNode, openFrom, openAt } = useMenu();
  const [closing, setClosing] = useState<WorkspaceTab | null>(null);
  const stripRef = useRef<HTMLDivElement>(null);

  // Keep the active tab in view when many are open.
  useEffect(() => {
    stripRef.current?.querySelector<HTMLElement>('[aria-selected="true"]')?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [activeTabId, tabs.length]);

  const requestClose = (t: WorkspaceTab) => (t.dirty ? setClosing(t) : closeTab(t.id));

  return (
    <div className="flex h-9 shrink-0 items-stretch border-b border-border bg-panel">
      <div ref={stripRef} role="tablist" aria-label="Open worksheets" className="flex min-w-0 flex-1 items-stretch overflow-x-auto vg-tabstrip-scroll">
        {tabs.map((t) => {
          const profile = profiles.find((p) => p.id === t.profileId);
          const isActive = t.id === activeTabId;
          const Icon = tabIcon(t.kind);
          const label = tabLabel(t);
          const where = [profile?.name, t.kind !== "valkey" ? t.database : null].filter(Boolean).join(" / ");
          return (
            <div
              key={t.id}
              role="tab"
              aria-selected={isActive}
              tabIndex={isActive ? 0 : -1}
              title={where ? `${KIND_NOUN[t.kind]}: ${label}\n${where}` : `${KIND_NOUN[t.kind]}: ${label}`}
              onClick={() => setActive(t.id)}
              onMouseDown={(e) => e.button === 1 && e.preventDefault()}
              onAuxClick={(e) => {
                if (e.button === 1) {
                  e.preventDefault();
                  requestClose(t);
                }
              }}
              onContextMenu={(e) =>
                openAt(e, [
                  { label: "Close", onSelect: () => requestClose(t) },
                  { label: "Close others", disabled: tabs.length < 2, onSelect: () => tabs.filter((o) => o.id !== t.id).forEach((o) => !o.dirty && closeTab(o.id)) },
                ])
              }
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") setActive(t.id);
              }}
              className={cn(
                "group relative flex max-w-56 min-w-28 shrink-0 cursor-default items-center gap-1.5 border-r border-border pl-3 pr-1.5 text-xs outline-none",
                isActive ? "bg-bg text-fg" : "text-muted hover:bg-panel-2 hover:text-fg",
              )}
            >
              <span aria-hidden className={cn("absolute inset-x-0 top-0 h-0.5", profile ? envBg[profile.environment] : "bg-transparent", !isActive && "opacity-50")} />
              <Icon className="size-3.5 shrink-0" />
              <span className="min-w-0 flex-1 truncate">{label}</span>
              <button
                type="button"
                aria-label={t.dirty ? `Close ${label} (unsaved changes)` : `Close ${label}`}
                onClick={(e) => {
                  e.stopPropagation();
                  requestClose(t);
                }}
                className="relative flex size-5 shrink-0 items-center justify-center rounded text-muted hover:bg-border hover:text-fg"
              >
                {t.dirty && <span className="absolute size-1.5 rounded-full bg-accent group-hover:hidden" />}
                <X className={cn("size-3.5", t.dirty ? "hidden group-hover:block" : isActive ? "block" : "opacity-0 group-hover:opacity-100")} />
              </button>
            </div>
          );
        })}
      </div>
      <div className="flex shrink-0 items-center px-1">
        <IconButton
          label="New tab"
          aria-haspopup="menu"
          onClick={(e) =>
            openFrom(e.currentTarget, [
              { label: "New SQL tab", icon: <FileCode2 className="size-3.5" />, hint: `${MOD} N`, onSelect: () => newSqlTab() },
              { label: "New Valkey console", icon: <SquareTerminal className="size-3.5" />, onSelect: () => newValkeyConsole() },
            ], "right")
          }
        >
          <Plus className="size-4" />
        </IconButton>
      </div>
      {menuNode}
      <ConfirmDialog
        open={closing !== null}
        title="Discard unsaved changes?"
        confirmText="Discard"
        danger
        onCancel={() => setClosing(null)}
        onConfirm={() => {
          if (closing) closeTab(closing.id);
          setClosing(null);
        }}
        body={<p>“{closing?.title}” has changes that have not been saved. Closing it will lose them.</p>}
      />
    </div>
  );
}
