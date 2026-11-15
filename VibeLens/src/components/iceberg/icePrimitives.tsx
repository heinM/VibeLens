// Presentational building blocks shared by the panes of the Iceberg table tab.
import type { ReactNode } from "react";
import { cn } from "../../lib/cn";
import { CopyIconButton } from "../ui/formKit";

export function PaneHeader({ title, subtitle, actions }: { title: string; subtitle?: ReactNode; actions?: ReactNode }) {
  return (
    <div className="mb-4 flex min-h-8 flex-wrap items-end justify-between gap-x-4 gap-y-2">
      <div className="min-w-0">
        <h2 className="text-[15px] font-semibold leading-6 text-fg">{title}</h2>
        {subtitle && <p className="text-[12px] leading-snug text-muted">{subtitle}</p>}
      </div>
      {actions && <div className="flex shrink-0 items-center gap-1.5">{actions}</div>}
    </div>
  );
}

export function Card({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("rounded-lg border border-border bg-panel", className)}>{children}</div>;
}

/** Label / value row of a definition list; rows separate with a hairline. */
export function KvRow({ label, children, hint }: { label: string; children: ReactNode; hint?: string }) {
  return (
    <div className="grid grid-cols-1 gap-x-4 gap-y-0.5 border-b border-border/70 px-3.5 py-2 last:border-b-0 @[560px]:grid-cols-[150px_minmax(0,1fr)]">
      <dt className="pt-px text-[12px] text-muted" title={hint}>
        {label}
      </dt>
      <dd className="min-w-0 text-[13px] text-fg">{children}</dd>
    </div>
  );
}

/** Selectable mono value with a copy button; long values wrap rather than truncate, because paths matter. */
export function CopyValue({ value, label }: { value: string; label: string }) {
  return (
    <span className="group inline-flex max-w-full items-start gap-1">
      <span className="selectable min-w-0 break-all font-mono text-[12px] leading-5">{value}</span>
      <CopyIconButton text={value} label={`Copy ${label}`} className="!size-5 shrink-0 opacity-0 group-hover:opacity-100 focus-visible:opacity-100" />
    </span>
  );
}

export function Stat({ label, value, sub }: { label: string; value: ReactNode; sub?: ReactNode }) {
  return (
    <div className="min-w-0 bg-panel px-4 py-3">
      <div className="truncate text-[20px] font-semibold leading-7 tabular-nums text-fg">{value}</div>
      <div className="text-[12px] text-muted">{label}</div>
      {sub && <div className="truncate text-[11px] text-muted/80">{sub}</div>}
    </div>
  );
}

export function FormatBadge({ version }: { version: number }) {
  return (
    <span className="inline-flex h-5 items-center rounded border border-accent/40 bg-accent/10 px-1.5 font-mono text-[11px] font-semibold leading-none text-accent" title={`Iceberg table format version ${version}`}>
      v{version}
    </span>
  );
}

export function InlineEmpty({ children }: { children: ReactNode }) {
  return <div className="rounded-lg border border-dashed border-border px-4 py-6 text-center text-[12px] leading-relaxed text-muted">{children}</div>;
}
