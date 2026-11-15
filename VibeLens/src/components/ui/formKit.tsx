// Form and dialog primitives used by the connection wizard, settings and agent panel.
// (Separate from ./index.tsx, owned by the lead, and ./extras.tsx, owned by fe-workspace.)
import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, Check, CheckCircle2, Copy, Info, OctagonAlert } from "lucide-react";
import { cn } from "../../lib/cn";
import { IconButton } from "./index";

/* -------------------------------------------------------------------------- */
/* Form field with an explicit id/label association, an error line and a hint. */
/* -------------------------------------------------------------------------- */

export function FormField({
  label,
  htmlFor,
  hint,
  error,
  children,
  className,
  optional,
  action,
}: {
  label: string;
  htmlFor?: string;
  hint?: ReactNode;
  error?: string | null;
  children: ReactNode;
  className?: string;
  optional?: boolean;
  /** Small control aligned to the right of the label (e.g. a "Replace" link). */
  action?: ReactNode;
}) {
  return (
    <div className={cn("flex min-w-0 flex-col gap-1", className)}>
      <div className="flex items-center justify-between gap-2">
        <label htmlFor={htmlFor} className="text-[12px] font-medium text-fg/90">
          {label}
          {optional && <span className="ml-1 font-normal text-muted">optional</span>}
        </label>
        {action}
      </div>
      {children}
      {error ? (
        <span role="alert" className="text-[11px] leading-snug text-danger">
          {error}
        </span>
      ) : (
        hint && <span className="text-[11px] leading-snug text-muted">{hint}</span>
      )}
    </div>
  );
}

/** Class for inputs that have a validation error. */
export const invalidRing = "!border-danger/70 focus:!border-danger focus:!ring-danger/30";

/* -------------------------------------------------------------------------- */
/* Switch                                                                      */
/* -------------------------------------------------------------------------- */

export function Switch({
  checked,
  onChange,
  label,
  description,
  disabled,
  id,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  description?: ReactNode;
  disabled?: boolean;
  id?: string;
}) {
  const auto = useId();
  const sid = id ?? auto;
  return (
    <div className="flex items-start gap-2.5">
      <button
        id={sid}
        type="button"
        role="switch"
        aria-checked={checked}
        disabled={disabled}
        onClick={() => onChange(!checked)}
        className={cn(
          "relative mt-0.5 h-[18px] w-8 shrink-0 rounded-full border transition-colors outline-none focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-50",
          checked ? "border-accent bg-accent" : "border-border bg-panel-2",
        )}
      >
        <span
          className={cn(
            "absolute top-1/2 size-3 -translate-y-1/2 rounded-full transition-all",
            checked ? "left-[17px] bg-accent-fg" : "left-[2px] bg-muted",
          )}
        />
      </button>
      <label htmlFor={sid} className="min-w-0 cursor-pointer select-none">
        <span className="block text-[13px] leading-[18px] text-fg">{label}</span>
        {description && <span className="block text-[11px] leading-snug text-muted">{description}</span>}
      </label>
    </div>
  );
}

/* -------------------------------------------------------------------------- */
/* Segmented control                                                           */
/* -------------------------------------------------------------------------- */

export interface SegmentOption<T extends string> {
  value: T;
  label: ReactNode;
  disabled?: boolean;
  title?: string;
}

