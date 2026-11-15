import { useMemo, useState } from "react";
import { Check, ChevronDown, ChevronRight, ChevronsDownUp, ChevronsUpDown } from "lucide-react";
import type { IcebergTableInfo } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { Button } from "../ui";
import { Chip } from "../ui/wsKit";
import { CopyIconButton } from "../ui/formKit";
import { countFields, flattenFields, nestedIds, partitionSources, sortSources, typeTone, type TypeTone } from "./iceTableModel";
import { Card, InlineEmpty, PaneHeader } from "./icePrimitives";

const TONE: Record<TypeTone, string> = {
  number: "text-amber-600 dark:text-amber-300",
  text: "text-emerald-600 dark:text-emerald-300",
  bool: "text-violet-600 dark:text-violet-300",
  time: "text-sky-600 dark:text-sky-300",
  nested: "text-fuchsia-600 dark:text-fuchsia-300",
  other: "text-fg",
};

export function SchemaPane({ info }: { info: IcebergTableInfo }) {
  const [collapsed, setCollapsed] = useState<ReadonlySet<number>>(() => new Set());
  const rows = useMemo(() => flattenFields(info.fields, collapsed), [info.fields, collapsed]);
  const total = countFields(info.fields);
  const nested = useMemo(() => nestedIds(info.fields), [info.fields]);
  const partSrc = useMemo(() => partitionSources(info), [info]);
  const sortSrc = useMemo(() => sortSources(info), [info]);
  const toggle = (id: number) =>
    setCollapsed((prev) => {
      const n = new Set(prev);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });

  return (
    <div>
      <PaneHeader
        title="Schema"
        subtitle={
          <>
            {total} {total === 1 ? "field" : "fields"} · schema id {info.currentSchemaId} · schema history: {info.schemaCount} {info.schemaCount === 1 ? "version" : "versions"}
          </>
        }
        actions={
          nested.length > 0 && (
            <>
              <Button size="sm" variant="ghost" onClick={() => setCollapsed(new Set())} title="Expand all nested fields">
                <ChevronsUpDown className="size-3.5" /> Expand
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setCollapsed(new Set(nested))} title="Collapse all nested fields">
                <ChevronsDownUp className="size-3.5" /> Collapse
              </Button>
            </>
          )
        }
      />
      {rows.length === 0 ? (
        <InlineEmpty>This table has no columns.</InlineEmpty>
      ) : (
        <Card className="overflow-hidden">
          <div className="overflow-x-auto">
            <table className="w-full min-w-[640px] text-[13px]">
              <thead>
                <tr className="border-b border-border bg-panel-2/50 text-left text-[11px] font-medium text-muted">
                  <th className="w-[34%] px-3 py-1.5 font-medium">Name</th>
                  <th className="w-14 px-2 py-1.5 text-right font-medium" title="Field IDs are permanent: renames and reorders never change them">
                    ID
                  </th>
                  <th className="px-2 py-1.5 font-medium">Type</th>
                  <th className="w-20 px-2 py-1.5 font-medium">Required</th>
                  <th className="px-2 py-1.5 font-medium">Doc</th>
                </tr>
              </thead>
              <tbody>
                {rows.map(({ field: f, depth, path, expandable, expanded }) => (
                  <tr key={path + f.id} className="group border-b border-border/60 last:border-b-0 hover:bg-panel-2/50">
                    <td className="px-3 py-1">
                      <div className="flex items-center gap-1" style={{ paddingLeft: depth * 18 }}>
                        {expandable ? (
                          <button type="button" aria-label={expanded ? `Collapse ${f.name}` : `Expand ${f.name}`} aria-expanded={expanded} onClick={() => toggle(f.id)} className="grid size-4 shrink-0 place-items-center rounded text-muted hover:bg-border hover:text-fg">
                            {expanded ? <ChevronDown className="size-3.5" /> : <ChevronRight className="size-3.5" />}
                          </button>
                        ) : (
                          <span className="size-4 shrink-0" />
                        )}
                        <span className={cn("selectable font-mono text-[12.5px]", depth === 0 && "font-medium")}>{f.name}</span>
                        {partSrc.has(f.id) && <Chip tone="accent" title="A partition field is derived from this column">partition</Chip>}
                        {sortSrc.has(f.id) && <Chip title="Part of the table's sort order">sort</Chip>}
                        <CopyIconButton text={path} label={`Copy ${path}`} className="!size-5 opacity-0 group-hover:opacity-100 focus-visible:opacity-100" />
                      </div>
                    </td>
                    <td className="px-2 py-1 text-right font-mono text-[12px] tabular-nums text-muted">{f.id}</td>
                    <td className={cn("selectable max-w-[22rem] break-words px-2 py-1 font-mono text-[12px]", TONE[typeTone(f.typeName)])} title={f.typeName}>
                      {expandable && f.typeName.startsWith("struct") ? "struct" : f.typeName}
                    </td>
                    <td className="px-2 py-1">
                      {f.required ? (
                        <span className="inline-flex items-center gap-1 text-[12px] text-fg">
                          <Check className="size-3.5 text-ok" /> required
                        </span>
                      ) : (
                        <span className="text-[12px] text-muted">optional</span>
                      )}
                    </td>
                    <td className="px-2 py-1 text-[12px] text-muted">{f.doc ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </Card>
      )}
      <p className="mt-3 max-w-prose text-[12px] leading-relaxed text-muted">
        Iceberg tracks columns by field ID, not by name or position, so renaming or reordering columns never breaks older snapshots or the files written under them.
      </p>
    </div>
  );
}
