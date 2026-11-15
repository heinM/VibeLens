// Lazy, virtualised Iceberg navigator: catalog > namespaces (nested) > tables and views > fields (nested).
// Each level is fetched once its parent is expanded (TanStack Query, keys ['ice', profileId, ...]).
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useQueries, useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AlertCircle, Copy, Eye, FileCode2, Folder, FolderOpen, Loader2, RefreshCw, Table2, TableProperties, Braces, Columns3, List, Rows3 } from "lucide-react";
import { asError } from "../../ipc/commands";
import type { ConnectionProfile, IcebergField, IcebergNamespace } from "../../ipc/types";
import { useUIStore } from "../../store/useUIStore";
import { useWorkspaceStore } from "../../store/useWorkspaceStore";
import { ErrorBox } from "../ui";
import { useMenu, type MenuEntry } from "../ui/Menu";
import { copyToClipboard } from "../ui/clipboard";
import { cn } from "../../lib/cn";
import { requestAutoRun } from "../editor/autoRun";
import { IcebergGlyph } from "../iceberg/IcebergGlyph";
import { compactNumber } from "../iceberg/iceFormat";
import { iceQueries } from "../iceberg/iceQueries";
import { qualifiedName, selectAllSql } from "../iceberg/iceSql";
import { joinNamespace, openIcebergTableTab } from "../iceberg/openIcebergTab";
import { catalogSummary } from "../connections/icebergForm";
import { TREE_ROW_H, TreeRow } from "./TreeRow";
import { buildIceRows, expandedNamespaces, isExpanded, nsId, partitionSummary, shortType, tableId, tableKey, totalRecords, type IceRow } from "./icebergTreeModel";
import type { QueryState } from "./pgTreeModel";

function toState<T>(q: UseQueryResult<T> | undefined): QueryState<T> {
  if (!q) return { status: "pending" };
  if (q.isError) return { status: "error", error: asError(q.error).message };
  if (q.isSuccess) return { status: "success", data: q.data };
  return { status: "pending" };
}
const sig = (qs: UseQueryResult<unknown>[]) => qs.map((q) => `${q.status}${q.dataUpdatedAt}${q.errorUpdatedAt}`).join("|");

/** Opens a SQL tab bound to the Iceberg connection and runs it. (`useTabActions.newSqlTab` is Postgres-only.) */
export function openIcebergSqlTab(profileId: string, title: string, contents: string): string {
  const id = useWorkspaceStore.getState().openTab({ kind: "sql", title, profileId, database: null, contents });
  requestAutoRun(id);
  return id;
}

function fieldIcon(f: IcebergField) {
  const t = f.typeName;
  if (t.startsWith("struct")) return <Braces className="size-3 text-violet-400" />;
  if (t.startsWith("list")) return <List className="size-3 text-amber-500" />;
  if (t.startsWith("map")) return <Rows3 className="size-3 text-amber-500" />;
  return <Columns3 className="size-3 text-muted/70" />;
}

