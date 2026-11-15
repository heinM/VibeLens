import { useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, History } from "lucide-react";
import { api, asError } from "../../ipc/commands";
import type { ConnectionProfile, ErrorPayload, IcebergTableInfo } from "../../ipc/types";
import { Button, ConfirmDialog, Dialog, ErrorBox } from "../ui";
import { FocusScope, FormField, NumberField, Notice } from "../ui/formKit";
import { showToast } from "../grid/toast";
import { DAY_MS, formatDateTime, fromLocalInput, groupDigits, relativeTime, toLocalInput } from "./iceFormat";
import { planExpire } from "./iceTableModel";
import { Card, PaneHeader } from "./icePrimitives";

export function MaintenancePane({ profile, info, onExpired }: { profile: ConnectionProfile; info: IcebergTableInfo; onExpired: () => void }) {
  const [open, setOpen] = useState(false);
  const oldest = info.snapshots.length ? info.snapshots[info.snapshots.length - 1] : null;

  return (
    <div>
      <PaneHeader title="Maintenance" subtitle="Housekeeping that changes the table's metadata. Nothing here touches your rows." />
      <Card className="p-4">
        <div className="flex flex-wrap items-start gap-4">
          <div className="grid size-9 shrink-0 place-items-center rounded-lg border border-border bg-panel-2 text-muted">
            <History className="size-4" />
          </div>
          <div className="min-w-0 flex-1 basis-64">
            <h3 className="text-[13px] font-semibold text-fg">Expire snapshots</h3>
            <p className="mt-0.5 max-w-prose text-[12.5px] leading-relaxed text-muted">
              Every commit keeps a snapshot so you can query the table as it was. Over time they pile up and make the metadata slower to load. Expiring removes old ones for good:{" "}
              <span className="text-fg">time travel to them stops working</span>. The current snapshot, branch and tag heads are always kept.
            </p>
            <p className="mt-2 text-[12px] tabular-nums text-muted">
              {info.snapshots.length} {info.snapshots.length === 1 ? "snapshot" : "snapshots"}
              {oldest && <> · oldest {formatDateTime(oldest.timestampMs)} ({relativeTime(oldest.timestampMs)})</>}
            </p>
          </div>
          <Button variant="secondary" onClick={() => setOpen(true)} disabled={info.snapshots.length < 2} title={info.snapshots.length < 2 ? "A table with one snapshot has nothing to expire" : undefined}>
            Expire snapshots…
          </Button>
        </div>
      </Card>
      {open && <ExpireDialog profile={profile} info={info} onClose={() => setOpen(false)} onDone={onExpired} />}
    </div>
  );
}

