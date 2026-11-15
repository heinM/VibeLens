// ER diagram tab (kind "erd"): every table of one schema as a card, FK edges attached to the exact
// column rows, pan/zoom, hover tracing, a details panel and SVG export. Layout maths live in
// layout.ts / model.ts (pure, tested); the pan/zoom surface is ErdCanvas.
import { useCallback, useEffect, useMemo, useRef, useState, useDeferredValue } from "react";
import { Download, Filter, Link2, KeyRound, Maximize, Minus, Plus, Search, Rows3, X } from "lucide-react";
import { asError, api } from "../../ipc/commands";
import { useSchemaMetadata } from "../../lib/schemaCache";
import { cn } from "../../lib/cn";
import type { WorkspaceTab } from "../../store/useWorkspaceStore";
import { useUIStore } from "../../store/useUIStore";
import { Button, EmptyState, ErrorBox, IconButton, Spinner } from "../ui";
import { openTableTab } from "../table/openTableTab";
import { fuzzyMatch } from "../palette/fuzzy";
import { ErdCanvas, type ErdCanvasHandle } from "./ErdCanvas";
import { ErdDetails } from "./ErdDetails";
import { estimateMeasure, layoutErd, type Measure, type TextKind } from "./layout";
import { ERD_MAX_TABLES, buildErdModel, isCollapsible, neighbourhood } from "./model";
import { renderErdSvg, type ExportPalette } from "./svgExport";

const PANEL_W = 320;

/** Canvas-backed text measurement using the app fonts; falls back to a character estimate. */
function makeMeasure(): Measure {
  try {
    const ctx = document.createElement("canvas").getContext("2d");
    if (!ctx) return estimateMeasure;
    const cs = getComputedStyle(document.documentElement);
    const sans = cs.getPropertyValue("--font-sans").trim() || "system-ui, sans-serif";
    const mono = cs.getPropertyValue("--font-mono").trim() || "ui-monospace, monospace";
    const fonts: Record<TextKind, string> = {
      title: `600 13px ${sans}`,
      name: `12px ${sans}`,
      type: `10.5px ${mono}`,
      meta: `10.5px ${mono}`,
    };
    const cache = new Map<string, number>();
    return (text, kind) => {
      const k = `${kind}\u0000${text}`;
      const hit = cache.get(k);
      if (hit !== undefined) return hit;
      ctx.font = fonts[kind];
      const w = ctx.measureText(text).width;
      cache.set(k, w);
      return w;
    };
  } catch {
    return estimateMeasure;
  }
}

function readPalette(dark: boolean): ExportPalette {
  const cs = getComputedStyle(document.documentElement);
  const v = (name: string, fb: string) => cs.getPropertyValue(name).trim() || fb;
  return {
    bg: v("--vg-bg", dark ? "#0b0f14" : "#f7f8fa"),
    panel: v("--vg-panel", dark ? "#11161d" : "#ffffff"),
    panel2: v("--vg-panel-2", dark ? "#171e27" : "#f0f2f5"),
    border: v("--vg-border", dark ? "#232c38" : "#dfe3e8"),
    fg: v("--vg-fg", dark ? "#e6edf3" : "#111827"),
    muted: v("--vg-muted", "#8b98a8"),
    accent: v("--vg-accent", "#0d9488"),
    amber: dark ? "#f59e0b" : "#b45309",
    dark,
    fontSans: v("--font-sans", "system-ui, sans-serif"),
    fontMono: v("--font-mono", "ui-monospace, monospace"),
  };
}