export function IcebergTree({ profile }: { profile: ConnectionProfile }) {
  const pid = profile.id;
  const qc = useQueryClient();
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);
  const { menuNode, openAt } = useMenu();
  const [toggled, setToggled] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const seeded = useRef(false);

  const ice = profile.iceberg;
  const catalogLabel = ice ? catalogSummary(ice.catalog) : "Catalog";

  const topQ = useQuery(iceQueries.namespaces(pid, []));

  // Walk the expanded part of the tree, level by level, so each expanded namespace fetches its own children.
  // (Depth is bounded by what the user opened; a child map built from the previous render drives the next.)
  const [childMapSig, setChildMapSig] = useState(0);
  const childMapRef = useRef(new Map<string, IcebergNamespace[] | undefined>());
  const expandedNs = useMemo(
    () => expandedNamespaces(topQ.data, childMapRef.current, toggled),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [topQ.data, toggled, childMapSig],
  );
  const childQs = useQueries({ queries: expandedNs.map((ns) => iceQueries.namespaces(pid, ns)) });
  const tableQs = useQueries({ queries: expandedNs.map((ns) => iceQueries.tables(pid, ns)) });
  const childSig = sig(childQs);
  useEffect(() => {
    let changed = false;
    expandedNs.forEach((ns, i) => {
      const k = joinNamespace(ns);
      const data = childQs[i]?.data;
      if (childMapRef.current.get(k) !== data) {
        childMapRef.current.set(k, data);
        changed = true;
      }
    });
    if (changed) setChildMapSig((n) => n + 1);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [childSig, expandedNs]);

  // Tables whose metadata we want: those the user expanded, plus the ones on screen (so the P chip and row count show).
  const [visibleKeys, setVisibleKeys] = useState<{ ns: string[]; name: string }[]>([]);
  const expandedTables = useMemo(() => {
    const out: { ns: string[]; name: string }[] = [];
    expandedNs.forEach((ns, i) => {
      for (const t of tableQs[i]?.data ?? []) if (isExpanded(toggled, tableId(ns, t.name))) out.push({ ns, name: t.name });
    });
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expandedNs, sig(tableQs), toggled]);
  const wanted = useMemo(() => {
    const seen = new Set<string>();
    return [...expandedTables, ...visibleKeys].filter((t) => {
      const k = tableKey(t.ns, t.name);
      if (seen.has(k)) return false;
      seen.add(k);
      return true;
    });
  }, [expandedTables, visibleKeys]);
  const describeQs = useQueries({ queries: wanted.map((t) => iceQueries.describe(pid, t.ns, t.name)) });

  const tablesSig = sig(tableQs);
  const describeSig = sig(describeQs);
  const rows = useMemo(() => {
    const children = new Map<string, QueryState<IcebergNamespace[]>>();
    const tables = new Map<string, QueryState<import("../../ipc/types").IcebergTableRef[]>>();
    expandedNs.forEach((ns, i) => {
      const k = joinNamespace(ns);
      children.set(k, toState(childQs[i]));
      tables.set(k, toState(tableQs[i]));
    });
    const describes = new Map<string, QueryState<import("../../ipc/types").IcebergTableInfo>>();
    wanted.forEach((t, i) => describes.set(tableKey(t.ns, t.name), toState(describeQs[i])));
    return buildIceRows({ catalogLabel, topLevel: toState(topQ), children, tables, describes, toggled });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [catalogLabel, topQ.status, topQ.dataUpdatedAt, childSig, tablesSig, describeSig, toggled, expandedNs, wanted]);

  // First load: open the default namespace (or the only namespace) so the tree isn't a single closed row.
  useEffect(() => {
    if (seeded.current || !topQ.data) return;
    seeded.current = true;
    const want = ice?.defaultNamespace?.split(".").filter(Boolean);
    const top = topQ.data.find((n) => want && n.levels[0] === want[0]) ?? (topQ.data.length === 1 ? topQ.data[0] : undefined);
    if (top) setToggled((m) => new Map(m).set(nsId(top.levels), true));
  }, [topQ.data, ice?.defaultNamespace]);

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => TREE_ROW_H,
    getItemKey: (i) => rows[i]?.id ?? i,
    overscan: 16,
  });

  // Debounced "what is on screen" -> metadata prefetch. A fling through a long list fetches nothing until it stops.
  const items = virt.getVirtualItems();
  const onScreen = items.map((it) => rows[it.index]).filter((r): r is IceRow => !!r && r.kind === "table" && !!r.namespace && !!r.table);
  const onScreenSig = onScreen.map((r) => r.id).join("|");
  useEffect(() => {
    const t = window.setTimeout(() => {
      const next = onScreen.slice(0, 40).map((r) => ({ ns: r.namespace as string[], name: (r.table as { name: string }).name }));
      setVisibleKeys((prev) => (prev.length === next.length && prev.every((p, i) => p.name === next[i].name && joinNamespace(p.ns) === joinNamespace(next[i].ns)) ? prev : next));
    }, 300);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [onScreenSig]);

  const toggle = useCallback((row: IceRow) => {
    setToggled((prev) => {
      const next = new Map(prev);
      next.set(row.id, !isExpanded(prev, row.id));
      return next;
    });
  }, []);

  const select = useCallback(
    (row: IceRow) => {
      setSelectedId(row.id);
      setActiveTarget(pid, null);
    },
    [pid, setActiveTarget],
  );

  const openTable = useCallback((row: IceRow) => row.table && row.namespace && row.kind === "table" && openIcebergTableTab(pid, row.namespace, row.table.name), [pid]);

  const activate = useCallback(
    (row: IceRow) => {
      if (row.kind === "table") openTable(row);
      else if (row.expandable) toggle(row);
    },
    [openTable, toggle],
  );

  const refresh = useCallback(
    (row: IceRow) => {
      if (row.kind === "catalog") void qc.invalidateQueries({ queryKey: ["ice", pid] });
      else if (row.kind === "namespace" && row.namespace) {
        const k = joinNamespace(row.namespace);
        void qc.invalidateQueries({ queryKey: ["ice", pid, "ns", k] });
        void qc.invalidateQueries({ queryKey: ["ice", pid, "tables", k] });
      } else if (row.kind === "table" && row.namespace && row.table) void qc.invalidateQueries({ queryKey: ["ice", pid, "describe", joinNamespace(row.namespace), row.table.name] });
    },
    [qc, pid],
  );

  const menuFor = (row: IceRow): MenuEntry[] => {
    const items: MenuEntry[] = [];
    if (row.kind === "table" && row.table && row.namespace) {
      const ns = row.namespace;
      const name = row.table.name;
      items.push({ label: "Open table", hint: "double-click", icon: <TableProperties className="size-3.5" />, onSelect: () => openTable(row) });
      items.push({ label: "Query in new SQL tab", icon: <FileCode2 className="size-3.5" />, onSelect: () => openIcebergSqlTab(pid, name, selectAllSql(ns, name)) });
      items.push("separator");
      items.push({ label: "Copy qualified name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(qualifiedName(ns, name)) });
      items.push("separator");
      items.push({ label: "Refresh", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
    } else if (row.kind === "namespace" && row.namespace) {
      const ns = row.namespace;
      items.push({ label: row.expanded ? "Collapse" : "Expand", onSelect: () => toggle(row) });
      items.push({ label: "Copy name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(ns.map((l) => qualifiedName([], l)).join(".")) });
      items.push("separator");
      items.push({ label: "Refresh", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
    } else if (row.kind === "field" && row.field) {
      items.push({ label: "Copy field name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(row.field?.name ?? "") });
      items.push({ label: "Copy field path", onSelect: () => void copyToClipboard(row.fieldPath ?? "") });
    } else if (row.kind === "catalog") {
      items.push({ label: "Refresh", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
    }
    return items;
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const idx = rows.findIndex((r) => r.id === selectedId);
    const cur = idx >= 0 ? rows[idx] : null;
    const go = (i: number) => {
      const clamped = Math.max(0, Math.min(rows.length - 1, i));
      const r = rows[clamped];
      if (r) {
        select(r);
        virt.scrollToIndex(clamped);
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
        if (cur?.expandable && !cur.expanded) {
          e.preventDefault();
          toggle(cur);
        } else go(idx + 1);
        break;
      case "ArrowLeft":
        if (cur?.expandable && cur.expanded) {
          e.preventDefault();
          toggle(cur);
        } else if (cur) {
          for (let i = idx - 1; i >= 0; i--) {
            if (rows[i].depth < cur.depth) {
              go(i);
              break;
            }
          }
        }
        break;
      case "Enter":
        if (cur) {
          e.preventDefault();
          activate(cur);
        }
        break;
    }
  };

  if (topQ.isError && !topQ.data) {
    return (
      <div className="space-y-2 p-2">
        <ErrorBox error={asError(topQ.error)} />
        <button type="button" onClick={() => void topQ.refetch()} className="inline-flex h-7 items-center gap-1.5 rounded-md border border-border bg-panel-2 px-2.5 text-xs font-medium hover:border-muted/60">
          <RefreshCw className="size-3.5" /> Try again
        </button>
      </div>
    );
  }

  return (
    <div ref={scrollRef} role="tree" aria-label={`${profile.name} catalog`} tabIndex={0} onKeyDown={onKeyDown} className="h-full overflow-auto outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent/40">
      <div style={{ height: virt.getTotalSize(), position: "relative", minWidth: "100%", width: "max-content" }}>
        {items.map((item) => {
          const row = rows[item.index];
          if (!row) return null;
          return (
            <div key={item.key} style={{ position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${item.start}px)` }}>
              <IceTreeRow row={row} selected={row.id === selectedId} onSelect={select} onToggle={toggle} onActivate={activate} onMenu={(e, r) => openAt(e, menuFor(r))} />
            </div>
          );
        })}
      </div>
      {menuNode}
    </div>
  );
}

/** Small "P" chip for partitioned tables. The tooltip spells out the partition spec. */
function PartitionChip({ title }: { title: string }) {
  return (
    <span title={title} className="grid h-3.5 min-w-3.5 place-items-center rounded-[3px] border border-accent/40 bg-accent/10 px-[3px] font-mono text-[9px] font-semibold leading-none text-accent">
      P
    </span>
  );
}

function IceTreeRow({
  row,
  selected,
  onSelect,
  onToggle,
  onActivate,
  onMenu,
}: {
  row: IceRow;
  selected: boolean;
  onSelect: (r: IceRow) => void;
  onToggle: (r: IceRow) => void;
  onActivate: (r: IceRow) => void;
  onMenu: (e: React.MouseEvent, r: IceRow) => void;
}) {
  const common = {
    depth: row.depth,
    expandable: row.expandable,
    expanded: row.expanded,
    selected,
    onToggle: () => onToggle(row),
    onContextMenu: (e: React.MouseEvent) => {
      onSelect(row);
      onMenu(e, row);
    },
  };
  const clickToggle = () => {
    onSelect(row);
    onToggle(row);
  };
  switch (row.kind) {
    case "status":
      return (
        <TreeRow
          depth={row.depth}
          muted
          icon={row.statusKind === "loading" ? <Loader2 className="size-3 animate-spin" /> : row.statusKind === "error" ? <AlertCircle className="size-3 text-danger" /> : undefined}
          label={<span className={cn("text-xs", row.statusKind === "error" && "text-danger", row.statusKind === "empty" && "italic")}>{row.label}</span>}
          title={row.label}
        />
      );
    case "catalog":
      return <TreeRow {...common} icon={<IcebergGlyph className="size-3.5 text-accent" />} label={<span className="font-medium">{row.label}</span>} title={`Catalog: ${row.label}`} onClick={clickToggle} />;
    case "namespace":
      return <TreeRow {...common} icon={row.expanded ? <FolderOpen className="size-3.5 text-muted" /> : <Folder className="size-3.5 text-muted" />} label={row.label} onClick={clickToggle} />;
    case "table": {
      const t = row.table;
      if (!t) return null;
      const view = t.kind === "view";
      const info = row.info;
      const parts = info ? partitionSummary(info) : "";
      const count = info ? totalRecords(info) : null;
      return (
        <TreeRow
          {...common}
          icon={view ? <Eye className="size-3.5 text-emerald-500" /> : <Table2 className="size-3.5 text-sky-500" />}
          label={t.name}
          right={
            <>
              {parts && <PartitionChip title={`Partitioned by ${parts}`} />}
              {count !== null && <span className="tabular-nums" title={`${count.toLocaleString("en-US")} rows in the current snapshot`}>{compactNumber(count)}</span>}
            </>
          }
          title={view ? `${t.name} (view)` : t.name}
          onClick={() => onSelect(row)}
          onDoubleClick={() => onActivate(row)}
        />
      );
    }
    case "field": {
      const f = row.field;
      if (!f) return null;
      return (
        <TreeRow
          {...common}
          icon={fieldIcon(f)}
          label={f.name}
          secondary={shortType(f)}
          right={
            <>
              <span className="tabular-nums text-muted/70" title="Field ID">#{f.id}</span>
              {!f.required && <span title="optional (nullable)">?</span>}
            </>
          }
          title={`${row.fieldPath} · ${f.typeName} · ${f.required ? "required" : "optional"} · field id ${f.id}${f.doc ? `\n${f.doc}` : ""}`}
          onClick={() => onSelect(row)}
        />
      );
    }
  }
}
