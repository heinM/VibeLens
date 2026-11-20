// Editors for table cells: a compact inline editor for scalars and booleans, and a roomy dialog for
// JSON, arrays and long or multi-line text.
import { useEffect, useMemo, useRef, useState } from "react";
import { Braces } from "lucide-react";
import type { Cell, ColumnInfo } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { Button, Dialog } from "../ui";
import { MOD } from "../ui/platform";
import { hasInexactNumber, isBytesCell } from "../grid/cellFormat";
import { showToast } from "../grid/toast";
import { cellToEditText, parseEditedText, typeCategory, uneditableReason } from "./sqlGen";

export type Move = "none" | "right" | "left";

export interface EditorColumn {
  name: string;
  dataType: string;
  nullable: boolean;
}

export const toEditorColumn = (c: ColumnInfo): EditorColumn => ({ name: c.name, dataType: c.dataType, nullable: c.nullable });

/** Identity and stored generated columns cannot be written. */
export const isGeneratedColumn = (c: ColumnInfo): boolean => /^GENERATED ALWAYS/i.test(c.default ?? "");

const MAX_BYTES_EDIT = 4096;
const LONG_TEXT = 160;

export type EditorMode = { mode: "inline" } | { mode: "dialog" } | { mode: "blocked"; reason: string };

/** Decide how a cell is edited. */
export function editorModeFor(c: ColumnInfo, value: Cell): EditorMode {
  if (isGeneratedColumn(c)) return { mode: "blocked", reason: `${c.name} is a generated column and cannot be edited.` };
  // A value that was cut when it was read: an editor would start from its first part and save that.
  const cut = uneditableReason(value);
  if (cut) return { mode: "blocked", reason: cut };
  const cat = typeCategory(c.dataType);
  if ((cat === "json" || cat === "jsonb" || cat === "array") && hasInexactNumber(value)) {
    return { mode: "blocked", reason: "This value holds numbers too large or precise to edit here without rounding them. Update it with SQL." };
  }
  if (cat === "json" || cat === "jsonb" || cat === "array") return { mode: "dialog" };
  if (cat === "bytea" && isBytesCell(value) && value.len > MAX_BYTES_EDIT) return { mode: "blocked", reason: "Binary values over 4 KB cannot be edited here." };
  if (cat === "bool") return { mode: "inline" };
  if (typeof value === "string" && (value.includes("\n") || value.length > LONG_TEXT)) return { mode: "dialog" };
  return { mode: "inline" };
}

