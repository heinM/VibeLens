// Small popup menu used for context menus (right-click, ⋯) and dropdowns. Rendered in a portal,
// closes on outside press / Escape / scroll / resize, supports arrow-key navigation.
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type MouseEvent as ReactMouseEvent, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { cn } from "../../lib/cn";

export interface MenuAction {
  label: string;
  onSelect: () => void;
  icon?: ReactNode;
  hint?: string;
  danger?: boolean;
  disabled?: boolean;
}
export type MenuEntry = MenuAction | "separator";

interface OpenMenu {
  x: number;
  y: number;
  items: MenuEntry[];
  /** When anchored to an element's right edge, x is the right edge and the menu grows leftwards. */
  alignRight: boolean;
}

function MenuPopup({ menu, onClose }: { menu: OpenMenu; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ left: menu.x, top: menu.y });
  const [focusIdx, setFocusIdx] = useState(-1);
  const actionIdx = menu.items.map((it, i) => (it !== "separator" && !it.disabled ? i : -1)).filter((i) => i >= 0);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    let left = menu.alignRight ? menu.x - r.width : menu.x;
    let top = menu.y;
    if (left + r.width > window.innerWidth - 6) left = window.innerWidth - r.width - 6;
    if (top + r.height > window.innerHeight - 6) top = Math.max(6, window.innerHeight - r.height - 6);
    setPos({ left: Math.max(6, left), top: Math.max(6, top) });
    el.focus({ preventScroll: true });
  }, [menu]);

  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const close = () => onClose();
    window.addEventListener("mousedown", down, true);
    window.addEventListener("blur", close);
    window.addEventListener("resize", close);
    window.addEventListener("wheel", close, { passive: true });
    return () => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("blur", close);
      window.removeEventListener("resize", close);
      window.removeEventListener("wheel", close);
    };
  }, [onClose]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const cur = actionIdx.indexOf(focusIdx);
      const nextPos = e.key === "ArrowDown" ? (cur + 1) % actionIdx.length : (cur - 1 + actionIdx.length) % actionIdx.length;
      setFocusIdx(actionIdx[cur < 0 ? (e.key === "ArrowDown" ? 0 : actionIdx.length - 1) : nextPos]);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      const it = menu.items[focusIdx];
      if (it && it !== "separator" && !it.disabled) {
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
      onContextMenu={(e) => e.preventDefault()}
      style={{ left: pos.left, top: pos.top }}
      className="fixed z-[70] min-w-44 max-w-72 rounded-lg border border-border bg-panel p-1 text-[13px] text-fg shadow-2xl outline-none"
    >
      {menu.items.map((it, i) =>
        it === "separator" ? (
          <div key={`s${i}`} role="separator" className="my-1 h-px bg-border" />
        ) : (
          <button
            key={`${it.label}${i}`}
            role="menuitem"
            disabled={it.disabled}
            onMouseEnter={() => setFocusIdx(i)}
            onClick={() => {
              onClose();
              it.onSelect();
            }}
            className={cn(
              "flex h-7 w-full items-center gap-2 rounded-md px-2 text-left outline-none disabled:opacity-40",
              focusIdx === i && "bg-panel-2",
              it.danger ? "text-danger" : "text-fg",
            )}
          >
            <span className="flex size-4 shrink-0 items-center justify-center text-muted">{it.icon}</span>
            <span className="min-w-0 flex-1 truncate">{it.label}</span>
            {it.hint && <span className="ml-3 shrink-0 text-[11px] text-muted">{it.hint}</span>}
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}

/** Returns the menu element to render once, plus openers for pointer positions and anchor elements. */
export function useMenu() {
  const [menu, setMenu] = useState<OpenMenu | null>(null);
  const close = useCallback(() => setMenu(null), []);

  const openAt = useCallback((e: ReactMouseEvent | MouseEvent, items: MenuEntry[]) => {
    e.preventDefault();
    e.stopPropagation();
    setMenu({ x: e.clientX, y: e.clientY, items, alignRight: false });
  }, []);

  const openFrom = useCallback((anchor: HTMLElement, items: MenuEntry[], align: "left" | "right" = "left") => {
    const r = anchor.getBoundingClientRect();
    setMenu({ x: align === "left" ? r.left : r.right, y: r.bottom + 4, items, alignRight: align === "right" });
  }, []);

  const node = menu ? <MenuPopup menu={menu} onClose={close} /> : null;
  return { menuNode: node, openAt, openFrom, close, isOpen: menu !== null };
}
