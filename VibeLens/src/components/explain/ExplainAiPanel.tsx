import { Loader2, RotateCw, Settings2, Sparkles, Square } from "lucide-react";
import { Button } from "../ui";
import { Notice } from "../ui/formKit";
import { Markdown } from "../agent/Markdown.tsx";
import { useUIStore } from "../../store/useUIStore";
import type { AiDataAccess } from "../../ipc/types";
import { AI_OFF_HINT } from "../../lib/aiInline";
import type { ExplainAiState } from "./useExplainAi";

export function ExplainAiPanel({
  state,
  onRun,
  onCancel,
  access,
  redacted,
  hasSql,
}: {
  state: ExplainAiState;
  onRun: () => void;
  onCancel: () => void;
  access: AiDataAccess | null;
  /** Literal values are replaced by `?` in the plan and SQL before they are sent. */
  redacted: boolean;
  hasSql: boolean;
}) {
  if (access === "none") {
    return (
      <div className="p-4">
        <Notice tone="info">{AI_OFF_HINT}. Nothing about this plan or query is sent to the assistant. Change AI access in the connection&rsquo;s settings to use this.</Notice>
      </div>
    );
  }
  if (state.status === "idle") {
    return (
      <div className="flex flex-col items-start gap-3 p-4">
        <div className="grid size-9 place-items-center rounded-lg border border-border bg-panel-2">
          <Sparkles className="size-4 text-accent" />
        </div>
        <div>
          <p className="text-[13px] font-medium text-fg">Get a diagnosis and fixes for this plan</p>
          <p className="mt-1 text-[12px] leading-[1.55] text-muted">
            The assistant gets your SQL, a trimmed copy of the plan and the findings from the Insights tab, then explains what is slow and proposes index DDL or rewrites with their costs.
          </p>
        </div>
        <Button variant="primary" size="sm" onClick={onRun}>
          <Sparkles className="size-3.5" /> Explain with AI
        </Button>
        <p className="text-[11px] leading-snug text-muted">
          It may look up table and index definitions, never row data.{" "}
          {redacted ? "Literal values are redacted before sending." : "Filter conditions in the plan can contain literal values from your query, and they are sent along with it."}
          {hasSql ? "" : " The SQL text isn't available for this result, so only the plan is sent."}
        </p>
      </div>
    );
  }

  const text = state.text;
  return (
    <div className="p-3">
      {state.status === "running" && (
        <div className="mb-2 flex items-center gap-2 text-[12px] text-muted" role="status">
          <Loader2 className="size-3.5 animate-spin text-accent" />
          <span>{state.activity ?? (text ? "Writing" : "Reading the plan")}…</span>
          <Button size="sm" variant="ghost" onClick={onCancel} className="ml-auto !text-danger">
            <Square className="size-3 fill-current" /> Stop
          </Button>
        </div>
      )}
      {redacted && state.status !== "error" && <p className="mb-2 text-[11px] text-muted">Literal values are redacted before sending.</p>}
      {text && <Markdown text={text} wrapCode />}
      {state.status === "stopped" && (
        <Notice tone="warn" className="mt-3">
          {state.reason === "approval"
            ? "Stopped: the assistant asked to run something that needs your approval. Explanations only use read-only lookups. Ask in the agent panel if you want it to run queries."
            : "Stopped."}
        </Notice>
      )}
      {state.status === "error" &&
        (state.code === "invalid" ? (
          <Notice
            tone="info"
            className="mt-3"
            action={
              <Button size="sm" onClick={() => useUIStore.getState().setSettingsOpen(true)}>
                <Settings2 className="size-3.5" /> Set up AI in Settings
              </Button>
            }
          >
            No AI provider is set up yet. Add a provider and API key (or a local model) to use this.
          </Notice>
        ) : (
          <Notice tone="danger" className="mt-3">
            <span className="font-mono opacity-70">[{state.code}]</span> {state.message}
          </Notice>
        ))}
      {state.status !== "running" && (
        <div className="mt-3">
          <Button size="sm" onClick={onRun}>
            <RotateCw className="size-3.5" /> {state.status === "done" ? "Ask again" : "Try again"}
          </Button>
        </div>
      )}
    </div>
  );
}
