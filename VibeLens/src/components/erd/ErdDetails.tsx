// Side panel for the selected table: columns, relations, indexes and quick actions.
import { Copy, KeyRound, Link2, Table2, X } from "lucide-react";
import { copyToClipboard } from "../ui/clipboard";
import { Button, IconButton } from "../ui";
import { Chip } from "../ui/wsKit";
import { formatRows } from "./layout";
import { relationsOf, type ErdModel, type ErdTable } from "./model";

export function ErdDetails({
  table,
  model,
  onClose,
  onOpenData,
  onFocus,
}: {
  table: ErdTable;
  model: ErdModel;
  onClose: () => void;
  onOpenData: () => void;
  onFocus: (key: string) => void;
}) {
  const { parents, children } = relationsOf(model, table.key);
  const outside = table.source.foreignKeys.filter((f) => f.referencedSchema !== table.schema);
  const rows = formatRows(table.rows);
  const section = "border-t border-border px-4 py-3";
  const heading = "mb-1.5 text-[11px] font-medium text-muted";

  const relBtn = (key: string, text: string, sub: string) => (
    <button
      key={`${key}${sub}`}
      type="button"
      onClick={() => onFocus(key)}
      className="flex w-full items-baseline justify-between gap-2 rounded px-1.5 py-1 text-left text-[12px] outline-none hover:bg-panel-2 focus-visible:ring-1 focus-visible:ring-accent/60"
    >
      <span className="truncate font-medium text-fg">{text}</span>
      <span className="shrink-0 truncate font-mono text-[11px] text-muted">{sub}</span>
    </button>
  );

  return (
    <aside aria-label={`Details of ${table.name}`} data-erd-ui className="absolute inset-y-0 right-0 z-20 flex w-[320px] flex-col border-l border-border bg-panel shadow-[-12px_0_32px_-16px_rgba(0,0,0,0.5)]">
      <div className="flex items-start gap-2 px-4 pb-3 pt-3.5">
        <Table2 className="mt-0.5 size-4 shrink-0 text-sky-500" aria-hidden />
        <div className="min-w-0 flex-1">
          <h3 className="truncate text-[14px] font-semibold text-fg" title={table.name}>
            {table.name}
          </h3>
          <p className="truncate text-[11px] text-muted">
            {table.kind === "partitionedTable" ? "Partitioned table" : table.kind === "foreignTable" ? "Foreign table" : "Table"} in {table.schema}
          </p>
        </div>
        <IconButton label="Close details" onClick={onClose} className="-mr-1.5 -mt-1">
          <X className="size-4" />
        </IconButton>
      </div>
      <div className="flex flex-wrap items-center gap-1.5 px-4 pb-3">
        {rows && <Chip title="Estimated row count">{rows} rows</Chip>}
        <Chip>{table.columns.length} columns</Chip>
        <Chip>{table.source.indexes.length} {table.source.indexes.length === 1 ? "index" : "indexes"}</Chip>
      </div>
      {table.comment && <p className="px-4 pb-3 text-[12px] leading-relaxed text-muted">{table.comment}</p>}
      <div className="flex gap-2 px-4 pb-3">
        <Button variant="primary" size="sm" onClick={onOpenData}>
          Open data
        </Button>
        <Button size="sm" onClick={() => void copyToClipboard(`"${table.schema}"."${table.name}"`)}>
          <Copy className="size-3.5" /> Copy name
        </Button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className={section}>
          <h4 className={heading}>Columns</h4>
          <ul className="space-y-px">
            {table.columns.map((c) => (
              <li key={c.name} className="flex items-center gap-2 px-1.5 py-[3px] text-[12px]">
                <span className="flex size-3.5 shrink-0 items-center justify-center">
                  {c.pk ? <KeyRound className="size-3 text-amber-500" aria-label="Primary key" /> : c.fk ? <Link2 className="size-3 text-accent" aria-label="Foreign key" /> : null}
                </span>
                <span className={c.pk ? "truncate font-semibold text-fg" : "truncate text-fg"}>{c.name}</span>
                <span className="ml-auto shrink-0 truncate font-mono text-[11px] text-muted">{c.type}</span>
                <span className="w-2 shrink-0 text-center text-[11px] text-muted/70" title={c.nullable ? "nullable" : "not null"} aria-label={c.nullable ? "nullable" : "not null"}>
                  {c.nullable ? "?" : ""}
                </span>
              </li>
            ))}
          </ul>
        </div>

        {(parents.length > 0 || outside.length > 0) && (
          <div className={section}>
            <h4 className={heading}>References</h4>
            {parents.map((e) => relBtn(e.to, e.to.slice(table.schema.length + 1), `${e.fromCols.join(", ")} → ${e.toCols.join(", ")}`))}
            {outside.map((f) => (
              <div key={f.name} className="flex items-baseline justify-between gap-2 px-1.5 py-1 text-[12px] text-muted" title="In another schema, so it is not drawn">
                <span className="truncate">
                  {f.referencedSchema}.{f.referencedTable}
                </span>
                <span className="shrink-0 truncate font-mono text-[11px]">{f.columns.join(", ")}</span>
              </div>
            ))}
          </div>
        )}

        {children.length > 0 && (
          <div className={section}>
            <h4 className={heading}>Referenced by</h4>
            {children.map((e) => relBtn(e.from, e.from.slice(table.schema.length + 1), `${e.fromCols.join(", ")} → ${e.toCols.join(", ")}`))}
          </div>
        )}

        {table.source.indexes.length > 0 && (
          <div className={section}>
            <h4 className={heading}>Indexes</h4>
            <ul className="space-y-1.5">
              {table.source.indexes.map((ix) => (
                <li key={ix.name} className="text-[12px]">
                  <div className="flex items-center gap-1.5">
                    <span className="truncate font-medium text-fg">{ix.name}</span>
                    {ix.isPrimary && <Chip tone="accent">primary</Chip>}
                    {!ix.isPrimary && ix.isUnique && <Chip>unique</Chip>}
                  </div>
                  <div className="selectable mt-0.5 break-words font-mono text-[10.5px] leading-snug text-muted">{ix.definition.replace(/^CREATE (UNIQUE )?INDEX \S+ ON /, "")}</div>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </aside>
  );
}
