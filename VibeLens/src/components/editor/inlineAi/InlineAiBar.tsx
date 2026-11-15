// The ⌘K bar: a pure view over InlineAiController state, rendered inside a Monaco view zone.
import { useSyncExternalStore, type KeyboardEvent } from "react";
import { AlertCircle, Check, CornerDownLeft, Loader2, RotateCcw, Settings, Sparkles, X } from "lucide-react";
import { cn } from "../../../lib/cn";
import { MOD } from "../../ui/platform";
import type { InlineAiController } from "./controller";
import { isImeKeyEvent } from "./ime";

const Kbd = ({ children }: { children: string }) => <span className="vg-ai-kbd">{children}</span>;

function Action({ onClick, children, primary, title }: { onClick: () => void; children: React.ReactNode; primary?: boolean; title?: string }) {
  return (
    <button
      type="button"
      title={title}
      // Keep focus in the input so Enter/Esc keep working after a click.
      onMouseDown={(e) => e.preventDefault()}
      onClick={onClick}
      className={cn(
        "inline-flex h-6 items-center gap-1.5 rounded-md px-2 text-[11px] font-medium outline-none transition-colors",
        primary ? "bg-accent text-accent-fg hover:brightness-110" : "text-muted hover:bg-panel-2 hover:text-fg",
      )}
    >
      {children}
    </button>
  );
}

export function InlineAiBar({ controller }: { controller: InlineAiController }) {
  const s = useSyncExternalStore(controller.subscribe, controller.getState);
  if (s.phase === "closed") return null;
  const fix = s.mode === "fix";
  const editable = !fix && s.phase !== "running";

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    // Nothing typed here should reach the editor underneath.
    e.stopPropagation();
    // Safari reports the IME's commit Enter with isComposing=false and keyCode 229.
    if (isImeKeyEvent({ isComposing: e.nativeEvent.isComposing, keyCode: e.keyCode })) return;
    if (e.key === "Escape") {
      e.preventDefault();
      controller.escape();
    } else if (e.key === "Enter") {
      e.preventDefault();
      if ((e.metaKey || e.ctrlKey) && s.phase === "preview") controller.accept();
      else controller.submit();
    } else if (e.key === "Tab") {
      e.preventDefault();
      if (s.phase === "preview") controller.accept();
      else if (s.phase === "prompt") controller.toggleScope();
    } else if ((e.key === "ArrowUp" || e.key === "ArrowDown") && s.phase === "prompt") {
      e.preventDefault();
      controller.recall(e.key === "ArrowUp" ? -1 : 1);
    }
  };

  return (
    <div className="vg-ai-bar" data-phase={s.phase} role="dialog" aria-label="Ask AI">
      <div className="flex items-center gap-2">
        <Sparkles className={cn("size-4 shrink-0 text-accent", s.phase === "running" && "animate-pulse")} aria-hidden />
        <input
          value={s.instruction}
          readOnly={!editable}
          onChange={(e) => controller.setInstruction(e.target.value)}
          onKeyDown={onKeyDown}
          onKeyUp={(e) => e.stopPropagation()}
          onMouseDown={(e) => e.stopPropagation()}
          spellCheck={false}
          autoComplete="off"
          placeholder={s.mode === "selection" || s.mode === "statement" ? "Describe the change, e.g. add a WHERE for paid orders" : "Describe the query, e.g. orders per customer last 30 days"}
          className={cn("min-w-0 flex-1 bg-transparent text-[13px] text-fg outline-none placeholder:text-muted/70", !editable && "text-muted")}
          aria-label="Instruction for the AI"
        />
        {s.phase === "running" ? <Loader2 className="size-3.5 shrink-0 animate-spin text-accent" aria-hidden /> : editable && <Kbd>↵</Kbd>}
      </div>

      <div className="flex min-h-6 items-center gap-2 pl-6 text-[11px] text-muted">
        {s.phase === "prompt" && (
          <>
            <span className="truncate">{s.scopeLabel}</span>
            {s.canSwitchScope && (
              <button type="button" onMouseDown={(e) => e.preventDefault()} onClick={() => controller.toggleScope()} className="inline-flex items-center gap-1 rounded px-1 text-muted hover:text-fg">
                <Kbd>⇥</Kbd> {s.mode === "statement" ? "insert new SQL instead" : "edit the statement instead"}
              </button>
            )}
            <span className="ml-auto inline-flex items-center gap-1.5">
              <Kbd>esc</Kbd> close
            </span>
          </>
        )}
        {s.phase === "running" && (
          <>
            <span className="truncate text-fg/80">{s.status}…</span>
            <span className="ml-auto inline-flex items-center gap-1.5">
              <Kbd>esc</Kbd> {fix ? "close" : "cancel"}
            </span>
          </>
        )}
        {s.phase === "preview" && (
          <>
            <span className={cn("truncate", s.noChange ? "text-warn" : "text-fg/80")}>{s.summary}</span>
            <span className="ml-auto flex items-center gap-1">
              <Action onClick={() => controller.reject()} title="Reject (Esc)">
                <X className="size-3" /> Reject <Kbd>esc</Kbd>
              </Action>
              <Action onClick={() => controller.retry()} title="Ask again (Enter). Edit the instruction first to steer it.">
                <RotateCcw className="size-3" /> Retry <CornerDownLeft className="size-3 opacity-60" />
              </Action>
              {!s.noChange && (
                <Action primary onClick={() => controller.accept()} title={`Accept (${MOD}+Enter or Tab)`}>
                  <Check className="size-3" /> Accept <span className="opacity-70">{MOD}↵</span>
                </Action>
              )}
            </span>
          </>
        )}
        {s.phase === "error" && (
          <>
            <AlertCircle className="size-3.5 shrink-0 text-danger" aria-hidden />
            <span className="min-w-0 flex-1 truncate text-danger" title={s.error?.message}>
              {s.error?.message}
            </span>
            <span className="flex shrink-0 items-center gap-1">
              {s.error?.setup ? (
                <Action primary onClick={() => controller.openSettings()}>
                  <Settings className="size-3" /> Set up AI in Settings
                </Action>
              ) : (
                <Action onClick={() => controller.retry()}>
                  <RotateCcw className="size-3" /> Retry
                </Action>
              )}
              <Action onClick={() => controller.reject()} title="Close (Esc)">
                <X className="size-3" /> <Kbd>esc</Kbd>
              </Action>
            </span>
          </>
        )}
      </div>
    </div>
  );
}
