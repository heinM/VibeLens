import type { IcebergTableInfo } from "../../ipc/types";
import { partitionRows, sortRows } from "./iceTableModel";
import { Card, InlineEmpty, PaneHeader } from "./icePrimitives";

function Th({ children, className = "" }: { children: React.ReactNode; className?: string }) {
  return <th className={`px-3 py-1.5 text-left text-[11px] font-medium text-muted ${className}`}>{children}</th>;
}

/** Partition spec and sort order, written the way you'd say them. */
export function LayoutPane({ info }: { info: IcebergTableInfo }) {
  const parts = partitionRows(info);
  const sorts = sortRows(info);
  return (
    <div className="space-y-7">
      <section>
        <PaneHeader
          title="Partitioning"
          subtitle={parts.length ? "Data files are grouped by these expressions. Queries that filter on the source columns skip whole groups." : undefined}
        />
        {parts.length === 0 ? (
          <InlineEmpty>
            Not partitioned. Every query considers every data file, narrowed only by the min/max statistics inside the files.
            <br />
            Changing this means writing a new partition spec from an engine that supports it, such as Athena or Spark.
          </InlineEmpty>
        ) : (
          <Card className="overflow-hidden">
            <table className="w-full text-[13px]">
              <thead>
                <tr className="border-b border-border bg-panel-2/50">
                  <Th>Expression</Th>
                  <Th>Partition column</Th>
                  <Th className="hidden @[640px]:table-cell">What it means</Th>
                </tr>
              </thead>
              <tbody>
                {parts.map((p) => (
                  <tr key={p.fieldId} className="border-b border-border/60 last:border-b-0">
                    <td className="selectable px-3 py-2 font-mono text-[12.5px] text-fg">{p.expression}</td>
                    <td className="px-3 py-2 font-mono text-[12px] text-muted">
                      <span className="mr-1.5 text-muted/60" aria-hidden>
                        →
                      </span>
                      {p.name}
                      <span className="ml-2 text-[11px] text-muted/70">#{p.fieldId}</span>
                    </td>
                    <td className="hidden px-3 py-2 text-[12px] text-muted @[640px]:table-cell">{p.explain}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </Card>
        )}
      </section>

      <section>
        <PaneHeader title="Sort order" subtitle={sorts.length ? "Writers sort rows within each file this way, which tightens statistics and speeds up range filters." : undefined} />
        {sorts.length === 0 ? (
          <InlineEmpty>No sort order. Rows are stored in the order they were written.</InlineEmpty>
        ) : (
          <Card className="overflow-hidden">
            <table className="w-full text-[13px]">
              <thead>
                <tr className="border-b border-border bg-panel-2/50">
                  <Th>Order by</Th>
                  <Th>Direction</Th>
                  <Th>Nulls</Th>
                </tr>
              </thead>
              <tbody>
                {sorts.map((s, i) => (
                  <tr key={i} className="border-b border-border/60 last:border-b-0">
                    <td className="selectable px-3 py-2 font-mono text-[12.5px]">{s.expression}</td>
                    <td className="px-3 py-2 text-[12px]">{s.direction === "ASC" ? "Ascending" : "Descending"}</td>
                    <td className="px-3 py-2 text-[12px] text-muted">{s.nulls === "NULLS FIRST" ? "First" : "Last"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </Card>
        )}
      </section>
    </div>
  );
}
