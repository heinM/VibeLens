// A tiny self-mounting toast. `showToast()` can be called from anywhere (event handlers, hooks);
// the first call mounts a host element on <body>, so no provider has to be added to the app shell.
import { useEffect, useSyncExternalStore } from "react";
import { createRoot } from "react-dom/client";
import { AlertCircle, Check, Copy, Info, X } from "lucide-react";
import { cn } from "../../lib/cn";
import { copyToClipboard } from "../ui/clipboard";

export type ToastTone = "ok" | "error" | "info";

export interface ToastOptions {
  title: string;
  /** Secondary line, rendered monospace and selectable (a saved path, an error). */
  detail?: string;
  tone?: ToastTone;
  /** Milliseconds before it dismisses itself; 0 keeps it until closed. Default 6000 (errors 10000). */
  durationMs?: number;
  /** Show a "Copy" button that copies `detail`. */
  copyDetail?: boolean;
}

interface ToastItem extends ToastOptions {
  id: number;
}

let items: ToastItem[] = [];
let nextId = 1;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};
const snapshot = () => items;

let mounted = false;
function ensureHost() {
  if (mounted || typeof document === "undefined") return;
  mounted = true;
  const el = document.createElement("div");
  el.id = "vg-toast-host";
  document.body.appendChild(el);
  createRoot(el).render(<ToastHost />);
}

export function dismissToast(id: number) {
  items = items.filter((t) => t.id !== id);
  emit();
}

/** Show a toast; returns its id. */
export function showToast(opts: ToastOptions): number {
  ensureHost();
  const id = nextId++;
  items = [...items.slice(-3), { ...opts, id }];
  emit();
  return id;
}

const toneIcon = {
  ok: <Check className="size-4 text-ok" />,
  error: <AlertCircle className="size-4 text-danger" />,
  info: <Info className="size-4 text-accent" />,
};

function ToastCard({ t }: { t: ToastItem }) {
  const tone = t.tone ?? "ok";
  useEffect(() => {
    const ms = t.durationMs ?? (tone === "error" ? 10_000 : 6_000);
    if (ms <= 0) return;
    const h = window.setTimeout(() => dismissToast(t.id), ms);
    return () => window.clearTimeout(h);
  }, [t.id, t.durationMs, tone]);
  return (
    <div
      role={tone === "error" ? "alert" : "status"}
      className={cn(
        "vg-toast pointer-events-auto flex w-[22rem] max-w-[calc(100vw-2rem)] items-start gap-2.5 rounded-lg border bg-panel p-3 text-[13px] text-fg shadow-2xl",
        tone === "error" ? "border-danger/50" : tone === "ok" ? "border-ok/40" : "border-border",
      )}
    >
      <span className="mt-0.5 shrink-0">{toneIcon[tone]}</span>
      <div className="min-w-0 flex-1">
        <div className="font-medium leading-5">{t.title}</div>
        {t.detail && <div className="selectable mt-0.5 break-all font-mono text-[11px] leading-4 text-muted">{t.detail}</div>}
      </div>
      {t.copyDetail && t.detail && (
        <button
          type="button"
          title="Copy"
          aria-label="Copy"
          onClick={() => void copyToClipboard(t.detail ?? "")}
          className="inline-flex size-6 shrink-0 items-center justify-center rounded text-muted hover:bg-panel-2 hover:text-fg"
        >
          <Copy className="size-3.5" />
        </button>
      )}
      <button
        type="button"
        title="Dismiss"
        aria-label="Dismiss"
        onClick={() => dismissToast(t.id)}
        className="inline-flex size-6 shrink-0 items-center justify-center rounded text-muted hover:bg-panel-2 hover:text-fg"
      >
        <X className="size-3.5" />
      </button>
    </div>
  );
}

function ToastHost() {
  const list = useSyncExternalStore(subscribe, snapshot, snapshot);
  return (
    <div aria-live="polite" className="pointer-events-none fixed bottom-10 right-4 z-[90] flex flex-col items-end gap-2">
      <style>{`@keyframes vg-toast-in{from{opacity:0;transform:translateY(6px)}to{opacity:1;transform:none}}.vg-toast{animation:vg-toast-in .16s ease-out}@media (prefers-reduced-motion:reduce){.vg-toast{animation:none}}`}</style>
      {list.map((t) => (
        <ToastCard key={t.id} t={t} />
      ))}
    </div>
  );
}
