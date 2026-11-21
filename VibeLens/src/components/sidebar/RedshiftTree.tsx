// Lazy, virtualised Redshift navigator: schemas > tables and views > columns, for the connection's own
// database. Each level is fetched through the Data API once its parent is expanded (keys ['rs', profileId, ...]).
// Read-only by design: no table editor, ER diagram or EXPLAIN view (those are Postgres features).
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useQueries, useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AlertCircle, Columns3, Copy, Eye, FileCode2, Folder, FolderOpen, Loader2, RefreshCw, Table2 } from "lucide-react";
import { asError } from "../../ipc/commands";
import type { ConnectionProfile } from "../../ipc/types";
import { useUIStore } from "../../store/useUIStore";
import { rsQueries } from "../../hooks/useRedshift";
import { useTabActions } from "../../hooks/useTabActions";
import { ErrorBox } from "../ui";
import { useMenu, type MenuEntry } from "../ui/Menu";
import { copyToClipboard } from "../ui/clipboard";
import { cn } from "../../lib/cn";
import { TREE_ROW_H, TreeRow } from "./TreeRow";
import type { QueryState } from "./pgTreeModel";
import { buildRsRows, columnType, expandedRsSchemas, isViewKind, rsIsExpanded, rsQualifiedName, rsSchemaId, rsSelectAllSql, rsTableId, rsTableKey, type RsRow } from "./redshiftTreeModel";

function toState<T>(q: UseQueryResult<T> | undefined): QueryState<T> {
  if (!q) return { status: "pending" };
  if (q.isError) return { status: "error", error: asError(q.error).message };
  if (q.isSuccess) return { status: "success", data: q.data };
  return { status: "pending" };
}
const sig = (qs: UseQueryResult<unknown>[]) => qs.map((q) => `${q.status}${q.dataUpdatedAt}${q.errorUpdatedAt}`).join("|");

