// Lazy, virtualised Postgres navigator: databases > schemas > groups > objects > columns.
// Each level is fetched only once its parent is expanded (TanStack Query, keys ['pg', profileId, db, ...]).
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactElement } from "react";
import { useQueries, useQuery, useQueryClient, type UseQueryResult } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AlertCircle, Copy, Database, Eye, FilePlus2, FileCode2, Folder, KeyRound, Layers, ListOrdered, Link2, Loader2, Network, RefreshCw, SquareFunction, Table2, Columns3, Code2 } from "lucide-react";
import { asError } from "../../ipc/commands";
import type { ConnectionProfile, ObjectKind, SchemaObject } from "../../ipc/types";
import { useUIStore } from "../../store/useUIStore";
import { pgQueries } from "../../hooks/usePg";
import { useTabActions, quoteIdent } from "../../hooks/useTabActions";
import { ErrorBox } from "../ui";
import { TreeSsoReauth } from "../connections/SsoReauth";
import { useMenu, type MenuEntry } from "../ui/Menu";
import { copyToClipboard } from "../ui/clipboard";
import { cn } from "../../lib/cn";
import { openErdTab, openTableTab } from "../table/openTableTab";
import { generateInsert, generateSelect, generateUpdate } from "./sqlTemplates";
import { TREE_ROW_H, TreeRow } from "./TreeRow";
import {
  boundLabel,
  buildPgRows,
  collectPartitionRelations,
  compactCount,
  dbId,
  defaultExpanded,
  describeKey,
  isBrowsable,
  isExpanded,
  isRelation,
  objKey,
  objectId,
  partitionStats,
  schemaId,
  type PgRow,
  type QueryState,
  type RelationRef,
} from "./pgTreeModel";

function toState<T>(q: UseQueryResult<T>): QueryState<T> {
  if (q.isError) return { status: "error", error: asError(q.error).message };
  if (q.isSuccess) return { status: "success", data: q.data };
  return { status: "pending" };
}

const sig = (qs: UseQueryResult<unknown>[]) => qs.map((q) => `${q.status}${q.dataUpdatedAt}${q.errorUpdatedAt}`).join("|");

const kindIcon: Record<ObjectKind, ReactElement> = {
  table: <Table2 className="size-3.5 text-sky-500" />,
  partitionedTable: <Table2 className="size-3.5 text-indigo-400" />,
  foreignTable: <Link2 className="size-3.5 text-sky-400" />,
  view: <Eye className="size-3.5 text-emerald-500" />,
  materializedView: <Layers className="size-3.5 text-emerald-400" />,
  sequence: <ListOrdered className="size-3.5 text-amber-500" />,
  function: <SquareFunction className="size-3.5 text-violet-400" />,
  procedure: <SquareFunction className="size-3.5 text-fuchsia-400" />,
};

