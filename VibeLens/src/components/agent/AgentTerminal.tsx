import { useMemo, useRef, useState } from "react";
import { ArrowDown, Settings2, Sparkles, SquarePen, Square } from "lucide-react";
import { cn } from "../../lib/cn";
import { useAgentContextInfo, NO_EXCLUSIONS, type ChipExclusions } from "../../hooks/useAgentContext";
import { useAgentProviders } from "../../hooks/useAgentProviders";
import { useAgentTurn } from "../../hooks/useAgentTurn";
import { useAgentStore, userPrompts, type TranscriptItem } from "../../store/useAgentStore";
import { useUIStore } from "../../store/useUIStore";
import { AgentEmpty } from "./AgentEmpty";
import { canAttachResults } from "./lastResult";
import { Button, ErrorBox, Select, Spinner } from "../ui";
import { Tip } from "../ui/formKit";
import { aiAccessMeta } from "../connections/aiAccess";
import { ApprovalCard, ApprovalNote } from "./ApprovalCard";
import { Composer } from "./Composer";
import { ContextChips } from "./ContextChips";
import { Markdown } from "./Markdown.tsx";
import { ToolCard } from "./ToolCard";
import { formatTokens } from "./toolSummary";
import { useStickToBottom } from "./useStickToBottom";

const ACCESS_TONE = {
  muted: "border-border text-muted",
  ok: "border-ok/40 bg-ok/10 text-ok",
  warn: "border-warn/40 bg-warn/10 text-warn",
  danger: "border-danger/50 bg-danger/10 text-danger",
} as const;

