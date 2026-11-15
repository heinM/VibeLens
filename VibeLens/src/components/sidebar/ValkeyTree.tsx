// Valkey navigator: logical databases (with key counts) > client-side namespace tree built from
// SCAN pages, split on the profile's delimiter. One database is expanded at a time.
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AlertCircle, Database, Folder, Loader2 } from "lucide-react";
import { asError } from "../../ipc/commands";
import type { ConnectionProfile, KeyType } from "../../ipc/types";
import { useUIStore } from "../../store/useUIStore";
import { useValkeyDbs, useValkeyScan } from "../../hooks/useValkey";
import { useTabActions } from "../../hooks/useTabActions";
import { Button, ErrorBox } from "../ui";
import { useMenu, type MenuEntry } from "../ui/Menu";
import { copyToClipboard } from "../ui/clipboard";
import { cn } from "../../lib/cn";
import { useInspectorStore } from "../valkey/inspectorStore";
import { TREE_ROW_H, TreeRow } from "./TreeRow";
import { buildKeyTree, flattenKeyTree, type KeyFolder, type KeyLeaf } from "./keyTree";

type VkRow =
  | { id: string; depth: number; kind: "db"; index: number; keys: number | null; expanded: boolean }
  | { id: string; depth: number; kind: "folder"; node: KeyFolder; expanded: boolean }
  | { id: string; depth: number; kind: "key"; node: KeyLeaf }
  | { id: string; depth: number; kind: "more"; loading: boolean }
  | { id: string; depth: number; kind: "status"; statusKind: "loading" | "error" | "empty"; label: string };

const glyph: Record<KeyType, { text: string; cls: string }> = {
  string: { text: "str", cls: "text-sky-500" },
  hash: { text: "hsh", cls: "text-violet-400" },
  list: { text: "lst", cls: "text-emerald-500" },
  set: { text: "set", cls: "text-amber-500" },
  zset: { text: "zst", cls: "text-rose-400" },
  stream: { text: "stm", cls: "text-cyan-500" },
  json: { text: "jsn", cls: "text-fuchsia-400" },
  unknown: { text: "key", cls: "text-muted" },
};