export function PgTree({ profile, showSystem }: { profile: ConnectionProfile; showSystem: boolean }) {
  const pid = profile.id;
  const qc = useQueryClient();
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);
  const { newSqlTab, openTable } = useTabActions();
  const { menuNode, openAt } = useMenu();
  const [toggled, setToggled] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const seeded = useRef({ db: false, schema: false });

  const dbsQ = useQuery(pgQueries.databases(pid));
  const currentDb = profile.database ?? dbsQ.data?.find((d) => !d.isTemplate)?.name ?? null;

  const expandedDbs = useMemo(() => (dbsQ.data ?? []).filter((d) => isExpanded(toggled, dbId(d.name))).map((d) => d.name), [dbsQ.data, toggled]);
  const schemaQs = useQueries({ queries: expandedDbs.map((db) => pgQueries.schemas(pid, db)) });

  const expandedSchemas = useMemo(() => {
    const out: { db: string; schema: string }[] = [];
    expandedDbs.forEach((db, i) => {
      for (const s of schemaQs[i]?.data ?? []) if (isExpanded(toggled, schemaId(db, s.name))) out.push({ db, schema: s.name });
    });
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expandedDbs, sig(schemaQs), toggled]);
  const objectQs = useQueries({ queries: expandedSchemas.map((e) => pgQueries.objects(pid, e.db, e.schema)) });

  const expandedTables = useMemo(() => {
    const out: { db: string; schema: string; table: string; object: SchemaObject }[] = [];
    expandedSchemas.forEach((e, i) => {
      for (const o of objectQs[i]?.data ?? []) if (!o.isPartition && isRelation(o.kind) && isExpanded(toggled, objectId(e.db, o))) out.push({ db: e.db, schema: o.schema, table: o.name, object: o });
    });
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expandedSchemas, sig(objectQs), toggled]);

  // Partition listings load only for expanded partitioned tables. The walk reads the query cache
  // directly, so a listing that just arrived reveals the next level (expanded sub-partitions, columns
  // of expanded partitions) on the following render, to any depth.
  const partitionWork: { listings: RelationRef[]; describes: RelationRef[] } = { listings: [], describes: [] };
  const getListing = (r: RelationRef) => qc.getQueryData(pgQueries.partitions(pid, r.db, r.schema, r.table).queryKey);
  for (const t of expandedTables) if (t.object.kind === "partitionedTable") collectPartitionRelations(t.db, t.object, toggled, getListing, partitionWork);
  const partitionQs = useQueries({ queries: partitionWork.listings.map((t) => pgQueries.partitions(pid, t.db, t.schema, t.table)) });
  const describeTargets: RelationRef[] = [...expandedTables.map((t) => ({ db: t.db, schema: t.schema, table: t.table })), ...partitionWork.describes];
  const describeQs = useQueries({ queries: describeTargets.map((t) => pgQueries.describe(pid, t.db, t.schema, t.table)) });

  const schemasSig = sig(schemaQs);
  const objectsSig = sig(objectQs);
  const describeSig = sig(describeQs);
  const partitionsSig = sig(partitionQs);
  const targetsKey = [...partitionWork.listings, ...describeTargets].map((t) => describeKey(t.db, t.schema, t.table)).join("|");
  const rows = useMemo(
    () =>
      buildPgRows({
        databases: toState(dbsQ),
        schemas: new Map(expandedDbs.map((db, i) => [db, toState(schemaQs[i])])),
        objects: new Map(expandedSchemas.map((e, i) => [objKey(e.db, e.schema), toState(objectQs[i])])),
        describes: new Map(describeTargets.map((t, i) => [describeKey(t.db, t.schema, t.table), toState(describeQs[i])])),
        partitions: new Map(partitionWork.listings.map((t, i) => [describeKey(t.db, t.schema, t.table), toState(partitionQs[i])])),
        toggled,
        showSystem,
        currentDb,
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [dbsQ.status, dbsQ.dataUpdatedAt, schemasSig, objectsSig, describeSig, partitionsSig, targetsKey, toggled, showSystem, currentDb, expandedDbs, expandedSchemas, expandedTables],
  );

  // First load: open the connection's own database and its main schema.
  useEffect(() => {
    if (seeded.current.db || !currentDb || !dbsQ.data) return;
    seeded.current.db = true;
    setToggled((m) => new Map(m).set(dbId(currentDb), true));
  }, [currentDb, dbsQ.data]);
  useEffect(() => {
    if (seeded.current.schema || !currentDb) return;
    const i = expandedDbs.indexOf(currentDb);
    const list = schemaQs[i]?.data;
    if (!list) return;
    seeded.current.schema = true;
    const pick = list.find((s) => s.name === "public") ?? list.find((s) => !s.isSystem);
    if (pick) setToggled((m) => new Map(m).set(schemaId(currentDb, pick.name), true));
  });

  const virt = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => TREE_ROW_H,
    getItemKey: (i) => rows[i]?.id ?? i,
    overscan: 16,
  });

  const toggle = useCallback((row: PgRow) => {
    setToggled((prev) => {
      const next = new Map(prev);
      next.set(row.id, !(prev.get(row.id) ?? defaultExpanded(row.id)));
      return next;
    });
  }, []);

  const select = useCallback(
    (row: PgRow) => {
      setSelectedId(row.id);
      if (row.db) setActiveTarget(pid, row.db);
    },
    [pid, setActiveTarget],
  );

  const activate = useCallback(
    (row: PgRow) => {
      if (row.kind === "object" && row.object && isBrowsable(row.object.kind)) openTableTab(pid, row.db ?? null, row.object.schema, row.object.name);
      else if (row.expandable) toggle(row);
    },
    [pid, toggle],
  );

  const refresh = useCallback(
    (row: PgRow) => {
      if (row.kind === "db" && row.db) void qc.invalidateQueries({ queryKey: ["pg", pid, row.db] });
      else if (row.kind === "schema" && row.db && row.schema) {
        void qc.invalidateQueries({ queryKey: ["pg", pid, row.db, "objects", row.schema] });
        void qc.invalidateQueries({ queryKey: ["pg", pid, row.db, "partitions", row.schema] });
      } else if (row.object) {
        void qc.invalidateQueries({ queryKey: ["pg", pid, row.db, "describe", row.object.schema, row.object.name] });
        if (row.object.kind === "partitionedTable") void qc.invalidateQueries({ queryKey: ["pg", pid, row.db, "partitions", row.object.schema, row.object.name] });
      }
    },
    [qc, pid],
  );

  /** Fetch the table's columns (cached with the tree) and drop a generated statement into a new SQL tab. */
  const generate = useCallback(
    async (row: PgRow, kind: "select" | "insert" | "update") => {
      const o = row.object;
      if (!o) return;
      const db = row.db ?? null;
      const title = `${o.name}-${kind}`;
      try {
        const desc = await qc.fetchQuery({ ...pgQueries.describe(pid, db, o.schema, o.name), staleTime: 60_000 });
        const contents = kind === "select" ? generateSelect(desc) : kind === "insert" ? generateInsert(desc) : generateUpdate(desc);
        newSqlTab({ profileId: pid, database: db, title, contents });
      } catch (e) {
        newSqlTab({ profileId: pid, database: db, title, contents: `-- Could not read the columns of ${o.schema}.${o.name}: ${asError(e).message}\n` });
      }
    },
    [qc, pid, newSqlTab],
  );

  const menuFor = (row: PgRow): MenuEntry[] => {
    const items: MenuEntry[] = [];
    if (row.kind === "object" && row.object) {
      const o = row.object;
      const qualified = `${quoteIdent(o.schema)}.${quoteIdent(o.name)}`;
      const db = row.db ?? null;
      if (isBrowsable(o.kind)) {
        const writable = o.kind === "table" || o.kind === "partitionedTable" || o.kind === "foreignTable";
        items.push({ label: "View data", hint: "double-click", icon: <Table2 className="size-3.5" />, onSelect: () => openTableTab(pid, db, o.schema, o.name) });
        items.push({ label: "Open in new SQL tab", icon: <FileCode2 className="size-3.5" />, onSelect: () => openTable(pid, db, o.schema, o.name) });
        items.push("separator");
        items.push({ label: "Generate SELECT", icon: <Code2 className="size-3.5" />, onSelect: () => void generate(row, "select") });
        items.push({ label: "Generate INSERT", icon: <Code2 className="size-3.5" />, disabled: !writable, onSelect: () => void generate(row, "insert") });
        items.push({ label: "Generate UPDATE", icon: <Code2 className="size-3.5" />, disabled: !writable, onSelect: () => void generate(row, "update") });
        items.push("separator");
      } else {
        items.push({ label: "New SQL tab here", icon: <FilePlus2 className="size-3.5" />, onSelect: () => newSqlTab({ profileId: pid, database: db }) });
        items.push("separator");
      }
      items.push({ label: "Copy name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(o.name) });
      items.push({ label: "Copy qualified name", icon: <Copy className="size-3.5" />, onSelect: () => void copyToClipboard(qualified) });
      if (isRelation(o.kind)) {
        items.push("separator");
        items.push({ label: `Open ER diagram of ${o.schema}`, icon: <Network className="size-3.5" />, onSelect: () => openErdTab(pid, db, o.schema) });
        items.push({ label: o.kind === "partitionedTable" ? "Refresh columns and partitions" : "Refresh columns", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
      }
    } else if (row.kind === "column" && row.column) {
      items.push({ label: "Copy column name", onSelect: () => void copyToClipboard(row.column?.name ?? "") });
      items.push({ label: "Copy as quoted identifier", onSelect: () => void copyToClipboard(quoteIdent(row.column?.name ?? "")) });
    } else if (row.kind === "schema" && row.db && row.schema) {
      const schema = row.schema;
      const db = row.db;
      items.push({ label: "Open ER diagram", icon: <Network className="size-3.5" />, onSelect: () => openErdTab(pid, db, schema) });
      items.push({ label: "Refresh", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
      items.push("separator");
      items.push({ label: row.expanded ? "Collapse" : "Expand", onSelect: () => toggle(row) });
      items.push({ label: "New SQL tab here", icon: <FilePlus2 className="size-3.5" />, onSelect: () => newSqlTab({ profileId: pid, database: db }) });
    } else if (row.kind === "db" || row.kind === "group") {
      items.push({ label: row.expanded ? "Collapse" : "Expand", onSelect: () => toggle(row) });
      items.push({ label: "New SQL tab here", icon: <FilePlus2 className="size-3.5" />, onSelect: () => newSqlTab({ profileId: pid, database: row.db ?? null }) });
      if (row.kind !== "group") items.push({ label: "Refresh", icon: <RefreshCw className="size-3.5" />, onSelect: () => refresh(row) });
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

  if (dbsQ.isError) {
    return (
      <div className="space-y-2 p-2">
        <ErrorBox error={asError(dbsQ.error)} />
        <TreeSsoReauth profile={profile} error={asError(dbsQ.error)} reload={() => void dbsQ.refetch()} />
      </div>
    );
  }

  return (
    <div ref={scrollRef} role="tree" aria-label={`${profile.name} objects`} tabIndex={0} onKeyDown={onKeyDown} className="h-full overflow-auto outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-accent/40">
      <div style={{ height: virt.getTotalSize(), position: "relative", minWidth: "100%", width: "max-content" }}>
        {virt.getVirtualItems().map((item) => {
          const row = rows[item.index];
          if (!row) return null;
          return (
            <div key={item.key} style={{ position: "absolute", top: 0, left: 0, right: 0, transform: `translateY(${item.start}px)` }}>
              <PgTreeRow row={row} selected={row.id === selectedId} onSelect={select} onToggle={toggle} onActivate={activate} onMenu={(e, r) => openAt(e, menuFor(r))} />
            </div>
          );
        })}
      </div>
      {menuNode}
    </div>
  );
}

function PgTreeRow({
  row,
  selected,
  onSelect,
  onToggle,
  onActivate,
  onMenu,
}: {
  row: PgRow;
  selected: boolean;
  onSelect: (r: PgRow) => void;
  onToggle: (r: PgRow) => void;
  onActivate: (r: PgRow) => void;
  onMenu: (e: React.MouseEvent, r: PgRow) => void;
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
          icon={<Database className={cn("size-3.5", row.isCurrentDb ? "text-accent" : "text-muted")} />}
          label={row.label}
          onClick={() => {
            onSelect(row);
            onToggle(row);
          }}
        />
      );
    case "schema":
      return (
        <TreeRow
          {...common}
          icon={<Folder className="size-3.5 text-muted" />}
          label={row.label}
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
          label={<span className="text-fg/80">{row.label}</span>}
          right={<span className="tabular-nums">{row.count}</span>}
          onClick={() => {
            onSelect(row);
            onToggle(row);
          }}
        />
      );
    case "object": {
      const o = row.object;
      if (!o) return null;
      const fn = o.kind === "function" || o.kind === "procedure";
      const part = row.partition;
      return (
        <TreeRow
          {...common}
          icon={kindIcon[o.kind]}
          label={o.name}
          secondary={fn ? o.signature : part ? boundLabel(part.bound) : undefined}
          right={part ? partitionStats(part) : !fn && o.estimatedRows !== null ? compactCount(o.estimatedRows) : undefined}
          title={part ? `${o.schema}.${o.name}\n${part.bound}` : o.comment ? `${o.name}: ${o.comment}` : o.name}
          onClick={() => onSelect(row)}
          onDoubleClick={() => onActivate(row)}
        />
      );
    }
    case "partitionSummary":
      return <TreeRow depth={row.depth} muted label={<span className="text-xs">{row.label}</span>} title={row.label} onClick={() => onSelect(row)} />;
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
          title={`${c.name} ${c.dataType} ${c.nullable ? "NULL" : "NOT NULL"}${c.isPrimaryKey ? " PRIMARY KEY" : ""}${c.default ? ` DEFAULT ${c.default}` : ""}${c.comment ? `\n${c.comment}` : ""}`}
          onClick={() => onSelect(row)}
        />
      );
    }
  }
}
