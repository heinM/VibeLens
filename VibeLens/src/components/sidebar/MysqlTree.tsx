// Lazy, virtualised MySQL navigator: databases > Tables / Views / Functions / Procedures > objects >
// columns. Each level is fetched once its parent is expanded (keys ['my', profileId, ...]). Read-only by
// design: no table editor, ER diagram or EXPLAIN view (those are Postgres features).
import { useCallback, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useQueries, useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AlertCircle, Columns3, Copy, Database, Eye, FileCode2, Folder, FolderOpen, KeyRound, Loader2, RefreshCw, SquareFunction, Table2, Workflow } from "lucide-react";
import { asError } from "../../ipc/commands";
import type { ConnectionProfile } from "../../ipc/types";
import { useUIStore } from "../../store/useUIStore";
import { myQueries } from "../../hooks/useMysql";
import { useTabActions } from "../../hooks/useTabActions";
import { ErrorBox } from "../ui";
import { TreeSsoReauth } from "../connections/SsoReauth";
import { useMenu, type MenuEntry } from "../ui/Menu";
import { copyToClipboard } from "../ui/clipboard";
import { cn } from "../../lib/cn";
import { TREE_ROW_H, TreeRow } from "./TreeRow";
import type { QueryState } from "./pgTreeModel";
import {
  buildMyRows,
  expandedMyDatabases,
  myCountSql,
  myDbId,
  myHasColumns,
  myIsExpanded,
  myObjectId,
  myQualifiedName,
  myRefreshKeys,
  mySelectAllSql,
  myShowCreateSql,
  myTableKey,
  type MyRow,
} from "./mysqlTreeModel";

function toState<T>(q: UseQueryResult<T> | undefined): QueryState<T> {
  if (!q) return { status: "pending" };
  if (q.isError) return { status: "error", error: asError(q.error).message };
  if (q.isSuccess) return { status: "success", data: q.data };
  return { status: "pending" };
}
const sig = (qs: UseQueryResult<unknown>[]) => qs.map((q) => `${q.status}${q.dataUpdatedAt}${q.errorUpdatedAt}`).join("|");

