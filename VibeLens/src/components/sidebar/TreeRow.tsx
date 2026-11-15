import type { MouseEvent, ReactNode } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { cn } from "../../lib/cn";

export const TREE_ROW_H = 24;
const INDENT = 14;

export interface TreeRowProps {
  depth: number;
  expandable?: boolean;
  expanded?: boolean;
  icon?: ReactNode;
  label: ReactNode;
  secondary?: ReactNode;
  right?: ReactNode;
  selected?: boolean;
  muted?: boolean;
  title?: string;
  onClick?: (e: MouseEvent) => void;
  onDoubleClick?: (e: MouseEvent) => void;
  onContextMenu?: (e: MouseEvent) => void;
  onToggle?: () => void;
}

/** One dense (24px) row of a navigator tree. */
export function TreeRow(p: TreeRowProps) {
  return (
    <div
      role="treeitem"
      aria-level={p.depth + 1}
      aria-expanded={p.expandable ? p.expanded : undefined}
      aria-selected={p.selected}
      title={p.title}
      onClick={p.onClick}
      onDoubleClick={p.onDoubleClick}
      onContextMenu={p.onContextMenu}
      className={cn(
        "group flex cursor-default items-center gap-1 pr-2 text-[13px] leading-6 whitespace-nowrap",
        p.selected ? "bg-accent/15 text-fg" : "text-fg/90 hover:bg-panel-2",
        p.muted && "text-muted",
      )}
      style={{ height: TREE_ROW_H, paddingLeft: 6 + p.depth * INDENT }}
    >
      <span
        className="flex size-4 shrink-0 items-center justify-center text-muted"
        onClick={
          p.expandable && p.onToggle
            ? (e) => {
                e.stopPropagation();
                p.onToggle?.();
              }
            : undefined
        }
      >
        {p.expandable ? p.expanded ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" /> : null}
      </span>
      {p.icon && <span className="flex size-4 shrink-0 items-center justify-center">{p.icon}</span>}
      <span className="min-w-0 truncate">{p.label}</span>
      {p.secondary && <span className="min-w-0 shrink truncate font-mono text-[11px] text-muted">{p.secondary}</span>}
      {p.right && <span className="ml-auto flex shrink-0 items-center gap-1 pl-2 text-[11px] text-muted">{p.right}</span>}
    </div>
  );
}

export function SectionHeader({
  title,
  count,
  actions,
  collapsed,
  onToggleCollapsed,
}: {
  title: string;
  count?: number;
  actions?: ReactNode;
  /** With `onToggleCollapsed`, the title becomes a disclosure button. */
  collapsed?: boolean;
  onToggleCollapsed?: () => void;
}) {
  const label = (
    <>
      <h2 className="text-xs font-semibold text-fg">{title}</h2>
      {count !== undefined && <span className="rounded bg-panel-2 px-1 text-[11px] tabular-nums text-muted">{count}</span>}
    </>
  );
  return (
    <div className={cn("flex h-8 shrink-0 items-center gap-1 bg-panel pl-1.5 pr-1.5", !collapsed && "border-b border-border", !onToggleCollapsed && "pl-3")}>
      {onToggleCollapsed ? (
        <button
          type="button"
          onClick={onToggleCollapsed}
          aria-expanded={!collapsed}
          title={collapsed ? `Show ${title}` : `Hide ${title}`}
          className="flex h-6 min-w-0 items-center gap-1 rounded pl-0.5 pr-1.5 outline-none hover:bg-panel-2 focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          {collapsed ? <ChevronRight className="size-3.5 shrink-0 text-muted" /> : <ChevronDown className="size-3.5 shrink-0 text-muted" />}
          {label}
        </button>
      ) : (
        label
      )}
      <div className="ml-auto flex items-center gap-0.5">{actions}</div>
    </div>
  );
}
