// Result tabs for a query run: one per statement, plus run-level errors and empty/running states.
import { useEffect, useMemo, useRef, useState } from "react";
import { AlertCircle, Check, ChevronLeft, ChevronRight, Loader2, Play } from "lucide-react";
import { cn } from "../../lib/cn";
import type { ErrorPayload } from "../../ipc/types";
import type { QueryRunner } from "../../hooks/useQueryRunner";
import { EmptyState, ErrorBox } from "../ui";
import { Kbd } from "../ui/wsKit";
import { MOD } from "../ui/platform";
import { ResultView, type ResultContext } from "./ResultView";
import { formatCount, formatElapsed } from "./cellFormat";
import { publishLastResult } from "../agent/lastResult";
import { isExplainJson } from "../explain";
import { collapseTabs, nextToPublish, sqlForPublish } from "./publishResult";


function useRunClock(running: boolean, runId: number): number {
  const [ms, setMs] = useState(0);
  useEffect(() => {
    if (!running) return;
    const t0 = performance.now();
    setMs(0);
    const id = window.setInterval(() => setMs(performance.now() - t0), 100);
    return () => window.clearInterval(id);
  }, [running, runId]);
  return ms;
}

export function ResultsPanel({ runner, localError, context }: { runner: QueryRunner; localError?: ErrorPayload | null; context?: ResultContext }) {
  const { results, running, error, runId, version, totalElapsedMs, getRows } = runner;
  const [active, setActive] = useState(0);
  const userPicked = useRef(false);
  const clock = useRunClock(running, runId);

  useEffect(() => {
    userPicked.current = false;
    setActive(0);
  }, [runId]);

  // When a script finishes, land on the last result set that has rows (that is usually the one you want).
  useEffect(() => {
    if (running || userPicked.current || results.length < 2) return;
    let pick = results.length - 1;
    for (let i = results.length - 1; i >= 0; i--) {
      if (results[i].columns.length > 0) {
        pick = i;
        break;
      }
    }
    setActive(pick);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [running]);

  // Offer finished results to the agent as an optional "last result" attachment (it only goes out if
  // the user attaches it and the connection's AI access allows rows). Statements finish in order, so a
  // moving pointer publishes each one exactly once, on its transition to done: switching result tabs
  // never republishes, and a worksheet that is not the visible tab never publishes.
  const published = useRef({ runId: -1, next: 0 });
  useEffect(() => {
    if (published.current.runId !== runId) published.current = { runId, next: 0 };
    const { items, next } = nextToPublish(results, published.current.next);
    published.current.next = next;
    if (context?.active === false) return;
    for (const m of items) {
      if (m.status !== "done" || m.columns.length === 0) continue;
      const rows = getRows(m.index);
      // Plans (Postgres JSON, or an Iceberg text EXPLAIN) are not result data to hand to the agent.
      if (context?.textPlan || isExplainJson(m.columns, rows)) continue;
      publishLastResult({ columns: m.columns, rows, rowCount: m.rowCount, sql: sqlForPublish(m.sql, context?.sql), profileId: context?.profileId, database: context?.database });
    }
    // `results` is updated in place: `version` signals change
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [version, runId, running]);

  // A failing statement should be visible immediately.
  const firstErr = results.findIndex((r) => r.status === "error");
  useEffect(() => {
    if (firstErr >= 0 && !userPicked.current) setActive(firstErr);
  }, [firstErr]);

  // Dismissal covers this run's error only: a new run (new runId) or a new error object shows again.
  const [dismissed, setDismissed] = useState<{ runId: number; error: ErrorPayload } | null>(null);
  const rawError = localError ?? error;
  const shownError = rawError && dismissed?.runId === runId && dismissed.error === rawError ? null : rawError;
  const dismissError = rawError ? () => setDismissed({ runId, error: rawError }) : undefined;
  const collapsed = collapseTabs(results.length);
  const current = results[Math.min(active, results.length - 1)];

  if (results.length === 0) {
    if (shownError) return <div className="h-full overflow-auto p-3"><ErrorBox error={shownError} onDismiss={dismissError} /></div>;
    if (running) {
      return (
        <div className="flex h-full items-center justify-center gap-2 text-muted">
          <Loader2 className="size-4 animate-spin" /> Running <span className="tabular-nums">{formatElapsed(clock)}</span>
        </div>
      );
    }
    return (
      <EmptyState icon={<Play className="size-5" />} title="Run a query to see results">
        <dl className="mt-1 grid grid-cols-[auto_auto] items-center justify-center gap-x-3 gap-y-1.5 text-left">
          <dt className="flex gap-1">
            <Kbd>{MOD}</Kbd>
            <Kbd>↵</Kbd>
          </dt>
          <dd>Run the selection or the statement at the cursor</dd>
          <dt className="flex gap-1">
            <Kbd>{MOD}</Kbd>
            <Kbd>⇧</Kbd>
            <Kbd>↵</Kbd>
          </dt>
          <dd>Run the whole script</dd>
        </dl>
      </EmptyState>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      {shownError && (
        <div className="shrink-0 border-b border-border p-2">
          <ErrorBox error={shownError} onDismiss={dismissError} />
        </div>
      )}
      {collapsed && (
        <StatementPicker
          results={results}
          active={Math.min(active, results.length - 1)}
          running={running}
          clock={clock}
          onPick={(i) => {
            userPicked.current = true;
            setActive(i);
          }}
        />
      )}
      {!collapsed && (results.length > 1 || running) && (
        <div role="tablist" aria-label="Result sets" className="flex h-8 shrink-0 items-stretch overflow-x-auto border-b border-border bg-panel">
          {results.map((r, i) => {
            const label = r.columns.length > 0 ? `${formatCount(r.rowCount)} ${r.rowCount === 1 ? "row" : "rows"}` : (r.commandTag ?? "Done");
            return (
              <button
                key={r.index}
                role="tab"
                aria-selected={i === active}
                onClick={() => {
                  userPicked.current = true;
                  setActive(i);
                }}
                className={cn(
                  "flex shrink-0 items-center gap-1.5 border-r border-border px-3 text-xs outline-none",
                  i === active ? "bg-bg text-fg shadow-[inset_0_-2px_0_var(--vg-accent)]" : "text-muted hover:text-fg",
                )}
              >
                {r.status === "running" ? <Loader2 className="size-3 animate-spin text-accent" /> : r.status === "error" ? <AlertCircle className="size-3 text-danger" /> : <Check className="size-3 text-ok" />}
                <span className="font-medium">Result {i + 1}</span>
                <span className="tabular-nums text-muted">{label}</span>
              </button>
            );
          })}
          {running && (
            <span className="ml-auto flex shrink-0 items-center gap-1.5 px-3 text-xs tabular-nums text-muted">
              <Loader2 className="size-3 animate-spin" /> {formatElapsed(clock)}
            </span>
          )}
        </div>
      )}
      <div className="min-h-0 flex-1">
        {current && (
          <ResultView
            key={`${runId}:${current.index}`}
            meta={current}
            rows={getRows(current.index)}
            version={current.status === "running" ? version : -1}
            totalElapsedMs={totalElapsedMs}
            context={context}
          />
        )}
      </div>
    </div>
  );
}

const statusMark = (r: QueryRunner["results"][number]) => (r.status === "running" ? "…" : r.status === "error" ? "✗" : "✓");
const resultLabel = (r: QueryRunner["results"][number]) => (r.columns.length > 0 ? `${formatCount(r.rowCount)} ${r.rowCount === 1 ? "row" : "rows"}` : (r.commandTag ?? "Done"));

/**
 * One control for a long script: "Statement 12 of 3,000", previous / next, a jump to the first failure
 * and a picker listing every statement. The list is only built once the run has finished, because
 * rebuilding thousands of options on every streamed frame would make the run itself slow.
 */
function StatementPicker({ results, active, running, clock, onPick }: { results: QueryRunner["results"]; active: number; running: boolean; clock: number; onPick: (i: number) => void }) {
  const total = results.length;
  const options = useMemo(
    () => (running ? null : results.map((r, i) => ({ i, text: `${i + 1}. ${statusMark(r)} ${resultLabel(r)}${r.sql ? ` · ${r.sql.replace(/\s+/g, " ").slice(0, 60)}` : ""}` }))),
    // `results` is updated in place; it only needs rebuilding when a run ends
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [running, total, results[0]],
  );
  const firstErr = running ? -1 : results.findIndex((r) => r.status === "error");
  const btn = "inline-flex size-6 items-center justify-center rounded text-muted outline-none hover:text-fg focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-40";
  return (
    <div className="flex h-8 shrink-0 items-center gap-2 border-b border-border bg-panel px-2 text-xs" aria-label="Result sets">
      <button type="button" className={btn} aria-label="Previous statement" disabled={active <= 0} onClick={() => onPick(active - 1)}>
        <ChevronLeft className="size-4" />
      </button>
      <span className="font-medium tabular-nums text-fg">
        Statement {formatCount(active + 1)} of {formatCount(total)}
      </span>
      <button type="button" className={btn} aria-label="Next statement" disabled={active >= total - 1} onClick={() => onPick(active + 1)}>
        <ChevronRight className="size-4" />
      </button>
      {options && (
        <select
          aria-label="Choose a statement"
          value={active}
          onChange={(e) => onPick(Number(e.target.value))}
          className="h-6 min-w-0 max-w-[50%] flex-1 truncate rounded border border-border bg-panel-2 px-1.5 text-xs text-fg outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        >
          {options.map((o) => (
            <option key={o.i} value={o.i}>
              {o.text}
            </option>
          ))}
        </select>
      )}
      {firstErr >= 0 && (
        <button type="button" onClick={() => onPick(firstErr)} className="inline-flex items-center gap-1 text-danger hover:underline">
          <AlertCircle className="size-3" /> First error: statement {formatCount(firstErr + 1)}
        </button>
      )}
      {running && (
        <span className="ml-auto flex shrink-0 items-center gap-1.5 tabular-nums text-muted">
          <Loader2 className="size-3 animate-spin" /> {formatElapsed(clock)}
        </span>
      )}
    </div>
  );
}