function ExpireDialog({ profile, info, onClose, onDone }: { profile: ConnectionProfile; info: IcebergTableInfo; onClose: () => void; onDone: () => void }) {
  const qc = useQueryClient();
  const [cutoff, setCutoff] = useState(() => toLocalInput(Date.now() - 5 * DAY_MS));
  const [retain, setRetain] = useState<number | null>(1);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ErrorPayload | null>(null);

  const cutoffMs = fromLocalInput(cutoff);
  const retainOk = retain !== null && Number.isInteger(retain) && retain >= 1;
  const valid = cutoffMs !== null && retainOk;
  const plan = useMemo(() => (valid ? planExpire(info, cutoffMs, retain) : null), [valid, cutoffMs, retain, info]);
  const prod = profile.environment === "prod";
  const count = plan?.expire.length ?? 0;

  const run = async () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    try {
      const res = await api.iceExpireSnapshots(profile.id, info.namespace, info.name, { olderThanMs: cutoffMs, retainLast: retain, confirmed: true });
      await qc.invalidateQueries({ queryKey: ["ice", profile.id, "describe"] });
      onDone();
      showToast({
        title: res.expired === 0 ? "Nothing to expire" : `Expired ${res.expired} ${res.expired === 1 ? "snapshot" : "snapshots"}`,
        detail: `${info.name}: ${res.remaining} ${res.remaining === 1 ? "snapshot" : "snapshots"} remain`,
        tone: res.expired === 0 ? "info" : "ok",
      });
      onClose();
    } catch (e) {
      setConfirming(false);
      setBusy(false);
      setError(asError(e));
    }
  };

  return (
    <>
      <Dialog
        open
        onClose={() => {
          if (!confirming && !busy) onClose();
        }}
        title={
          <span className="flex items-center gap-2">
            Expire snapshots <span className="font-mono font-normal text-muted">{info.name}</span>
          </span>
        }
        width="max-w-lg"
        footer={
          <>
            <Button variant="ghost" onClick={onClose} disabled={busy}>
              Cancel
            </Button>
            <Button variant="danger" onClick={() => setConfirming(true)} disabled={!valid || count === 0 || busy}>
              Expire {count > 0 ? `${count} ` : ""}
              {count === 1 ? "snapshot" : "snapshots"}…
            </Button>
          </>
        }
      >
        <FocusScope>
          <div className="space-y-4">
            <div className="grid gap-3 sm:grid-cols-[1fr_9rem]">
              <FormField label="Older than" htmlFor="ice-exp-cutoff" error={cutoffMs === null ? "Choose a date and time." : undefined} hint="Snapshots committed before this moment can be removed. Default is 5 days ago.">
                <input
                  id="ice-exp-cutoff"
                  data-autofocus
                  type="datetime-local"
                  value={cutoff}
                  onChange={(e) => setCutoff(e.target.value)}
                  className="h-8 w-full rounded-md border border-border bg-bg px-2.5 text-[13px] tabular-nums text-fg outline-none focus:border-accent/70 focus:ring-1 focus:ring-accent/40 [color-scheme:dark]"
                />
              </FormField>
              <FormField label="Always keep latest" htmlFor="ice-exp-retain" error={!retainOk ? "At least 1." : undefined} hint="Per branch.">
                <NumberField id="ice-exp-retain" value={retain} min={1} step={1} invalid={!retainOk} onChange={(v) => setRetain(v)} />
              </FormField>
            </div>

            <div className="rounded-md border border-border bg-panel-2/40 px-3 py-2 text-[12.5px]" aria-live="polite">
              {!plan ? (
                <span className="text-muted">Fix the fields above to see what would be removed.</span>
              ) : count === 0 ? (
                <span className="text-muted">With these settings no snapshot qualifies, so nothing would change.</span>
              ) : (
                <>
                  <span className="font-medium tabular-nums text-fg">
                    About {groupDigits(count)} of {groupDigits(info.snapshots.length)} snapshots
                  </span>{" "}
                  would be removed; {groupDigits(plan.keep.length)} stay. <span className="text-muted">An estimate: the table&rsquo;s own retention properties can keep more.</span>
                </>
              )}
            </div>

            <Notice tone="warn">
              <strong className="font-semibold">This can&rsquo;t be undone.</strong> Once expired, queries &ldquo;as of&rdquo; those snapshots fail and you can&rsquo;t roll back to them. The current data is not affected. This action rewrites the table&rsquo;s metadata; storage isn&rsquo;t reclaimed until a separate clean-up removes files nothing refers to.
            </Notice>
            {error && <ErrorBox error={error} />}
          </div>
        </FocusScope>
      </Dialog>
      <ConfirmDialog
        open={confirming}
        title="Permanently expire snapshots"
        danger
        confirmText={busy ? "Expiring…" : `Expire ${count} ${count === 1 ? "snapshot" : "snapshots"}`}
        requireTyping={prod ? profile.name : undefined}
        onCancel={() => !busy && setConfirming(false)}
        onConfirm={() => void run()}
        body={
          <div className="space-y-2">
            <p className="flex items-start gap-2">
              <AlertTriangle className="mt-0.5 size-4 shrink-0 text-warn" />
              <span>
                Remove about <strong className="font-semibold tabular-nums">{count}</strong> {count === 1 ? "snapshot" : "snapshots"} older than <strong className="font-semibold tabular-nums">{cutoffMs !== null ? formatDateTime(cutoffMs) : ""}</strong> from{" "}
                <span className="font-mono">{[...info.namespace, info.name].join(".")}</span>, keeping the latest {retain}.
              </span>
            </p>
            <p className="text-muted">Time travel to them stops working for everyone who uses this table.{prod ? " This is a production connection." : ""}</p>
          </div>
        }
      />
    </>
  );
}
