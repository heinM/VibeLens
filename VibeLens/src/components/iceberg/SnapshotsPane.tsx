import { useMemo, useState } from "react";
import { Check, ChevronDown, ChevronRight, Clock, Copy, GitBranch, Tag } from "lucide-react";
import type { IcebergRef, IcebergSnapshot, IcebergTableInfo } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { Button } from "../ui";
import { Chip } from "../ui/wsKit";
import { copyToClipboard } from "../ui/clipboard";
import { formatDateTime, groupDigits, relativeTime, signed } from "./iceFormat";
import { opTone, refsBySnapshot, snapshotDelta, type OpTone } from "./iceTableModel";
import { selectAsOfSnapshotSql } from "./iceSql";
import { InlineEmpty, PaneHeader } from "./icePrimitives";

const PAGE = 40;
const DOT: Record<OpTone, string> = { ok: "bg-ok border-ok", warn: "bg-warn border-warn", danger: "bg-danger border-danger", muted: "bg-muted border-muted" };

/** Newest-first timeline of table history. Each entry can open a time-travel query or hand over its id. */
export function SnapshotsPane({ info, onQueryAsOf }: { info: IcebergTableInfo; onQueryAsOf: (snapshotId: string, sql: string) => void }) {
  const [shown, setShown] = useState(PAGE);
  const byRef = useMemo(() => refsBySnapshot(info.refs), [info.refs]);
  const list = info.snapshots;

  return (
    <div>
      <PaneHeader
        title="Snapshots"
        subtitle={list.length ? `${list.length} ${list.length === 1 ? "snapshot" : "snapshots"} of history. Each commit adds one; you can query the table as it was at any of them.` : undefined}
      />
      {list.length === 0 ? (
        <InlineEmpty>No snapshots yet. The first write to this table creates one.</InlineEmpty>
      ) : (
        <ol className="relative">
          {list.slice(0, shown).map((s, i) => (
            <SnapshotItem
              key={s.snapshotId}
              s={s}
              current={s.snapshotId === info.currentSnapshotId}
              last={i === Math.min(shown, list.length) - 1 && shown >= list.length}
              refs={byRef.get(s.snapshotId) ?? []}
              onQueryAsOf={() => onQueryAsOf(s.snapshotId, selectAsOfSnapshotSql(info.namespace, info.name, s.snapshotId))}
            />
          ))}
        </ol>
      )}
      {shown < list.length && (
        <div className="pl-7">
          <Button size="sm" onClick={() => setShown((n) => n + PAGE)}>
            Show {Math.min(PAGE, list.length - shown)} older snapshots
          </Button>
        </div>
      )}
    </div>
  );
}

function RefChip({ r }: { r: IcebergRef }) {
  const tag = r.kind.toLowerCase() === "tag";
  return (
    <Chip tone="accent" icon={tag ? <Tag className="size-3" /> : <GitBranch className="size-3" />} title={`${tag ? "Tag" : "Branch"} ${r.name}`}>
      {r.name}
    </Chip>
  );
}

function SnapshotItem({ s, current, last, refs, onQueryAsOf }: { s: IcebergSnapshot; current: boolean; last: boolean; refs: IcebergRef[]; onQueryAsOf: () => void }) {
  const [open, setOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const d = snapshotDelta(s);
  const tone = opTone(s.operation);
  const summary = Object.entries(s.summary).filter(([k]) => k !== "operation");

  const copy = async () => {
    if (await copyToClipboard(s.snapshotId)) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1400);
    }
  };

  return (
    <li className="relative pb-3 pl-7">
      {!last && <span aria-hidden className="absolute bottom-0 left-[6px] top-4 w-px bg-border" />}
      <span aria-hidden className={cn("absolute left-0 top-[13px] size-[13px] rounded-full border-2 bg-bg", DOT[tone].split(" ")[1], current && DOT[tone].split(" ")[0])} />
      <div className={cn("rounded-lg border bg-panel", current ? "border-accent/50" : "border-border")}>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1.5 px-3 py-2">
          <Chip tone={tone}>{s.operation ?? "unknown"}</Chip>
          <span className="inline-flex items-center gap-1 text-[12.5px] tabular-nums text-fg">
            <Clock className="size-3 text-muted" />
            {formatDateTime(s.timestampMs)}
          </span>
          <span className="text-[12px] text-muted">{relativeTime(s.timestampMs)}</span>
          {current && <Chip tone="accent">current</Chip>}
          {refs.map((r) => (
            <RefChip key={`${r.kind}:${r.name}`} r={r} />
          ))}
          <span className="ml-auto flex items-center gap-1.5">
            <Button size="sm" variant="secondary" onClick={onQueryAsOf} title="Open a SQL tab that reads the table as of this snapshot">
              Query as of this snapshot
            </Button>
            <Button size="sm" variant="ghost" onClick={() => void copy()} aria-label={`Copy snapshot id ${s.snapshotId}`}>
              {copied ? <Check className="size-3.5 text-ok" /> : <Copy className="size-3.5" />} {copied ? "Copied" : "Copy snapshot id"}
            </Button>
          </span>
        </div>

        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-border/70 px-3 py-1.5 text-[12px]">
          <span className="selectable font-mono text-[11.5px] text-muted" title="Snapshot id">
            {s.snapshotId}
          </span>
          <span className="ml-auto flex flex-wrap items-center gap-x-3 gap-y-1 tabular-nums">
            {d.filesAdded !== null && d.filesAdded > 0 && <span className="text-ok">+{groupDigits(d.filesAdded)} {d.filesAdded === 1 ? "file" : "files"}</span>}
            {d.recordsAdded !== null && d.recordsAdded > 0 && <span className="text-ok">{signed(d.recordsAdded, "+")} records</span>}
            {d.filesRemoved !== null && d.filesRemoved > 0 && <span className="text-danger">−{groupDigits(d.filesRemoved)} {d.filesRemoved === 1 ? "file" : "files"}</span>}
            {d.recordsRemoved !== null && d.recordsRemoved > 0 && <span className="text-danger">{signed(d.recordsRemoved, "−")} records</span>}
            {d.totalRecords !== null && <span className="text-muted">{groupDigits(d.totalRecords)} total</span>}
          </span>
        </div>

        {summary.length > 0 && (
          <div className="border-t border-border/70">
            <button type="button" aria-expanded={open} onClick={() => setOpen((v) => !v)} className="flex w-full items-center gap-1 px-2.5 py-1 text-left text-[11px] text-muted hover:text-fg">
              {open ? <ChevronDown className="size-3" /> : <ChevronRight className="size-3" />} Summary ({summary.length})
              {s.parentSnapshotId === null && <span className="ml-2 text-muted/70">first snapshot</span>}
            </button>
            {open && (
              <dl className="grid grid-cols-1 gap-x-6 gap-y-0.5 px-3 pb-2 pt-0.5 font-mono text-[11.5px] @[640px]:grid-cols-2">
                {summary.map(([k, v]) => (
                  <div key={k} className="flex justify-between gap-3 border-b border-border/40 py-0.5">
                    <dt className="truncate text-muted">{k}</dt>
                    <dd className="selectable shrink-0 text-fg">{v}</dd>
                  </div>
                ))}
                <div className="col-span-full flex justify-between gap-3 py-0.5">
                  <dt className="text-muted">parent snapshot</dt>
                  <dd className="selectable text-fg">{s.parentSnapshotId ?? "none"}</dd>
                </div>
              </dl>
            )}
          </div>
        )}
      </div>
    </li>
  );
}
