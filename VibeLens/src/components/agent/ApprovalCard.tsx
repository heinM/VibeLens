import { useEffect, useRef, useState } from "react";
import { Ban, Check, ShieldAlert } from "lucide-react";
import type { ApprovalDecision } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { Button } from "../ui";
import { Key, modKey } from "../ui/formKit";
import type { ApprovalItem } from "./agentReducer";
import { CodeBody } from "./CodeView";
import { compactJson, mentionsProd, previewKind, summarizeToolArgs } from "./toolSummary";

/** Compact record of a resolved approval, shown in the transcript. */
export function ApprovalNote({ item }: { item: ApprovalItem }) {
  const approved = item.status === "approved";
  return (
    <div className="flex items-center gap-1.5 pl-1 text-[11.5px] text-muted">
      {approved ? <Check className="size-3 text-ok" /> : <Ban className="size-3" />}
      <span>
        {approved ? "You approved" : "You rejected"} <span className="font-mono">{item.tool}</span>
        {item.rejectReason ? `: ${item.rejectReason}` : ""}
      </span>
    </div>
  );
}

/**
 * Pending approval. Pinned above the composer so it never scrolls away.
 * Keys: Ctrl/Cmd+Enter approves, Esc rejects, while focus is in the agent panel (or nowhere).
 */
export function ApprovalCard({
  item,
  scope,
  prodContext,
  onResolve,
}: {
  item: ApprovalItem;
  /** The agent panel root: shortcuts only fire when focus is inside it (or on <body>). */
  scope: React.RefObject<HTMLElement | null>;
  prodContext: boolean;
  onResolve: (d: ApprovalDecision) => Promise<boolean>;
}) {
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const decide = useRef<(approve: boolean) => void>(() => undefined);
  const danger = mentionsProd(item.reason) || prodContext;
  const kind = previewKind(item.tool, item.preview);

  decide.current = (approve: boolean) => {
    if (busy) return;
    setBusy(true);
    void onResolve(approve ? { decision: "approve" } : { decision: "reject", reason: reason.trim() || null }).then((ok) => {
      if (!ok) setBusy(false);
    });
  };

  useEffect(() => {
    // Only keystrokes while focus is inside the agent panel may approve or reject. Safari/WKWebView
    // doesn't focus clicked buttons (focus falls back to <body>), so treating <body> as "in scope"
    // let a habitual ⌘↵ after clicking Run in the editor approve a pending write.
    const inScope = () => {
      const a = document.activeElement;
      return !!a && a !== document.body && !!scope.current?.contains(a);
    };
    // Pull focus to the card only when nothing else holds it (or the panel already does).
    const a0 = document.activeElement;
    if (!a0 || a0 === document.body || scope.current?.contains(a0)) ref.current?.focus({ preventScroll: true });
    const onKey = (e: KeyboardEvent) => {
      if (!inScope() || document.querySelector('[role="dialog"]')) return;
      if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
        e.preventDefault();
        decide.current(true);
      } else if (e.key === "Escape") {
        e.preventDefault();
        decide.current(false);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [scope]);

  return (
    <div
      ref={ref}
      tabIndex={-1}
      role="group"
      aria-label={`Approval needed for ${item.tool}`}
      className={cn(
        "mx-3 mb-2 flex max-h-[70%] min-h-[9.5rem] flex-col overflow-hidden rounded-lg border outline-none focus-visible:ring-2 focus-visible:ring-accent/60",
        danger ? "border-danger/60 bg-danger/[0.07]" : "border-warn/50 bg-warn/[0.06]",
      )}
    >
      <div className="flex shrink-0 items-start gap-2 px-3 pb-2 pt-2.5">
        <ShieldAlert className={cn("mt-0.5 size-4 shrink-0", danger ? "text-danger" : "text-warn")} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-2 text-[11.5px] text-muted">
            <span className={cn("font-semibold", danger ? "text-danger" : "text-warn")}>{danger ? "Approval needed: production" : "Approval needed"}</span>
            <span className="font-mono">{item.tool}</span>
          </div>
          <p className="selectable mt-0.5 text-[13px] font-medium leading-snug text-fg">{item.reason}</p>
        </div>
      </div>
      <div className="mx-3 mb-2 flex min-h-0 flex-1 flex-col overflow-hidden rounded-md border border-border bg-bg">
        {item.preview ? (
          <CodeBody kind={kind} code={item.preview} className="min-h-0 flex-1 max-h-44" />
        ) : (
          <pre className="selectable max-h-24 overflow-auto px-3 py-2 font-mono text-[12px] text-muted">{summarizeToolArgs(item.tool, item.args) || compactJson(item.args, 400)}</pre>
        )}
      </div>
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-t border-border/70 px-3 py-2">
        <input
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.metaKey && !e.ctrlKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              decide.current(false);
            }
          }}
          placeholder="Reason for rejecting (optional)"
          aria-label="Reason for rejecting, optional"
          disabled={busy}
          className="h-8 min-w-40 flex-1 rounded-md border border-border bg-bg px-2.5 text-[12.5px] text-fg outline-none placeholder:text-muted/70 focus:border-accent/70 focus:ring-1 focus:ring-accent/40"
        />
        <Button onClick={() => decide.current(false)} disabled={busy}>
          Reject <Key>Esc</Key>
        </Button>
        <Button variant="primary" onClick={() => decide.current(true)} loading={busy} className={danger ? "!bg-danger !text-white" : undefined}>
          Approve <Key>{modKey}↵</Key>
        </Button>
      </div>
    </div>
  );
}
