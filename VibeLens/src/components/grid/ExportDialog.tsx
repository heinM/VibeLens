// "Export CSV… / JSON… / SQL INSERTs…": choose options, then the native Save dialog opens via api.exportFile.
import { useEffect, useMemo, useState } from "react";
import { FileDown } from "lucide-react";
import { api, asError } from "../../ipc/commands";
import type { Cell, ColumnMeta } from "../../ipc/types";
import { readStorage, writeStorage } from "../layout/storage";
import { Button, Dialog, Field, Input } from "../ui";
import { suggestFileName, toCsv, toJson, toSqlInserts, type InsertTarget } from "./exportFormats";
import { formatCount } from "./cellFormat";
import { showToast } from "./toast";

export type ExportFormat = "csv" | "json" | "sql";

const TITLES: Record<ExportFormat, string> = { csv: "Export CSV", json: "Export JSON", sql: "Export as SQL INSERTs" };
const EXT: Record<ExportFormat, string> = { csv: "csv", json: "json", sql: "sql" };

function Check({ checked, onChange, label, hint }: { checked: boolean; onChange: (v: boolean) => void; label: string; hint?: string }) {
  return (
    <label className="flex cursor-pointer items-start gap-2 text-[13px]">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} className="mt-0.5 size-3.5 accent-[var(--vg-accent)]" />
      <span>
        <span className="text-fg">{label}</span>
        {hint && <span className="block text-[11px] text-muted">{hint}</span>}
      </span>
    </label>
  );
}

export function ExportDialog({
  format,
  onClose,
  columns,
  getRows,
  baseName,
  table,
}: {
  /** null = closed */
  format: ExportFormat | null;
  onClose: () => void;
  columns: ColumnMeta[];
  /** Called when the user confirms, so the (possibly still growing) row buffer is read at that moment. */
  getRows: () => Cell[][];
  baseName: string;
  table: InsertTarget | null;
}) {
  const [bom, setBom] = useState(() => readStorage("vg.export.bom") === "1");
  const [header, setHeader] = useState(true);
  const [delimiter, setDelimiter] = useState<"," | ";" | "\t">(() => {
    const v = readStorage("vg.export.delim");
    return v === ";" || v === "\t" ? v : ",";
  });
  const [pretty, setPretty] = useState(true);
  const [tableName, setTableName] = useState("");
  const [batch, setBatch] = useState("500");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (format) {
      setError(null);
      setTableName(table ? (table.schema ? `${table.schema}.${table.name}` : table.name) : "");
    }
  }, [format, table]);

  const rowCount = format ? getRows().length : 0;
  const fileName = useMemo(() => (format ? suggestFileName(baseName, EXT[format]) : ""), [format, baseName]);

  if (!format) return null;

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      const rows = getRows();
      let contents: string;
      if (format === "csv") {
        contents = toCsv(columns, rows, { bom, header, delimiter });
        writeStorage("vg.export.bom", bom ? "1" : "0");
        writeStorage("vg.export.delim", delimiter);
      } else if (format === "json") contents = toJson(columns, rows, pretty);
      else {
        const t = tableName.trim();
        let target: InsertTarget | null = null;
        if (t) {
          const dot = t.indexOf(".");
          target = dot > 0 ? { schema: t.slice(0, dot), name: t.slice(dot + 1) } : { name: t };
        }
        contents = toSqlInserts(columns, rows, target, Math.max(1, Math.min(5000, Number.parseInt(batch, 10) || 500)));
      }
      const path = await api.exportFile(fileName, contents);
      if (path) {
        showToast({ title: `Exported ${formatCount(rows.length)} ${rows.length === 1 ? "row" : "rows"}`, detail: path, copyDetail: true });
        onClose();
      }
    } catch (e) {
      setError(asError(e).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog
      open
      onClose={onClose}
      title={TITLES[format]}
      width="max-w-md"
      footer={
        <>
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" loading={busy} disabled={rowCount === 0 && format !== "json"} onClick={() => void run()}>
            <FileDown className="size-3.5" /> Choose location…
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <p className="text-[13px] text-muted">
          {formatCount(rowCount)} loaded {rowCount === 1 ? "row" : "rows"}, {columns.length} {columns.length === 1 ? "column" : "columns"}. Only rows already loaded in the grid are exported.
        </p>
        {format === "csv" && (
          <div className="space-y-3">
            <Check checked={header} onChange={setHeader} label="Include a header row" />
            <Check checked={bom} onChange={setBom} label="Add a UTF-8 byte-order mark" hint="Lets Excel open accented and non-Latin text correctly. Leave off for most other tools." />
            <Field label="Delimiter">
              <select
                value={delimiter}
                onChange={(e) => setDelimiter(e.target.value as "," | ";" | "\t")}
                className="h-8 w-full rounded-md border border-border bg-bg px-2 text-[13px] text-fg outline-none focus:border-accent/70"
              >
                <option value=",">Comma (,)</option>
                <option value=";">Semicolon (;)</option>
                <option value={"\t"}>Tab</option>
              </select>
            </Field>
          </div>
        )}
        {format === "json" && (
          <div className="space-y-3">
            <Check checked={pretty} onChange={setPretty} label="Indent for readability" />
            <p className="text-[11px] text-muted">An array of objects. bigint and numeric values stay strings so no digits are lost; binary values become \x hex text.</p>
          </div>
        )}
        {format === "sql" && (
          <div className="space-y-3">
            <Field label="Target table" hint={table ? undefined : "This result is not tied to a table. Name the table to load the rows into, or leave blank for a placeholder."}>
              <Input value={tableName} onChange={(e) => setTableName(e.target.value)} placeholder="schema.table" className="font-mono" />
            </Field>
            <Field label="Rows per INSERT">
              <Input value={batch} onChange={(e) => setBatch(e.target.value.replace(/\D/g, ""))} inputMode="numeric" className="w-24 tabular-nums" />
            </Field>
          </div>
        )}
        <div className="text-[11px] text-muted">
          Suggested name <span className="font-mono text-fg">{fileName}</span>
        </div>
        {error && <div className="selectable rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-xs text-danger">{error}</div>}
      </div>
    </Dialog>
  );
}
