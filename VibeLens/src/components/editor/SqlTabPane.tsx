// A SQL worksheet: toolbar, Monaco editor over the results grid, prod write-guard and save flow.
import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Group, Panel, Separator, useDefaultLayout } from "react-resizable-panels";
import { AlertCircle, AlignLeft, ChevronDown, Gauge, ListVideo, Lock, LockOpen, Play, Save, Sparkles, Square, X, Zap } from "lucide-react";
import { api, asError } from "../../ipc/commands";
import type { ErrorPayload } from "../../ipc/types";
import { selectAiEnabled, useUIStore } from "../../store/useUIStore";
import { useWorkspaceStore, type WorkspaceTab } from "../../store/useWorkspaceStore";
import { useConnect, useConnectionStatus } from "../../hooks/useConnectionStatus";
import { useProfile } from "../../hooks/useProfiles";
import { useQueryRunner } from "../../hooks/useQueryRunner";
import { WS_FILES_KEY } from "../../hooks/useWorkspaceFiles";
import { Button, ConfirmDialog, IconButton } from "../ui";
import { cn } from "../../lib/cn";
import { ResultsPanel } from "../grid/ResultsPanel";
import { MOD } from "../ui/platform";
import { layoutStorage, panelStyle } from "../layout/storage";
import { consumeAutoRun, consumeAutoRunReadOnly } from "./autoRun";
import { redactDbError } from "../../lib/sqlIntel/redact";
import { dialectOf, isIcebergDialect } from "../../lib/sqlIntel/dialect";
import { ICEBERG_NO_ANALYZE, MYSQL_NO_EXPLAIN, REDSHIFT_NO_EXPLAIN, planExplain, startsWithExplain } from "./explainGuard";
import { AI_OFF_HINT } from "../../lib/aiInline";
import { SaveAsDialog } from "./SaveAsDialog";
import { SqlEditor, runnableAtCursor, type CodeEditor, type SqlEditorControls, type SqlEditorHandlers, type TrackedRange } from "./SqlEditor";
import { splitStatements } from "./sqlSplit";
import { TargetPicker } from "./TargetPicker";
import { requestedDatabaseError } from "../connections/redshiftForm";
import { DropdownMenu, type DropdownItem } from "./ToolbarMenu";

const stripSemicolon = (s: string) => s.trim().replace(/;+\s*$/, "");

type RunMode = "run" | "plan" | "analyze";
type ExplainGate = { state: "checking" } | { state: "ok" } | { state: "blocked"; reason: string };

interface ExecOpts {
  confirmed?: boolean;
  /** Forces a READ ONLY transaction for this run regardless of the toolbar toggle. */
  readOnly?: boolean;
  /** An Iceberg EXPLAIN: the result is a text plan, not Postgres plan JSON. */
  textPlan?: boolean;
}