export function AgentTerminal() {
  const root = useRef<HTMLDivElement>(null);
  const transcript = useAgentStore((s) => s.transcript);
  const running = useAgentStore((s) => s.running);
  const pendingCallId = useAgentStore((s) => s.pendingApproval);
  const reset = useAgentStore((s) => s.reset);
  const setDefaultProviderId = useAgentStore((s) => s.setDefaultProviderId);
  const setSettingsOpen = useUIStore((s) => s.setSettingsOpen);
  const primary = useUIStore((s) => s.aiMode === "agentFirst");

  const info = useAgentContextInfo();
  const { usable, selected, query: providersQuery } = useAgentProviders();
  const { send, cancel, resolveApproval } = useAgentTurn();
  const stick = useStickToBottom(transcript.length > 0);

  const [draft, setDraft] = useState("");
  const [exclude, setExclude] = useState<ChipExclusions>(NO_EXCLUSIONS);
  const [focusToken, setFocusToken] = useState(0);
  const [attachResult, setAttachResult] = useState(false);

  const pending = useMemo(
    () => (pendingCallId ? (transcript.find((i) => i.kind === "approval" && i.callId === pendingCallId && i.status === "pending") ?? null) : null),
    [pendingCallId, transcript],
  );
  const history = useMemo(() => userPrompts(transcript), [transcript]);
  const access = info.profile ? aiAccessMeta(info.profile.aiDataAccess) : null;

  const sendDisabledReason = providersQuery.isPending
    ? "Loading providers…"
    : !selected
      ? "Set a model and API key for a provider in Settings to start."
      : null;

  const submit = () => {
    if (!selected || !draft.trim()) return;
    const text = draft;
    setDraft("");
    stick.jumpToBottom();
    const attach = attachResult && !!info.lastResult && canAttachResults(info.profile?.aiDataAccess);
    void send({ prompt: text, providerId: selected.config.id, exclude, profileName: info.profile?.name ?? null, attachResult: attach });
    setExclude(NO_EXCLUSIONS); // chips are per-turn
    setAttachResult(false); // and so is a result attachment: it has to be a fresh choice every time
  };

  // Streaming should never steal the viewport from someone reading history.
  const lastKind = transcript[transcript.length - 1]?.kind;
  const showWorking = running && !pending && (lastKind === "user" || lastKind === "tool" || lastKind === "approval");

  return (
    <section ref={root} aria-label="AI agent" className="flex h-full min-h-0 flex-col bg-panel">
      {/* Header */}
      <header className="@container flex shrink-0 flex-wrap items-center gap-x-2 gap-y-1.5 border-b border-border px-3 py-1.5">
        <div className="flex items-center gap-1.5 pr-1">
          <Sparkles className="size-3.5 text-accent" />
          <h2 className="text-[13px] font-semibold">AI Agent</h2>
        </div>

        {usable.length > 0 ? (
          <Select
            aria-label="AI provider"
            value={selected?.config.id ?? ""}
            onChange={(e) => setDefaultProviderId(e.target.value)}
            disabled={running}
            className="!h-7 !w-auto max-w-56 text-xs"
          >
            {usable.map((p) => (
              <option key={p.config.id} value={p.config.id}>
                {p.config.label}
                {p.config.model ? ` · ${p.config.model}` : ""}
              </option>
            ))}
          </Select>
        ) : (
          !providersQuery.isPending && (
            <Button size="sm" onClick={() => setSettingsOpen(true)}>
              <Settings2 className="size-3.5" /> Configure a provider
            </Button>
          )
        )}

        <div className="order-last w-full min-w-0 @[760px]:order-none @[760px]:w-auto @[760px]:flex-1">
          <ContextChips info={info} exclude={exclude} onChange={setExclude} attachResult={attachResult} onToggleResult={() => setAttachResult((v) => !v)} />
        </div>

        <div className="ml-auto flex items-center gap-1.5">
          {access && info.profile && (
            <Tip
              content={
                <>
                  <span className="mb-0.5 block font-medium">AI access for {info.profile.name}</span>
                  {access.leaves}
                </>
              }
            >
              <span
                tabIndex={0}
                aria-label={`AI access: ${access.badge}. ${access.leaves}`}
                className={cn("inline-flex h-6 items-center rounded-md border px-2 text-[11.5px] font-medium outline-none focus-visible:ring-2 focus-visible:ring-accent/60", ACCESS_TONE[access.tone])}
              >
                {access.badge}
              </span>
            </Tip>
          )}
          {running && (
            <Button size="sm" onClick={() => void cancel()} className="!text-danger">
              <Square className="size-3 fill-current" /> Cancel
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={reset} title="Clear the conversation and start a new session">
            <SquarePen className="size-3.5" /> New session
          </Button>
        </div>
      </header>

      {/* Transcript */}
      <div className="relative min-h-0 flex-1">
        <div ref={stick.scroller} onScroll={stick.onScroll} className="h-full overflow-y-auto" role="log" aria-live="polite" aria-label="Conversation">
          <div ref={stick.content} className={cn("@container mx-auto flex flex-col gap-3 px-4 py-3", primary ? "max-w-3xl" : "max-w-4xl")}>
            {transcript.length === 0 ? (
              <AgentEmpty
                profile={info.profile}
                database={info.database}
                hasEditorText={!!info.tab && info.tab.kind === "sql" && info.tab.contents.trim().length > 0}
                canSend={!sendDisabledReason}
                primary={primary}
                onPick={(sug) => {
                  setDraft(sug.prompt);
                  setFocusToken((n) => n + 1);
                }}
              />
            ) : (
              transcript.map((item) => <Row key={item.id} item={item} />)
            )}
            {showWorking && (
              <div className="flex items-center gap-2 text-[12px] text-muted" role="status">
                <Spinner className="size-3.5" /> Working…
              </div>
            )}
          </div>
        </div>
        {!stick.atBottom && transcript.length > 0 && (
          <button
            type="button"
            onClick={stick.jumpToBottom}
            className="absolute bottom-2 right-4 inline-flex h-7 items-center gap-1 rounded-full border border-border bg-panel-2 px-2.5 text-[11.5px] text-fg shadow-lg outline-none hover:border-muted/60 focus-visible:ring-2 focus-visible:ring-accent/60"
          >
            <ArrowDown className="size-3" /> {running ? "Latest" : "Jump to end"}
          </button>
        )}
      </div>

      {pending && pending.kind === "approval" && (
        <ApprovalCard
          key={pending.callId}
          item={pending}
          scope={root}
          prodContext={info.profile?.environment === "prod"}
          onResolve={(d) => resolveApproval(pending.callId, d)}
        />
      )}

      <Composer
        value={draft}
        onChange={setDraft}
        onSend={submit}
        onCancel={() => void cancel()}
        running={running}
        sendDisabledReason={sendDisabledReason}
        history={history}
        focusToken={focusToken}
        primary={primary}
      />
    </section>
  );
}

function Row({ item }: { item: TranscriptItem }) {
  switch (item.kind) {
    case "user":
      return (
        <div className="flex justify-end">
          <div className="max-w-[85%] rounded-lg rounded-br-sm border border-border bg-panel-2 px-3 py-2">
            <p className="selectable whitespace-pre-wrap break-words text-[13px] leading-[1.5]">{item.text}</p>
            {item.attached.length > 0 && <p className="mt-1 truncate text-[11px] text-muted">with {item.attached.join(", ")}</p>}
          </div>
        </div>
      );
    case "assistant":
      return <Markdown text={item.text} />;
    case "tool":
      return <ToolCard item={item} />;
    case "approval":
      return item.status === "pending" ? null : <ApprovalNote item={item} />;
    case "error":
      return <ErrorBox error={item.error} />;
    case "usage":
      return (
        <div className="flex justify-end gap-2 text-[11px] tabular-nums text-muted">
          {item.usage && (
            <span>
              {formatTokens(item.usage.inputTokens)} in · {formatTokens(item.usage.outputTokens)} out tokens
            </span>
          )}
          {item.stopReason && !["end_turn", "stop", "endTurn", "end"].includes(item.stopReason) && <span>stopped: {item.stopReason}</span>}
        </div>
      );
  }
}