export function ErdTab({ tab, active }: { tab: WorkspaceTab; active: boolean }) {
  const schema = tab.objectRef?.schema ?? "public";
  const dark = useUIStore((s) => s.theme) === "dark";
  const meta = useSchemaMetadata(tab.profileId, tab.database);
  const canvas = useRef<ErdCanvasHandle>(null);
  const wrapRef = useRef<HTMLDivElement>(null);

  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [hover, setHover] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const [zoom, setZoom] = useState(100);
  const [exportNote, setExportNote] = useState<{ ok: boolean; text: string } | null>(null);
  const measure = useMemo(makeMeasure, []);

  // Layout (dagre) is expensive on big schemas: let typing stay responsive and lay out the deferred value.
  const deferredFilter = useDeferredValue(filter);
  const model = useMemo(() => buildErdModel(meta.data?.tables ?? [], schema, { filter: deferredFilter }), [meta.data, schema, deferredFilter])
  const layout = useMemo(() => layoutErd(model, expanded, measure), [model, expanded, measure]);

  const selectedTable = selected ? model.tables.find((t) => t.key === selected) : undefined;
  useEffect(() => {
    if (selected && !selectedTable) setSelected(null);
  }, [selected, selectedTable]);

  const focusKey = hover ?? selected;
  const related = useMemo(() => (focusKey ? neighbourhood(model, focusKey) : null), [model, focusKey]);
  const panelInset = selectedTable ? PANEL_W : 0;

  // Frame the whole diagram when it first appears and whenever the set of tables changes.
  const fitSig = `${schema}|${model.tables.length}|${deferredFilter}|${meta.dataUpdatedAt}`;
  const fitted = useRef("");
  useEffect(() => {
    if (layout.nodes.length === 0 || fitted.current === fitSig) return;
    // Mark as framed only when the frame actually ran (StrictMode runs effects twice and cancels the first).
    const id = requestAnimationFrame(() => {
      fitted.current = fitSig;
      canvas.current?.fit(false);
    });
    return () => cancelAnimationFrame(id);
  }, [fitSig, layout.nodes.length]);

  const focusTable = useCallback(
    (key: string) => {
      setSelected(key);
      canvas.current?.focusNode(key, PANEL_W);
    },
    [],
  );

  const toggleExpand = useCallback((key: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);

  const collapsible = useMemo(() => model.tables.filter(isCollapsible), [model.tables]);
  const allOpen = collapsible.length > 0 && collapsible.every((t) => expanded.has(t.key));

  const open = useCallback(
    (key: string) => {
      const t = model.tables.find((x) => x.key === key);
      if (t && tab.profileId) openTableTab(tab.profileId, tab.database, t.schema, t.name);
    },
    [model.tables, tab.profileId, tab.database],
  );

  const exportSvg = async () => {
    try {
      const svg = renderErdSvg(layout, expanded, readPalette(dark), measure, `${schema} schema`);
      const path = await api.exportFile(`${schema}-diagram.svg`, svg);
      if (path) setExportNote({ ok: true, text: `Saved ${path.split(/[\\/]/).pop()}` });
    } catch (e) {
      setExportNote({ ok: false, text: asError(e).message });
    }
  };
  useEffect(() => {
    if (!exportNote) return;
    const t = window.setTimeout(() => setExportNote(null), 4000);
    return () => window.clearTimeout(t);
  }, [exportNote]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.target instanceof HTMLInputElement || e.metaKey || e.ctrlKey || e.altKey) return;
    switch (e.key) {
      case "+":
      case "=":
        e.preventDefault();
        canvas.current?.zoomBy(1.25);
        break;
      case "-":
        e.preventDefault();
        canvas.current?.zoomBy(0.8);
        break;
      case "0":
      case "f":
        e.preventDefault();
        canvas.current?.fit(true, panelInset);
        break;
      case "Escape":
        setSelected(null);
        break;
    }
  };

  // ---- states ----
  if (!tab.profileId) return <EmptyState title="No connection">This diagram is not attached to a connection.</EmptyState>;
  if (meta.isLoading) {
    return (
      <div className="flex h-full items-center justify-center gap-2 text-xs text-muted">
        <Spinner /> Reading the {schema} schema…
      </div>
    );
  }
  if (meta.isError) {
    return (
      <div className="mx-auto flex h-full max-w-md flex-col items-start justify-center gap-3 p-6">
        <div className="text-sm font-medium text-fg">Couldn’t read the schema</div>
        <ErrorBox error={asError(meta.error)} />
        <Button onClick={() => void meta.refetch()}>Try again</Button>
      </div>
    );
  }
  if (model.total === 0) {
    return (
      <EmptyState title={`No tables in ${schema}`}>
        {meta.data?.tables.length ? "Pick another schema in the navigator and choose Open ER diagram." : "This database has no tables yet. Create some and refresh the navigator."}
      </EmptyState>
    );
  }

  const showFilter = model.total > ERD_MAX_TABLES || filter !== "";
  const rightInset = panelInset ? panelInset + 12 : 12;
  const relCount = model.edges.length;

  return (
    <div ref={wrapRef} tabIndex={0} onKeyDown={onKeyDown} className="relative h-full min-h-0 bg-bg outline-none" data-active={active ? "" : undefined}>
      <ErdCanvas
        ref={canvas}
        layout={layout}
        measure={measure}
        dark={dark}
        focusKey={focusKey}
        related={related}
        selected={selected}
        onSelect={setSelected}
        onHover={setHover}
        onOpen={open}
        onToggleExpand={toggleExpand}
        onZoomChange={setZoom}
      />

      {/* top-left: what this is, and find-a-table */}
      <div data-erd-ui className="absolute left-3 top-3 z-10 flex max-w-[calc(100%-24px)] flex-col items-start gap-2">
        <div className="flex items-center gap-2 rounded-lg border border-border bg-panel/95 py-1.5 pl-3 pr-1.5 shadow-lg backdrop-blur">
          <div className="min-w-0">
            <div className="truncate text-[13px] font-semibold leading-tight text-fg">{schema}</div>
            <div className="text-[11px] leading-tight text-muted">
              {layout.nodes.length} {layout.nodes.length === 1 ? "table" : "tables"}, {relCount} {relCount === 1 ? "relation" : "relations"}
            </div>
          </div>
          <div className="mx-1 h-7 w-px bg-border" aria-hidden />
          <TableFinder tables={model.tables} onPick={focusTable} />
        </div>

        {(model.capped || showFilter) && (
          <div className="flex max-w-[460px] flex-col gap-2 rounded-lg border border-warn/40 bg-panel/95 p-2.5 text-[12px] shadow-lg backdrop-blur" role="status">
            {model.capped ? (
              <p className="leading-snug text-fg">
                Showing the {model.tables.length} most connected of {model.total} tables{filter ? " that match" : ""}. Filter to see the rest.
              </p>
            ) : (
              <p className="leading-snug text-muted">
                {model.total - model.hidden} of {model.total} tables match.
              </p>
            )}
            <label className="flex h-7 items-center gap-1.5 rounded-md border border-border bg-bg px-2 focus-within:border-accent/70 focus-within:ring-1 focus-within:ring-accent/40">
              <Filter className="size-3.5 shrink-0 text-muted" aria-hidden />
              <input
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                aria-label="Filter tables by name"
                placeholder="Filter tables by name"
                spellCheck={false}
                className="min-w-0 flex-1 bg-transparent text-xs text-fg outline-none placeholder:text-muted/70"
              />
              {filter && (
                <button type="button" aria-label="Clear filter" onClick={() => setFilter("")} className="text-muted hover:text-fg">
                  <X className="size-3.5" />
                </button>
              )}
            </label>
          </div>
        )}
      </div>

      {/* top-right: actions */}
      <div data-erd-ui className="absolute top-3 z-10 flex items-center gap-1.5" style={{ right: rightInset }}>
        {exportNote && (
          <span role="status" className={cn("rounded-md border px-2 py-1 text-[11px]", exportNote.ok ? "border-ok/40 bg-ok/10 text-ok" : "border-danger/40 bg-danger/10 text-danger")}>
            {exportNote.text}
          </span>
        )}
        {collapsible.length > 0 && (
          <Button size="sm" onClick={() => setExpanded(allOpen ? new Set() : new Set(collapsible.map((t) => t.key)))} title="Wide tables start collapsed to their name">
            <Rows3 className="size-3.5" /> {allOpen ? "Collapse wide tables" : `Expand ${collapsible.length} wide ${collapsible.length === 1 ? "table" : "tables"}`}
          </Button>
        )}
        <Button size="sm" onClick={() => void exportSvg()} title="Save the diagram as an SVG file">
          <Download className="size-3.5" /> Export SVG…
        </Button>
      </div>

      {/* bottom-left: zoom */}
      <div data-erd-ui className="absolute bottom-3 left-3 z-10 flex items-center gap-0.5 rounded-lg border border-border bg-panel/95 p-0.5 shadow-lg backdrop-blur">
        <IconButton label="Zoom out" onClick={() => canvas.current?.zoomBy(0.8)}>
          <Minus className="size-4" />
        </IconButton>
        <span className="w-11 text-center text-[11px] tabular-nums text-muted" aria-live="off">
          {zoom}%
        </span>
        <IconButton label="Zoom in" onClick={() => canvas.current?.zoomBy(1.25)}>
          <Plus className="size-4" />
        </IconButton>
        <div className="mx-0.5 h-5 w-px bg-border" aria-hidden />
        <IconButton label="Fit to screen (0)" onClick={() => canvas.current?.fit(true, panelInset)}>
          <Maximize className="size-4" />
        </IconButton>
      </div>

      {/* bottom-right: legend */}
      <div data-erd-ui className="pointer-events-none absolute bottom-3 z-10 hidden items-center gap-3 rounded-lg border border-border bg-panel/90 px-3 py-1.5 text-[11px] text-muted shadow-lg backdrop-blur md:flex" style={{ right: rightInset }}>
        <span className="flex items-center gap-1">
          <KeyRound className="size-3 text-amber-500" aria-hidden /> primary key
        </span>
        <span className="flex items-center gap-1">
          <Link2 className="size-3 text-accent" aria-hidden /> foreign key
        </span>
        <span>Double-click a table to open its data</span>
      </div>

      {selectedTable && (
        <ErdDetails
          table={selectedTable}
          model={model}
          onClose={() => setSelected(null)}
          onOpenData={() => open(selectedTable.key)}
          onFocus={focusTable}
        />
      )}
    </div>
  );
}

