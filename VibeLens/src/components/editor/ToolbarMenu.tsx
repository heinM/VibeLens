// Dropdown for toolbar split buttons. Unlike the context menu it can explain *why* an item is
// disabled (a second line under the label, plus a tooltip), which matters for "Explain Analyze".
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { cn } from "../../lib/cn";

export interface DropdownItem {
  id: string;
  label: string;
  icon?: ReactNode;
  /** Keyboard shortcut or short note on the right. */
  hint?: string;
  disabled?: boolean;
  /** Muted second line. Shown for every item that has one; for disabled items it is the reason. */
  detail?: string;
  onSelect: () => void;
}

export function DropdownMenu({ anchor, items, onClose, width = 288 }: { anchor: HTMLElement; items: DropdownItem[]; onClose: () => void; width?: number }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: 0, top: 0 });
  const enabled = items.map((it, i) => (it.disabled ? -1 : i)).filter((i) => i >= 0);
  const [focus, setFocus] = useState(enabled[0] ?? -1);

  useLayoutEffect(() => {
    const r = anchor.getBoundingClientRect();
    const el = ref.current;
    const h = el?.getBoundingClientRect().height ?? 0;
    let left = r.left;
    if (left + width > window.innerWidth - 8) left = Math.max(8, window.innerWidth - width - 8);
    const top = r.bottom + 4 + h > window.innerHeight - 8 ? Math.max(8, r.top - h - 4) : r.bottom + 4;
    setPos({ left, top });
    el?.focus({ preventScroll: true });
  }, [anchor, width, items.length]);

  useEffect(() => {
    const down = (e: MouseEvent) => {
      const t = e.target as Node;
      if (ref.current && !ref.current.contains(t) && !anchor.contains(t)) onClose();
    };
    window.addEventListener("mousedown", down, true);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  }, [anchor, onClose]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const cur = enabled.indexOf(focus);
      const next = e.key === "ArrowDown" ? (cur + 1) % enabled.length : (cur - 1 + enabled.length) % enabled.length;
      setFocus(enabled[cur < 0 ? 0 : next]);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      const it = items[focus];
      if (it && !it.disabled) {
        onClose();
        it.onSelect();
      }
    }
  };

  return createPortal(
    <div
      ref={ref}
      role="menu"
      tabIndex={-1}
      onKeyDown={onKeyDown}
      style={{ left: pos.left, top: pos.top, width }}
      className="fixed z-[70] rounded-lg border border-border bg-panel p-1 text-[13px] text-fg shadow-2xl outline-none"
    >
      {items.map((it, i) => (
        <button
          key={it.id}
          type="button"
          role="menuitem"
          aria-disabled={it.disabled}
          title={it.disabled ? it.detail : undefined}
          onMouseEnter={() => !it.disabled && setFocus(i)}
          onClick={() => {
            if (it.disabled) return;
            onClose();
            it.onSelect();
          }}
          className={cn("flex w-full items-start gap-2 rounded-md px-2 py-1.5 text-left outline-none", it.disabled ? "cursor-not-allowed opacity-60" : focus === i && "bg-panel-2")}
        >
          <span className="mt-0.5 flex size-4 shrink-0 items-center justify-center text-muted">{it.icon}</span>
          <span className="min-w-0 flex-1">
            <span className="block truncate">{it.label}</span>
            {it.detail && <span className="mt-0.5 block text-[11px] leading-snug text-muted">{it.detail}</span>}
          </span>
          {it.hint && <span className="ml-2 mt-0.5 shrink-0 text-[11px] text-muted">{it.hint}</span>}
        </button>
      ))}
    </div>,
    document.body,
  );
}