export function SqlTabPane({ tab, active }: { tab: WorkspaceTab; active: boolean }) {
  const qc = useQueryClient();
  const profile = useProfile(tab.profileId);
  const status = useConnectionStatus(tab.profileId);
  const connect = useConnect();
  const runner = useQueryRunner();
  const updateTab = useWorkspaceStore((s) => s.updateTab);
  const saveTab = useWorkspaceStore((s) => s.saveTab);
  const aiEnabled = useUIStore(selectAiEnabled);

  const [readOnly, setReadOnly] = useState(false);
  const [localError, setLocalError] = useState<ErrorPayload | null>(null);
  const [pending, setPending] = useState<{ sql: string; message: string; readOnly?: boolean } | null>(null);
  const [saveOpen, setSaveOpen] = useState(false);
  const [notice, setNotice] = useState<{ text: string; tone: "info" | "error" } | null>(null);
  const [menu, setMenu] = useState<{ kind: "run" | "explain"; anchor: HTMLElement } | null>(null);
  const [gate, setGate] = useState<ExplainGate>({ state: "checking" });
  const [fixDismissedRun, setFixDismissedRun] = useState(-1);
  /** The SQL that produced the current results (what PlanView and exports refer to). */
  const [ranSql, setRanSql] = useState<string | null>(null);
  /** The current results are an Iceberg text plan (EXPLAIN through DataFusion or Athena). */
  const [ranTextPlan, setRanTextPlan] = useState(false);
  const editorRef = useRef<CodeEditor | null>(null);
  const controlsRef = useRef<SqlEditorControls | null>(null);
  const lastRun = useRef<{ sql: string; tracked: TrackedRange | null } | null>(null);
  const handlers = useRef<SqlEditorHandlers>({ runCurrent: () => undefined, runScript: () => undefined, runReadOnly: () => undefined, format: () => undefined, save: () => undefined });

  const layout = useDefaultLayout({ id: "vg-sql-split", panelIds: ["editor", "results"], storage: layoutStorage });
  // Iceberg connections have a catalog and namespaces, not databases: the database picker is replaced
  // by the default namespace, and no database is ever sent with a run.
  const dialect = dialectOf(profile);
  const isIce = isIcebergDialect(dialect);
  const isRedshift = dialect === "redshift";
  // Iceberg engines and Redshift (Data API) have no READ ONLY transaction: read-only means the statement
  // is checked and refused before it is sent.
  const refusesWrites = isIce || isRedshift;
  const isMysql = dialect === "mysql";
  // EXPLAIN view and Explain Analyze are Postgres (JSON plan) and Iceberg (text plan) features. Not offered
  // on Redshift or MySQL (whose plans are neither); EXPLAIN <statement> still runs as an ordinary query.
  const canExplain = !isRedshift && !isMysql;
  const defaultNamespace = isIce ? (profile?.iceberg?.defaultNamespace ?? null) : null;
  const database = isIce ? null : isRedshift ? (tab.database ?? profile?.redshift?.database ?? null) : (tab.database ?? profile?.database ?? null);

  const executeNow = useCallback(
    async (sql: string, opts: ExecOpts = {}) => {
      setLocalError(null);
      if (!sql.trim()) {
        setLocalError({ code: "empty", message: "There is nothing to run. Type a statement or select some SQL." });
        return;
      }
      if (!profile) {
        setLocalError({ code: "no_connection", message: "Choose a connection for this tab before running." });
        return;
      }
      const dbProblem = profile.engine === "redshift" ? requestedDatabaseError(tab.database) : null;
      if (dbProblem) {
        setLocalError({ code: "invalid", message: dbProblem });
        return;
      }
      if (status?.state !== "connected") {
        try {
          await connect.mutateAsync(profile.id);
        } catch (e) {
          setLocalError(asError(e));
          return;
        }
      }
      const ro = opts.readOnly ?? readOnly;
      setRanSql(sql);
      setRanTextPlan(opts.textPlan === true);
      const err = await runner.run({
        profileId: profile.id,
        engine: profile.engine,
        profileName: profile.name,
        environment: profile.environment,
        database: isIce ? null : tab.database,
        sql,
        options: { readOnly: ro },
        confirmed: opts.confirmed,
      });
      if (err?.code === "confirm_required") setPending({ sql, message: err.message, readOnly: opts.readOnly });
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [profile, status?.state, tab.database, isIce, readOnly, runner.run, connect.mutateAsync],
  );

  // One run at a time: a repeated Cmd+Enter must not re-send a write or orphan the running query.
  const runningRef = useRef(false);
  runningRef.current = runner.running;
  const starting = useRef(false);
  const execute = useCallback(
    async (sql: string, opts: ExecOpts = {}) => {
      if (starting.current || runningRef.current) return;
      starting.current = true;
      try {
        await executeNow(sql, opts);
      } finally {
        starting.current = false;
      }
    },
    [executeNow],
  );

  /** Remember where the SQL we are about to run sits in the buffer, so "Fix with AI" can find it again. */
  const track = useCallback((sql: string, start: number, end: number) => {
    lastRun.current?.tracked?.clear();
    lastRun.current = { sql, tracked: controlsRef.current?.trackRange(start, end) ?? null };
  }, []);

  const runCurrent = useCallback(
    (mode: RunMode = "run", readOnlyOnce = false) => {
      const ed = editorRef.current;
      const r = ed ? runnableAtCursor(ed, dialect) : null;
      if (!r) return void setLocalError({ code: "empty", message: "There is nothing to run at the cursor." });
      let sql = r.sql;
      let ro = readOnlyOnce;
      let textPlan = false;
      if (mode !== "run") {
        if (!canExplain) return void setNotice({ tone: "info", text: isMysql ? MYSQL_NO_EXPLAIN : REDSHIFT_NO_EXPLAIN });
        // Explain and Explain Analyze never get write access: EXPLAIN ANALYZE really executes the
        // statement, and a statement that already starts with EXPLAIN is refused rather than run as is.
        // On Iceberg connections the engine answers a plain `EXPLAIN <sql>` with a text plan.
        const plan = planExplain(r.sql, mode, dialect);
        if (!plan.ok) return void setNotice({ tone: "info", text: plan.message });
        sql = plan.sql;
        ro = plan.readOnly;
        textPlan = plan.text === true;
      }
      track(r.sql, r.start, r.end);
      void execute(sql, { readOnly: ro || undefined, textPlan: textPlan || undefined });
    },
    [execute, track, dialect, canExplain, isMysql],
  );

  const runScript = useCallback(
    (readOnlyOnce = false) => {
      const sql = editorRef.current?.getModel()?.getValue() ?? tab.contents;
      track(sql, 0, sql.length);
      void execute(sql, { readOnly: readOnlyOnce || undefined });
    },
    [execute, tab.contents, track],
  );

  const save = useCallback(async () => {
    if (tab.path) {
      try {
        await saveTab(tab.id);
        void qc.invalidateQueries({ queryKey: WS_FILES_KEY });
      } catch (e) {
        setLocalError(asError(e));
      }
    } else setSaveOpen(true);
  }, [tab.path, tab.id, saveTab, qc]);

  const format = useCallback(() => {
    const out = controlsRef.current?.format();
    if (!out) return;
    if (!out.ok) setNotice({ tone: "error", text: `Could not format: ${out.message}` });
    else if (!out.changed) setNotice({ tone: "info", text: out.scope === "selection" ? "Selection is already formatted" : "Already formatted" });
    editorRef.current?.focus();
  }, []);

  handlers.current = { runCurrent: () => runCurrent("run"), runScript: () => runScript(), runReadOnly: () => runCurrent("run", true), format, save: () => void save() };

  // Open-table double-click: run once when the pane first has a connection to run against.
  useEffect(() => {
    if (profile && consumeAutoRun(tab.id)) void execute(tab.contents, { readOnly: consumeAutoRunReadOnly(tab.id) || undefined });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [profile]);

  useEffect(() => {
    if (active) editorRef.current?.focus();
  }, [active]);

  useEffect(() => {
    if (!notice) return;
    const t = window.setTimeout(() => setNotice(null), notice.tone === "error" ? 6000 : 3200);
    return () => window.clearTimeout(t);
  }, [notice]);

  useEffect(() => () => lastRun.current?.tracked?.clear(), []);

  // -- Explain Analyze gate: ask the classifier when the menu opens, never per keystroke ------------
  const openExplainMenu = (anchor: HTMLElement) => {
    const ed = editorRef.current;
    const r = ed ? runnableAtCursor(ed, dialect) : null;
    setMenu({ kind: "explain", anchor });
    // The Iceberg engines have no read-only ANALYZE that this app can vouch for: Explain only.
    if (isIce) return setGate({ state: "blocked", reason: ICEBERG_NO_ANALYZE });
    if (!r) return setGate({ state: "blocked", reason: "There is no statement at the cursor." });
    if (startsWithExplain(r.sql)) return setGate({ state: "blocked", reason: "This statement already starts with EXPLAIN. Remove it to use Explain Analyze." });
    setGate({ state: "checking" });
    api
      .sqlClassify(stripSemicolon(r.sql))
      .then((c) => {
        if (c.parseOk && c.statements.length === 1 && c.overall === "readOnly") setGate({ state: "ok" });
        else
          setGate({
            state: "blocked",
            reason: c.parseOk && c.statements.length === 1 ? `Disabled: ANALYZE really runs the statement, and this one can change data or session state (${c.summary}).` : "Disabled: ANALYZE really runs the statement, and this SQL could not be confirmed as a single read-only query.",
          });
      })
      .catch(() => setGate({ state: "blocked", reason: "Disabled: could not check that this statement is read-only." }));
  };

  const menuItems: DropdownItem[] =
    menu?.kind === "run"
      ? [
          { id: "run", label: "Run selection or statement", hint: `${MOD}↵`, icon: <Play className="size-3.5" />, onSelect: () => runCurrent("run") },
          {
            id: "ro",
            label: refusesWrites ? "Run read-only" : "Run in read-only transaction",
            hint: `${MOD}⌥↵`,
            icon: <Lock className="size-3.5" />,
            detail: refusesWrites
              ? "Refuses statements that would change data or tables."
              : isMysql
                ? "Checks each statement is a plain read before sending it, and wraps the run in START TRANSACTION READ ONLY."
                : "Wraps this run in BEGIN READ ONLY. Writes fail instead of changing data.",
            onSelect: () => runCurrent("run", true),
          },
          { id: "script", label: "Run whole script", hint: `${MOD}⇧↵`, icon: <ListVideo className="size-3.5" />, onSelect: () => runScript() },
          { id: "script-ro", label: refusesWrites ? "Run script read-only" : "Run script in read-only transaction", icon: <Lock className="size-3.5" />, onSelect: () => runScript(true) },
        ]
      : menu?.kind === "explain"
        ? [
            { id: "plan", label: "Explain", icon: <Zap className="size-3.5" />, detail: isIce ? "Shows the engine's estimated plan as text without running the statement." : "Shows the estimated plan without running the statement.", onSelect: () => runCurrent("plan") },
            {
              id: "analyze",
              label: "Explain Analyze",
              icon: <Gauge className="size-3.5" />,
              disabled: gate.state !== "ok",
              detail: gate.state === "ok" ? "Runs the statement in a read-only transaction and reports real timings and buffers." : gate.state === "checking" ? "Checking that the statement only reads…" : gate.reason,
              onSelect: () => runCurrent("analyze"),
            },
          ]
        : [];

  // -- Fix with AI -------------------------------------------------------------------------------
  const failed = !runner.running ? runner.results.find((r) => r.status === "error") : undefined;
  const failure = failed?.error ?? (!runner.running ? runner.error : null);
  // With AI access off the assistant learns nothing about this connection: no button, no tooltip.
  const aiAccess = profile?.aiDataAccess ?? null;
  const aiOff = aiAccess === "none";
  const canFix = aiEnabled && !aiOff && !!failure && failure.code === "database" && lastRun.current !== null && fixDismissedRun !== runner.runId;

  const fixWithAi = () => {
    const ed = editorRef.current;
    const model = ed?.getModel();
    const run = lastRun.current;
    const tr = run?.tracked?.getRange();
    if (!ed || !model || !run || !tr || !failure) return;
    let range = tr;
    // A script: narrow to the statement the server rejected when our splitter agrees on the count.
    if (failed && model.getValueInRange(tr) === run.sql) {
      const stmts = splitStatements(run.sql, { mysql: isMysql });
      const stmt = stmts.length === runner.statementCount ? stmts[failed.index] : undefined;
      if (stmt) {
        const base = model.getOffsetAt({ lineNumber: tr.startLineNumber, column: tr.startColumn });
        const a = model.getPositionAt(base + stmt.start);
        const b = model.getPositionAt(base + stmt.end);
        range = { startLineNumber: a.lineNumber, startColumn: a.column, endLineNumber: b.lineNumber, endColumn: b.column };
      }
    }
    setFixDismissedRun(runner.runId);
    // DETAIL / WHERE lines and quoted values carry row data: only a redacted error leaves under schema-only.
    controlsRef.current?.fixWithAi(range, redactDbError(failure.message, aiAccess, profile?.environment));
  };

  const isProd = profile?.environment === "prod";
  const running = runner.running;
  const split = "inline-flex h-7 items-center justify-center text-xs font-medium outline-none focus-visible:ring-2 focus-visible:ring-accent/60 disabled:pointer-events-none disabled:opacity-50";

  return (
    <div className="relative flex h-full min-h-0 flex-col">
      {isProd && <div className="h-0.5 shrink-0 bg-env-prod" aria-hidden />}
      <div className="@container flex h-10 shrink-0 items-center gap-2 overflow-hidden border-b border-border bg-panel px-2 [&>button]:shrink-0">
        <TargetPicker tab={tab} />
        <div className="mx-1 h-5 w-px bg-border" aria-hidden />
        {running ? (
          <Button size="sm" variant="danger" onClick={runner.cancel} aria-label="Cancel running query">
            <Square className="size-3 fill-current" /> Cancel
          </Button>
        ) : (
          <div className="inline-flex shrink-0">
            <button
              type="button"
              onClick={() => runCurrent("run")}
              title={`Run selection or statement (${MOD}+Enter)`}
              className={cn(split, "gap-1.5 rounded-l-md bg-accent px-2.5 text-accent-fg hover:brightness-110")}
            >
              <Play className="size-3 fill-current" /> Run
              <span className="ml-1 hidden text-[11px] font-normal opacity-70 @[900px]:inline">{MOD}↵</span>
            </button>
            <button
              type="button"
              aria-label="More ways to run"
              aria-haspopup="menu"
              aria-expanded={menu?.kind === "run"}
              onClick={(e) => setMenu(menu?.kind === "run" ? null : { kind: "run", anchor: e.currentTarget })}
              title="Run in read-only transaction, run script…"
              className={cn(split, "w-6 rounded-r-md border-l border-accent-fg/25 bg-accent text-accent-fg hover:brightness-110")}
            >
              <ChevronDown className="size-3.5" />
            </button>
          </div>
        )}
        {canExplain && (
        <div className="inline-flex shrink-0">
          <button
            type="button"
            disabled={running}
            onClick={() => runCurrent("plan")}
            title="Explain: show the estimated execution plan (does not run the statement)"
            aria-label="Explain"
            className={cn(split, "gap-1.5 rounded-l-md border border-border bg-panel-2 px-2.5 text-fg hover:border-muted/60")}
          >
            <Zap className="size-3.5" /> <span className="hidden @[760px]:inline">Explain</span>
          </button>
          <button
            type="button"
            disabled={running}
            aria-label="Explain options"
            aria-haspopup="menu"
            aria-expanded={menu?.kind === "explain"}
            onClick={(e) => (menu?.kind === "explain" ? setMenu(null) : openExplainMenu(e.currentTarget))}
            title="Explain Analyze…"
            className={cn(split, "w-6 rounded-r-md border border-l-0 border-border bg-panel-2 text-muted hover:text-fg")}
          >
            <ChevronDown className="size-3.5" />
          </button>
        </div>
        )}
        <Button size="sm" variant="ghost" disabled={running} onClick={() => runScript()} title={`Run the whole script (${MOD}+Shift+Enter)`} aria-label="Run script">
          <ListVideo className="size-3.5" /> <span className="hidden @[1040px]:inline">Run script</span>
        </Button>
        <button
          type="button"
          role="switch"
          aria-checked={readOnly}
          aria-label="Read-only mode"
          onClick={() => setReadOnly((v) => !v)}
          title={
            refusesWrites
              ? readOnly
                ? "Read-only: statements that change data or tables are refused"
                : "Writes allowed. Click to refuse statements that change data or tables"
              : readOnly
                ? "Read-only: statements run inside a READ ONLY transaction"
                : "Writes allowed. Click to run inside a READ ONLY transaction"
          }
          className={cn(
            "inline-flex h-7 items-center gap-1.5 rounded-md border px-2 text-xs font-medium",
            readOnly ? "border-accent/40 bg-accent/10 text-accent" : "border-border bg-panel-2 text-muted hover:text-fg",
          )}
        >
          {readOnly ? <Lock className="size-3.5" /> : <LockOpen className="size-3.5" />}
          <span className="hidden @[1040px]:inline">Read-only</span>
        </button>
        <div className="ml-auto flex items-center gap-1">
          {aiEnabled && aiOff && (
            <span className="inline-flex h-7 items-center gap-1.5 px-2 text-xs text-muted" title={AI_OFF_HINT} aria-label={AI_OFF_HINT}>
              <Sparkles className="size-3.5 opacity-50" aria-hidden /> <span className="hidden @[760px]:inline">AI off</span>
            </span>
          )}
          {aiEnabled && !aiOff && (
            <Button size="sm" variant="ghost" onClick={() => controlsRef.current?.askAi()} title={`Ask AI to write or change SQL (${MOD}+K)`} aria-label="Ask AI">
              <Sparkles className="size-3.5 text-accent" /> <span className="hidden @[760px]:inline">Ask AI</span>
              <span className="hidden rounded border border-border bg-panel-2 px-1 font-mono text-[10px] text-muted @[900px]:inline">{MOD}K</span>
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={format} title={`Format SQL (${MOD === "⌘" ? "⇧⌥F" : "Shift+Alt+F"}). Formats the selection if there is one.`} aria-label="Format SQL">
            <AlignLeft className="size-3.5" /> <span className="hidden @[1040px]:inline">Format</span>
          </Button>
          <Button size="sm" variant="ghost" onClick={() => void save()} title={`Save (${MOD}+S)`} aria-label="Save worksheet">
            <Save className="size-3.5" /> <span className="hidden @[1040px]:inline">{tab.path ? "Save" : "Save as…"}</span>
          </Button>
        </div>
      </div>
      {menu && <DropdownMenu anchor={menu.anchor} items={menuItems} onClose={() => setMenu(null)} width={menu.kind === "run" ? 300 : 320} />}

      <div className="min-h-0 flex-1">
        <Group orientation="vertical" defaultLayout={layout.defaultLayout} onLayoutChanged={layout.onLayoutChanged} className="h-full">
          <Panel style={panelStyle} id="editor" minSize={80} defaultSize="45%">
            <div className="relative flex h-full flex-col bg-bg">
              <div className="min-h-0 flex-1">
                <SqlEditor
                  value={tab.contents}
                  onChange={(contents) => updateTab(tab.id, { contents, dirty: true })}
                  onSelectionChange={(selectedText) => updateTab(tab.id, { selectedText })}
                  handlers={handlers}
                  editorRef={editorRef}
                  controlsRef={controlsRef}
                  target={{ profileId: tab.profileId, database, dialect, defaultSchema: defaultNamespace }}
                  aiDataAccess={aiAccess}
                  onAiBlocked={() => setNotice({ tone: "info", text: `${AI_OFF_HINT}. Change it in the connection's settings.` })}
                />
              </div>
              {canFix && failure && (
                <div className="flex h-9 shrink-0 items-center gap-2 border-t border-danger/30 bg-danger/10 px-3 text-xs" role="status">
                  <AlertCircle className="size-3.5 shrink-0 text-danger" aria-hidden />
                  <span className="min-w-0 flex-1 truncate text-danger" title={failure.message}>
                    {failure.message}
                  </span>
                  <Button size="sm" variant="secondary" onClick={fixWithAi} title="Ask the AI for a corrected statement. You review it before anything changes.">
                    <Sparkles className="size-3.5 text-accent" /> Fix with AI
                  </Button>
                  <IconButton label="Dismiss" className="size-6 shrink-0" onClick={() => setFixDismissedRun(runner.runId)}>
                    <X className="size-3.5" aria-hidden />
                  </IconButton>
                </div>
              )}
              {notice && (
                <div
                  role="status"
                  className={cn(
                    "pointer-events-none absolute bottom-3 left-1/2 z-20 -translate-x-1/2 rounded-md border px-3 py-1.5 text-xs shadow-lg",
                    notice.tone === "error" ? "border-danger/40 bg-panel text-danger" : "border-border bg-panel text-muted",
                  )}
                >
                  {notice.text}
                </div>
              )}
            </div>
          </Panel>
          <Separator className="vg-sep" />
          <Panel style={panelStyle} id="results" minSize={80} defaultSize="55%">
            <div className="h-full bg-bg">
              <ResultsPanel runner={runner} localError={localError} context={{ sql: ranSql, active, profileId: tab.profileId, database, name: profile?.name, textPlan: ranTextPlan, dialect }} />
            </div>
          </Panel>
        </Group>
      </div>

      <ConfirmDialog
        open={pending !== null}
        title={isProd ? "Confirm change on a production database" : "Confirm statement"}
        danger
        confirmText="Run anyway"
        requireTyping={isProd && profile ? profile.name : undefined}
        onCancel={() => setPending(null)}
        onConfirm={() => {
          const p = pending;
          setPending(null);
          if (p) void execute(p.sql, { confirmed: true, readOnly: p.readOnly });
        }}
        body={
          <>
            <p>{pending?.message}</p>
            <pre className="selectable max-h-40 overflow-auto rounded-md border border-border bg-bg p-2 font-mono text-[12px]">{pending?.sql.trim().slice(0, 2000)}</pre>
          </>
        }
      />
      <SaveAsDialog
        open={saveOpen}
        initialName={/\.[A-Za-z0-9]+$/.test(tab.title) ? tab.title : `${tab.title}.sql`}
        onClose={() => setSaveOpen(false)}
        onSave={async (path) => {
          await saveTab(tab.id, path);
          void qc.invalidateQueries({ queryKey: WS_FILES_KEY });
        }}
      />
    </div>
  );
}
