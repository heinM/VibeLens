import { Check, FileCode2, Plug, Table2, TextCursorInput, X } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "../../lib/cn";
import type { AgentContextInfo, ChipExclusions } from "../../hooks/useAgentContext";
import { EnvBadge } from "../ui";
import { attachBlockedReason, LAST_RESULT_ROWS } from "./lastResult";

function Chip({ label, excluded, onToggle, children, title }: { label: string; excluded: boolean; onToggle: () => void; children: ReactNode; title?: string }) {
  return (
    <span
      className={cn(
        "inline-flex h-6 max-w-64 items-center gap-1.5 rounded-md border pl-1.5 pr-0.5 text-[12px]",
        excluded ? "border-dashed border-border text-muted/70" : "border-border bg-panel-2 text-fg",
      )}
      title={title}
    >
      <span className={cn("flex min-w-0 items-center gap-1.5", excluded && "line-through decoration-muted/60")}>{children}</span>
      <button
        type="button"
        onClick={onToggle}
        aria-label={excluded ? `Include ${label} in the next message` : `Remove ${label} from the next message`}
        title={excluded ? "Include again" : "Leave out of the next message"}
        className="grid size-5 shrink-0 place-items-center rounded text-muted outline-none hover:bg-bg hover:text-fg focus-visible:ring-2 focus-visible:ring-accent/60"
      >
        {excluded ? <span className="text-[13px] leading-none">+</span> : <X className="size-3" />}
      </button>
    </span>
  );
}

/** Right-anchored tooltip (the chip sits at the end of the row; the shared Tip clipped at the panel edge). */
function ChipTip({ content, children }: { content: ReactNode; children: ReactNode }) {
  return (
    <span className="group/tip relative inline-flex">
      {children}
      <span
        role="tooltip"
        className="pointer-events-none absolute right-0 top-full z-50 mt-1.5 w-64 rounded-md border border-border bg-panel p-2 text-[11px] leading-snug text-fg opacity-0 shadow-xl transition-opacity group-hover/tip:opacity-100 group-focus-within/tip:opacity-100"
      >
        {content}
      </span>
    </span>
  );
}

/**
 * Offers the newest query result as context. Unlike the other chips it starts out NOT attached:
 * nothing from a result leaves the machine unless the user turns it on.
 */
function LastResultChip({ info, attached, onToggle }: { info: AgentContextInfo; attached: boolean; onToggle: () => void }) {
  const snap = info.lastResult;
  if (!snap) return null;
  const blocked = attachBlockedReason(info.profile?.aiDataAccess);
  const shown = Math.min(snap.rows.length, LAST_RESULT_ROWS);
  const detail = `${shown} of ${snap.rowCount.toLocaleString("en-US")} rows · ${snap.columns.length} col${snap.columns.length === 1 ? "" : "s"}`;
  const chip = (
    <button
      type="button"
      onClick={blocked ? undefined : onToggle}
      aria-disabled={blocked ? true : undefined}
      aria-pressed={blocked ? undefined : attached}
      aria-label={blocked ? `Attach last result (unavailable): ${blocked}` : attached ? "Last result is attached to the next message. Click to remove." : `Attach the last result (${detail}) to the next message`}
      className={cn(
        "inline-flex h-6 max-w-72 items-center gap-1.5 rounded-md border px-1.5 text-[12px] outline-none focus-visible:ring-2 focus-visible:ring-accent/60",
        blocked ? "cursor-not-allowed border-dashed border-border text-muted/60" : attached ? "border-accent/60 bg-accent/10 text-fg" : "border-dashed border-border text-muted hover:border-accent/50 hover:text-fg",
      )}
    >
      {attached && !blocked ? <Check className="size-3 shrink-0 text-accent" /> : <Table2 className="size-3 shrink-0" />}
      <span>last result</span>
      <span className="truncate text-[11px] text-muted">{blocked ? "unavailable" : attached ? detail : "attach"}</span>
    </button>
  );
  return blocked ? (
    <ChipTip content={blocked}>{chip}</ChipTip>
  ) : (
    <ChipTip content={attached ? `Sends the column names and first ${shown} rows of your last result with the next message.` : `Add your last result (${detail}) to the next message. Off by default.`}>{chip}</ChipTip>
  );
}

/** What the next message will carry. Each chip can be left out for one turn. */
export function ContextChips({
  info,
  exclude,
  onChange,
  attachResult,
  onToggleResult,
}: {
  info: AgentContextInfo;
  exclude: ChipExclusions;
  onChange: (e: ChipExclusions) => void;
  attachResult: boolean;
  onToggleResult: () => void;
}) {
  const hasConn = !!info.profileId;
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-1.5" role="group" aria-label="Context for the next message">
      {hasConn ? (
        <Chip label="connection" excluded={exclude.connection} onToggle={() => onChange({ ...exclude, connection: !exclude.connection })} title="The connection the agent works against">
          <Plug className="size-3 shrink-0 text-muted" />
          <span className="truncate">{info.profile?.name ?? "Connection"}</span>
          {info.profile && <EnvBadge env={info.profile.environment} />}
          {(info.database || info.valkeyDb != null) && (
            <span className="truncate font-mono text-[11px] text-muted">{info.database ?? `db${info.valkeyDb}`}</span>
          )}
        </Chip>
      ) : (
        <span className="inline-flex h-6 items-center gap-1.5 rounded-md border border-dashed border-border px-1.5 text-[12px] text-muted">
          <Plug className="size-3" /> No connection selected
        </span>
      )}
      {info.fileName && (
        <Chip label="file" excluded={exclude.file} onToggle={() => onChange({ ...exclude, file: !exclude.file })} title="The contents of the active editor tab">
          <FileCode2 className="size-3 shrink-0 text-muted" />
          <span className="truncate font-mono text-[11.5px]">{info.fileName}</span>
        </Chip>
      )}
      {info.selectionChars > 0 && (
        <Chip label="selection" excluded={exclude.selection} onToggle={() => onChange({ ...exclude, selection: !exclude.selection })} title="The text selected in the editor">
          <TextCursorInput className="size-3 shrink-0 text-muted" />
          <span>selection</span>
          <span className="text-[11px] text-muted">{info.selectionChars.toLocaleString("en-US")} chars</span>
        </Chip>
      )}
      <LastResultChip info={info} attached={attachResult} onToggle={onToggleResult} />
    </div>
  );
}
