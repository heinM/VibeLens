// Query history: every statement run from the editor, searchable, grouped by day, virtualised.
// Fills its container; the navigator mounts it in the sidebar.
import { memo, useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Copy, History, Play, Search, Trash2, X } from "lucide-react";
import { cn } from "../../lib/cn";
import { TRUNCATED_MARK, useHistoryStore, type HistoryEntry } from "../../store/useHistoryStore";
import { REDACTED } from "../../lib/sqlIntel/redact";
import { useProfiles } from "../../hooks/useProfiles";
import { useTabActions } from "../../hooks/useTabActions";
import { ConfirmDialog, EmptyState } from "../ui";
import { copyToClipboard } from "../ui/clipboard";
import { formatCount, formatElapsed } from "../grid/cellFormat";
import { showToast } from "../grid/toast";
import { clockTime, collapseSql, filterEntries, groupByDay, timeAgo, type HistoryRow } from "./historyModel";

const DAY_H = 28;
const ENTRY_H = 70;

/** Re-render every `ms` so "5m ago" stays honest while the panel is open. */
function useNow(ms: number): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = window.setInterval(() => setNow(Date.now()), ms);
    return () => window.clearInterval(id);
  }, [ms]);
  return now;
}

const HistoryItem = memo(function HistoryItem({
  entry,
  now,
  focused,
  onOpen,
  onRun,
  onCopy,
  onRemove,
}: {
  entry: HistoryEntry;
  now: number;
  focused: boolean;
  onOpen: (e: HistoryEntry) => void;
  onRun: (e: HistoryEntry) => void;
  onCopy: (e: HistoryEntry) => void;
  onRemove: (e: HistoryEntry) => void;
}) {
  const age = now - entry.at < 86_400_000 ? timeAgo(entry.at, now) : clockTime(entry.at);
  const engine = entry.engine === "iceberg" ? "Iceberg" : null; // Postgres is the default; don't label it
  const connection = [entry.profileName, engine, entry.database].filter(Boolean).join(" · ");
  const action = (fn: (e: HistoryEntry) => void) => (ev: { stopPropagation: () => void }) => {
    ev.stopPropagation();
    fn(entry);
  };
  return (
    <div
      role="option"
      aria-selected={focused}
      id={`hist-${entry.id}`}
      onClick={() => onOpen(entry)}
      title={`${new Date(entry.at).toLocaleString()}\n${entry.ok ? "Succeeded" : `Failed: ${entry.error ?? "unknown error"}`}\nClick to open in a new SQL tab`}
      className={cn(
        "group relative flex h-full cursor-default gap-2 border-b border-border/50 px-3 py-2 hover:bg-panel-2",
        focused && "bg-panel-2 shadow-[inset_2px_0_0_var(--vg-accent)]",
      )}
    >
      <span
        role="img"
        aria-label={entry.ok ? "Succeeded" : "Failed"}
        className={cn("mt-[5px] size-2 shrink-0 rounded-full", entry.ok ? "bg-ok" : "bg-danger")}
      />
      <div className="min-w-0 flex-1">
        <div className={cn("break-all font-mono text-[12px] leading-[1.4] text-fg", entry.ok ? "line-clamp-2" : "line-clamp-1")}>{collapseSql(entry.sql)}</div>
        {!entry.ok && <div className="truncate text-[11px] leading-4 text-danger">{entry.error ?? "Failed"}</div>}
        <div className="mt-1 flex min-w-0 items-center gap-1.5 text-[11px] leading-4 text-muted">
          {connection && <span className="min-w-0 truncate">{connection}</span>}
          <span className="shrink-0 tabular-nums">{age}</span>
          {entry.ok && entry.elapsedMs !== null && <span className="shrink-0 tabular-nums">{formatElapsed(entry.elapsedMs)}</span>}
          {entry.ok && entry.rowCount !== null && (
            <span className="shrink-0 tabular-nums">
              {formatCount(entry.rowCount)} {entry.rowCount === 1 ? "row" : "rows"}
            </span>
          )}
        </div>
      </div>
      <div className="absolute right-1.5 top-1.5 hidden items-center gap-0.5 rounded-md border border-border bg-panel p-0.5 shadow-md group-hover:flex group-focus-within:flex">
        <button type="button" title="Run again in a new tab" aria-label="Run again in a new tab" onClick={action(onRun)} className="flex size-6 items-center justify-center rounded text-muted hover:bg-panel-2 hover:text-accent">
          <Play className="size-3.5" />
        </button>
        <button type="button" title="Copy SQL" aria-label="Copy SQL" onClick={action(onCopy)} className="flex size-6 items-center justify-center rounded text-muted hover:bg-panel-2 hover:text-fg">
          <Copy className="size-3.5" />
        </button>
        <button type="button" title="Delete from history" aria-label="Delete from history" onClick={action(onRemove)} className="flex size-6 items-center justify-center rounded text-muted hover:bg-panel-2 hover:text-danger">
          <Trash2 className="size-3.5" />
        </button>
      </div>
    </div>
  );
});