export function SegmentedControl<T extends string>({
  value,
  onChange,
  options,
  ariaLabel,
  className,
}: {
  value: T;
  onChange: (v: T) => void;
  options: SegmentOption<T>[];
  ariaLabel: string;
  className?: string;
}) {
  return (
    <div role="radiogroup" aria-label={ariaLabel} className={cn("inline-flex flex-wrap rounded-md border border-border bg-bg p-0.5", className)}>
      {options.map((o) => {
        const on = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={on}
            title={o.title}
            disabled={o.disabled}
            onClick={() => onChange(o.value)}
            className={cn(
              "inline-flex h-7 items-center justify-center gap-1.5 rounded px-2.5 text-xs font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-40",
              on ? "bg-panel-2 text-fg shadow-[inset_0_0_0_1px_var(--vg-border)]" : "text-muted hover:text-fg",
            )}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/* -------------------------------------------------------------------------- */
/* Radio cards                                                                 */
/* -------------------------------------------------------------------------- */

export interface RadioCardOption<T extends string> {
  value: T;
  title: ReactNode;
  description?: ReactNode;
  icon?: ReactNode;
  badge?: ReactNode;
  disabled?: boolean;
  /** Tailwind classes applied to the card when selected (border / ring tint). */
  selectedClass?: string;
}

export function RadioCards<T extends string>({
  value,
  onChange,
  options,
  ariaLabel,
  columns = 1,
}: {
  value: T;
  onChange: (v: T) => void;
  options: RadioCardOption<T>[];
  ariaLabel: string;
  columns?: 1 | 2 | 3;
}) {
  const onKey = (e: React.KeyboardEvent) => {
    const enabled = options.filter((o) => !o.disabled);
    const i = enabled.findIndex((o) => o.value === value);
    let next = -1;
    if (e.key === "ArrowDown" || e.key === "ArrowRight") next = (i + 1) % enabled.length;
    else if (e.key === "ArrowUp" || e.key === "ArrowLeft") next = (i - 1 + enabled.length) % enabled.length;
    if (next >= 0) {
      e.preventDefault();
      onChange(enabled[next].value);
      requestAnimationFrame(() => (e.currentTarget as HTMLElement).querySelector<HTMLElement>('[aria-checked="true"]')?.focus());
    }
  };
  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      onKeyDown={onKey}
      className={cn("grid gap-2", columns === 2 && "sm:grid-cols-2", columns === 3 && "sm:grid-cols-3")}
    >
      {options.map((o) => {
        const on = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={on}
            tabIndex={on ? 0 : -1}
            disabled={o.disabled}
            onClick={() => onChange(o.value)}
            className={cn(
              "flex min-w-0 items-start gap-2.5 rounded-lg border bg-bg p-3 text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/60 disabled:cursor-not-allowed disabled:opacity-50",
              on ? (o.selectedClass ?? "border-accent/70 bg-accent/[0.06]") : "border-border hover:border-muted/50",
            )}
          >
            <span aria-hidden className={cn("mt-0.5 grid size-4 shrink-0 place-items-center rounded-full border", on ? "border-accent" : "border-muted/60")}>
              {on && <span className="size-2 rounded-full bg-accent" />}
            </span>
            {o.icon && <span className="mt-0.5 shrink-0 text-muted">{o.icon}</span>}
            <span className="min-w-0 flex-1">
              <span className="flex items-center gap-2 text-[13px] font-medium text-fg">
                {o.title}
                {o.badge}
              </span>
              {o.description && <span className="mt-0.5 block text-[12px] leading-snug text-muted">{o.description}</span>}
            </span>
          </button>
        );
      })}
    </div>
  );
}

/* -------------------------------------------------------------------------- */
/* Notice                                                                      */
/* -------------------------------------------------------------------------- */

export type NoticeTone = "info" | "warn" | "danger" | "ok";

const noticeStyle: Record<NoticeTone, string> = {
  info: "border-border bg-panel-2/60 text-muted",
  warn: "border-warn/40 bg-warn/10 text-warn",
  danger: "border-danger/50 bg-danger/10 text-danger",
  ok: "border-ok/40 bg-ok/10 text-ok",
};

export function Notice({ tone = "info", children, className, action }: { tone?: NoticeTone; children: ReactNode; className?: string; action?: ReactNode }) {
  const Icon = tone === "danger" ? OctagonAlert : tone === "warn" ? AlertTriangle : tone === "ok" ? CheckCircle2 : Info;
  return (
    <div role={tone === "danger" || tone === "warn" ? "alert" : "note"} className={cn("flex items-start gap-2 rounded-md border px-2.5 py-2 text-[12px] leading-snug", noticeStyle[tone], className)}>
      <Icon className="mt-px size-3.5 shrink-0" />
      <div className={cn("min-w-0 flex-1", tone === "info" ? "text-muted" : "text-fg/90")}>{children}</div>
      {action}
    </div>
  );
}

/* -------------------------------------------------------------------------- */
/* Copy button                                                                 */
/* -------------------------------------------------------------------------- */

export function useCopy(timeout = 1500) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | null>(null);
  useEffect(
    () => () => {
      if (timer.current != null) window.clearTimeout(timer.current);
    },
    [],
  );
  const copy = useCallback(
    async (text: string) => {
      try {
        await navigator.clipboard.writeText(text);
        setCopied(true);
        if (timer.current != null) window.clearTimeout(timer.current);
        timer.current = window.setTimeout(() => setCopied(false), timeout);
      } catch {
        setCopied(false);
      }
    },
    [timeout],
  );
  return { copied, copy };
}

export function CopyIconButton({ text, label = "Copy", className }: { text: string; label?: string; className?: string }) {
  const { copied, copy } = useCopy();
  return (
    <IconButton label={copied ? "Copied" : label} className={className} onClick={() => void copy(text)}>
      {copied ? <Check className="size-3.5 text-ok" /> : <Copy className="size-3.5" />}
    </IconButton>
  );
}

/* -------------------------------------------------------------------------- */
/* Keyboard hint                                                               */
/* -------------------------------------------------------------------------- */

export function Key({ children }: { children: ReactNode }) {
  return <kbd className="rounded border border-border bg-bg px-1 font-mono text-[10px] leading-4 text-muted">{children}</kbd>;
}

export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
export const modKey = isMac ? "⌘" : "Ctrl";

/* -------------------------------------------------------------------------- */
/* Tooltip (CSS only)                                                          */
/* -------------------------------------------------------------------------- */

export function Tip({ content, children, side = "bottom", className }: { content: ReactNode; children: ReactNode; side?: "top" | "bottom"; className?: string }) {
  return (
    <span className={cn("group/tip relative inline-flex", className)}>
      {children}
      <span
        role="tooltip"
        className={cn(
          "pointer-events-none absolute right-0 z-50 w-64 rounded-md border border-border bg-panel p-2 text-[11px] leading-snug text-fg opacity-0 shadow-xl transition-opacity group-hover/tip:opacity-100 group-focus-within/tip:opacity-100",
          side === "bottom" ? "top-full mt-1.5" : "bottom-full mb-1.5",
        )}
      >
        {content}
      </span>
    </span>
  );
}

/* -------------------------------------------------------------------------- */
/* Focus management for dialogs                                                */
/* -------------------------------------------------------------------------- */

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * Render inside a `Dialog`. On mount it moves focus to `[data-autofocus]` (or the first field),
 * traps Tab within the enclosing `[role=dialog]` while it is the top-most dialog, and restores
 * focus to the previously focused element on unmount.
 */
export function FocusScope({ children, className = "contents" }: { children: ReactNode; className?: string }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    const host = ref.current?.closest<HTMLElement>('[role="dialog"]') ?? null;
    const first =
      ref.current?.querySelector<HTMLElement>("[data-autofocus]") ?? ref.current?.querySelector<HTMLElement>("input:not([disabled]), select:not([disabled]), textarea:not([disabled])");
    first?.focus({ preventScroll: true });

    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab" || !host) return;
      const dialogs = document.querySelectorAll('[role="dialog"]');
      if (dialogs[dialogs.length - 1] !== host) return;
      const items = Array.from(host.querySelectorAll<HTMLElement>(FOCUSABLE)).filter((el) => el.offsetParent !== null);
      if (items.length === 0) return;
      const active = document.activeElement as HTMLElement | null;
      const firstEl = items[0];
      const lastEl = items[items.length - 1];
      if (e.shiftKey && (active === firstEl || !host.contains(active))) {
        e.preventDefault();
        lastEl.focus();
      } else if (!e.shiftKey && (active === lastEl || !host.contains(active))) {
        e.preventDefault();
        firstEl.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      if (previous && document.contains(previous)) previous.focus({ preventScroll: true });
    };
  }, []);
  return (
    <div ref={ref} className={className}>
      {children}
    </div>
  );
}