/** A bare word becomes a contains-match; anything with glob characters is passed through. */
export function normalisePattern(text: string): string {
  const t = text.trim();
  if (!t) return "*";
  return /[*?[]/.test(t) ? t : `*${t}*`;
}

export function ValkeyTree({ profile, pattern }: { profile: ConnectionProfile; pattern: string }) {
  const pid = profile.id;
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);
  const openInspector = useInspectorStore((s) => s.open);
  const { newValkeyConsole } = useTabActions();
  const { menuNode, openAt } = useMenu();
  const dbsQ = useValkeyDbs(pid);
  const [activeDb, setActiveDb] = useState<number | null>(null);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  const dbList = useMemo(() => (dbsQ.data && dbsQ.data.length > 0 ? dbsQ.data : [{ index: 0, keys: null }]), [dbsQ.data]);

  // Start on the first database that has keys (db0 on a fresh server).
  useEffect(() => {
    if (activeDb !== null || (!dbsQ.data && dbsQ.isLoading)) return;
    setActiveDb((dbList.find((d) => (d.keys ?? 0) > 0) ?? dbList[0]).index);
  }, [activeDb, dbList, dbsQ.data, dbsQ.isLoading]);

  const scanDb = activeDb !== null && activeDb >= 0 ? activeDb : null;
  const scan = useValkeyScan(pid, scanDb, pattern, profile.valkey.scanCount);
  const filtering = pattern !== "*";

  // SCAN may return empty pages with a live cursor; keep going a little so the tree isn't misleadingly empty.
  const pageCount = scan.data?.pages.length ?? 0;
  useEffect(() => {
    if (scan.hasNextPage && !scan.isFetchingNextPage && !scan.isFetching && scan.keys.length < 40 && pageCount < 25) void scan.fetchNextPage();
  }, [scan.hasNextPage, scan.isFetchingNextPage, scan.isFetching, scan.keys.length, pageCount, scan]);

  const tree = useMemo(() => buildKeyTree(scan.keys, profile.valkey.keyDelimiter), [scan.keys, profile.valkey.keyDelimiter]);
  const autoExpandAll = filtering && scan.keys.length <= 300;

  const allFolderIds = useMemo(() => {
    if (!autoExpandAll) return null;
    const ids = new Set<string>();
    const walk = (f: KeyFolder) => {
      for (const c of f.children) if (c.kind === "folder") (ids.add(c.id), walk(c));
    };
    walk(tree);
    return ids;
  }, [autoExpandAll, tree]);

  const rows = useMemo<VkRow[]>(() => {
    const out: VkRow[] = [];
    for (const d of dbList) {
      const isActive = d.index === activeDb;
      out.push({ id: `db:${d.index}`, depth: 0, kind: "db", index: d.index, keys: d.keys, expanded: isActive });
      if (!isActive) continue;
      if (scan.isError) {
        out.push({ id: "st:err", depth: 1, kind: "status", statusKind: "error", label: asError(scan.error).message });
        continue;
      }
      if (scan.isLoading) {
        out.push({ id: "st:load", depth: 1, kind: "status", statusKind: "loading", label: "Scanning keys…" });
        continue;
      }
      const flat = flattenKeyTree(tree, allFolderIds ?? expanded);
      for (const { node, depth } of flat) {
        if (node.kind === "folder") out.push({ id: node.id, depth: depth + 1, kind: "folder", node, expanded: (allFolderIds ?? expanded).has(node.id) });
        else out.push({ id: node.id, depth: depth + 1, kind: "key", node });
      }
      if (flat.length === 0 && scan.done) out.push({ id: "st:empty", depth: 1, kind: "status", statusKind: "empty", label: filtering ? "No keys match this filter" : "This database is empty" });
      if (!scan.done) out.push({ id: "more", depth: 1, kind: "more", loading: scan.isFetchingNextPage || scan.isFetching });
    }
    return out;
  }, [dbList, activeDb, scan.isError, scan.error, scan.isLoading, scan.done, scan.isFetchingNextPage, scan.isFetching, tree, expanded, allFolderIds, filtering]);

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => TREE_ROW_H,
    getItemKey: (i) => rows[i]?.id ?? i,
    overscan: 16,
  });

  const toggleFolder = useCallback((id: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  }, []);

  const inspect = useCallback(
    (key: string) => {
      if (scanDb === null) return;
      openInspector({ profileId: pid, db: scanDb, key });
    },
    [openInspector, pid, scanDb],
  );

  const activate = (row: VkRow) => {
    setSelectedId(row.id);
    if (row.kind === "db") {
      setActiveDb(row.expanded ? -1 : row.index); // -1 = every database collapsed
      setActiveTarget(pid, null);
    } else if (row.kind === "folder") toggleFolder(row.id);
    else if (row.kind === "key") inspect(row.node.key);
    else if (row.kind === "more") void scan.fetchNextPage();
  };

  const menuFor = (row: VkRow): MenuEntry[] => {
    if (row.kind === "key")
      return [
        { label: "Inspect key", onSelect: () => inspect(row.node.key) },
        { label: "Copy key", onSelect: () => void copyToClipboard(row.node.key) },
        { label: "Open console", onSelect: () => newValkeyConsole({ profileId: pid, db: scanDb ?? 0 }) },
      ];
    if (row.kind === "folder")
      return [
        { label: row.expanded ? "Collapse" : "Expand", onSelect: () => toggleFolder(row.id) },
        { label: "Copy prefix", onSelect: () => void copyToClipboard(row.node.prefix) },
      ];
    if (row.kind === "db") return [{ label: "Open console on this database", onSelect: () => newValkeyConsole({ profileId: pid, db: row.index }) }];
    return [];
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const idx = rows.findIndex((r) => r.id === selectedId);
    const cur = idx >= 0 ? rows[idx] : null;
    const go = (i: number) => {
      const c = Math.max(0, Math.min(rows.length - 1, i));
      const r = rows[c];
      if (r) {
        setSelectedId(r.id);
        virt.scrollToIndex(c);
      }
    };
    if (e.key === "ArrowDown") (e.preventDefault(), go(idx + 1));
    else if (e.key === "ArrowUp") (e.preventDefault(), go(idx < 0 ? 0 : idx - 1));
    else if (e.key === "Enter" && cur) (e.preventDefault(), activate(cur));
    else if (e.key === "ArrowRight" && cur?.kind === "folder" && !cur.expanded) (e.preventDefault(), toggleFolder(cur.id));
    else if (e.key === "ArrowLeft" && cur?.kind === "folder" && cur.expanded) (e.preventDefault(), toggleFolder(cur.id));
  };

  if (dbsQ.isError && !dbsQ.data) {
    return (
      <div className="p-2">
        <ErrorBox error={asError(dbsQ.error)} />
      </div>
    );
  }

  return (
    <div ref={scrollRef} role="tree" aria-label={`${profile.name} keys`} tabIndex={0} onKeyDown={onKeyDown} className="h-full overflow-auto outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent/40">
      <div style={{ height: virt.getTotalSize(), position: "relative", minWidth: "100%", width: "max-content" }}>
        {virt.getVirtualItems().map((item) => {
          const row = rows[item.index];
          if (!row) return null;
          const selected = row.id === selectedId;
          let el;
          switch (row.kind) {
            case "db":
              el = (
                <TreeRow
                  depth={0}
                  expandable
                  expanded={row.expanded}
                  selected={selected}
                  icon={<Database className={cn("size-3.5", row.expanded ? "text-accent" : "text-muted")} />}
                  label={`db${row.index}`}
                  right={row.keys !== null ? <span className="tabular-nums">{row.keys.toLocaleString("en-US")}</span> : undefined}
                  onClick={() => activate(row)}
                  onToggle={() => activate(row)}
                  onContextMenu={(e) => openAt(e, menuFor(row))}
                />
              );
              break;
            case "folder":
              el = (
                <TreeRow
                  depth={row.depth}
                  expandable
                  expanded={row.expanded}
                  selected={selected}
                  icon={<Folder className="size-3.5 text-muted" />}
                  label={row.node.name || "(empty)"}
                  right={<span className="tabular-nums">{row.node.count.toLocaleString("en-US")}</span>}
                  onClick={() => activate(row)}
                  onToggle={() => toggleFolder(row.id)}
                  onContextMenu={(e) => openAt(e, menuFor(row))}
                />
              );
              break;
            case "key": {
              const g = glyph[row.node.keyType ?? "unknown"];
              el = (
                <TreeRow
                  depth={row.depth}
                  selected={selected}
                  icon={<span className={cn("font-mono text-[9px] font-bold leading-none", g.cls)}>{g.text}</span>}
                  label={row.node.name || "(empty)"}
                  title={row.node.key}
                  onClick={() => activate(row)}
                  onContextMenu={(e) => {
                    setSelectedId(row.id);
                    openAt(e, menuFor(row));
                  }}
                />
              );
              break;
            }
            case "more":
              el = (
                <div style={{ paddingLeft: 6 + row.depth * 14 + 20 }} className="flex h-6 items-center">
                  <Button size="sm" variant="ghost" className="h-6 px-2 text-xs text-accent" loading={row.loading} onClick={() => void scan.fetchNextPage()}>
                    Load more keys
                  </Button>
                </div>
              );
              break;
            case "status":
              el = (
                <TreeRow
                  depth={row.depth}
                  muted
                  icon={row.statusKind === "loading" ? <Loader2 className="size-3 animate-spin" /> : row.statusKind === "error" ? <AlertCircle className="size-3 text-danger" /> : undefined}
                  label={<span className={cn("text-xs", row.statusKind === "error" && "text-danger", row.statusKind === "empty" && "italic")}>{row.label}</span>}
                  title={row.label}
                />
              );
              break;
          }
          return (
            <div key={item.key} style={{ position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${item.start}px)` }}>
              {el}
            </div>
          );
        })}
      </div>
      {menuNode}
    </div>
  );
}