export function MysqlTree({ profile, showSystem }: { profile: ConnectionProfile; showSystem: boolean }) {
  const pid = profile.id;
  const defaultDb = profile.database?.trim() || null;
  const qc = useQueryClient();
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);
  const { newSqlTab } = useTabActions();
  const { menuNode, openAt } = useMenu();
  const [toggled, setToggled] = useState<ReadonlyMap<string, boolean>>(() => new Map(defaultDb ? [[myDbId(defaultDb), true]] : []));
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  const dbsQ = useQuery(myQueries.databases(pid));
  const openDbs = useMemo(() => expandedMyDatabases(dbsQ.data, toggled), [dbsQ.data, toggled]);
  const objectQs = useQueries({ queries: openDbs.map((d) => myQueries.objects(pid, d)) });
  const objectsSig = sig(objectQs);

  const openTables = useMemo(() => {
    const out: { db: string; name: string }[] = [];
    openDbs.forEach((db, i) => {
      for (const o of objectQs[i]?.data ?? []) if (myHasColumns(o.kind) && myIsExpanded(toggled, myObjectId(db, o))) out.push({ db, name: o.name });
    });
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [openDbs, objectsSig, toggled]);
  const describeQs = useQueries({ queries: openTables.map((t) => myQueries.describe(pid, t.db, t.name)) });
  const describeSig = sig(describeQs);

  const rows = useMemo(
    () =>
      buildMyRows({
        databases: toState(dbsQ),
        objects: new Map(openDbs.map((d, i) => [d, toState(objectQs[i])])),
        describes: new Map(openTables.map((t, i) => [myTableKey(t.db, t.name), toState(describeQs[i])])),
        toggled,
        showSystem,
        defaultDb,
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [dbsQ.status, dbsQ.dataUpdatedAt, objectsSig, describeSig, toggled, showSystem, openDbs, openTables, defaultDb],
  );

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => TREE_ROW_H,
    getItemKey: (i) => rows[i]?.id ?? i,
    overscan: 16,
  });

  const toggle = useCallback((row: MyRow) => {
    if (!row.expandable) return;
    setToggled((prev) => new Map(prev).set(row.id, !myIsExpanded(prev, row.id)));
  }, []);
  const select = useCallback(
    (row: MyRow) => {
      setSelectedId(row.id);
      // New SQL tabs open in the database of the selected node.
      setActiveTarget(pid, row.db ?? null);
    },
    [pid, setActiveTarget],
  );
  const openSql = useCallback(
    (db: string, title: string, contents: string, autoRun: boolean) => newSqlTab({ profileId: pid, database: db, title, contents, autoRun }),
    [newSqlTab, pid],
  );
  const activate = useCallback(
    (row: MyRow) => {
      if (row.kind === "object" && row.db && row.object && myHasColumns(row.object.kind)) openSql(row.db, row.object.name, mySelectAllSql(row.db, row.object.name), true);
      else toggle(row);
    },
    [openSql, toggle],
  );
  const refresh = (row?: MyRow) => {
    for (const queryKey of myRefreshKeys(pid, row)) void qc.invalidateQueries({ queryKey });
  };

  const menuFor = (row: MyRow): MenuEntry[] => {
    const items: MenuEntry[] = [];
    if (row.kind === "object" && row.db && row.object) {
      const { db } = row;
      const o = row.object;
      if (myHasColumns(o.kind)) {
        items.push({ label: "Query in new SQL tab", hint: "double-click", icon: <FileCode2 className="size-3.5" />, onSelect: () => openSql(db, o.name, mySelectAllSql(db, o.name), true) });
        items.push({ label: "Count rows", onSelect: () => openSql(db, o.name, myCountSql(db, o.name), true) });
        items.push({ label: row.expanded ? "Hide columns" : "Show columns", onSelect: () => toggle(row) });
      }
      items.push({ label: "Show CREATE statement", onSelect: () => openSql(db, o.name, myShowCreateSql(db, o.name, o.kind), true) });
      items.push("separator");
      items.push({ label: "Copy qualified name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(myQualifiedName(db, o.name)) });
      items.push("separator");
      items.push({ label: "Refresh", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
    } else if (row.kind === "db" && row.db) {
      const { db } = row;
      items.push({ label: "New SQL tab here", icon: <FileCode2 className="size-3.5" />, onSelect: () => openSql(db, `${db}.sql`, "", false) });
      items.push({ label: row.expanded ? "Collapse" : "Expand", onSelect: () => toggle(row) });
      items.push({ label: "Copy name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(db) });
      items.push("separator");
      items.push({ label: "Refresh", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
    } else if (row.kind === "group") {
      items.push({ label: row.expanded ? "Collapse" : "Expand", onSelect: () => toggle(row) });
      items.push({ label: "Refresh", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
    } else if (row.kind === "column" && row.column) {
      const name = row.column.name;
      items.push({ label: "Copy column name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(name) });
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

  if (dbsQ.isError && !dbsQ.data) {
    return (
      <div className="space-y-2 p-2">
        <ErrorBox error={asError(dbsQ.error)} />
        <TreeSsoReauth profile={profile} error={asError(dbsQ.error)} reload={() => void dbsQ.refetch()} />
        <button type="button" onClick={() => void dbsQ.refetch()} className="inline-flex h-7 items-center gap-1.5 rounded-md border border-border bg-panel-2 px-2.5 text-xs font-medium hover:border-muted/60">
          <RefreshCw className="size-3.5" /> Try again
        </button>
      </div>
    );
  }

  return (
    <div ref={scrollRef} role="tree" aria-label={`${profile.name} databases`} tabIndex={0} onKeyDown={onKeyDown} className="h-full overflow-auto outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent/40">
      <div style={{ height: virt.getTotalSize(), position: "relative", minWidth: "100%", width: "max-content" }}>
        {virt.getVirtualItems().map((item) => {
          const row = rows[item.index];
          if (!row) return null;
          return (
            <div key={item.key} style={{ position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${item.start}px)` }}>
              <MyTreeRow
                row={row}
                isDefault={row.kind === "db" && row.db === defaultDb}
                selected={row.id === selectedId}
                onSelect={select}
                onToggle={toggle}
                onActivate={activate}
                onMenu={(e, r) => {
                  const items = menuFor(r);
                  if (items.length) openAt(e, items);
                }}
              />
            </div>
          );
        })}
      </div>
      {menuNode}
    </div>
  );
}

function MyTreeRow({
  row,
  isDefault,
  selected,
  onSelect,
  onToggle,
  onActivate,
  onMenu,
}: {
  row: MyRow;
  isDefault: boolean;
  selected: boolean;
  onSelect: (r: MyRow) => void;
  onToggle: (r: MyRow) => void;
  onActivate: (r: MyRow) => void;
  onMenu: (e: React.MouseEvent, r: MyRow) => void;
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
    case "db":
      return (
        <TreeRow
          {...common}
          icon={<Database className="size-3.5 text-muted" />}
          label={row.label}
          right={isDefault ? <span className="text-[10px]">default</span> : undefined}
          onClick={() => {
            onSelect(row);
            onToggle(row);
          }}
        />
      );
    case "group":
      return (
        <TreeRow
          {...common}
          icon={row.expanded ? <FolderOpen className="size-3.5 text-muted" /> : <Folder className="size-3.5 text-muted" />}
          label={row.label}
          right={row.count != null ? <span>{row.count}</span> : undefined}
          onClick={() => {
            onSelect(row);
            onToggle(row);
          }}
        />
      );
    case "object": {
      const o = row.object;
      if (!o) return null;
      const icon =
        o.kind === "view" ? (
          <Eye className="size-3.5 text-emerald-500" />
        ) : o.kind === "function" ? (
          <SquareFunction className="size-3.5 text-amber-500" />
        ) : o.kind === "procedure" ? (
          <Workflow className="size-3.5 text-amber-500" />
        ) : (
          <Table2 className="size-3.5 text-sky-500" />
        );
      return (
        <TreeRow
          {...common}
          icon={icon}
          label={o.name}
          title={`${row.db}.${o.name} (${o.kind})${o.comment ? `: ${o.comment}` : ""}${myHasColumns(o.kind) ? ". Double-click to query." : ""}`}
          onClick={() => onSelect(row)}
          onDoubleClick={() => onActivate(row)}
        />
      );
    }
    case "column": {
      const c = row.column;
      if (!c) return null;
      return (
        <TreeRow
          {...common}
          icon={c.isPrimaryKey ? <KeyRound className="size-3 text-amber-500" /> : <Columns3 className="size-3 text-muted/70" />}
          label={c.name}
          secondary={c.dataType}
          right={c.nullable ? <span title="nullable">?</span> : undefined}
          title={`${c.name} · ${c.dataType} · ${c.nullable ? "nullable" : "not null"}${c.isPrimaryKey ? " · primary key" : ""}${c.default != null ? ` · default ${c.default}` : ""}`}
          onClick={() => onSelect(row)}
        />
      );
    }
  }
}
