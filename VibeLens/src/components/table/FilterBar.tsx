// Filter builder: rows of column / operator / value combined with AND, or a raw WHERE clause.
import { useRef } from "react";
import { Plus, X } from "lucide-react";
import { cn } from "../../lib/cn";
import { uid } from "../../lib/id";
import { Button, IconButton } from "../ui";
import { Segmented, Kbd } from "../ui/wsKit";
import { MOD } from "../ui/platform";
import { FILTER_OPS, opNeedsValue, type ColumnRef, type FilterOp, type FilterRow } from "./sqlGen";

export type FilterMode = "builder" | "raw";

export const newFilter = (column: string, op: FilterOp = "=", value = ""): FilterRow => ({ id: uid(), column, op, value });

const selectCls =
  "h-7 min-w-0 rounded-md border border-border bg-bg px-1.5 text-xs text-fg outline-none hover:border-muted/60 focus-visible:border-accent/70";

export function FilterBar({
  columns,
  mode,
  onModeChange,
  rows,
  onRowsChange,
  raw,
  onRawChange,
  errors,
  dirty,
  active,
  onApply,
  onClear,
}: {
  columns: ColumnRef[];
  mode: FilterMode;
  onModeChange: (m: FilterMode) => void;
  rows: FilterRow[];
  onRowsChange: (r: FilterRow[]) => void;
  raw: string;
  onRawChange: (v: string) => void;
  /** Validation messages by filter id ("raw" for the raw box); shown against the row. */
  errors: Record<string, string>;
  /** Drafts differ from what is applied. */
  dirty: boolean;
  /** Something is applied. */
  active: boolean;
  onApply: () => void;
  onClear: () => void;
}) {
  const firstValue = useRef<HTMLInputElement>(null);
  const patch = (id: string, p: Partial<FilterRow>) => onRowsChange(rows.map((r) => (r.id === id ? { ...r, ...p } : r)));
  const onKey = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && (mode === "builder" || e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      onApply();
    }
  };

  return (
    <div className="shrink-0 border-b border-border bg-panel/60 px-3 py-2" role="group" aria-label="Filters">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1 space-y-1.5">
          {mode === "builder" ? (
            rows.length === 0 ? (
              <div className="flex h-7 items-center text-xs text-muted">No filters. Add one, or right-click a cell and choose Filter by this value.</div>
            ) : (
              rows.map((r, i) => {
                const err = errors[r.id];
                return (
                  <div key={r.id}>
                    <div className="flex items-center gap-1.5">
                      <span className="w-8 shrink-0 text-right text-[11px] text-muted">{i === 0 ? "where" : "and"}</span>
                      <select aria-label="Column" value={r.column} onChange={(e) => patch(r.id, { column: e.target.value })} className={cn(selectCls, "w-44 font-mono")}>
                        {columns.map((c) => (
                          <option key={c.name} value={c.name}>
                            {c.name}
                          </option>
                        ))}
                      </select>
                      <select aria-label="Operator" value={r.op} onChange={(e) => patch(r.id, { op: e.target.value as FilterOp })} className={cn(selectCls, "w-28")}>
                        {FILTER_OPS.map((o) => (
                          <option key={o.op} value={o.op}>
                            {o.label}
                          </option>
                        ))}
                      </select>
                      {opNeedsValue(r.op) ? (
                        <input
                          ref={i === 0 ? firstValue : undefined}
                          aria-label="Value"
                          aria-invalid={err ? true : undefined}
                          value={r.value}
                          onChange={(e) => patch(r.id, { value: e.target.value })}
                          onKeyDown={onKey}
                          spellCheck={false}
                          autoComplete="off"
                          placeholder={r.op === "IN" ? "1, 2, 'a,b'" : r.op === "LIKE" || r.op === "ILIKE" ? "%pattern%" : "value"}
                          className={cn(
                            "h-7 min-w-0 flex-1 rounded-md border bg-bg px-2 font-mono text-xs text-fg outline-none placeholder:text-muted/60 focus:border-accent/70",
                            err ? "border-danger/60" : "border-border",
                          )}
                        />
                      ) : (
                        <div className="flex-1" />
                      )}
                      <IconButton label="Remove filter" onClick={() => onRowsChange(rows.filter((x) => x.id !== r.id))}>
                        <X className="size-3.5" />
                      </IconButton>
                    </div>
                    {err && <div className="ml-[2.375rem] mt-0.5 text-[11px] text-danger">{err}</div>}
                  </div>
                );
              })
            )
          ) : (
            <div>
              <div className="flex items-start gap-1.5">
                <span className="mt-1.5 w-8 shrink-0 text-right font-mono text-[11px] text-accent">WHERE</span>
                <textarea
                  aria-label="Raw WHERE clause"
                  aria-invalid={errors.raw ? true : undefined}
                  value={raw}
                  onChange={(e) => onRawChange(e.target.value)}
                  onKeyDown={onKey}
                  rows={2}
                  spellCheck={false}
                  placeholder={`status = 'paid' AND total > 100`}
                  className={cn(
                    "selectable min-h-[3.25rem] flex-1 resize-y rounded-md border bg-bg px-2 py-1.5 font-mono text-xs leading-5 text-fg outline-none placeholder:text-muted/60 focus:border-accent/70",
                    errors.raw ? "border-danger/60" : "border-border",
                  )}
                />
              </div>
              {errors.raw ? (
                <div className="ml-[2.375rem] mt-0.5 text-[11px] text-danger">{errors.raw}</div>
              ) : (
                <div className="ml-[2.375rem] mt-0.5 text-[11px] text-muted">
                  A single condition, no semicolons. Runs inside a read-only transaction. <Kbd>{MOD}</Kbd> <Kbd>↵</Kbd> applies.
                </div>
              )}
            </div>
          )}
        </div>
        <div className="flex shrink-0 flex-col items-end gap-1.5">
          <Segmented<FilterMode>
            label="Filter mode"
            value={mode}
            onChange={onModeChange}
            options={[
              { value: "builder", label: "Builder" },
              { value: "raw", label: "Raw WHERE" },
            ]}
          />
          <div className="flex items-center gap-1.5">
            {mode === "builder" && (
              <Button size="sm" variant="ghost" onClick={() => onRowsChange([...rows, newFilter(columns[0]?.name ?? "")])} disabled={columns.length === 0}>
                <Plus className="size-3.5" /> Add filter
              </Button>
            )}
            <Button size="sm" variant="ghost" onClick={onClear} disabled={!active && !dirty && rows.length === 0 && raw === ""}>
              Clear
            </Button>
            <Button size="sm" variant={dirty ? "primary" : "secondary"} onClick={onApply} title={mode === "builder" ? "Enter" : `${MOD}+Enter`}>
              Apply
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}