export function RedshiftTree({ profile, showSystem }: { profile: ConnectionProfile; showSystem: boolean }) {
  const pid = profile.id;
  // The tree browses the connection's own database (null = the profile's database on the Rust side).
  const db: string | null = null;
  const qc = useQueryClient();
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);
  const { newSqlTab } = useTabActions();
  const { menuNode, openAt } = useMenu();
  const [toggled, setToggled] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const seeded = useRef(false);

  const schemasQ = useQuery(rsQueries.schemas(pid, db));
  const expandedSchemas = useMemo(() => expandedRsSchemas(schemasQ.data, toggled), [schemasQ.data, toggled]);
  const tableQs = useQueries({ queries: expandedSchemas.map((s) => rsQueries.tables(pid, db, s)) });
  const tablesSig = sig(tableQs);

  const expandedTables = useMemo(() => {
    const out: { schema: string; name: string }[] = [];
    expandedSchemas.forEach((schema, i) => {
      for (const t of tableQs[i]?.data ?? []) if (rsIsExpanded(toggled, rsTableId(schema, t.name))) out.push({ schema, name: t.name });
    });
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expandedSchemas, tablesSig, toggled]);
  const describeQs = useQueries({ queries: expandedTables.map((t) => rsQueries.describe(pid, db, t.schema, t.name)) });
  const describeSig = sig(describeQs);

  const rows = useMemo(
    () =>
      buildRsRows({
        schemas: toState(schemasQ),
        tables: new Map(expandedSchemas.map((s, i) => [s, toState(tableQs[i])])),
        describes: new Map(expandedTables.map((t, i) => [rsTableKey(t.schema, t.name), toState(describeQs[i])])),
        toggled,
        showSystem,
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [schemasQ.status, schemasQ.dataUpdatedAt, tablesSig, describeSig, toggled, showSystem, expandedSchemas, expandedTables],
  );

  // First load: open `public` (or the only user schema) so there is something to see.
  useEffect(() => {
    if (seeded.current || !schemasQ.data) return;
    seeded.current = true;
    const user = schemasQ.data.filter((s) => !s.toLowerCase().startsWith("pg_") && s !== "information_schema");
    const pick = user.includes("public") ? "public" : user.length === 1 ? user[0] : null;
    if (pick) setToggled((m) => new Map(m).set(rsSchemaId(pick), true));
  }, [schemasQ.data]);

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => TREE_ROW_H,
    getItemKey: (i) => rows[i]?.id ?? i,
    overscan: 16,
  });

  const toggle = useCallback((row: RsRow) => {
    if (!row.expandable) return;
    setToggled((prev) => new Map(prev).set(row.id, !(prev.get(row.id) ?? false)));
  }, []);
  const select = useCallback(
    (row: RsRow) => {
      setSelectedId(row.id);
      setActiveTarget(pid, null);
    },
    [pid, setActiveTarget],
  );
  const openQuery = useCallback(
    (schema: string, name: string) => newSqlTab({ profileId: pid, database: db, title: name, contents: rsSelectAllSql(schema, name), autoRun: true }),
    [newSqlTab, pid, db],
  );
  const activate = useCallback(
    (row: RsRow) => {
      if (row.kind === "table" && row.schema && row.table) openQuery(row.schema, row.table.name);
      else toggle(row);
    },
    [openQuery, toggle],
  );
  const refresh = (row?: RsRow) => {
    if (row?.kind === "table" && row.schema && row.table) void qc.invalidateQueries({ queryKey: rsQueries.describe(pid, db, row.schema, row.table.name).queryKey });
    else if (row?.kind === "schema" && row.schema) void qc.invalidateQueries({ queryKey: rsQueries.tables(pid, db, row.schema).queryKey });
    else void qc.invalidateQueries({ queryKey: ["rs", pid] });
  };

  const menuFor = (row: RsRow): MenuEntry[] => {
    const items: MenuEntry[] = [];
    if (row.kind === "table" && row.schema && row.table) {
      const { schema } = row;
      const name = row.table.name;
      items.push({ label: "Query in new SQL tab", hint: "double-click", icon: <FileCode2 className="size-3.5" />, onSelect: () => openQuery(schema, name) });
      items.push({ label: row.expanded ? "Hide columns" : "Show columns", onSelect: () => toggle(row) });
      items.push("separator");
      items.push({ label: "Copy qualified name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(rsQualifiedName(schema, name)) });
      items.push("separator");
      items.push({ label: "Refresh", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
    } else if (row.kind === "schema" && row.schema) {
      const { schema } = row;
      items.push({ label: row.expanded ? "Collapse" : "Expand", onSelect: () => toggle(row) });
      items.push({ label: "Copy name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(schema) });
      items.push("separator");
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

  if (schemasQ.isError && !schemasQ.data) {
    return (
      <div className="space-y-2 p-2">
        <ErrorBox error={asError(schemasQ.error)} />
        <button type="button" onClick={() => void schemasQ.refetch()} className="inline-flex h-7 items-center gap-1.5 rounded-md border border-border bg-panel-2 px-2.5 text-xs font-medium hover:border-muted/60">
          <RefreshCw className="size-3.5" /> Try again
        </button>
      </div>
    );
  }

  return (
    <div ref={scrollRef} role="tree" aria-label={`${profile.name} schemas`} tabIndex={0} onKeyDown={onKeyDown} className="h-full overflow-auto outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent/40">
      <div style={{ height: virt.getTotalSize(), position: "relative", minWidth: "100%", width: "max-content" }}>
        {virt.getVirtualItems().map((item) => {
          const row = rows[item.index];
          if (!row) return null;
          return (
            <div key={item.key} style={{ position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${item.start}px)` }}>
              <RsTreeRow
                row={row}
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

function RsTreeRow({
  row,
  selected,
  onSelect,
  onToggle,
  onActivate,
  onMenu,
}: {
  row: RsRow;
  selected: boolean;
  onSelect: (r: RsRow) => void;
  onToggle: (r: RsRow) => void;
  onActivate: (r: RsRow) => void;
  onMenu: (e: React.MouseEvent, r: RsRow) => void;
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
    case "schema":
      return (
        <TreeRow
          {...common}
          icon={row.expanded ? <FolderOpen className="size-3.5 text-muted" /> : <Folder className="size-3.5 text-muted" />}
          label={row.label}
          onClick={() => {
            onSelect(row);
            onToggle(row);
          }}
        />
      );
    case "table": {
      const t = row.table;
      if (!t) return null;
      const view = isViewKind(t.kind);
      return (
        <TreeRow
          {...common}
          icon={view ? <Eye className="size-3.5 text-emerald-500" /> : <Table2 className="size-3.5 text-sky-500" />}
          label={t.name}
          right={t.kind && t.kind.toUpperCase() !== "TABLE" && t.kind.toUpperCase() !== "VIEW" ? <span className="text-[10px] uppercase">{t.kind.toLowerCase()}</span> : undefined}
          title={`${row.schema}.${t.name} (${t.kind.toLowerCase() || "table"}). Double-click to query.`}
          onClick={() => onSelect(row)}
          onDoubleClick={() => onActivate(row)}
        />
      );
    }
    case "column": {
      const c = row.column;
      if (!c) return null;
      const type = columnType(c);
      return (
        <TreeRow
          {...common}
          icon={<Columns3 className="size-3 text-muted/70" />}
          label={c.name}
          secondary={type}
          right={c.nullable ? <span title="nullable">?</span> : undefined}
          title={`${c.name} · ${type} · ${c.nullable ? "nullable" : "not null"}${c.defaultValue ? ` · default ${c.defaultValue}` : ""}`}
          onClick={() => onSelect(row)}
        />
      );
    }
  }
}
