// Plain-text query plan (Iceberg: DataFusion or Athena). Monospaced and verbatim, with copy,
// wrap and an optional AI explanation that follows the same data-access rules as Postgres plans.
import { useEffect, useMemo, useState } from "react";
import { Sparkles, WrapText } from "lucide-react";
import { cn } from "../../lib/cn";
import { AI_OFF_HINT } from "../../lib/aiInline";
import { selectAiEnabled, useUIStore } from "../../store/useUIStore";
import { useProfile } from "../../hooks/useProfiles";
import { Button } from "../ui";
import { CopyIconButton } from "../ui/formKit";
import { engineBadge } from "../grid/statsFormat";
import { ExplainAiPanel } from "./ExplainAiPanel";
import { explainNeedsRedaction } from "./planRedact";
import { buildTextPlanPrompt, planAsText, type TextPlanSection } from "./textPlan";
import { useExplainAi } from "./useExplainAi";

export interface TextPlanViewProps {
  sections: readonly TextPlanSection[];
  /** `datafusion` or `athena`. */
  engine: string;
  sql?: string;
  profileId?: string | null;
}

const OPERATOR = /^(\s*(?:[-|+]\s*)?)([A-Za-z][A-Za-z0-9_]*)(?=[:\[(\s]|$)/;

/** One plan line with its operator name picked out (`FilterExec`, `TableScan`, `Project`...). */
function PlanLine({ line }: { line: string }) {
  const m = OPERATOR.exec(line);
  // Only emphasise names that look like operators: capitalised, and followed by the usual `:` or `[`.
  if (!m || !/^[A-Z]/.test(m[2]) || !/^[:\[]/.test(line.slice(m[0].length))) return <>{line}</>;
  return (
    <>
      {m[1]}
      <span className="font-semibold text-accent">{m[2]}</span>
      {line.slice(m[0].length)}
    </>
  );
}

export function TextPlanView({ sections, engine, sql, profileId = null }: TextPlanViewProps) {
  const badge = engineBadge(engine);
  const aiEnabled = useUIStore(selectAiEnabled);
  const profile = useProfile(profileId);
  const [wrap, setWrap] = useState(false);
  const [showAi, setShowAi] = useState(false);

  const access = profile?.aiDataAccess ?? null;
  const aiOff = access === "none";
  const redacted = explainNeedsRedaction(access, profile?.environment);
  const engineLabel = engine.toLowerCase() === "athena" ? "Amazon Athena" : "Apache DataFusion";
  // Fail closed: when the plan or the SQL cannot be redacted, nothing is sent.
  const prompt = useMemo(() => buildTextPlanPrompt({ sql, sections, engineLabel, redact: redacted }), [sql, sections, engineLabel, redacted]);
  const ai = useExplainAi({ prompt: prompt ?? "", profileId, database: null, access, blocked: prompt === null });
  const aiBusy = ai.state.status === "running";

  useEffect(() => {
    if (!aiEnabled) setShowAi(false);
  }, [aiEnabled]);

  const copyText = useMemo(() => planAsText(sections), [sections]);
  const startAi = () => {
    setShowAi(true);
    if (ai.state.status === "idle") void ai.run();
  };

  return (
    <div className="@container flex h-full min-h-[200px] flex-col bg-bg" aria-label="Query plan">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border bg-panel px-3 text-xs">
        <span className="font-medium text-fg">Query plan</span>
        <span className="text-muted" title={badge.title}>
          {badge.label}
        </span>
        <span className="hidden text-muted @[640px]:inline">Estimated: the statement was not run.</span>
        <div className="ml-auto flex items-center gap-1">
          <button
            type="button"
            role="switch"
            aria-checked={wrap}
            onClick={() => setWrap((w) => !w)}
            title="Wrap long lines"
            className={cn("inline-flex h-7 items-center gap-1.5 rounded-md border px-2 text-xs font-medium", wrap ? "border-accent/40 bg-accent/10 text-accent" : "border-border bg-panel-2 text-muted hover:text-fg")}
          >
            <WrapText className="size-3.5" /> <span className="hidden @[520px]:inline">Wrap</span>
          </button>
          <CopyIconButton text={copyText} label="Copy plan" />
          {aiEnabled && (
            <Button
              variant={ai.state.status === "idle" ? "primary" : "secondary"}
              size="sm"
              onClick={() => (showAi && ai.state.status !== "idle" ? setShowAi(false) : startAi())}
              loading={aiBusy}
              disabled={aiOff || prompt === null}
              title={aiOff ? AI_OFF_HINT : prompt === null ? "This plan could not be prepared safely for the assistant." : undefined}
            >
              {!aiBusy && <Sparkles className="size-3.5" />} {ai.state.status === "idle" ? "Explain with AI" : showAi ? "Hide AI" : "AI explanation"}
            </Button>
          )}
        </div>
      </div>
      <div className={cn("flex min-h-0 flex-1 flex-col", "@[900px]:flex-row")}>
        <div className="min-h-0 min-w-0 flex-1 overflow-auto p-3" data-testid="text-plan">
          {sections.map((s, i) => (
            <section key={i} className={cn(i > 0 && "mt-4")}>
              {s.title && <h3 className="mb-1 font-mono text-[11px] font-semibold uppercase tracking-wide text-muted">{s.title}</h3>}
              <pre className={cn("selectable font-mono text-[12px] leading-5 text-fg", wrap ? "whitespace-pre-wrap break-words" : "whitespace-pre")}>
                {s.text.split("\n").map((line, j) => (
                  <div key={j}>
                    <PlanLine line={line} />
                    {line === "" ? " " : null}
                  </div>
                ))}
              </pre>
            </section>
          ))}
        </div>
        {aiEnabled && showAi && (
          <aside className="min-h-0 shrink-0 overflow-y-auto border-t border-border bg-panel @[900px]:w-[380px] @[900px]:border-l @[900px]:border-t-0 max-h-[50%] @[900px]:max-h-none" aria-label="AI explanation">
            <ExplainAiPanel state={ai.state} onRun={() => void ai.run()} onCancel={ai.cancel} access={access} redacted={redacted} hasSql={!!sql?.trim()} />
          </aside>
        )}
      </div>
    </div>
  );
}
