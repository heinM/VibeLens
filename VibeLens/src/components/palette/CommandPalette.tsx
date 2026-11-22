// ⌘P "Go to anything" and ⌘⇧P "Commands" (or type ">" in the first). Mounted once in App.
// The shortcut lives in hooks/useGlobalShortcuts; open state lives in paletteStore.
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Bot, Database, Eye, FileCode2, FileText, History, Moon, Network, PanelLeft, Plug, Plus, RefreshCw, Search, Server, Settings2, SlidersHorizontal, SquareFunction, SquareTerminal, Sun, Table2, Trash2, Unplug, XCircle, Zap } from "lucide-react";
import { Warehouse } from "lucide-react";
import { useProfiles } from "../../hooks/useProfiles";
import { useConnect, useConnectionStatuses, useDisconnect } from "../../hooks/useConnectionStatus";
import { useTabActions } from "../../hooks/useTabActions";
import { WS_FILES_KEY } from "../../hooks/useWorkspaceFiles";
import { schemaMetadataKey } from "../../lib/schemaCache";
import { cn } from "../../lib/cn";
import { useHistoryStore } from "../../store/useHistoryStore";
import { useUIStore } from "../../store/useUIStore";
import { useWorkspaceStore } from "../../store/useWorkspaceStore";
import { ConfirmDialog, EnvBadge } from "../ui";
import { MOD } from "../ui/platform";
import { Kbd, StatusDot } from "../ui/wsKit";
import { openErdTab } from "../table/openTableTab";
import { IcebergGlyph } from "../iceberg/IcebergGlyph";
import { buildCommands, COMMAND_GROUP_ORDER, type CommandEnv } from "./commands";
import { resolveErdTarget, schemasByTableCount } from "./erdTarget";
import { BROWSE_CAPS, GO_GROUP_ORDER, groupRanked, timeAgo, type IconKey, type PaletteItem, type Row, type Tone } from "./items";
import { ICEBERG_TABLES_GROUP, openIcebergQueryTab, resolveIcebergTarget } from "./icebergPalette";
import { modeOf, usePaletteStore } from "./paletteStore";
import { useIcebergTablePicker } from "./useIcebergTables";
import { mruRanks, readMru, recordMru } from "./mru";
import { positionsToRanges, splitTerms } from "./fuzzy";
import { rankItems, type Ranked } from "./rank";
import { usePaletteCatalog, type PaletteCatalog } from "./usePaletteCatalog";
import { importWorkspaceFiles } from "../../lib/workspaceImport";
import "./palette.css";

const ICONS: Record<IconKey, ReactNode> = {
  pg: <Database className="size-3.5" />,
  iceberg: <IcebergGlyph className="size-3.5" />,
  redshift: <Warehouse className="size-3.5" />,
  mysql: <Server className="size-3.5" />,
  valkey: <Zap className="size-3.5" />,
  table: <Table2 className="size-3.5" />,
  view: <Eye className="size-3.5" />,
  function: <SquareFunction className="size-3.5" />,
  schema: <Network className="size-3.5" />,
  file: <FileCode2 className="size-3.5" />,
  fileValkey: <SquareTerminal className="size-3.5" />,
  history: <History className="size-3.5" />,
  historyFail: <XCircle className="size-3.5" />,
  newTab: <FileText className="size-3.5" />,
  newConnection: <Plus className="size-3.5" />,
  settings: <Settings2 className="size-3.5" />,
  mode: <SlidersHorizontal className="size-3.5" />,
  sun: <Sun className="size-3.5" />,
  moon: <Moon className="size-3.5" />,
  sidebar: <PanelLeft className="size-3.5" />,
  agent: <Bot className="size-3.5" />,
  plug: <Plug className="size-3.5" />,
  unplug: <Unplug className="size-3.5" />,
  refresh: <RefreshCw className="size-3.5" />,
  trash: <Trash2 className="size-3.5" />,
  diagram: <Network className="size-3.5" />,
};

const TONES: Record<Tone, string> = {
  sky: "bg-sky-500/12 text-sky-500",
  cyan: "bg-cyan-400/12 text-cyan-400",
  rose: "bg-rose-400/12 text-rose-400",
  emerald: "bg-emerald-500/12 text-emerald-500",
  violet: "bg-violet-400/14 text-violet-400",
  amber: "bg-amber-500/14 text-amber-500",
  accent: "bg-accent/14 text-accent",
  muted: "bg-panel-2 text-muted",
  danger: "bg-danger/12 text-danger",
};

