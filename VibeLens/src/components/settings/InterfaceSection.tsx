import { useUIStore } from "../../store/useUIStore";
import { cn } from "../../lib/cn";
import { AI_MODES } from "../layout/aiModes";
import { Button } from "../ui";

/** Settings → Interface & AI: switch between classic, assist and agent-first. */
export function InterfaceSection({ onConfigureProviders }: { onConfigureProviders: () => void }) {
  const aiMode = useUIStore((s) => s.aiMode);
  const setAiMode = useUIStore((s) => s.setAiMode);
  return (
    <div className="space-y-4">
      <div>
        <h3 className="text-[13px] font-semibold">Interface &amp; AI</h3>
        <p className="mt-1 text-[12px] text-muted">
          The database engines work the same in every mode. This only controls whether, and where, the AI agent appears.
        </p>
      </div>
      <div role="radiogroup" aria-label="Interface mode" className="space-y-2">
        {AI_MODES.map((m) => {
          const selected = aiMode === m.id;
          return (
            <button
              key={m.id}
              type="button"
              role="radio"
              aria-checked={selected}
              onClick={() => setAiMode(m.id)}
              className={cn(
                "flex w-full items-start gap-3 rounded-lg border px-3.5 py-3 text-left outline-none focus-visible:ring-2 focus-visible:ring-accent/60",
                selected ? "border-accent/70 bg-accent/[0.07]" : "border-border hover:border-muted/60",
              )}
            >
              <span className={cn("mt-1 size-3.5 shrink-0 rounded-full border", selected ? "border-accent bg-accent" : "border-muted")} />
              <span>
                <span className="block text-[13px] font-medium">{m.title}</span>
                <span className="block text-[12px] text-muted">{m.tagline}</span>
              </span>
            </button>
          );
        })}
      </div>
      {aiMode !== "classic" && (
        <div className="flex items-center justify-between rounded-lg border border-border bg-bg/50 px-3.5 py-3 text-[12px] text-muted">
          <span>The agent needs a provider: Anthropic, an OpenAI-compatible endpoint, or a local model through Ollama.</span>
          <Button size="sm" onClick={onConfigureProviders}>
            Configure providers
          </Button>
        </div>
      )}
    </div>
  );
}
