import { useMemo, useState } from "react";
import { Search } from "lucide-react";
import type { IcebergTableInfo } from "../../ipc/types";
import { Chip } from "../ui/wsKit";
import { formatDateTime, groupDigits, humanBytes, relativeTime } from "./iceFormat";
import { opTone, partitionRows, sortRows, tableTotals } from "./iceTableModel";
import { Card, CopyValue, FormatBadge, KvRow, PaneHeader, Stat } from "./icePrimitives";
import type { PaneId } from "./IcebergTableTab";

export function OverviewPane({ info, onGo }: { info: IcebergTableInfo; onGo: (p: PaneId) => void }) {
  const totals = tableTotals(info);
  const current = info.snapshots.find((s) => s.snapshotId === info.currentSnapshotId);
  const props = useMemo(() => Object.entries(info.properties).sort(([a], [b]) => a.localeCompare(b)), [info.properties]);
  const [filter, setFilter] = useState("");
  const shown = filter.trim() ? props.filter(([k, v]) => `${k} ${v}`.toLowerCase().includes(filter.trim().toLowerCase())) : props;
  const parts = partitionRows(info);
  const sorts = sortRows(info);
  const isView = info.kind === "view";
  const delTotal = totals ? (totals.deleteFiles ?? 0) : 0;

  return (
    <div>
      <PaneHeader title="Overview" subtitle={isView ? "An Iceberg view: a stored query over other tables." : "Where this table lives and what its current snapshot holds."} />

      {!isView && (
        <Card className="mb-4 grid grid-cols-2 gap-px overflow-hidden !border-0 bg-border ring-1 ring-border @[720px]:grid-cols-4">
          {totals ? (
            <>
              <Stat label="Records" value={groupDigits(totals.records)} sub={current ? `as of ${relativeTime(current.timestampMs)}` : undefined} />
              <Stat label="Data files" value={groupDigits(totals.dataFiles)} sub={totals.dataFiles && totals.records ? `~${groupDigits(Math.round(totals.records / totals.dataFiles))} records per file` : undefined} />
              <Stat label="Size on storage" value={humanBytes(totals.sizeBytes)} sub={totals.dataFiles && totals.sizeBytes ? `~${humanBytes(totals.sizeBytes / totals.dataFiles)} per file` : undefined} />
              <Stat
                label="Delete files"
                value={groupDigits(totals.deleteFiles)}
                sub={delTotal > 0 ? `${groupDigits(totals.positionDeletes)} position, ${groupDigits(totals.equalityDeletes)} equality deletes` : "Nothing to merge on read"}
              />
            </>
          ) : (
            <div className="col-span-full bg-panel px-4 py-3 text-[12px] text-muted">No snapshot yet. This table has no data until the first commit.</div>
          )}
        </Card>
      )}

      <Card className="mb-5">
        <dl>
          <KvRow label="Format">
            <span className="inline-flex items-center gap-2">
              <FormatBadge version={info.formatVersion} />
              <span className="text-muted">{isView ? "view" : "table"}</span>
            </span>
          </KvRow>
          <KvRow label="Location">
            <CopyValue value={info.location} label="location" />
          </KvRow>
          <KvRow label="Metadata file" hint="The metadata.json the catalog currently points to">
            {info.metadataLocation ? <CopyValue value={info.metadataLocation} label="metadata location" /> : <span className="text-muted">Not reported by this catalog</span>}
          </KvRow>
          {info.tableUuid && (
            <KvRow label="Table UUID">
              <CopyValue value={info.tableUuid} label="table UUID" />
            </KvRow>
          )}
          <KvRow label="Last updated">
            {info.lastUpdatedMs !== null ? (
              <span className="tabular-nums">
                {formatDateTime(info.lastUpdatedMs)} <span className="text-muted">· {relativeTime(info.lastUpdatedMs)}</span>
              </span>
            ) : (
              <span className="text-muted">–</span>
            )}
          </KvRow>
          {!isView && (
            <KvRow label="Current snapshot">
              {current ? (
                <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
                  <button type="button" onClick={() => onGo("snapshots")} className="font-mono text-[12px] text-accent hover:underline" title="Show in the snapshot history">
                    {current.snapshotId}
                  </button>
                  <Chip tone={opTone(current.operation)}>{current.operation ?? "unknown"}</Chip>
                  <span className="tabular-nums text-muted">{formatDateTime(current.timestampMs)}</span>
                </span>
              ) : (
                <span className="text-muted">None</span>
              )}
            </KvRow>
          )}
          <KvRow label="Schema">
            <span className="flex flex-wrap items-center gap-x-2">
              <span>id {info.currentSchemaId}</span>
              <span className="text-muted">·</span>
              <button type="button" onClick={() => onGo("schema")} className="text-accent hover:underline">
                {info.schemaCount} {info.schemaCount === 1 ? "version" : "versions"} in history
              </button>
            </span>
          </KvRow>
          {!isView && (
            <>
              <KvRow label="Partitioned by">
                {parts.length ? (
                  <button type="button" onClick={() => onGo("layout")} className="text-left font-mono text-[12px] hover:text-accent" title="Show partitioning and sort order">
                    {parts.map((p) => p.expression).join(", ")}
                  </button>
                ) : (
                  <span className="text-muted">Not partitioned</span>
                )}
              </KvRow>
              <KvRow label="Sorted by">
                {sorts.length ? <span className="font-mono text-[12px]">{sorts.map((s) => `${s.expression} ${s.direction}`).join(", ")}</span> : <span className="text-muted">No sort order</span>}
              </KvRow>
            </>
          )}
        </dl>
      </Card>

      <PaneHeader
        title="Properties"
        subtitle={props.length ? `${props.length} set on this table` : undefined}
        actions={
          props.length > 8 ? (
            <label className="flex h-7 w-52 items-center gap-1.5 rounded-md border border-border bg-bg px-2 focus-within:border-accent/70">
              <Search className="size-3.5 text-muted" />
              <input value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter properties" placeholder="Filter" className="min-w-0 flex-1 bg-transparent text-xs outline-none placeholder:text-muted/70" />
            </label>
          ) : undefined
        }
      />
      {props.length === 0 ? (
        <Card className="px-4 py-5 text-[12px] text-muted">No properties set. Engines fall back to their defaults, such as write.format.default = parquet.</Card>
      ) : (
        <Card className="overflow-hidden">
          <table className="w-full table-fixed text-[12px]">
            <tbody>
              {shown.map(([k, v]) => (
                <tr key={k} className="border-b border-border/70 last:border-b-0">
                  <th scope="row" className="w-[42%] break-all px-3.5 py-1.5 text-left align-top font-mono font-normal text-muted">
                    {k}
                  </th>
                  <td className="selectable break-all px-3.5 py-1.5 align-top font-mono text-fg">{v}</td>
                </tr>
              ))}
              {shown.length === 0 && (
                <tr>
                  <td className="px-3.5 py-4 text-center text-muted">No property matches “{filter}”.</td>
                </tr>
              )}
            </tbody>
          </table>
        </Card>
      )}
    </div>
  );
}
