import { memo, useState } from "react";
import { Ban, Check, ChevronRight, Cable, Database, FileText, FolderOpen, KeyRound, Pencil, ShieldAlert, Table2, Terminal, Wrench, X } from "lucide-react";
import { cn } from "../../lib/cn";
import { Spinner } from "../ui";
import type { ToolItem } from "./agentReducer";
import { compactJson, formatElapsed, summarizeToolArgs, toolIconName, type ToolIconName } from "./toolSummary";

const ICONS: Record<ToolIconName, typeof Wrench> = {
  list: Cable,
  schema: Database,
  table: Table2,
  sql: Terminal,
  key: KeyRound,
  files: FolderOpen,
  file: FileText,
  write: Pencil,
  other: Wrench,
};

function Status({ item }: { item: ToolItem }) {
  switch (item.status) {
    case "proposed":
    case "running":
      return <Spinner className="size-3.5" />;
    case "awaitingApproval":
      return (
        <span className="inline-flex items-center gap-1 text-[11px] text-warn">
          <ShieldAlert className="size-3.5" /> Needs approval
        </span>
      );
    case "ok":
      return <Check className="size-3.5 text-ok" aria-label="Succeeded" />;
    case "error":
      return <X className="size-3.5 text-danger" aria-label="Failed" />;
    case "denied":
      return (
        <span className="inline-flex items-center gap-1 text-[11px] text-muted">
          <Ban className="size-3.5" /> Denied
        </span>
      );
    case "rejected":
      return (
        <span className="inline-flex items-center gap-1 text-[11px] text-muted">
          <Ban className="size-3.5" /> Rejected
        </span>
      );
  }
}

export const ToolCard = memo(function ToolCard({ item }: { item: ToolItem }) {
  const [open, setOpen] = useState(false);
  const Icon = ICONS[toolIconName(item.tool)];
  const muted = item.status === "denied" || item.status === "rejected";
  const detail = item.status === "denied" ? (item.reason ?? item.summary) : item.summary;
  const argText = summarizeToolArgs(item.tool, item.args);
  const sql = item.tool === "run_sql";
  return (
    <div className={cn("rounded-md border bg-bg/60", item.status === "error" ? "border-danger/40" : "border-border", muted && "opacity-70")}>
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="flex min-h-8 w-full items-center gap-2 rounded-md px-2 py-1 text-left outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
      >
        <ChevronRight className={cn("size-3 shrink-0 text-muted transition-transform", open && "rotate-90")} />
        <Icon className="size-3.5 shrink-0 text-muted" />
        <span className="shrink-0 font-mono text-[12px] text-fg">{item.tool}</span>
        <span className={cn("min-w-0 flex-1 truncate text-[12px] text-muted", sql && "font-mono")}>{argText}</span>
        <span className="flex shrink-0 items-center gap-2">
          {item.elapsedMs != null && !muted && <span className="font-mono text-[11px] tabular-nums text-muted">{formatElapsed(item.elapsedMs)}</span>}
          <Status item={item} />
        </span>
      </button>
      {detail && !open && <p className={cn("selectable line-clamp-2 px-2 pb-1.5 pl-[42px] text-[12px] leading-snug", item.status === "error" ? "text-danger" : "text-muted")}>{detail}</p>}
      {open && (
        <div className="space-y-2 border-t border-border px-3 py-2">
          <div>
            <div className="mb-0.5 text-[11px] text-muted">Arguments</div>
            <pre className="selectable max-h-48 overflow-auto rounded bg-bg p-2 font-mono text-[11.5px] leading-snug text-fg/90">{safeStringify(item.args) || compactJson(item.args)}</pre>
          </div>
          {detail && (
            <div>
              <div className="mb-0.5 text-[11px] text-muted">{item.status === "denied" ? "Why it was denied" : "Result"}</div>
              <p className={cn("selectable whitespace-pre-wrap text-[12px] leading-snug", item.status === "error" ? "text-danger" : "text-fg/90")}>{detail}</p>
            </div>
          )}
        </div>
      )}
    </div>
  );
});

function safeStringify(v: unknown): string {
  try {
    return JSON.stringify(v, null, 2) ?? "";
  } catch {
    return "";
  }
}