function TableFinder({ tables, onPick }: { tables: readonly { key: string; name: string }[]; onPick: (key: string) => void }) {
  const [q, setQ] = useState("");
  const [open, setOpen] = useState(false);
  const [sel, setSel] = useState(0);
  const matches = useMemo(() => {
    const term = q.trim();
    if (!term) return [];
    const out: { key: string; name: string; score: number; positions: number[] }[] = [];
    for (const t of tables) {
      const m = fuzzyMatch(term, t.name);
      if (m) out.push({ key: t.key, name: t.name, score: m.score, positions: m.positions });
    }
    return out.sort((a, b) => b.score - a.score).slice(0, 8);
  }, [q, tables]);

  const pick = (key: string) => {
    onPick(key);
    setQ("");
    setOpen(false);
  };

  return (
    <div className="relative">
      <label className="flex h-7 w-52 items-center gap-1.5 rounded-md border border-border bg-bg px-2 focus-within:border-accent/70 focus-within:ring-1 focus-within:ring-accent/40">
        <Search className="size-3.5 shrink-0 text-muted" aria-hidden />
        <input
          value={q}
          onChange={(e) => {
            setQ(e.target.value);
            setOpen(true);
            setSel(0);
          }}
          onFocus={() => setOpen(true)}
          onBlur={() => window.setTimeout(() => setOpen(false), 120)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setSel((s) => Math.min(s + 1, matches.length - 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setSel((s) => Math.max(s - 1, 0));
            } else if (e.key === "Enter" && matches[sel]) {
              e.preventDefault();
              pick(matches[sel].key);
            } else if (e.key === "Escape") {
              setQ("");
              setOpen(false);
              (e.target as HTMLInputElement).blur();
            }
          }}
          role="combobox"
          aria-expanded={open && matches.length > 0}
          aria-label="Find a table in the diagram"
          placeholder="Find a table"
          spellCheck={false}
          className="min-w-0 flex-1 bg-transparent text-xs text-fg outline-none placeholder:text-muted/70"
        />
      </label>
      {open && matches.length > 0 && (
        <ul role="listbox" className="absolute left-0 top-9 z-30 w-64 overflow-hidden rounded-lg border border-border bg-panel p-1 shadow-2xl">
          {matches.map((m, i) => (
            <li
              key={m.key}
              role="option"
              aria-selected={i === sel}
              onMouseDown={(e) => {
                e.preventDefault();
                pick(m.key);
              }}
              onMouseMove={() => setSel(i)}
              className={cn("flex h-7 cursor-default items-center rounded-md px-2 text-[12px]", i === sel ? "bg-accent/12 text-fg" : "text-fg/90")}
            >
              <span className="truncate">
                {[...m.name].map((ch, ci) => (
                  <span key={ci} className={m.positions.includes(ci) ? "font-semibold text-accent" : undefined}>
                    {ch}
                  </span>
                ))}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
