// What the agent shows before the first message: who it is talking to, and a handful of starting points.
import type { ComponentType } from "react";
import { BookText, Gauge, GitCompareArrows, Hash, KeyRound, ListTree, Network, ScanSearch, Search, Sparkles, Table2, Timer, Wand2 } from "lucide-react";
import { cn } from "../../lib/cn";
import type { ConnectionProfile } from "../../ipc/types";
import { suggestionsFor, type Suggestion, type SuggestionIcon } from "./suggestions";

const ICONS: Record<SuggestionIcon, ComponentType<{ className?: string }>> = {
  schema: Network,
  slow: Gauge,
  index: ScanSearch,
  migrate: GitCompareArrows,
  review: Search,
  query: Wand2,
  doc: BookText,
  keys: KeyRound,
  ttl: Timer,
  pattern: Hash,
  size: Table2,
  list: ListTree,
};

const ACCESS_LINE: Record<ConnectionProfile["aiDataAccess"], string> = {
  none: "I can't see this connection, only what you paste or attach.",
  schemaOnly: "I see table and column names, never row values.",
  resultsWithApproval: "I see query results only after you approve each read.",
  results: "Query results are sent to your AI provider as I read them.",
};

export function AgentEmpty({
  profile,
  database,
  hasEditorText,
  canSend,
  primary,
  onPick,
}: {
  profile: ConnectionProfile | null;
  database: string | null;
  hasEditorText: boolean;
  canSend: boolean;
  /** Agent-first layout: roomy cards. Otherwise a compact list for the short bottom panel. */
  primary: boolean;
  onPick: (s: Suggestion) => void;
}) {
  const items = suggestionsFor({
    engine: profile?.engine ?? null,
    connectionName: profile?.name ?? null,
    database: profile?.engine === "postgres" ? (database ?? profile.database ?? null) : null,
    access: profile?.aiDataAccess ?? null,
    hasEditorText,
  });
  const where = profile ? [profile.name, profile.engine === "postgres" ? (database ?? profile.database) : null].filter(Boolean).join(" / ") : null;

  return (
    <div className={cn("mx-auto flex w-full flex-col", primary ? "max-w-2xl gap-6 px-2 pb-6 pt-[6vh]" : "max-w-3xl gap-2.5 py-2")}>
      <div className="flex items-start gap-3">
        <div className={cn("grid shrink-0 place-items-center rounded-xl border border-accent/30 bg-accent/10", primary ? "size-11" : "size-9")}>
          <Sparkles className={cn("text-accent", primary ? "size-5" : "size-4")} />
        </div>
        <div className="min-w-0">
          <h3 className={cn("font-semibold leading-tight tracking-tight text-fg", primary ? "text-[22px]" : "text-sm")}>
            {profile ? "What should we look at?" : "Pick a connection to begin"}
          </h3>
          <p className={cn("mt-1 leading-snug text-muted", primary ? "text-[13.5px]" : "text-xs")}>
            {profile ? (
              <>
                Working in <span className="font-medium text-fg">{where}</span>. {ACCESS_LINE[profile.aiDataAccess]} I ask before changing anything.
              </>
            ) : (
              "Select a connection in the sidebar and I'll suggest where to start. You can also just ask a question."
            )}
          </p>
        </div>
      </div>

      <div className={cn("grid gap-2", "grid-cols-1 @[560px]:grid-cols-2")}>
        {(primary ? items : items.slice(0, 4)).map((s) => {
          const Icon = ICONS[s.icon];
          const off = !canSend || !!s.disabledReason;
          return (
            <button
              key={s.id}
              type="button"
              disabled={!canSend}
              aria-disabled={s.disabledReason ? true : undefined}
              title={s.disabledReason ?? (s.incomplete ? "Fills the message box so you can finish it" : undefined)}
              onClick={() => !s.disabledReason && onPick(s)}
              className={cn(
                "group flex items-start gap-3 rounded-lg border border-border bg-bg text-left outline-none transition-colors focus-visible:ring-2 focus-visible:ring-accent/60",
                primary ? "p-3.5" : "items-center px-2.5 py-1.5",
                off ? "cursor-not-allowed opacity-55" : "hover:border-accent/50 hover:bg-panel-2",
              )}
            >
              <span className={cn("grid shrink-0 place-items-center rounded-md border border-border bg-panel-2 text-muted transition-colors", primary ? "size-8" : "size-6", !off && "group-hover:border-accent/40 group-hover:text-accent")}>
                <Icon className={primary ? "size-4" : "size-3.5"} />
              </span>
              <span className="min-w-0">
                <span className={cn("block truncate font-medium text-fg", primary ? "text-[13.5px]" : "text-[12.5px]")}>{s.title}</span>
                <span className={cn("block text-muted", primary ? "mt-0.5 text-[12px]" : "hidden")}>{s.disabledReason ? "Needs results access" : s.hint}</span>
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
