import { useMemo, useState } from "react";
import { Copy } from "lucide-react";
import type { Cell, ColumnMeta } from "../../ipc/types";
import { Button, Dialog } from "../ui";
import { Segmented } from "../ui/wsKit";
import { copyToClipboard } from "../ui/clipboard";
import { JsonTree } from "../json/JsonTree";
import { BinaryView } from "../json/BinaryView";
import { cellKind, copyText, formatBytes, isTruncatedCell, type BytesCell } from "./cellFormat";

/** Parse text that looks like a JSON object/array; null otherwise. */
function tryParseJson(s: string): unknown | null {
  const t = s.trimStart();
  if (t[0] !== "{" && t[0] !== "[") return null;
  try {
    const v: unknown = JSON.parse(s);
    return typeof v === "object" && v !== null ? v : null;
  } catch {
    return null;
  }
}

export function CellViewerDialog({
  open,
  onClose,
  column,
  rowIndex,
  cell,
}: {
  open: boolean;
  onClose: () => void;
  column: ColumnMeta | null;
  rowIndex: number;
  cell: Cell;
}) {
  const kind = cellKind(cell);
  const parsed = useMemo(() => (kind === "text" ? tryParseJson(cell as string) : null), [kind, cell]);
  const [mode, setMode] = useState<"tree" | "raw">("tree");
  const title = column ? `${column.name}  ·  row ${rowIndex + 1}  ·  ${column.typeName}` : "Cell";

  // A value that was too large to load in full: say so, then show the start that did arrive.
  const cut = isTruncatedCell(cell) ? cell.$truncated : null;
  const cutText = cut && (cut.kind === "text" || cut.kind === "json") ? cut.preview : null;

  let body;
  if (cut)
    body = (
      <div className="flex h-[56vh] flex-col gap-2">
        <p role="note" className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-[12px] leading-5 text-fg">
          This value is {formatBytes(cut.bytes)} ({cut.bytes.toLocaleString("en-US")} bytes), too large to load in full.{" "}
          {cut.kind === "other" ? "None of it is shown." : "Only its start is shown below; the rest was not loaded."} It cannot be edited here, and copies and exports carry a note in its place. To
          read more of it, select a part with SQL, for example <code className="font-mono">substr(column, 1, 100000)</code>.
        </p>
        {cut.kind === "bytes" ? (
          <div className="min-h-0 flex-1">
            <BinaryView base64={cut.preview} len={Math.floor((cut.preview.replace(/=+$/, "").length * 3) / 4)} />
          </div>
        ) : cutText !== null ? (
          <pre className="selectable min-h-24 flex-1 overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-bg p-3 font-mono text-[12px] leading-5">{cutText}</pre>
        ) : null}
      </div>
    );
  else if (kind === "json") body = <JsonTree value={cell} defaultExpandDepth={2} className="h-[56vh] rounded-md border border-border bg-bg" />;
  else if (kind === "bytes") body = <div className="h-[56vh]"><BinaryView base64={(cell as BytesCell).$bytes} len={(cell as BytesCell).len} /></div>;
  else if (parsed !== null && mode === "tree") body = <JsonTree value={parsed} defaultExpandDepth={2} className="h-[56vh] rounded-md border border-border bg-bg" />;
  else
    body = (
      <pre className="selectable max-h-[56vh] min-h-24 overflow-auto whitespace-pre-wrap break-words rounded-md border border-border bg-bg p-3 font-mono text-[12px] leading-5">
        {kind === "null" ? "NULL" : copyText(cell)}
      </pre>
    );

  return (
    <Dialog
      open={open}
      onClose={onClose}
      title={<span className="font-mono text-[13px]">{title}</span>}
      width="max-w-4xl"
      footer={
        <>
          {parsed !== null && (
            <div className="mr-auto">
              <Segmented
                label="Text format"
                value={mode}
                onChange={setMode}
                options={[
                  { value: "tree", label: "Tree" },
                  { value: "raw", label: "Raw" },
                ]}
              />
            </div>
          )}
          {cut ? (
            // The label says what is copied: the part on screen, not the value.
            cutText !== null && (
              <Button variant="secondary" onClick={() => void copyToClipboard(cutText)}>
                <Copy className="size-3.5" /> Copy the part shown
              </Button>
            )
          ) : (
            <Button variant="secondary" onClick={() => void copyToClipboard(kind === "json" ? JSON.stringify(cell, null, 2) : copyText(cell))}>
              <Copy className="size-3.5" /> Copy value
            </Button>
          )}
          <Button variant="primary" onClick={onClose}>
            Close
          </Button>
        </>
      }
    >
      {body}
    </Dialog>
  );
}
