// Query plan visualiser: summary, collapsible tree with per-node time share and misestimate badges,
// rule-based insights, a node inspector and an optional AI diagnosis.
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Lightbulb, ListTree, Sparkles } from "lucide-react";
import { cn } from "../../lib/cn";
import { selectAiEnabled, useUIStore } from "../../store/useUIStore";
import { useProfile } from "../../hooks/useProfiles";
import { Button, EmptyState } from "../ui";
import { Notice } from "../ui/formKit";
import { ancestorsOf, hotSpots, parsePlan, type ParsedPlan } from "./planModel";
import { buildInsights } from "./planInsights";
import { buildExplainPrompt } from "./planPrompt";
import { explainNeedsRedaction, redactPlanLiterals } from "./planRedact";
import { redactSqlLiterals } from "../../lib/sqlIntel/redact";
import { PlanSummary } from "./PlanSummary";
import { PlanTree } from "./PlanTree";
import { NodeDetails } from "./NodeDetails";
import { InsightsList } from "./InsightsList";
import { ExplainAiPanel } from "./ExplainAiPanel";
import { useExplainAi } from "./useExplainAi";
import { AI_OFF_HINT } from "../../lib/aiInline";

type PanelTab = "insights" | "node" | "ai";
const WIDE_PX = 900;

export interface PlanViewProps {
  /** The raw `QUERY PLAN` cell: array of one object holding `Plan` (+ timings after ANALYZE). */
  plan: unknown;
  sql?: string;
  profileId?: string | null;
  database?: string | null;
}

export function PlanView({ plan, sql, profileId = null, database = null }: PlanViewProps) {
  const parsed = useMemo(() => parsePlan(plan), [plan]);
  if (!parsed) {
    return (
      <EmptyState title="This isn't a JSON query plan">
        Run <code className="font-mono">EXPLAIN (FORMAT JSON)</code>, or <code className="font-mono">EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)</code> for real timings.
      </EmptyState>
    );
  }
  return <PlanViewInner key={parsed.root.id + String(parsed.totalCost) + String(parsed.executionTime)} parsed={parsed} rawPlan={plan} sql={sql} profileId={profileId} database={database} />;
}

