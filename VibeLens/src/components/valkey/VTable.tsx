// Minimal virtualised table for hash / list / set / zset / stream bodies.
import { useRef, type ReactNode } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { cn } from "../../lib/cn";

export interface VColumn<T> {
  label: string;
  /** CSS grid track, e.g. "64px" or "minmax(0,1fr)". */
  width: string;
  cell: (row: T, index: number) => ReactNode;
  className?: string;
}

const ROW_H = 26;

export function VTable<T>({ rows, columns, rowKey }: { rows: readonly T[]; columns: VColumn<T>[]; rowKey?: (row: T, index: number) => string }) {
  const ref = useRef<HTMLDivElement>(null);
  const virt = useVirtualizer({ count: rows.length, getScrollElement: () => ref.current, estimateSize: () => ROW_H, overscan: 12 });
  const template = columns.map((c) => c.width).join(" ");
  return (
    <div ref={ref} className="selectable h-full overflow-auto" role="table">
      <div role="row" className="sticky top-0 z-10 grid h-7 items-center border-b border-border bg-panel px-3 text-[11px] font-medium text-muted" style={{ gridTemplateColumns: template }}>
        {columns.map((c) => (
          <div key={c.label} role="columnheader" className={cn("truncate pr-3", c.className)}>
            {c.label}
          </div>
        ))}
      </div>
      <div style={{ height: virt.getTotalSize(), position: "relative" }}>
        {virt.getVirtualItems().map((item) => {
          const row = rows[item.index];
          return (
            <div
              key={rowKey ? rowKey(row, item.index) : item.key}
              role="row"
              className={cn("absolute left-0 grid w-full items-center border-b border-border/40 px-3 font-mono text-[12px] hover:bg-panel-2/60", item.index % 2 === 1 && "bg-panel/40")}
              style={{ top: 0, height: ROW_H, transform: `translateY(${item.start}px)`, gridTemplateColumns: template }}
            >
              {columns.map((c) => (
                <div key={c.label} role="cell" className={cn("truncate pr-3", c.className)}>
                  {c.cell(row, item.index)}
                </div>
              ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}