function BooleanEditor({ column, value, onCommit, onCancel }: { column: EditorColumn; value: Cell; onCommit: (c: Cell, m: Move) => void; onCancel: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const done = useRef(false);
  const options: { label: string; value: Cell }[] = [
    { label: "true", value: true },
    { label: "false", value: false },
    ...(column.nullable ? [{ label: "NULL", value: null as Cell }] : []),
  ];
  const [idx, setIdx] = useState(Math.max(0, options.findIndex((o) => o.value === (value ?? null))));
  useEffect(() => ref.current?.focus(), []);
  const finish = (v: Cell, m: Move = "none") => {
    if (done.current) return;
    done.current = true;
    onCommit(v, m);
  };
  return (
    <div
      ref={ref}
      tabIndex={0}
      role="radiogroup"
      aria-label={`${column.name} value`}
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null) && !done.current) {
          done.current = true;
          onCancel();
        }
      }}
      onKeyDown={(e) => {
        e.stopPropagation();
        if (e.key === "Escape") {
          done.current = true;
          onCancel();
        } else if (e.key === "Enter") {
          e.preventDefault();
          finish(options[idx].value);
        } else if (e.key === "Tab") {
          e.preventDefault();
          finish(options[idx].value, e.shiftKey ? "left" : "right");
        } else if (e.key === "ArrowRight" || e.key === " ") {
          e.preventDefault();
          setIdx((i) => (i + 1) % options.length);
        } else if (e.key === "ArrowLeft") {
          e.preventDefault();
          setIdx((i) => (i - 1 + options.length) % options.length);
        } else if (e.key.toLowerCase() === "t") finish(true);
        else if (e.key.toLowerCase() === "f") finish(false);
        else if (e.key.toLowerCase() === "n" && column.nullable) finish(null);
      }}
      className="flex h-full items-center gap-1 px-1 outline-none"
    >
      {options.map((o, i) => (
        <button
          key={o.label}
          type="button"
          role="radio"
          aria-checked={i === idx}
          tabIndex={-1}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => finish(o.value)}
          className={cn(
            "h-5 rounded px-1.5 font-sans text-[11px] font-medium",
            i === idx ? "bg-accent text-accent-fg" : "bg-panel-2 text-muted hover:text-fg",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function InlineCellEditor({
  column,
  value,
  initialText,
  onCommit,
  onCancel,
}: {
  column: EditorColumn;
  value: Cell;
  /** A character typed on the selected cell: replaces the content instead of selecting it. */
  initialText?: string;
  onCommit: (cell: Cell, move: Move) => void;
  onCancel: () => void;
}) {
  if (typeCategory(column.dataType) === "bool") return <BooleanEditor column={column} value={value} onCommit={onCommit} onCancel={onCancel} />;
  return <TextEditor column={column} value={value} initialText={initialText} onCommit={onCommit} onCancel={onCancel} />;
}

function TextEditor({ column, value, initialText, onCommit, onCancel }: { column: EditorColumn; value: Cell; initialText?: string; onCommit: (c: Cell, m: Move) => void; onCancel: () => void }) {
  const [text, setText] = useState(initialText ?? cellToEditText(value));
  const [err, setErr] = useState<string | null>(null);
  const done = useRef(false);
  const input = useRef<HTMLInputElement>(null);
  const numeric = ["int", "float", "numeric"].includes(typeCategory(column.dataType));

  useEffect(() => {
    const el = input.current;
    if (!el) return;
    el.focus();
    if (initialText !== undefined) el.setSelectionRange(el.value.length, el.value.length);
    else el.select();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const commit = (move: Move): boolean => {
    if (done.current) return true;
    // Leaving an empty editor on a NULL cell is not an edit to ''.
    if ((value === null || value === undefined) && text === "") {
      done.current = true;
      onCancel();
      return true;
    }
    const p = parseEditedText(text, column.dataType);
    if (!p.ok) {
      setErr(p.error);
      return false;
    }
    done.current = true;
    onCommit(p.cell, move);
    return true;
  };

  return (
    <div className="relative h-full">
      <input
        ref={input}
        value={text}
        spellCheck={false}
        autoComplete="off"
        autoCapitalize="off"
        inputMode={numeric ? "decimal" : undefined}
        aria-label={`Edit ${column.name}`}
        aria-invalid={err ? true : undefined}
        onChange={(e) => {
          setText(e.target.value);
          setErr(null);
        }}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter") {
            e.preventDefault();
            commit("none");
          } else if (e.key === "Tab") {
            e.preventDefault();
            commit(e.shiftKey ? "left" : "right");
          } else if (e.key === "Escape") {
            e.preventDefault();
            done.current = true;
            onCancel();
          }
        }}
        onBlur={() => {
          if (done.current) return;
          if (!commit("none")) {
            done.current = true;
            showToast({ title: "Edit discarded", detail: err ?? "The value was not valid for this column.", tone: "error" });
            onCancel();
          }
        }}
        className={cn("block h-full w-full bg-transparent font-mono text-[12px] leading-6 text-fg outline-none", column.nullable ? "pl-2 pr-11" : "px-2", numeric && "tabular-nums")}
      />
      {column.nullable && (
        <button
          type="button"
          tabIndex={-1}
          title="Set to NULL"
          aria-label="Set to NULL"
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => {
            if (done.current) return;
            done.current = true;
            onCommit(null, "none");
          }}
          className="absolute right-1 top-1/2 h-4 -translate-y-1/2 rounded bg-panel-2 px-1 font-sans text-[10px] font-semibold leading-4 text-muted hover:bg-border hover:text-fg"
        >
          NULL
        </button>
      )}
      {err && (
        <div role="alert" className="absolute left-0 top-full z-30 mt-1 w-max max-w-80 rounded-md border border-danger/50 bg-panel px-2 py-1 font-sans text-[11px] leading-4 text-danger shadow-xl">
          {err}
        </div>
      )}
    </div>
  );
}

/** JSON, arrays and long or multi-line text. */
export function LongValueEditor({
  column,
  value,
  title,
  onClose,
  onCommit,
}: {
  column: EditorColumn;
  value: Cell;
  title: string;
  onClose: () => void;
  onCommit: (cell: Cell) => void;
}) {
  const cat = typeCategory(column.dataType);
  const isJson = cat === "json" || cat === "jsonb";
  const [text, setText] = useState(cellToEditText(value));
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    ref.current?.focus();
  }, []);
  const parsed = useMemo(() => parseEditedText(text, column.dataType), [text, column.dataType]);
  const apply = () => {
    if (parsed.ok) onCommit(parsed.cell);
  };
  return (
    <Dialog
      open
      onClose={onClose}
      width="max-w-3xl"
      title={
        <span className="flex items-center gap-2 font-mono text-[13px]">
          {isJson && <Braces className="size-3.5 text-muted" />}
          {title}
        </span>
      }
      footer={
        <>
          <div className="mr-auto flex items-center gap-2">
            {isJson && (
              <Button
                variant="secondary"
                disabled={!parsed.ok}
                onClick={() => parsed.ok && setText(JSON.stringify(parsed.cell, null, 2))}
                title="Pretty-print the JSON"
              >
                Format
              </Button>
            )}
            {column.nullable && (
              <Button variant="secondary" onClick={() => onCommit(null)} title="Store NULL instead of a value">
                Set NULL
              </Button>
            )}
          </div>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" disabled={!parsed.ok} onClick={apply} title={`${MOD}+Enter`}>
            Apply
          </Button>
        </>
      }
    >
      <textarea
        ref={ref}
        value={text}
        spellCheck={false}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            apply();
          }
        }}
        className="selectable block h-[52vh] w-full resize-none rounded-md border border-border bg-bg p-3 font-mono text-[12px] leading-5 text-fg outline-none focus:border-accent/70"
        aria-label={`Edit ${column.name}`}
      />
      <div className={cn("mt-2 min-h-4 text-[11px]", parsed.ok ? "text-muted" : "text-danger")} role={parsed.ok ? undefined : "alert"}>
        {parsed.ok ? (isJson ? "Valid JSON" : `${text.length.toLocaleString("en-US")} characters`) : parsed.error}
      </div>
    </Dialog>
  );
}
