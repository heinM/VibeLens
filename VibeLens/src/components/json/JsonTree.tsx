// Collapsible, virtualised, type-coloured JSON viewer. Handles very large payloads: only expanded
// nodes are flattened and only the visible window is rendered.
import { memo, useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { Check, ChevronDown, ChevronRight, Copy, Link2 } from "lucide-react";
import { cn } from "../../lib/cn";
import { copyToClipboard } from "../ui/clipboard";

const ROW_H = 22;
const INDENT = 14;
const MAX_CHILDREN = 5000; // per container; protects the DOM-less flatten from megabyte arrays

type Kind = "object" | "array" | "string" | "number" | "boolean" | "null" | "bytes" | "more";
type PathSeg = string | number;

interface FlatRow {
  id: string;
  depth: number;
  label: string | null;
  labelIsIndex: boolean;
  kind: Kind;
  value: unknown;
  childCount: number;
  expanded: boolean;
  path: PathSeg[];
}

function kindOf(v: unknown): Kind {
  if (v === null || v === undefined) return "null";
  if (Array.isArray(v)) return "array";
  switch (typeof v) {
    case "string":
      return "string";
    case "number":
    case "bigint":
      return "number";
    case "boolean":
      return "boolean";
    default: {
      const o = v as Record<string, unknown>;
      if (typeof o.$bytes === "string" && typeof o.len === "number" && Object.keys(o).length === 2) return "bytes";
      return "object";
    }
  }
}

const idOf = (path: PathSeg[]) => path.map(String).join("\u0001");

export function jsonPathString(path: readonly PathSeg[]): string {
  let s = "$";
  for (const seg of path) {
    if (typeof seg === "number") s += `[${seg}]`;
    else if (/^[A-Za-z_][A-Za-z0-9_]*$/.test(seg)) s += `.${seg}`;
    else s += `[${JSON.stringify(seg)}]`;
  }
  return s;
}

function entriesOf(v: unknown, kind: Kind): [PathSeg, unknown][] {
  if (kind === "array") return (v as unknown[]).map((x, i) => [i, x]);
  return Object.entries(v as Record<string, unknown>);
}

function flatten(root: unknown, rootLabel: string | null, toggled: ReadonlyMap<string, boolean>, defaultDepth: number): FlatRow[] {
  const out: FlatRow[] = [];
  const walk = (value: unknown, path: PathSeg[], depth: number, label: string | null, labelIsIndex: boolean) => {
    const kind = kindOf(value);
    const id = idOf(path);
    const container = kind === "object" || kind === "array";
    const childCount = container ? (kind === "array" ? (value as unknown[]).length : Object.keys(value as object).length) : 0;
    const expanded = container && childCount > 0 && (toggled.get(id) ?? depth < defaultDepth);
    out.push({ id, depth, label, labelIsIndex, kind, value, childCount, expanded, path });
    if (!expanded) return;
    const entries = entriesOf(value, kind);
    const shown = Math.min(entries.length, MAX_CHILDREN);
    for (let i = 0; i < shown; i++) walk(entries[i][1], [...path, entries[i][0]], depth + 1, String(entries[i][0]), kind === "array");
    if (entries.length > shown) {
      out.push({ id: `${id}\u0001…more`, depth: depth + 1, label: null, labelIsIndex: false, kind: "more", value: entries.length - shown, childCount: 0, expanded: false, path });
    }
  };
  walk(root, [], 0, rootLabel, false);
  return out;
}

const valueClass: Record<Kind, string> = {
  object: "text-muted",
  array: "text-muted",
  string: "text-emerald-700 dark:text-emerald-300",
  number: "text-amber-700 dark:text-amber-300",
  boolean: "text-violet-700 dark:text-violet-300",
  null: "italic text-muted",
  bytes: "text-muted",
  more: "italic text-muted",
};

function scalarText(row: FlatRow): string {
  switch (row.kind) {
    case "string": {
      const s = row.value as string;
      const flat = s.replace(/\r?\n/g, "↵");
      return `"${flat.length > 600 ? flat.slice(0, 600) + "…" : flat}"`;
    }
    case "null":
      return "null";
    case "bytes":
      return `‹bytes ${(row.value as { len: number }).len}›`;
    case "more":
      return `… ${row.value as number} more not shown`;
    default:
      return String(row.value);
  }
}

function valueToClipboard(value: unknown, kind: Kind): string {
  if (kind === "string") return value as string;
  try {
    return JSON.stringify(value, null, 2) ?? String(value);
  } catch {
    return String(value);
  }
}

const Row = memo(function Row({
  row,
  selected,
  onToggle,
  onSelect,
  onCopy,
  copied,
}: {
  row: FlatRow;
  selected: boolean;
  onToggle: (row: FlatRow) => void;
  onSelect: (id: string) => void;
  onCopy: (row: FlatRow, what: "path" | "value") => void;
  copied: string | null;
}) {
  const container = row.kind === "object" || row.kind === "array";
  const summary = container ? (row.kind === "array" ? `[ ${row.childCount} ]` : `{ ${row.childCount} }`) : null;
  return (
    <div
      role="treeitem"
      aria-expanded={container ? row.expanded : undefined}
      aria-selected={selected}
      onClick={() => {
        onSelect(row.id);
        if (container) onToggle(row);
      }}
      className={cn(
        "group flex h-[22px] cursor-default items-center whitespace-nowrap pr-2 font-mono text-[12px] leading-[22px]",
        selected ? "bg-accent/15" : "hover:bg-panel-2/70",
      )}
      style={{ paddingLeft: 6 + row.depth * INDENT }}
    >
      <span className="flex size-4 shrink-0 items-center justify-center text-muted">
        {container && row.childCount > 0 ? row.expanded ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" /> : null}
      </span>
      {row.label !== null && (
        <span className={cn("mr-1 shrink-0", row.labelIsIndex ? "text-muted" : "text-sky-700 dark:text-sky-300")}>
          {row.labelIsIndex ? row.label : row.label}
          <span className="text-muted">:</span>
        </span>
      )}
      <span className={cn("min-w-0 truncate", valueClass[row.kind])}>{summary ?? scalarText(row)}</span>
      {container && !row.expanded && row.childCount > 0 && <span className="ml-1 text-muted/70">…</span>}
      {row.kind !== "more" && (
        <span className="ml-auto flex shrink-0 items-center gap-0.5 pl-3 opacity-0 group-hover:opacity-100">
          <button
            type="button"
            title="Copy path"
            aria-label="Copy path"
            onClick={(e) => {
              e.stopPropagation();
              onCopy(row, "path");
            }}
            className="rounded p-0.5 text-muted hover:bg-border hover:text-fg"
          >
            {copied === `${row.id}:path` ? <Check className="size-3 text-ok" /> : <Link2 className="size-3" />}
          </button>
          <button
            type="button"
            title="Copy value"
            aria-label="Copy value"
            onClick={(e) => {
              e.stopPropagation();
              onCopy(row, "value");
            }}
            className="rounded p-0.5 text-muted hover:bg-border hover:text-fg"
          >
            {copied === `${row.id}:value` ? <Check className="size-3 text-ok" /> : <Copy className="size-3" />}
          </button>
        </span>
      )}
    </div>
  );
});

export interface JsonTreeProps {
  value: unknown;
  /** Label for the root node, e.g. a key name. Omit for an anonymous root. */
  rootLabel?: string;
  /** Depth that starts expanded (root = 0). Default 1: the root and its direct children. */
  defaultExpandDepth?: number;
  /** When set the tree sizes to its content up to this many pixels instead of filling its parent. */
  maxHeight?: number;
  className?: string;
}

export function JsonTree({ value, rootLabel, defaultExpandDepth = 1, maxHeight, className }: JsonTreeProps) {
  const [toggled, setToggled] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [selected, setSelected] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const copiedTimer = useRef<number | undefined>(undefined);

  // A different payload starts from the default expansion again.
  useEffect(() => {
    setToggled(new Map());
    setSelected(null);
  }, [value]);
  useEffect(() => () => window.clearTimeout(copiedTimer.current), []);

  const rows = useMemo(() => flatten(value, rootLabel ?? null, toggled, defaultExpandDepth), [value, rootLabel, toggled, defaultExpandDepth]);

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_H,
    getItemKey: (i) => rows[i]?.id ?? i,
    overscan: 12,
  });

  const toggle = useCallback((row: FlatRow) => {
    setToggled((prev) => {
      const next = new Map(prev);
      next.set(row.id, !row.expanded);
      return next;
    });
  }, []);

  const copy = useCallback((row: FlatRow, what: "path" | "value") => {
    const text = what === "path" ? jsonPathString(row.path) : valueToClipboard(row.value, row.kind);
    void copyToClipboard(text).then((ok) => {
      if (!ok) return;
      setCopied(`${row.id}:${what}`);
      window.clearTimeout(copiedTimer.current);
      copiedTimer.current = window.setTimeout(() => setCopied(null), 1200);
    });
  }, []);

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const idx = rows.findIndex((r) => r.id === selected);
    const cur = idx >= 0 ? rows[idx] : null;
    const go = (i: number) => {
      const r = rows[Math.max(0, Math.min(rows.length - 1, i))];
      if (r) {
        setSelected(r.id);
        virt.scrollToIndex(rows.indexOf(r));
      }
    };
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        go(idx + 1);
        break;
      case "ArrowUp":
        e.preventDefault();
        go(idx < 0 ? 0 : idx - 1);
        break;
      case "ArrowRight":
        if (cur && !cur.expanded && cur.childCount > 0) {
          e.preventDefault();
          toggle(cur);
        } else go(idx + 1);
        break;
      case "ArrowLeft":
        if (cur?.expanded) {
          e.preventDefault();
          toggle(cur);
        } else if (cur && cur.depth > 0) {
          // jump to parent
          for (let i = idx - 1; i >= 0; i--) {
            if (rows[i].depth < cur.depth) {
              go(i);
              break;
            }
          }
        }
        break;
      case "Enter":
      case " ":
        if (cur && cur.childCount > 0) {
          e.preventDefault();
          toggle(cur);
        }
        break;
      case "c":
        if ((e.metaKey || e.ctrlKey) && cur) {
          e.preventDefault();
          copy(cur, "value");
        }
        break;
    }
  };

  const contentH = rows.length * ROW_H;
  const style = maxHeight !== undefined ? { height: Math.min(contentH + 2, maxHeight) } : undefined;

  return (
    <div
      ref={scrollRef}
      role="tree"
      aria-label="JSON"
      tabIndex={0}
      onKeyDown={onKeyDown}
      style={style}
      className={cn("selectable overflow-auto outline-none focus-visible:ring-1 focus-visible:ring-accent/50", maxHeight === undefined && "h-full", className)}
    >
      <div style={{ height: virt.getTotalSize(), position: "relative", minWidth: "100%", width: "max-content" }}>
        {virt.getVirtualItems().map((item) => {
          const row = rows[item.index];
          if (!row) return null;
          return (
            <div key={item.key} style={{ position: "absolute", top: 0, left: 0, right: 0, height: ROW_H, transform: `translateY(${item.start}px)` }}>
              <Row row={row} selected={row.id === selected} onToggle={toggle} onSelect={setSelected} onCopy={copy} copied={copied} />
            </div>
          );
        })}
      </div>
    </div>
  );
}
