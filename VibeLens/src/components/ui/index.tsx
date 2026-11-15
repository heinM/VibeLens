// Shared UI primitives. Both frontend modules use these so the app looks like one product.
// Extend by adding new exports; don't change existing prop contracts.
import { forwardRef, useEffect, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes } from "react";
import { Loader2, X } from "lucide-react";
import { cn } from "../../lib/cn";
import type { Environment } from "../../ipc/types";

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md";

export const Button = forwardRef<HTMLButtonElement, ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: Size; loading?: boolean }>(
  ({ variant = "secondary", size = "md", loading, className, children, disabled, ...rest }, ref) => (
    <button
      ref={ref}
      disabled={disabled || loading}
      className={cn(
        "inline-flex items-center justify-center gap-1.5 rounded-md font-medium transition-colors outline-none focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-50 disabled:pointer-events-none whitespace-nowrap",
        size === "sm" ? "h-7 px-2.5 text-xs" : "h-8 px-3 text-[13px]",
        variant === "primary" && "bg-accent text-accent-fg hover:brightness-110",
        variant === "secondary" && "bg-panel-2 text-fg border border-border hover:border-muted/60",
        variant === "ghost" && "text-muted hover:text-fg hover:bg-panel-2",
        variant === "danger" && "bg-danger text-white hover:brightness-110",
        className,
      )}
      {...rest}
    >
      {loading && <Loader2 className="size-3.5 animate-spin" />}
      {children}
    </button>
  ),
);
Button.displayName = "Button";

export const IconButton = forwardRef<HTMLButtonElement, ButtonHTMLAttributes<HTMLButtonElement> & { label: string }>(
  ({ label, className, children, ...rest }, ref) => (
    <button
      ref={ref}
      title={label}
      aria-label={label}
      className={cn("inline-flex size-7 items-center justify-center rounded-md text-muted hover:text-fg hover:bg-panel-2 disabled:opacity-40", className)}
      {...rest}
    >
      {children}
    </button>
  ),
);
IconButton.displayName = "IconButton";

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(({ className, ...rest }, ref) => (
  <input
    ref={ref}
    className={cn(
      "h-8 w-full rounded-md border border-border bg-bg px-2.5 text-[13px] text-fg placeholder:text-muted/70 outline-none focus:border-accent/70 focus:ring-1 focus:ring-accent/40",
      className,
    )}
    {...rest}
  />
));
Input.displayName = "Input";

export const Select = forwardRef<HTMLSelectElement, SelectHTMLAttributes<HTMLSelectElement>>(({ className, children, ...rest }, ref) => (
  <select
    ref={ref}
    className={cn("h-8 w-full rounded-md border border-border bg-bg px-2 text-[13px] text-fg outline-none focus:border-accent/70", className)}
    {...rest}
  >
    {children}
  </select>
));
Select.displayName = "Select";

export function Field({ label, hint, children, className }: { label: string; hint?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <label className={cn("flex flex-col gap-1", className)}>
      <span className="text-[11px] font-medium uppercase tracking-wide text-muted">{label}</span>
      {children}
      {hint && <span className="text-[11px] text-muted">{hint}</span>}
    </label>
  );
}

export function Spinner({ className }: { className?: string }) {
  return <Loader2 className={cn("size-4 animate-spin text-muted", className)} />;
}

const envColor: Record<Environment, string> = {
  dev: "bg-env-dev/15 text-env-dev border-env-dev/30",
  test: "bg-env-test/15 text-env-test border-env-test/30",
  staging: "bg-env-staging/15 text-env-staging border-env-staging/30",
  prod: "bg-env-prod/15 text-env-prod border-env-prod/40",
};

export function EnvBadge({ env, className }: { env: Environment; className?: string }) {
  return (
    <span className={cn("inline-flex items-center rounded border px-1.5 text-[10px] font-semibold uppercase leading-4 tracking-wider", envColor[env], className)}>
      {env}
    </span>
  );
}

export function Badge({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cn("inline-flex items-center rounded bg-panel-2 px-1.5 text-[11px] leading-5 text-muted border border-border", className)}>{children}</span>;
}

export function Dialog({
  open,
  onClose,
  title,
  children,
  footer,
  width = "max-w-xl",
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  width?: string;
}) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-[2px] p-6" onMouseDown={onClose}>
      <div
        role="dialog"
        aria-modal="true"
        className={cn("flex max-h-full w-full flex-col rounded-xl border border-border bg-panel shadow-2xl", width)}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between border-b border-border px-4 py-3">
          <h2 className="text-sm font-semibold">{title}</h2>
          <IconButton label="Close" onClick={onClose}>
            <X className="size-4" />
          </IconButton>
        </div>
        <div className="min-h-0 flex-1 overflow-auto p-4">{children}</div>
        {footer && <div className="flex justify-end gap-2 border-t border-border px-4 py-3">{footer}</div>}
      </div>
    </div>
  );
}

/** Typed-confirmation dialog used for prod writes and destructive approvals. */
export function ConfirmDialog({
  open,
  title,
  body,
  confirmText = "Run",
  requireTyping,
  danger,
  onConfirm,
  onCancel,
}: {
  open: boolean;
  title: string;
  body: ReactNode;
  confirmText?: string;
  /** If set, user must type this exact string to enable the confirm button. */
  requireTyping?: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <Dialog
      open={open}
      onClose={onCancel}
      title={title}
      footer={
        <>
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <ConfirmButton requireTyping={requireTyping} danger={danger} onConfirm={onConfirm}>
            {confirmText}
          </ConfirmButton>
        </>
      }
    >
      <div className="space-y-3 text-[13px]">{body}</div>
      {requireTyping && (
        <div className="mt-4">
          <Field label={`Type "${requireTyping}" to confirm`}>
            <Input id="vg-confirm-typing" autoFocus autoComplete="off" />
          </Field>
        </div>
      )}
    </Dialog>
  );
}

function ConfirmButton({ requireTyping, danger, onConfirm, children }: { requireTyping?: string; danger?: boolean; onConfirm: () => void; children: ReactNode }) {
  return (
    <Button
      variant={danger ? "danger" : "primary"}
      onClick={() => {
        if (requireTyping) {
          const v = (document.getElementById("vg-confirm-typing") as HTMLInputElement | null)?.value;
          if (v !== requireTyping) return;
        }
        onConfirm();
      }}
    >
      {children}
    </Button>
  );
}

export function EmptyState({ icon, title, children }: { icon?: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2 p-8 text-center text-muted">
      {icon}
      <div className="text-sm font-medium text-fg">{title}</div>
      {children && <div className="max-w-sm text-xs">{children}</div>}
    </div>
  );
}

export function ErrorBox({ error }: { error: { code?: string; message: string } | null | undefined }) {
  if (!error) return null;
  return (
    <div className="selectable rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-xs text-danger">
      {error.code && <span className="mr-1 font-mono opacity-70">[{error.code}]</span>}
      {error.message}
    </div>
  );
}
