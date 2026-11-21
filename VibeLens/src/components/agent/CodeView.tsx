import { memo, useEffect, useRef, useState } from "react";
import { Check, CornerDownRight, SquareArrowOutUpRight } from "lucide-react";
import { cn } from "../../lib/cn";
import { useWorkspaceStore } from "../../store/useWorkspaceStore";
import { currentAgentTarget } from "../../hooks/useAgentContext";
import { insertIntoActiveEditor } from "./editorInsert";
import { openAgentTab } from "./openInTab";
import { Button } from "../ui";
import { CopyIconButton } from "../ui/formKit";
import { tokenizeSql, type SqlTokenKind } from "./sqlHighlight";

const SQL_CLASS: Record<SqlTokenKind, string> = {
  kw: "text-accent font-medium",
  str: "text-ok",
  num: "text-warn",
  com: "text-muted italic",
  text: "",
};

export const SqlText = memo(function SqlText({ sql }: { sql: string }) {
  return (
    <>
      {tokenizeSql(sql).map((t, i) => (
        <span key={i} className={SQL_CLASS[t.k]}>
          {t.v}
        </span>
      ))}
    </>
  );
});

export const DiffText = memo(function DiffText({ diff }: { diff: string }) {
  const lines = diff.split("\n");
  return (
    <>
      {lines.map((l, i) => {
        const add = l.startsWith("+") && !l.startsWith("+++");
        const del = l.startsWith("-") && !l.startsWith("---");
        const hunk = l.startsWith("@@");
        const head = l.startsWith("+++") || l.startsWith("---") || l.startsWith("diff ");
        return (
          <div
            key={i}
            className={cn(
              "-mx-3 min-h-[1.25em] whitespace-pre px-3",
              add && "bg-ok/12 text-ok",
              del && "bg-danger/12 text-danger",
              hunk && "text-accent/90",
              head && "font-medium text-muted",
            )}
          >
            {l === "" ? " " : l}
          </div>
        );
      })}
    </>
  );
});

export type PreviewBodyKind = "sql" | "diff" | "text";

export function CodeBody({ kind, code, className, wrap }: { kind: PreviewBodyKind; code: string; className?: string; wrap?: boolean }) {
  return (
    <pre className={cn("selectable overflow-auto px-3 py-2 font-mono text-[12px] leading-[1.45] text-fg", wrap && "whitespace-pre-wrap break-words", className)}>
      <code>{kind === "sql" ? <SqlText sql={code} /> : kind === "diff" ? <DiffText diff={code} /> : code}</code>
    </pre>
  );
}

const SQL_LANGS = new Set(["sql", "pgsql", "postgres", "postgresql", "psql", "plpgsql"]);
const VALKEY_LANGS = new Set(["redis", "valkey"]);

export function kindForLang(lang: string): PreviewBodyKind {
  if (SQL_LANGS.has(lang)) return "sql";
  if (lang === "diff" || lang === "patch") return "diff";
  return "text";
}

export function CodeBlock({ lang, code, open, wrap }: { lang: string; code: string; open: boolean; wrap?: boolean }) {
  const kind = kindForLang(lang);
  const openable = SQL_LANGS.has(lang) || VALKEY_LANGS.has(lang);
  const isSql = SQL_LANGS.has(lang);
  // Offer "Insert" only when there is a SQL tab to put it in; say what it will do to the selection.
  // 0: no SQL tab, 1: a SQL tab, 2: a SQL tab with a selection. A primitive, so the selector is stable.
  const tabState = useWorkspaceStore((s) => {
    const t = s.tabs.find((x) => x.id === s.activeTabId);
    return t?.kind === "sql" ? (t.selectedText ? 2 : 1) : 0;
  });
  const sqlTab = tabState > 0 ? { replaces: tabState === 2 } : null;
  const [inserted, setInserted] = useState<"editor" | "appended" | null>(null);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const insert = async () => {
    const outcome = await insertIntoActiveEditor(code);
    if (outcome === "no-tab") return;
    setInserted(outcome);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setInserted(null), 1800);
  };
  const openInTab = () => {
    // The pinned target while pinned, else the focused connection.
    const target = currentAgentTarget();
    openAgentTab({ kind: isSql ? "sql" : "valkey", contents: code, profileId: target.profileId, database: target.database });
  };
  return (
    <div className="my-2 overflow-hidden rounded-md border border-border bg-bg">
      <div className="flex h-7 items-center gap-1 border-b border-border bg-panel-2/60 pl-3 pr-1">
        <span className="font-mono text-[11px] text-muted">{lang || "text"}</span>
        {open && <span className="ml-1 text-[10px] text-muted">writing…</span>}
        <span className="flex-1" />
        {isSql && !open && sqlTab && (
          <Button
            size="sm"
            variant="ghost"
            className="!h-6 !px-1.5"
            onClick={() => void insert()}
            title={sqlTab.replaces ? "Replace the selected text in the active SQL tab with this" : "Insert at the cursor in the active SQL tab"}
          >
            {inserted ? <Check className="size-3 text-ok" /> : <CornerDownRight className="size-3" />}
            {inserted === "appended" ? "Added to end" : inserted ? "Inserted" : sqlTab.replaces ? "Replace selection" : "Insert into editor"}
          </Button>
        )}
        {openable && !open && (
          <Button size="sm" variant="ghost" className="!h-6 !px-1.5" onClick={openInTab}>
            <SquareArrowOutUpRight className="size-3" /> Open in new tab
          </Button>
        )}
        <CopyIconButton text={code} label="Copy code" className="!size-6" />
      </div>
      <CodeBody kind={kind} code={code} className="max-h-72" wrap={wrap} />
    </div>
  );
}