/* -------------------------------------------------------------------------- */
/* Number field that tolerates empty / in-progress text                        */
/* -------------------------------------------------------------------------- */

export function NumberField({
  value,
  onChange,
  min,
  max,
  placeholder,
  invalid,
  suffix,
  id,
  allowEmpty,
  step,
}: {
  /** NaN = "empty / not a number yet" when `allowEmpty` is false. */
  value: number | null;
  onChange: (v: number | null) => void;
  min?: number;
  max?: number;
  placeholder?: string;
  invalid?: boolean;
  suffix?: string;
  id?: string;
  allowEmpty?: boolean;
  step?: number;
}) {
  const [text, setText] = useState(value == null || Number.isNaN(value) ? "" : String(value));
  useEffect(() => {
    const parsed = text.trim() === "" ? (allowEmpty ? null : Number.NaN) : Number(text);
    const same = parsed === value || (typeof parsed === "number" && typeof value === "number" && Number.isNaN(parsed) && Number.isNaN(value));
    if (!same) setText(value == null || Number.isNaN(value) ? "" : String(value));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [value]);
  return (
    <div className="relative">
      <input
        id={id}
        inputMode="decimal"
        type="number"
        min={min}
        max={max}
        step={step}
        value={text}
        placeholder={placeholder}
        onChange={(e) => {
          const t = e.target.value;
          setText(t);
          if (t.trim() === "") onChange(allowEmpty ? null : Number.NaN);
          else {
            const n = Number(t);
            onChange(Number.isFinite(n) ? n : Number.NaN);
          }
        }}
        className={cn(
          "h-8 w-full rounded-md border border-border bg-bg px-2.5 text-[13px] text-fg tabular-nums placeholder:text-muted/70 outline-none [appearance:textfield] focus:border-accent/70 focus:ring-1 focus:ring-accent/40 [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none",
          suffix && "pr-10",
          invalid && invalidRing,
        )}
      />
      {suffix && <span className="pointer-events-none absolute inset-y-0 right-2.5 grid place-items-center text-[11px] text-muted">{suffix}</span>}
    </div>
  );
}