export function HistoryPanel() {
  const entries = useHistoryStore((s) => s.entries);
  const remove = useHistoryStore((s) => s.remove);
  const clear = useHistoryStore((s) => s.clear);
  const { data: profiles } = useProfiles();
  const { newSqlTab } = useTabActions();
  const now = useNow(30_000);

  const [query, setQuery] = useState("");
  const [onlyFailed, setOnlyFailed] = useState(false);
  const [confirmClear, setConfirmClear] = useState(false);
  const [focusId, setFocusId] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  const filtered = useMemo(() => filterEntries(entries, query, onlyFailed), [entries, query, onlyFailed]);
  // `now` only matters for day labels at midnight; recomputing per tick is cheap (<= 1000 entries).
  const rows = useMemo<HistoryRow[]>(() => groupByDay(filtered, now), [filtered, now]);
  const failedCount = useMemo(() => entries.reduce((n, e) => n + (e.ok ? 0 : 1), 0), [entries]);

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => (rows[i]?.type === "day" ? DAY_H : ENTRY_H),
    overscan: 10,
    getItemKey: (i) => {
      const r = rows[i];
      return r ? (r.type === "day" ? `d:${r.key}` : r.entry.id) : i;
    },
  });
  useEffect(() => {
    virt.measure();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rows]);

  const target = useCallback(
    (e: HistoryEntry) => {
      const known = e.profileId && profiles?.some((p) => p.id === e.profileId);
      return { profileId: known ? e.profileId : null, database: known ? e.database : null };
    },
    [profiles],
  );

  const open = useCallback(
    (e: HistoryEntry) => {
      newSqlTab({ ...target(e), contents: e.sql });
    },
    [newSqlTab, target],
  );
  const run = useCallback(
    (e: HistoryEntry) => {
      const t = target(e);
      if (!t.profileId) {
        showToast({ title: "Connection no longer exists", detail: "The statement was opened without a connection. Choose one, then run it.", tone: "info" });
        newSqlTab({ ...t, contents: e.sql });
        return;
      }
      // A redacted or cut copy is not the statement that ran: never execute it unreviewed.
      if (e.sql.includes(REDACTED) || e.sql.includes(TRUNCATED_MARK)) {
        showToast({ title: "Review before running", detail: "History keeps a redacted or shortened copy of this statement. Restore the original values, then run it.", tone: "info" });
        newSqlTab({ ...t, contents: e.sql });
        return;
      }
      newSqlTab({ ...t, contents: e.sql, autoRun: true });
    },
    [newSqlTab, target],
  );
  const copy = useCallback((e: HistoryEntry) => void copyToClipboard(e.sql).then((ok) => ok && showToast({ title: "Copied SQL", durationMs: 1800 })), []);

  const entryRows = useMemo(() => rows.filter((r): r is Extract<HistoryRow, { type: "entry" }> => r.type === "entry"), [rows]);
  const focusIdx = focusId ? entryRows.findIndex((r) => r.entry.id === focusId) : -1;

  const move = (delta: number) => {
    if (entryRows.length === 0) return;
    const next = Math.min(entryRows.length - 1, Math.max(0, (focusIdx < 0 ? (delta > 0 ? -1 : entryRows.length) : focusIdx) + delta));
    const entry = entryRows[next].entry;
    setFocusId(entry.id);
    const rowIndex = rows.findIndex((r) => r.type === "entry" && r.entry.id === entry.id);
    if (rowIndex >= 0) virt.scrollToIndex(rowIndex, { align: "auto" });
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const focused = focusIdx >= 0 ? entryRows[focusIdx].entry : null;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      move(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      move(-1);
    } else if (e.key === "Enter" && focused) {
      e.preventDefault();
      if (e.metaKey || e.ctrlKey) run(focused);
      else open(focused);
    } else if ((e.key === "Delete" || e.key === "Backspace") && focused) {
      e.preventDefault();
      const nextEntry = entryRows[focusIdx + 1] ?? entryRows[focusIdx - 1];
      remove(focused.id);
      setFocusId(nextEntry?.entry.id ?? null);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-panel text-fg">
      <div className="flex shrink-0 items-center gap-1.5 border-b border-border px-2 py-2">
        <div className="relative min-w-0 flex-1">
          <Search className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-muted" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setQuery("");
              else if (e.key === "ArrowDown") {
                e.preventDefault();
                scrollRef.current?.focus();
                move(1);
              }
            }}
            placeholder="Search SQL, connection, error"
            aria-label="Search query history"
            spellCheck={false}
            className="h-7 w-full rounded-md border border-border bg-bg pl-7 pr-6 text-xs text-fg outline-none placeholder:text-muted/70 focus:border-accent/70"
          />
          {query && (
            <button type="button" aria-label="Clear search" onClick={() => setQuery("")} className="absolute right-1 top-1/2 flex size-5 -translate-y-1/2 items-center justify-center rounded text-muted hover:bg-panel-2 hover:text-fg">
              <X className="size-3" />
            </button>
          )}
        </div>
        <button
          type="button"
          aria-pressed={onlyFailed}
          onClick={() => setOnlyFailed((v) => !v)}
          disabled={failedCount === 0 && !onlyFailed}
          title="Show only failed statements"
          className={cn(
            "inline-flex h-7 shrink-0 items-center gap-1 rounded-md border px-2 text-xs font-medium disabled:opacity-40",
            onlyFailed ? "border-danger/50 bg-danger/10 text-danger" : "border-border bg-panel-2 text-muted hover:text-fg",
          )}
        >
          <span className="size-1.5 rounded-full bg-danger" /> Failed
        </button>
        <button
          type="button"
          onClick={() => setConfirmClear(true)}
          disabled={entries.length === 0}
          title="Clear history"
          aria-label="Clear history"
          className="inline-flex size-7 shrink-0 items-center justify-center rounded-md text-muted hover:bg-panel-2 hover:text-danger disabled:opacity-40"
        >
          <Trash2 className="size-3.5" />
        </button>
      </div>

      {entries.length === 0 ? (
        <EmptyState icon={<History className="size-5" />} title="No queries yet">
          Statements you run from the editor show up here, newest first, so you can open or re-run them later. Production connections are not recorded, and literal passwords and keys are redacted.
        </EmptyState>
      ) : filtered.length === 0 ? (
        <EmptyState icon={<Search className="size-5" />} title={query ? `Nothing matches “${query}”` : "No failed statements"}>
          {query ? "Search looks at the SQL, connection name, database and error text." : "Everything in your history ran successfully."}
        </EmptyState>
      ) : (
        <>
          <div className="shrink-0 px-3 py-1 text-[11px] tabular-nums text-muted" aria-live="polite">
            {filtered.length === entries.length ? `${formatCount(entries.length)} ${entries.length === 1 ? "statement" : "statements"}` : `${formatCount(filtered.length)} of ${formatCount(entries.length)}`}
          </div>
          <div
            ref={scrollRef}
            role="listbox"
            tabIndex={0}
            aria-label="Query history"
            aria-activedescendant={focusId ? `hist-${focusId}` : undefined}
            onKeyDown={onKeyDown}
            className="relative min-h-0 flex-1 overflow-auto outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent/50"
          >
            <div style={{ height: virt.getTotalSize(), position: "relative" }}>
              {virt.getVirtualItems().map((item) => {
                const r = rows[item.index];
                if (!r) return null;
                return (
                  <div key={item.key} className="absolute inset-x-0 top-0" style={{ transform: `translateY(${item.start}px)`, height: item.size }}>
                    {r.type === "day" ? (
                      <div className="flex h-full items-end justify-between border-b border-border/60 bg-panel px-3 pb-1 text-[11px] font-semibold text-muted">
                        <span>{r.label}</span>
                        <span className="font-normal tabular-nums">{r.count}</span>
                      </div>
                    ) : (
                      <HistoryItem entry={r.entry} now={now} focused={r.entry.id === focusId} onOpen={open} onRun={run} onCopy={copy} onRemove={(e) => remove(e.id)} />
                    )}
                  </div>
                );
              })}
            </div>
          </div>
        </>
      )}

      <ConfirmDialog
        open={confirmClear}
        title="Clear query history?"
        confirmText="Clear history"
        danger
        onCancel={() => setConfirmClear(false)}
        onConfirm={() => {
          clear();
          setConfirmClear(false);
          setFocusId(null);
        }}
        body={<p>This removes all {formatCount(entries.length)} saved statements from this computer. It cannot be undone.</p>}
      />
    </div>
  );
}
