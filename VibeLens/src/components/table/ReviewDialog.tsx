// "Review & apply (N)": the exact script that will run, highlighted, before anything is written.
import { memo, useMemo } from "react";
import { Check, Copy, SquareArrowOutUpRight } from "lucide-react";
import type { Environment, ErrorPayload } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { tokenizeSql, type SqlTokenKind } from "../agent/sqlHighlight";
import { Button, Dialog, EnvBadge, ErrorBox } from "../ui";
import { Chip } from "../ui/wsKit";
import { copyToClipboard } from "../ui/clipboard";
import { useState } from "react";
import type { ApplyScript } from "./pendingChanges";

const TOKEN_CLASS: Record<SqlTokenKind, string> = {
  kw: "text-accent font-medium",
  str: "text-ok",
  num: "text-warn",
  com: "text-muted italic",
  text: "",
};

const Statement = memo(function Statement({ sql }: { sql: string }) {
  const verb = sql.slice(0, 6).toUpperCase();
  const bar = verb === "UPDATE" ? "border-warn" : verb === "INSERT" ? "border-ok" : verb === "DELETE" ? "border-danger" : "border-border";
  const dim = verb === "BEGIN;" || verb === "COMMIT";
  return (
    <div className={cn("border-l-2 py-1 pl-3 pr-2 [overflow-wrap:anywhere] whitespace-pre-wrap", bar, dim && "text-muted")}>
      {tokenizeSql(sql).map((t, i) => (
        <span key={i} className={TOKEN_CLASS[t.k]}>
          {t.v}
        </span>
      ))}
    </div>
  );
});

export function ReviewDialog({
  open,
  tableLabel,
  script,
  scriptError,
  problems,
  applying,
  error,
  environment,
  connectionName,
  onApply,
  onClose,
  onOpenInSql,
}: {
  open: boolean;
  tableLabel: string;
  script: ApplyScript | null;
  /** The script could not be generated (a value does not fit its column type). */
  scriptError: string | null;
  /** Reasons the script would fail on the server; Apply is disabled while there are any. */
  problems: string[];
  applying: boolean;
  error: ErrorPayload | null;
  environment: Environment | null;
  connectionName: string | null;
  onApply: () => void;
  onClose: () => void;
  onOpenInSql: (sql: string) => void;
}) {
  const [copied, setCopied] = useState(false);
  const lines = useMemo(() => script?.sql.split("\n") ?? [], [script]);
  const c = script?.counts;
  const plural = (n: number, w: string) => `${n} ${w}${n === 1 ? "" : "s"}`;

  return (
    <Dialog
      open={open}
      onClose={() => !applying && onClose()}
      width="max-w-3xl"
      title={
        <span className="flex flex-wrap items-center gap-2">
          <span>Review changes to</span>
          <span className="font-mono text-[13px]">{tableLabel}</span>
          {environment && <EnvBadge env={environment} />}
        </span>
      }
      footer={
        <>
          <div className="mr-auto flex items-center gap-1">
            <Button
              variant="ghost"
              disabled={!script}
              onClick={async () => {
                if (script && (await copyToClipboard(script.sql))) {
                  setCopied(true);
                  window.setTimeout(() => setCopied(false), 1400);
                }
              }}
            >
              {copied ? <Check className="size-3.5 text-ok" /> : <Copy className="size-3.5" />} {copied ? "Copied" : "Copy SQL"}
            </Button>
            <Button variant="ghost" disabled={!script} onClick={() => script && onOpenInSql(script.sql)}>
              <SquareArrowOutUpRight className="size-3.5" /> Open in SQL tab
            </Button>
          </div>
          <Button variant="ghost" onClick={onClose} disabled={applying}>
            Cancel
          </Button>
          <Button variant="primary" onClick={onApply} loading={applying} disabled={!script || problems.length > 0}>
            {c ? `Apply ${plural(c.total, "change")}` : "Apply"}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        {c && (
          <div className="flex flex-wrap items-center gap-1.5">
            {c.updates > 0 && <Chip tone="warn">{plural(c.updates, "update")}</Chip>}
            {c.inserts > 0 && <Chip tone="ok">{plural(c.inserts, "insert")}</Chip>}
            {c.deletes > 0 && <Chip tone="danger">{plural(c.deletes, "delete")}</Chip>}
            <span className="text-[12px] text-muted">in one transaction. Nothing is written until you apply.</span>
          </div>
        )}
        {scriptError ? (
          <ErrorBox error={{ code: "invalid", message: scriptError }} />
        ) : (
          <div className="selectable max-h-[46vh] overflow-auto rounded-md border border-border bg-bg py-2 font-mono text-[12px] leading-[1.5] text-fg">
            {lines.map((l, i) => (
              <Statement key={i} sql={l} />
            ))}
          </div>
        )}
        {problems.length > 0 && (
          <ul className="space-y-0.5 rounded-md border border-warn/40 bg-warn/10 px-3 py-2 text-[12px] text-warn" role="alert">
            {problems.map((p) => (
              <li key={p}>{p}</li>
            ))}
          </ul>
        )}
        {environment === "prod" && connectionName && (
          <p className="text-[12px] text-env-prod">This is a production database. You will be asked to type the connection name before the change runs.</p>
        )}
        {error && (
          <div className="space-y-1">
            <ErrorBox error={error} />
            <p className="text-[12px] text-muted">The transaction was rolled back and nothing was changed. Your pending edits are still here.</p>
          </div>
        )}
      </div>
    </Dialog>
  );
}
