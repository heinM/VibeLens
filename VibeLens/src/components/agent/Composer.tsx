import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { ArrowUp, Square } from "lucide-react";
import { cn } from "../../lib/cn";
import { Key } from "../ui/formKit";

export function Composer({
  value,
  onChange,
  onSend,
  onCancel,
  running,
  sendDisabledReason,
  history,
  focusToken,
  primary = false,
}: {
  value: string;
  onChange: (v: string) => void;
  onSend: () => void;
  onCancel: () => void;
  running: boolean;
  /** When set, sending is blocked and this explains why. */
  sendDisabledReason: string | null;
  /** Previous prompts, oldest first, for ArrowUp recall. */
  history: string[];
  /** Change this to move focus to the input (e.g. after picking a suggestion). */
  focusToken: number;
  /** Agent-first layout: a roomier input that takes focus when the panel opens. */
  primary?: boolean;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [hist, setHist] = useState<number | null>(null);
  const wasRunning = useRef(running);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 168)}px`;
  }, [value]);

  useEffect(() => {
    if (focusToken > 0) ref.current?.focus();
  }, [focusToken]);

  // The panel just opened: put the cursor in the box, unless the user is typing in a dialog.
  useEffect(() => {
    const id = requestAnimationFrame(() => {
      const a = document.activeElement;
      if (a && a.closest("[role=dialog]")) return;
      ref.current?.focus({ preventScroll: true });
    });
    return () => cancelAnimationFrame(id);
  }, []);

  // Give focus back when a turn ends, unless the user went somewhere else to type.
  useEffect(() => {
    if (wasRunning.current && !running) {
      const a = document.activeElement;
      const root = ref.current?.closest("section");
      const idle = !a || a === document.body || (!!root && root.contains(a) && !(a instanceof HTMLInputElement || a instanceof HTMLTextAreaElement || a instanceof HTMLSelectElement));
      if (idle) ref.current?.focus({ preventScroll: true });
    }
    wasRunning.current = running;
  }, [running]);

  const canSend = !running && !sendDisabledReason && value.trim().length > 0;

  const recall = (idx: number | null) => {
    setHist(idx);
    onChange(idx == null ? "" : (history[idx] ?? ""));
    requestAnimationFrame(() => {
      const el = ref.current;
      if (el) el.setSelectionRange(el.value.length, el.value.length);
    });
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Safari sends the IME-commit Enter with isComposing=false but keyCode 229.
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    if (e.key === "Enter" && !e.shiftKey && !e.altKey) {
      e.preventDefault();
      if (canSend) {
        setHist(null);
        onSend();
      }
      return;
    }
    const el = e.currentTarget;
    if (e.key === "ArrowUp" && history.length > 0 && (hist != null || value === "") && el.selectionStart === 0 && el.selectionEnd === 0) {
      e.preventDefault();
      recall(hist == null ? history.length - 1 : Math.max(0, hist - 1));
    } else if (e.key === "ArrowDown" && hist != null && el.selectionStart === el.value.length) {
      e.preventDefault();
      recall(hist + 1 >= history.length ? null : hist + 1);
    }
  };

  return (
    <div className={cn("border-t border-border px-3 pb-2.5 pt-2", primary && "px-4 pb-3 pt-3")}>
      <div
        className={cn(
          "mx-auto flex items-end gap-2 border border-border bg-bg transition-colors focus-within:border-accent/70 focus-within:ring-1 focus-within:ring-accent/40",
          primary ? "max-w-3xl rounded-xl px-3 py-2.5 shadow-[0_1px_0_rgb(0_0_0/0.08),0_8px_24px_-12px_rgb(0_0_0/0.45)]" : "rounded-lg px-2.5 py-1.5",
          running && "opacity-70",
        )}
      >
        <textarea
          ref={ref}
          rows={primary ? 2 : 1}
          value={value}
          disabled={running}
          aria-label="Message to the AI agent"
          placeholder="Ask about this database, or have the agent write a migration…"
          onChange={(e) => {
            setHist(null);
            onChange(e.target.value);
          }}
          onKeyDown={onKeyDown}
          className={cn("max-h-[168px] flex-1 resize-none bg-transparent py-0.5 leading-5 text-fg outline-none placeholder:text-muted/70 disabled:cursor-not-allowed", primary ? "min-h-10 text-[14px] leading-[22px]" : "min-h-6 text-[13px]")}
        />
        {running ? (
          <button
            type="button"
            onClick={onCancel}
            aria-label="Cancel the running turn"
            title="Cancel"
            className="grid size-7 shrink-0 place-items-center rounded-md bg-danger/15 text-danger outline-none hover:bg-danger/25 focus-visible:ring-2 focus-visible:ring-danger/50"
          >
            <Square className="size-3 fill-current" />
          </button>
        ) : (
          <button
            type="button"
            onClick={() => canSend && onSend()}
            disabled={!canSend}
            aria-label="Send message"
            title={sendDisabledReason ?? "Send"}
            className="grid size-7 shrink-0 place-items-center rounded-md bg-accent text-accent-fg outline-none transition-opacity hover:brightness-110 focus-visible:ring-2 focus-visible:ring-accent/60 disabled:opacity-35"
          >
            <ArrowUp className="size-4" />
          </button>
        )}
      </div>
      <div className={cn("mt-1 flex items-center gap-3 px-1 text-[11px] text-muted", primary && "mx-auto max-w-3xl")}>
        {sendDisabledReason && !running ? (
          <span className="text-warn">{sendDisabledReason}</span>
        ) : (
          <>
            <span>
              <Key>Enter</Key> send
            </span>
            <span>
              <Key>Shift</Key>+<Key>Enter</Key> new line
            </span>
            {history.length > 0 && (
              <span>
                <Key>↑</Key> previous prompt
              </span>
            )}
          </>
        )}
      </div>
    </div>
  );
}