const ROW_H = 38;
const HEADER_H = 28;
const MAX_LIST_H = 440;

export function CommandPalette() {
  const open = usePaletteStore((s) => s.open);
  const catalog = usePaletteCatalog();
  const [confirmClear, setConfirmClear] = useState(false);
  const clearHistory = useHistoryStore((s) => s.clear);
  const count = useHistoryStore((s) => s.entries.length);
  return (
    <>
      {open && <PaletteDialog catalog={catalog} onAskClearHistory={() => setConfirmClear(true)} />}
      <ConfirmDialog
        open={confirmClear}
        title="Clear query history?"
        body={<p>This removes all {count} saved {count === 1 ? "statement" : "statements"} from this computer. It does not touch your databases or worksheets.</p>}
        confirmText="Clear history"
        danger
        onConfirm={() => {
          clearHistory();
          setConfirmClear(false);
        }}
        onCancel={() => setConfirmClear(false)}
      />
    </>
  );
}

function useCommandItems(onAskClearHistory: () => void): PaletteItem[] {
  const qc = useQueryClient();
  const { data: profiles } = useProfiles();
  const { data: statuses } = useConnectionStatuses();
  const aiMode = useUIStore((s) => s.aiMode);
  const theme = useUIStore((s) => s.theme);
  const historyCount = useHistoryStore((s) => s.entries.length);
  const connect = useConnect();
  const disconnect = useDisconnect();
  const { newSqlTab, newValkeyConsole } = useTabActions();

  return useMemo(() => {
    const list = profiles ?? [];
    const stateOf = (id: string) => statuses?.find((s) => s.profileId === id)?.state ?? "disconnected";
    const ui = useUIStore.getState();
    const tab = useWorkspaceStore.getState().activeTab();
    const erdTarget = resolveErdTarget({
      activeTab: tab,
      activeProfileId: ui.activeProfileId,
      activeDatabase: ui.activeDatabase,
      profiles: list,
      schemasOf: (pid, db) => {
        const meta = qc.getQueryData<{ tables: { schema: string }[] }>(schemaMetadataKey(pid, db)) ?? qc.getQueryData<{ tables: { schema: string }[] }>(schemaMetadataKey(pid, list.find((p) => p.id === pid)?.database ?? null));
        return schemasByTableCount(meta?.tables);
      },
    });
    const connectedPg = list.filter((p) => p.engine === "postgres" && stateOf(p.id) === "connected");
    const connectedIce = list.filter((p) => p.engine === "iceberg" && stateOf(p.id) === "connected");
    const connectedRs = list.filter((p) => p.engine === "redshift" && stateOf(p.id) === "connected");
    const connectedMy = list.filter((p) => p.engine === "mysql" && stateOf(p.id) === "connected");
    const iceberg = resolveIcebergTarget({ profiles: list, activeTabProfileId: tab?.profileId, activeProfileId: ui.activeProfileId, stateOf });
    const env: CommandEnv = {
      aiMode,
      theme,
      profiles: list,
      stateOf,
      // Connections "Refresh schema cache" touches: Postgres, Iceberg and Redshift.
      connectedPg: connectedPg.length + connectedIce.length + connectedRs.length,
      hasValkeyProfile: list.some((p) => p.engine === "valkey"),
      historyCount,
      iceberg,
      erdTarget,
      mod: MOD,
      actions: {
        // On an Iceberg connection a blank worksheet must stay bound to it (`newSqlTab` is Postgres-only).
        newSqlTab: () => (iceberg && iceberg.id === (tab?.profileId ?? ui.activeProfileId) ? void openIcebergQueryTab(iceberg) : void newSqlTab()),
        newValkeyConsole: () => newValkeyConsole(),
        newConnection: () => useUIStore.getState().openWizard(),
        importConnections: () => useUIStore.getState().setImportOpen(true),
        importWorkspaceFiles: () => void importWorkspaceFiles(qc, useUIStore.getState().activeProfileId),
        openSettings: () => useUIStore.getState().setSettingsOpen(true),
        setAiMode: (m) => useUIStore.getState().setAiMode(m),
        toggleTheme: () => {
          const s = useUIStore.getState();
          s.setTheme(s.theme === "dark" ? "light" : "dark");
        },
        toggleSidebar: () => useUIStore.getState().toggleSidebar(),
        toggleAgent: () => useUIStore.getState().toggleAgentPanel(),
        connect: (id) => connect.mutate(id),
        disconnect: (id) => disconnect.mutate(id),
        refreshSchema: () => {
          for (const p of connectedPg) void qc.invalidateQueries({ queryKey: ["pg", p.id] });
          for (const p of connectedIce) void qc.invalidateQueries({ queryKey: ["ice", p.id] });
          for (const p of connectedRs) void qc.invalidateQueries({ queryKey: ["rs", p.id] });
          for (const p of connectedMy) void qc.invalidateQueries({ queryKey: ["my", p.id] });
        },
        refreshFiles: () => void qc.invalidateQueries({ queryKey: WS_FILES_KEY }),
        openErd: (t) => openErdTab(t.profileId, t.database, t.schema),
        clearHistory: onAskClearHistory,
        newIcebergQuery: (p) => {
          useUIStore.getState().setActiveTarget(p.id, null);
          if (stateOf(p.id) === "disconnected") connect.mutate(p.id);
          openIcebergQueryTab(p);
        },
        pickIcebergTable: (p) => {
          if (stateOf(p.id) === "disconnected") connect.mutate(p.id);
          usePaletteStore.getState().openScoped({ kind: "iceTables", profileId: p.id });
        },
      },
    };
    return buildCommands(env);
    // Rebuilt on open (the dialog mounts per open) and when its inputs change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [profiles, statuses, aiMode, theme, historyCount]);
}

function PaletteDialog({ catalog, onAskClearHistory }: { catalog: PaletteCatalog; onAskClearHistory: () => void }) {
  const query = usePaletteStore((s) => s.query);
  const setQuery = usePaletteStore((s) => s.setQuery);
  const close = usePaletteStore((s) => s.close);
  const commandItems = useCommandItems(onAskClearHistory);
  // A command can open a picker (Iceberg: open table…). It replaces both modes until it is left.
  const scope = usePaletteStore((s) => s.scope);
  const leaveScope = usePaletteStore((s) => s.leaveScope);
  const { data: allProfiles } = useProfiles();
  const scopedProfile = scope ? allProfiles?.find((p) => p.id === scope.profileId) : undefined;
  const picker = useIcebergTablePicker(scopedProfile);
  const mode = scope ? "go" : modeOf(query);
  const text = mode === "commands" && !scope ? query.slice(1) : query;
  const hasQuery = splitTerms(text).length > 0;

  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const prevFocus = useRef<HTMLElement | null>(null);
  const [mruList] = useState(() => readMru());
  const ranks = useMemo(() => mruRanks(mruList), [mruList]);
  const [sel, setSel] = useState(0);

  useLayoutEffect(() => {
    prevFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    inputRef.current?.focus();
    // Put the caret after a pre-filled ">".
    const el = inputRef.current;
    if (el) el.setSelectionRange(el.value.length, el.value.length);
  }, []);

  const source = scope ? picker.items : mode === "commands" ? commandItems : catalog.items;

  const rows = useMemo<Row<Ranked<PaletteItem>>[]>(() => {
    const order = scope ? [ICEBERG_TABLES_GROUP] : mode === "commands" ? COMMAND_GROUP_ORDER : GO_GROUP_ORDER;
    if (hasQuery) return groupRanked(rankItems(source, text, ranks, 250));
    // Browse view: recent picks first, then each group (capped for the go list).
    const byId = new Map(source.map((i) => [i.id, i]));
    const recent = mruList.map((id) => byId.get(id)).filter((i): i is PaletteItem => !!i).slice(0, 6);
    const recentIds = new Set(recent.map((i) => i.id));
    // Cap each group while scanning so browsing a 5,000-item catalogue allocates a few dozen rows, not thousands.
    const totals = new Map<string, number>();
    const rest: Ranked<PaletteItem>[] = [];
    const none: number[] = [];
    for (const item of source) {
      if (recentIds.has(item.id)) continue;
      const n = (totals.get(item.group) ?? 0) + 1;
      totals.set(item.group, n);
      const cap = mode === "go" ? BROWSE_CAPS[item.group] : undefined;
      if (cap === undefined || n <= cap) rest.push({ item, score: 0, positions: none });
    }
    const head: Row<Ranked<PaletteItem>>[] = recent.length
      ? [{ type: "header", label: "Recent", count: recent.length }, ...recent.map((item, index): Row<Ranked<PaletteItem>> => ({ type: "item", index, entry: { item, score: 0, positions: [] } }))]
      : [];
    return [...head, ...groupRanked(rest, { order, start: recent.length, totals })];
  }, [source, text, hasQuery, mode, scope, ranks, mruList]);

  const flat = useMemo(() => rows.filter((r) => r.type === "item").map((r) => r.entry as Ranked<PaletteItem>), [rows]);
  const rowOfItem = useMemo(() => {
    const m: number[] = [];
    rows.forEach((r, i) => {
      if (r.type === "item" && r.index !== undefined) m[r.index] = i;
    });
    return m;
  }, [rows]);

  useEffect(() => setSel(0), [text, mode]);
  const selClamped = Math.min(sel, Math.max(flat.length - 1, 0));

  const totalH = useMemo(() => rows.reduce((h, r) => h + (r.type === "header" ? HEADER_H : ROW_H), 0), [rows]);
  const listH = Math.min(totalH, MAX_LIST_H);
  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => listRef.current,
    estimateSize: (i) => (rows[i]?.type === "header" ? HEADER_H : ROW_H),
    overscan: 10,
    getItemKey: (i) => {
      const r = rows[i];
      return r?.type === "item" ? (r.entry as Ranked<PaletteItem>).item.id : `h:${r?.label}`;
    },
  });

  const reveal = useCallback(
    (itemIndex: number) => {
      const ri = rowOfItem[itemIndex];
      if (ri === undefined) return;
      // Show the group header too when moving onto the first item of a group.
      const prev = rows[ri - 1];
      virt.scrollToIndex(prev?.type === "header" && itemIndex === 0 ? 0 : ri, { align: "auto" });
    },
    [rowOfItem, rows, virt],
  );
  const move = (to: number) => {
    if (flat.length === 0) return;
    const n = ((to % flat.length) + flat.length) % flat.length;
    setSel(n);
    reveal(n);
  };

  const dismiss = useCallback(() => {
    close();
    const el = prevFocus.current;
    if (el) requestAnimationFrame(() => el.isConnected && el.focus());
  }, [close]);

  const run = useCallback(
    (r: Ranked<PaletteItem> | undefined, alt: boolean) => {
      if (!r) return;
      const fn = alt ? r.item.alt?.run : r.item.run;
      if (!fn) return;
      recordMru(r.item.id);
      close();
      fn();
    },
    [close],
  );

  const onKeyDown = (e: React.KeyboardEvent) => {
    const mod = e.metaKey || e.ctrlKey;
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        move(selClamped + 1);
        break;
      case "ArrowUp":
        e.preventDefault();
        move(selClamped - 1);
        break;
      case "PageDown":
        e.preventDefault();
        move(Math.min(selClamped + 8, flat.length - 1));
        break;
      case "PageUp":
        e.preventDefault();
        move(Math.max(selClamped - 8, 0));
        break;
      case "Home":
        if (mod) {
          e.preventDefault();
          move(0);
        }
        break;
      case "End":
        if (mod) {
          e.preventDefault();
          move(flat.length - 1);
        }
        break;
      case "Enter":
        e.preventDefault();
        run(flat[selClamped], mod);
        break;
      case "Escape":
        e.preventDefault();
        e.stopPropagation();
        dismiss();
        break;
      case "Backspace":
        // Backspace in an empty picker steps back out of it.
        if (scope && query === "") {
          e.preventDefault();
          leaveScope();
        }
        break;
      case "Tab":
        e.preventDefault();
        break;
    }
  };

  const current = flat[selClamped]?.item;
  const emptyHint = scope
    ? picker.error
      ? `Could not list the tables: ${picker.error}`
      : picker.loading
        ? "Listing namespaces and tables…"
        : picker.items.length === 0
          ? "This catalog has no tables yet."
          : "No table matches. Check the spelling."
    : mode === "go" && catalog.connectedPg.length === 0
      ? "Connect to a PostgreSQL database to search its tables, views and functions. Connections, files and history are searchable now."
      : "Nothing matches. Check the spelling, or type > to run a command.";

  return (
    <div className="vg-palette-scrim fixed inset-0 z-[65] flex items-start justify-center bg-black/45 px-4 pt-[11vh] backdrop-blur-[2px]" onMouseDown={dismiss}>
      <div
        role="dialog"
        aria-modal="true"
        aria-label={scope ? "Open Iceberg table" : mode === "commands" ? "Commands" : "Go to anything"}
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
        className="vg-palette-panel flex w-full max-w-[680px] flex-col overflow-hidden rounded-xl border border-border bg-panel shadow-[0_24px_80px_-12px_rgba(0,0,0,0.6)] ring-1 ring-white/[0.03]"
      >
        <div className="flex h-12 items-center gap-3 border-b border-border px-4">
          <Search className="size-4 shrink-0 text-muted" aria-hidden />
          {scope && (
            <button
              type="button"
              onClick={leaveScope}
              title="Back to Go to anything (Backspace on an empty search)"
              className="inline-flex h-6 shrink-0 items-center gap-1.5 rounded-md bg-cyan-400/12 px-2 text-[11.5px] font-medium text-cyan-400 outline-none hover:bg-cyan-400/20 focus-visible:ring-2 focus-visible:ring-accent/60"
            >
              <IcebergGlyph className="size-3.5" /> <span className="max-w-[160px] truncate">{scopedProfile?.name ?? "Iceberg"}</span> <XCircle className="size-3 opacity-70" aria-hidden />
            </button>
          )}
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            role="combobox"
            aria-expanded
            aria-controls="vgp-list"
            aria-activedescendant={current ? `vgp-opt-${selClamped}` : undefined}
            aria-label={scope ? "Search Iceberg tables" : mode === "commands" ? "Run a command" : "Search tables, connections, files and history"}
            placeholder={scope ? "Open an Iceberg table or view" : mode === "commands" ? ">  Run a command" : "Go to table, view, function, connection, file or query"}
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="off"
            spellCheck={false}
            className="min-w-0 flex-1 bg-transparent text-[15px] text-fg outline-none placeholder:text-muted/70"
          />
          <ModeSwitch
            mode={mode}
            onChange={(m) => {
              if (scope) leaveScope();
              setQuery(m === "commands" ? ">" : "");
            }}
          />
        </div>

        <div id="vgp-list" role="listbox" ref={listRef} style={{ height: flat.length === 0 ? undefined : listH }} className="overflow-y-auto overscroll-contain">
          {flat.length === 0 ? (
            <div className="px-6 py-9 text-center text-[13px] leading-relaxed text-muted">
              <div className="mx-auto max-w-sm">{hasQuery || source.length === 0 || scope ? emptyHint : "Nothing to show yet."}</div>
            </div>
          ) : (
            <div style={{ height: virt.getTotalSize(), position: "relative" }}>
              {virt.getVirtualItems().map((v) => {
                const row = rows[v.index];
                if (!row) return null;
                return (
                  <div key={v.key} style={{ position: "absolute", top: 0, left: 0, right: 0, height: v.size, transform: `translateY(${v.start}px)` }}>
                    {row.type === "header" ? (
                      <GroupHeader label={row.label ?? ""} count={row.count} />
                    ) : (
                      <PaletteRow
                        ranked={row.entry as Ranked<PaletteItem>}
                        index={row.index as number}
                        selected={row.index === selClamped}
                        onHover={setSel}
                        onPick={(r, alt) => run(r, alt)}
                      />
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>

        <div className="flex h-9 items-center gap-3 border-t border-border bg-panel-2/40 px-4 text-[11px] text-muted">
          <span className="min-w-0 flex-1 truncate">
            {scope && picker.loading ? (
              <span className="inline-flex items-center gap-1.5">
                <RefreshCw className="size-3 animate-spin" aria-hidden /> Listing namespaces and tables…
              </span>
            ) : scope ? (
              <>{picker.truncated ? "Showing the tables of the first 400 namespaces. " : ""}<Kbd>⌫</Kbd> on an empty search goes back</>
            ) : catalog.loading > 0 && mode === "go" ? (
              <span className="inline-flex items-center gap-1.5">
                <RefreshCw className="size-3 animate-spin" aria-hidden /> Reading schemas…
              </span>
            ) : mode === "go" ? (
              <>
                Type <Kbd>&gt;</Kbd> for commands
              </>
            ) : (
              <>Delete the <Kbd>&gt;</Kbd> to search objects again</>
            )}
          </span>
          {current && (
            <span className="flex shrink-0 items-center gap-1">
              <Kbd>↵</Kbd> {current.primary}
            </span>
          )}
          {current?.alt && (
            <span className="flex shrink-0 items-center gap-1">
              <Kbd>{MOD}</Kbd>
              <Kbd>↵</Kbd> {current.alt.label}
            </span>
          )}
          <span className="flex shrink-0 items-center gap-1">
            <Kbd>esc</Kbd> close
          </span>
        </div>
      </div>
    </div>
  );
}

function ModeSwitch({ mode, onChange }: { mode: "go" | "commands"; onChange: (m: "go" | "commands") => void }) {
  const seg = (m: "go" | "commands", label: string, keys: string) => (
    <button
      type="button"
      onClick={() => {
        onChange(m);
      }}
      title={`${label} (${keys})`}
      aria-pressed={mode === m}
      className={cn("h-6 rounded-md px-2 text-[11px] font-medium outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/60", mode === m ? "bg-accent/15 text-accent" : "text-muted hover:text-fg")}
    >
      {label}
    </button>
  );
  return (
    <div className="flex shrink-0 items-center rounded-lg border border-border bg-bg p-0.5" role="group" aria-label="Palette mode">
      {seg("go", "Go to", `${MOD}P`)}
      {seg("commands", "Commands", `${MOD}⇧P`)}
    </div>
  );
}

function GroupHeader({ label, count }: { label: string; count?: number }) {
  return (
    <div className="flex h-full items-end justify-between px-4 pb-1 text-[11px] font-medium text-muted" role="presentation">
      <span>{label}</span>
      {count !== undefined && count > 0 && <span className="tabular-nums text-muted/70">{count}</span>}
    </div>
  );
}

function Highlight({ text, positions }: { text: string; positions: readonly number[] }) {
  if (positions.length === 0) return <>{text}</>;
  const ranges = positionsToRanges(positions.filter((p) => p < text.length));
  const out: ReactNode[] = [];
  let at = 0;
  ranges.forEach(([s, e], i) => {
    if (s > at) out.push(text.slice(at, s));
    out.push(
      <span key={i} className="font-semibold text-accent">
        {text.slice(s, e)}
      </span>,
    );
    at = e;
  });
  if (at < text.length) out.push(text.slice(at));
  return <>{out}</>;
}

const PaletteRow = memo(function PaletteRow({
  ranked,
  index,
  selected,
  onHover,
  onPick,
}: {
  ranked: Ranked<PaletteItem>;
  index: number;
  selected: boolean;
  onHover: (i: number) => void;
  onPick: (r: Ranked<PaletteItem>, alt: boolean) => void;
}) {
  const it = ranked.item;
  const mono = it.group === "History";
  const meta = it.meta ?? (it.ts ? timeAgo(Date.now(), it.ts) : undefined);
  return (
    <div
      id={`vgp-opt-${index}`}
      role="option"
      aria-selected={selected}
      onMouseMove={() => !selected && onHover(index)}
      onClick={(e) => onPick(ranked, e.metaKey || e.ctrlKey)}
      className={cn("relative flex h-full cursor-default items-center gap-3 px-4", selected ? "bg-accent/[0.09]" : "")}
    >
      {selected && <span aria-hidden className="absolute inset-y-1.5 left-0 w-0.5 rounded-r bg-accent" />}
      <span className={cn("flex size-6 shrink-0 items-center justify-center rounded-md", TONES[it.tone])}>{ICONS[it.icon]}</span>
      <span className="flex min-w-0 flex-1 items-baseline gap-2">
        <span className={cn("min-w-0 truncate text-fg", mono ? "font-mono text-[12px]" : "text-[13.5px]")}>
          <Highlight text={it.title} positions={ranked.positions} />
        </span>
        {it.subtitle && <span className="min-w-0 shrink truncate font-mono text-[11px] text-muted">{it.subtitle}</span>}
      </span>
      <span className="flex shrink-0 items-center gap-2 text-[11.5px] text-muted">
        {it.crumbs?.map((c, i) => (
          <span key={i} className={cn("max-w-[150px] truncate", i > 0 && "rounded bg-panel-2 px-1.5 font-mono text-[11px]")}>
            {c}
          </span>
        ))}
        {it.env && <EnvBadge env={it.env} />}
        {meta && <span className="tabular-nums">{meta}</span>}
        {it.state && <StatusDot state={it.state} />}
        {it.shortcut && (
          <span className="flex gap-1">
            {it.shortcut.map((k, i) => (
              <Kbd key={i}>{k}</Kbd>
            ))}
          </span>
        )}
      </span>
    </div>
  );
});

// Re-export for callers that want to open it programmatically.
export { usePaletteStore } from "./paletteStore";