function PlanViewInner({ parsed, rawPlan, sql, profileId, database }: { parsed: ParsedPlan; rawPlan: unknown; sql?: string; profileId: string | null; database: string | null }) {
  const aiEnabled = useUIStore(selectAiEnabled);
  const profile = useProfile(profileId);
  const insights = useMemo(() => buildInsights(parsed), [parsed]);

  const defaultCollapsed = useMemo(() => initialCollapsed(parsed), [parsed]);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(defaultCollapsed);
  const [expandedConds, setExpandedConds] = useState<ReadonlySet<string>>(() => new Set());
  // Land on the problem: a node that dominates the plan starts selected (and scrolled into view).
  const [initialId] = useState(() => hotSpots(parsed, 1, 0.25)[0]?.id ?? parsed.root.id);
  const [selectedId, setSelectedId] = useState<string | null>(initialId);
  const [tab, setTab] = useState<PanelTab>(insights.length > 0 ? "insights" : "node");
  const [scrollNonce, setScrollNonce] = useState(initialId === parsed.root.id ? 0 : 1);

  // Wide: tree beside the side panel. Narrow (agent column open, small window): side panel below.
  const rootRef = useRef<HTMLDivElement>(null);
  const [wide, setWide] = useState(true);
  useLayoutEffect(() => {
    const el = rootRef.current;
    if (!el) return;
    const measure = () => setWide(el.clientWidth >= WIDE_PX);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // What the assistant may see. `none` sends nothing at all; `schemaOnly` (and approval-gated prod)
  // sends the plan and SQL with every constant replaced by `?`, since Postgres prints the query's
  // literal values inside Filter / Index Cond / ... and those can be row data.
  const access = profile?.aiDataAccess ?? null;
  const aiOff = access === "none";
  const redacted = explainNeedsRedaction(access, profile?.environment);
  const promptInput = useMemo(() => {
    if (!redacted) return { plan: parsed, insights, sql };
    const clean = redactPlanLiterals(rawPlan);
    const plan = clean ? parsePlan(clean) : null;
    // Fail closed: if the plan cannot be redacted and re-read, nothing is sent.
    return plan ? { plan, insights: buildInsights(plan), sql: sql ? redactSqlLiterals(sql) : sql } : null;
  }, [redacted, rawPlan, parsed, insights, sql]);
  const prompt = useMemo(() => (promptInput ? buildExplainPrompt(promptInput) : ""), [promptInput]);
  const ai = useExplainAi({ prompt, profileId, database, access, blocked: promptInput === null });

  // If AI is switched off while the AI tab is showing, fall back to the insights.
  useEffect(() => {
    if (!aiEnabled && tab === "ai") setTab("insights");
  }, [aiEnabled, tab]);

  const reveal = useCallback(
    (id: string, panel?: PanelTab) => {
      setCollapsed((c) => {
        const hidden = ancestorsOf(parsed, id).filter((a) => c.has(a.id));
        if (hidden.length === 0) return c;
        const next = new Set(c);
        hidden.forEach((a) => next.delete(a.id));
        return next;
      });
      setSelectedId(id);
      setScrollNonce((n) => n + 1);
      if (panel) setTab(panel);
    },
    [parsed],
  );

  const select = useCallback((id: string) => {
    setSelectedId(id);
    setTab((t) => (t === "ai" ? t : "node"));
  }, []);

  const toggleCollapse = useCallback((id: string) => {
    setCollapsed((c) => {
      const next = new Set(c);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const toggleConds = useCallback((id: string) => {
    setExpandedConds((c) => {
      const next = new Set(c);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const collapseAll = useCallback(() => setCollapsed(new Set(parsed.nodes.filter((n) => n.children.length > 0 && n.id !== parsed.root.id).map((n) => n.id))), [parsed]);
  const expandAll = useCallback(() => setCollapsed(new Set()), []);

  const startAi = () => {
    setTab("ai");
    void ai.run();
  };

  const selected = selectedId ? (parsed.byId.get(selectedId) ?? null) : null;
  const aiBusy = ai.state.status === "running";

  const tabs: { id: PanelTab; label: string; icon: typeof Lightbulb; badge?: number; show: boolean }[] = [
    { id: "insights", label: "Insights", icon: Lightbulb, badge: insights.length, show: true },
    { id: "node", label: "Node", icon: ListTree, show: true },
    { id: "ai", label: "AI", icon: Sparkles, show: aiEnabled },
  ];

  return (
    <div ref={rootRef} className="flex h-full min-h-[480px] flex-col bg-bg" aria-label="Query plan">
      <PlanSummary
        plan={parsed}
        selectedId={selectedId}
        onSelect={(id) => reveal(id, "node")}
        actions={
          aiEnabled ? (
            <Button variant={ai.state.status === "idle" ? "primary" : "secondary"} size="sm" onClick={() => (ai.state.status === "idle" ? startAi() : setTab("ai"))} loading={aiBusy} disabled={aiOff} title={aiOff ? AI_OFF_HINT : undefined}>
              {!aiBusy && <Sparkles className="size-3.5" />} {ai.state.status === "idle" ? "Explain with AI" : aiBusy ? "Explaining" : "AI explanation"}
            </Button>
          ) : null
        }
      />
      {!parsed.analyzed && (
        <div className="shrink-0 px-3 pt-2">
          <Notice tone="info">
            This is the planner&rsquo;s estimate. Bars show estimated cost, and misestimate checks need real row counts. Run <code className="font-mono">EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)</code> to see actual time (it executes the statement).
          </Notice>
        </div>
      )}

      <div className={cn("flex min-h-0 flex-1", wide ? "flex-row" : "flex-col")}>
        <div className={cn("min-h-0 min-w-0", wide ? "flex-1" : "h-[55%] border-b border-border")}>
          <PlanTree
            plan={parsed}
            selectedId={selectedId}
            collapsed={collapsed}
            expandedConds={expandedConds}
            onSelect={select}
            onToggleCollapse={toggleCollapse}
            onToggleConds={toggleConds}
            onCollapseAll={collapseAll}
            onExpandAll={expandAll}
            scrollNonce={scrollNonce}
          />
        </div>

        <aside className={cn("flex min-h-0 flex-col bg-panel", wide ? "w-[380px] shrink-0 border-l border-border" : "min-h-0 flex-1")} aria-label="Plan details">
          <div className="flex shrink-0 items-center gap-0.5 border-b border-border px-2 pt-1.5" role="tablist">
            {tabs
              .filter((t) => t.show)
              .map((t) => {
                const Icon = t.icon;
                const active = tab === t.id;
                return (
                  <button
                    key={t.id}
                    type="button"
                    role="tab"
                    aria-selected={active}
                    onClick={() => setTab(t.id)}
                    className={cn(
                      "-mb-px inline-flex h-8 items-center gap-1.5 border-b-2 px-2.5 text-[12.5px] outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/60",
                      active ? "border-accent font-medium text-fg" : "border-transparent text-muted hover:text-fg",
                    )}
                  >
                    <Icon className="size-3.5" />
                    {t.label}
                    {t.badge !== undefined && t.badge > 0 && <span className="rounded-full bg-warn/20 px-1.5 text-[10.5px] font-semibold leading-4 tabular-nums text-warn">{t.badge}</span>}
                    {t.id === "ai" && aiBusy && <span className="size-1.5 animate-pulse rounded-full bg-accent" />}
                  </button>
                );
              })}
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto" role="tabpanel">
            {tab === "insights" && <InsightsList plan={parsed} insights={insights} selectedId={selectedId} onSelectNode={(id) => reveal(id, "node")} />}
            {tab === "node" &&
              (selected ? (
                <NodeDetails plan={parsed} node={selected} insights={insights} onSelectInsight={(i) => { reveal(i.nodeId); setTab("insights"); }} />
              ) : (
                <EmptyState title="Select a node">Click a row in the tree to see all of its properties.</EmptyState>
              ))}
            {tab === "ai" && aiEnabled && <ExplainAiPanel state={ai.state} onRun={() => void ai.run()} onCancel={ai.cancel} access={access} redacted={redacted} hasSql={!!sql?.trim()} />}
          </div>
        </aside>
      </div>
    </div>
  );
}

/** Big plans open with the quiet subtrees folded away; the expensive path stays open. */
export function initialCollapsed(plan: ParsedPlan, limit = 60): ReadonlySet<string> {
  if (plan.nodes.length <= limit) return new Set();
  const collapsed = new Set<string>();
  // Keep nodes open when they or something below them takes at least 5% of the total.
  const weight = (n: ParsedPlan["root"]): number => n.share + n.children.reduce((a, c) => a + weight(c), 0);
  for (const n of plan.nodes) if (n.children.length > 0 && n.depth >= 2 && weight(n) < 0.05) collapsed.add(n.id);
  return collapsed;
}
