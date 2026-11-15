import { Bot, Check, Columns2, Database } from "lucide-react";
import { useUIStore, type AiMode } from "../../store/useUIStore";
import { cn } from "../../lib/cn";
import { AI_MODES } from "./aiModes";

const ICONS: Record<AiMode, typeof Database> = { classic: Database, assist: Columns2, agentFirst: Bot };

/** First-run choice of interface. Renders nothing once a mode has been chosen. */
export function ModeChooser() {
  const aiMode = useUIStore((s) => s.aiMode);
  const setAiMode = useUIStore((s) => s.setAiMode);
  if (aiMode !== null) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-6 backdrop-blur-sm">
      <div role="dialog" aria-modal="true" aria-labelledby="vg-mode-title" className="w-full max-w-4xl rounded-2xl border border-border bg-panel p-8 shadow-2xl">
        <div className="mb-6 flex items-center gap-3">
          <img src="/vibelens.svg" alt="" className="size-9" />
          <div>
            <h1 id="vg-mode-title" className="text-lg font-semibold tracking-tight">
              How do you want to work?
            </h1>
            <p className="text-[13px] text-muted">You can change this any time in Settings → Interface &amp; AI.</p>
          </div>
        </div>
        <div className="grid gap-3 md:grid-cols-3">
          {AI_MODES.map((m) => {
            const Icon = ICONS[m.id];
            return (
              <button
                key={m.id}
                type="button"
                onClick={() => setAiMode(m.id)}
                className={cn(
                  "group flex flex-col gap-3 rounded-xl border border-border bg-bg/60 p-5 text-left outline-none transition",
                  "hover:border-accent/60 hover:bg-panel-2 focus-visible:ring-2 focus-visible:ring-accent/60",
                )}
              >
                <div className="flex items-center gap-2.5">
                  <span className="flex size-9 items-center justify-center rounded-lg bg-panel-2 text-accent group-hover:bg-accent/15">
                    <Icon className="size-[18px]" />
                  </span>
                  <span className="text-[15px] font-semibold">{m.title}</span>
                </div>
                <p className="text-[13px] leading-snug text-fg/90">{m.tagline}</p>
                <ul className="space-y-1.5">
                  {m.points.map((p) => (
                    <li key={p} className="flex gap-2 text-[12px] leading-snug text-muted">
                      <Check className="mt-0.5 size-3.5 shrink-0 text-accent/80" />
                      {p}
                    </li>
                  ))}
                </ul>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
