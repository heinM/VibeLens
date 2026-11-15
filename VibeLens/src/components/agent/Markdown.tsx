import { memo, type ReactNode } from "react";
import { cn } from "../../lib/cn";
import { openExternal } from "../ui/vgExternal";
import { CodeBlock } from "./CodeView";
import { parseBlocks, type Inline } from "./markdownParser";

function renderInline(nodes: Inline[]): ReactNode[] {
  return nodes.map((n, i) => {
    switch (n.t) {
      case "text":
        return n.v;
      case "code":
        return (
          <code key={i} className="rounded bg-panel-2 px-1 py-px font-mono text-[0.92em] text-fg">
            {n.v}
          </code>
        );
      case "bold":
        return (
          <strong key={i} className="font-semibold text-fg">
            {renderInline(n.v)}
          </strong>
        );
      case "italic":
        return <em key={i}>{renderInline(n.v)}</em>;
      case "link":
        return (
          <a
            key={i}
            href={n.href}
            className="text-accent underline-offset-2 hover:underline"
            onClick={(e) => {
              e.preventDefault();
              void openExternal(n.href);
            }}
          >
            {renderInline(n.v)}
          </a>
        );
    }
  });
}

export const Markdown = memo(function Markdown({ text, className, wrapCode }: { text: string; className?: string; wrapCode?: boolean }) {
  const blocks = parseBlocks(text);
  return (
    <div className={cn("selectable space-y-2 text-[13px] leading-[1.55] text-fg/95", className)}>
      {blocks.map((b, i) => {
        switch (b.t) {
          case "p":
            return <p key={i}>{renderInline(b.v)}</p>;
          case "h":
            return (
              <p key={i} className={cn("font-semibold text-fg", b.level === 1 ? "text-[15px]" : "text-[13.5px]")}>
                {renderInline(b.v)}
              </p>
            );
          case "ul":
            return (
              <ul key={i} className="ml-4 list-disc space-y-0.5 marker:text-muted">
                {b.items.map((it, j) => (
                  <li key={j}>{renderInline(it)}</li>
                ))}
              </ul>
            );
          case "ol":
            return (
              <ol key={i} start={b.start} className="ml-5 list-decimal space-y-0.5 marker:text-muted">
                {b.items.map((it, j) => (
                  <li key={j}>{renderInline(it)}</li>
                ))}
              </ol>
            );
          case "quote":
            return (
              <blockquote key={i} className="border-l-2 border-border pl-3 text-muted">
                {renderInline(b.v)}
              </blockquote>
            );
          case "table":
            return (
              <div key={i} className="my-2 overflow-x-auto rounded-md border border-border">
                <table className="w-full border-collapse text-[12px] leading-snug">
                  <thead className="bg-panel-2/60">
                    <tr>
                      {b.head.map((h, j) => (
                        <th key={j} style={{ textAlign: b.align[j] ?? "left" }} className="border-b border-border px-2.5 py-1.5 font-semibold text-fg">
                          {renderInline(h)}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {b.rows.map((r, ri) => (
                      <tr key={ri} className={ri > 0 ? "border-t border-border/60" : undefined}>
                        {r.map((c, j) => (
                          <td key={j} style={{ textAlign: b.align[j] ?? "left" }} className="px-2.5 py-1.5 align-top text-fg/90">
                            {renderInline(c)}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
          case "code":
            return <CodeBlock key={i} lang={b.lang} code={b.code} open={b.open} wrap={wrapCode} />;
        }
      })}
    </div>
  );
});
