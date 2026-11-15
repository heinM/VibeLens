// Additional primitives for the workspace UI (the shared index.tsx stays untouched).
import type { ReactNode } from "react";
import { cn } from "../../lib/cn";
import type { ConnectionState } from "../../ipc/types";

export function Kbd({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <kbd className={cn("inline-flex h-4 min-w-4 items-center justify-center rounded border border-border bg-panel-2 px-1 font-mono text-[10px] leading-none text-muted", className)}>
      {children}
    </kbd>
  );
}

export function Skeleton({ className }: { className?: string }) {
  return <div className={cn("animate-pulse rounded bg-panel-2", className)} />;
}

export function SkeletonRows({ rows = 5, indent = 0 }: { rows?: number; indent?: number }) {
  return (
    <div className="space-y-1.5 px-3 py-2" style={{ paddingLeft: 12 + indent }} aria-hidden>
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className="h-3.5" />
      ))}
    </div>
  );
}

const dotClass: Record<ConnectionState, string> = {
  connected: "bg-ok",
  connecting: "bg-warn animate-pulse",
  error: "bg-danger",
  disconnected: "bg-muted/40",
};
const dotLabel: Record<ConnectionState, string> = {
  connected: "Connected",
  connecting: "Connecting",
  error: "Connection error",
  disconnected: "Disconnected",
};
export function StatusDot({ state, className }: { state: ConnectionState; className?: string }) {
  return <span role="img" aria-label={dotLabel[state]} title={dotLabel[state]} className={cn("inline-block size-2 shrink-0 rounded-full", dotClass[state], className)} />;
}
export const connectionLabel = dotLabel;

type Tone = "muted" | "warn" | "danger" | "ok" | "accent";
const toneClass: Record<Tone, string> = {
  muted: "border-border bg-panel-2 text-muted",
  warn: "border-warn/40 bg-warn/10 text-warn",
  danger: "border-danger/40 bg-danger/10 text-danger",
  ok: "border-ok/40 bg-ok/10 text-ok",
  accent: "border-accent/40 bg-accent/10 text-accent",
};
export function Chip({ tone = "muted", icon, children, title, className }: { tone?: Tone; icon?: ReactNode; children: ReactNode; title?: string; className?: string }) {
  return (
    <span title={title} className={cn("inline-flex h-5 items-center gap-1 rounded border px-1.5 text-[11px] font-medium leading-none whitespace-nowrap", toneClass[tone], className)}>
      {icon}
      {children}
    </span>
  );
}

export function Segmented<T extends string>({
  value,
  onChange,
  options,
  label,
}: {
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: string; icon?: ReactNode }[];
  label: string;
}) {
  return (
    <div role="radiogroup" aria-label={label} className="inline-flex h-7 items-center rounded-md border border-border bg-bg p-0.5">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          onClick={() => onChange(o.value)}
          className={cn(
            "inline-flex h-5 items-center gap-1 rounded px-2 text-xs font-medium outline-none",
            o.value === value ? "bg-panel-2 text-fg shadow-sm" : "text-muted hover:text-fg",
          )}
        >
          {o.icon}
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** Compact native select (h-7) with a drawn chevron; used for target pickers and db selectors. */
export function MiniSelect({
  value,
  onChange,
  children,
  label,
  className,
  disabled,
  leading,
}: {
  value: string;
  onChange: (v: string) => void;
  children: ReactNode;
  label: string;
  className?: string;
  disabled?: boolean;
  leading?: ReactNode;
}) {
  return (
    <div className={cn("relative inline-flex h-7 items-center", className)}>
      {leading && <span className="pointer-events-none absolute left-2 z-10 flex items-center">{leading}</span>}
      <select
        aria-label={label}
        title={label}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
        className={cn(
          "h-7 w-full min-w-0 appearance-none truncate rounded-md border border-border bg-panel-2 pr-6 text-xs text-fg outline-none hover:border-muted/60 focus-visible:border-accent/70 disabled:opacity-50",
          leading ? "pl-6" : "pl-2",
        )}
      >
        {children}
      </select>
      <svg aria-hidden viewBox="0 0 12 12" className="pointer-events-none absolute right-2 size-3 text-muted">
        <path d="M3 4.5 6 7.5 9 4.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
      </svg>
    </div>
  );
}
