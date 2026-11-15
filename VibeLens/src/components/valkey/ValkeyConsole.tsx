// Valkey console tab: quoting-aware command line with history, per-db execution, JSON-tree results
// and the "confirm, then resend with allowWrite" flow for mutating commands.
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from "react";
import { CornerDownLeft, Eraser, Plug } from "lucide-react";
import { api, asError } from "../../ipc/commands";
import type { ErrorPayload } from "../../ipc/types";
import { useWorkspaceStore, type WorkspaceTab } from "../../store/useWorkspaceStore";
import { useUIStore } from "../../store/useUIStore";
import { useConnect, useConnectionStatus } from "../../hooks/useConnectionStatus";
import { useProfile, useProfiles } from "../../hooks/useProfiles";
import { useValkeyDbs } from "../../hooks/useValkey";
import { Button, ConfirmDialog, EnvBadge, ErrorBox } from "../ui";
import { Kbd, MiniSelect, StatusDot } from "../ui/wsKit";
import { envBg } from "../ui/env";
import { cn } from "../../lib/cn";
import { JsonTree } from "../json/JsonTree";
import { formatElapsed } from "../grid/cellFormat";
import { formatArgv, parseArgv } from "./argv";

interface Entry {
  id: number;
  profileId: string;
  line: string;
  argv: string[];
  db: number;
  status: "pending" | "ok" | "error" | "confirm" | "cancelled";
  result?: unknown;
  elapsedMs?: number;
  error?: ErrorPayload | { code?: string; message: string };
  reason?: string;
}

const MAX_ENTRIES = 400;

function ScalarResult({ value }: { value: unknown }) {
  if (value === null || value === undefined) return <span className="italic text-muted">(nil)</span>;
  if (typeof value === "number") return <span className="text-amber-700 dark:text-amber-300">(integer) {value}</span>;
  if (typeof value === "boolean") return <span className="text-violet-700 dark:text-violet-300">{String(value)}</span>;
  if (typeof value === "string") {
    return value.includes("\n") ? (
      <pre className="selectable whitespace-pre-wrap break-words text-emerald-700 dark:text-emerald-300">{value}</pre>
    ) : (
      <span className="selectable break-all text-emerald-700 dark:text-emerald-300">&quot;{value}&quot;</span>
    );
  }
  return null;
}

function ResultBlock({ value }: { value: unknown }) {
  if (value !== null && typeof value === "object") {
    if (Array.isArray(value) && value.length === 0) return <span className="italic text-muted">(empty array)</span>;
    return <JsonTree value={value} defaultExpandDepth={1} maxHeight={300} className="rounded-md border border-border bg-panel" />;
  }
  return <ScalarResult value={value} />;
}

export function ValkeyConsole({ tab, active }: { tab: WorkspaceTab; active: boolean }) {
  const updateTab = useWorkspaceStore((s) => s.updateTab);
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);
  const openWizard = useUIStore((s) => s.openWizard);
  const profile = useProfile(tab.profileId);
  const { data: profiles = [] } = useProfiles();
  const vk = profiles.filter((p) => p.engine === "valkey");
  const status = useConnectionStatus(tab.profileId);
  const connect = useConnect();
  const dbs = useValkeyDbs(tab.profileId);

  const [entries, setEntries] = useState<Entry[]>([]);
  const [input, setInput] = useState("");
  const [parseError, setParseError] = useState<string | null>(null);
  const [confirmFor, setConfirmFor] = useState<number | null>(null);
  const history = useRef<string[]>([]);
  const histIdx = useRef(-1);
  const draft = useRef("");
  const nextId = useRef(1);
  const inputRef = useRef<HTMLInputElement>(null);
  const logRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  const db = tab.valkeyDb;
  const state = status?.state ?? "disconnected";
  const isProd = profile?.environment === "prod";

  useEffect(() => {
    if (active) inputRef.current?.focus();
  }, [active]);

  // Stay pinned to the newest output unless the user scrolled up.
  useLayoutEffect(() => {
    const el = logRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [entries]);

  const patch = useCallback((id: number, p: Partial<Entry>) => setEntries((es) => es.map((e) => (e.id === id ? { ...e, ...p } : e))), []);

  const send = useCallback(
    async (entry: Entry, allowWrite: boolean) => {
      patch(entry.id, { status: "pending", error: undefined });
      try {
        // A write runs where it was typed, even if the selector has moved since.
        if (entry.profileId !== profile?.id || state !== "connected") await connect.mutateAsync(entry.profileId);
        const out = await api.vkCommand(entry.profileId, entry.db, entry.argv, allowWrite, allowWrite);
        if (out.status === "requiresConfirmation") {
          patch(entry.id, { status: "confirm", reason: out.reason });
          setConfirmFor(entry.id);
        } else {
          patch(entry.id, { status: "ok", result: out.result, elapsedMs: out.elapsedMs });
        }
      } catch (e) {
        patch(entry.id, { status: "error", error: asError(e) });
      }
    },
    [profile, state, connect, patch],
  );

  const submit = useCallback(() => {
    const line = input.trim();
    if (!line) return;
    if (line.toLowerCase() === "clear" || line.toLowerCase() === "cls") {
      setEntries([]);
      setInput("");
      return;
    }
    const parsed = parseArgv(line);
    if (!parsed.ok) {
      setParseError(parsed.error);
      return;
    }
    if (parsed.argv.length === 0) return;
    setParseError(null);
    if (history.current[history.current.length - 1] !== line) history.current.push(line);
    histIdx.current = -1;
    setInput("");
    stick.current = true;
    const entry: Entry = { id: nextId.current++, profileId: profile?.id ?? "", line, argv: parsed.argv, db, status: "pending" };
    setEntries((es) => [...es.slice(-(MAX_ENTRIES - 1)), entry]);
    if (!profile) {
      patch(entry.id, { status: "error", error: { code: "no_connection", message: "Choose a Valkey connection for this console first." } });
      return;
    }
    void send(entry, false);
  }, [input, db, profile, send, patch]);

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      submit();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      const h = history.current;
      if (h.length === 0) return;
      if (histIdx.current === -1) {
        draft.current = input;
        histIdx.current = h.length - 1;
      } else histIdx.current = Math.max(0, histIdx.current - 1);
      setInput(h[histIdx.current]);
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      const h = history.current;
      if (histIdx.current === -1) return;
      if (histIdx.current >= h.length - 1) {
        histIdx.current = -1;
        setInput(draft.current);
      } else {
        histIdx.current++;
        setInput(h[histIdx.current]);
      }
    } else if (e.key === "l" && e.ctrlKey) {
      e.preventDefault();
      setEntries([]);
    } else if (e.key === "Escape") {
      setInput("");
      setParseError(null);
      histIdx.current = -1;
    }
  };

  const confirming = entries.find((e) => e.id === confirmFor);
  const confirmProfile = vk.find((p) => p.id === confirming?.profileId);
  const dbCount = Math.max(dbs.data?.length ?? 16, db + 1);

  return (
    <div className="relative flex h-full min-h-0 flex-col bg-bg">
      {isProd && <div className="h-0.5 shrink-0 bg-env-prod" aria-hidden />}
      <div className="flex h-10 shrink-0 items-center gap-2 overflow-hidden border-b border-border bg-panel px-2">
        <MiniSelect
          label="Valkey connection for this console"
          value={tab.profileId ?? ""}
          className="w-44 shrink-0"
          leading={profile ? <span className={cn("size-2 rounded-full", envBg[profile.environment])} /> : undefined}
          onChange={(id) => {
            if (id === "__new") return openWizard();
            updateTab(tab.id, { profileId: id || null });
            if (id) setActiveTarget(id, null);
          }}
        >
          <option value="">Choose connection…</option>
          {vk.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
          <option value="__new">New connection…</option>
        </MiniSelect>
        {profile && (
          <>
            <MiniSelect label="Logical database" value={String(db)} className="w-28 shrink-0" disabled={profile.valkey.cluster} onChange={(v) => updateTab(tab.id, { valkeyDb: Number(v) })}>
              {Array.from({ length: profile.valkey.cluster ? 1 : dbCount }, (_, i) => {
                const info = dbs.data?.find((d) => d.index === i);
                return (
                  <option key={i} value={i}>
                    db{i}
                    {info?.keys != null ? ` (${info.keys.toLocaleString("en-US")})` : ""}
                  </option>
                );
              })}
            </MiniSelect>
            <EnvBadge env={profile.environment} />
            {state === "connected" ? (
              <StatusDot state="connected" />
            ) : (
              <Button size="sm" variant="secondary" loading={state === "connecting" || connect.isPending} onClick={() => connect.mutate(profile.id)}>
                <Plug className="size-3.5" /> Connect
              </Button>
            )}
          </>
        )}
        <Button size="sm" variant="ghost" className="ml-auto" disabled={entries.length === 0} onClick={() => setEntries([])} title="Clear output (Ctrl+L)">
          <Eraser className="size-3.5" /> Clear
        </Button>
      </div>

      <div
        ref={logRef}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
        onMouseUp={() => {
          // Clicking the log should not steal an in-progress text selection, but empty clicks focus the prompt.
          if (!window.getSelection()?.toString()) inputRef.current?.focus();
        }}
        className="selectable min-h-0 flex-1 overflow-auto px-3 py-2 font-mono text-[12px] leading-5"
      >
        {entries.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-center font-sans text-muted">
            <div className="text-sm font-medium text-fg">{profile ? `${profile.name}, db${db}` : "Choose a connection to start"}</div>
            <div className="max-w-sm text-xs leading-relaxed">
              Type a command such as <code className="rounded bg-panel-2 px-1 font-mono text-fg">SCAN 0 MATCH user:* COUNT 20</code>. Quote values with spaces. Commands that change data ask for confirmation first.
            </div>
          </div>
        ) : (
          entries.map((e) => (
            <div key={e.id} className="mb-3">
              <div className="flex items-baseline gap-2">
                <span className="select-none text-accent">db{e.db}&gt;</span>
                <span className="min-w-0 flex-1 break-all text-fg">{formatArgv(e.argv)}</span>
                {e.elapsedMs !== undefined && <span className="shrink-0 font-sans text-[11px] tabular-nums text-muted">{formatElapsed(e.elapsedMs)}</span>}
              </div>
              <div className="mt-1 pl-1">
                {e.status === "pending" && <span className="font-sans text-muted">Running…</span>}
                {e.status === "ok" && <ResultBlock value={e.result} />}
                {e.status === "error" && <ErrorBox error={e.error} />}
                {e.status === "cancelled" && <span className="font-sans italic text-muted">Cancelled. Nothing was sent.</span>}
                {e.status === "confirm" && (
                  <span className="inline-flex items-center gap-2 font-sans text-warn">
                    Waiting for confirmation: {e.reason}
                    <Button size="sm" variant="secondary" onClick={() => setConfirmFor(e.id)}>
                      Review
                    </Button>
                  </span>
                )}
              </div>
            </div>
          ))
        )}
      </div>

      <div className="shrink-0 border-t border-border bg-panel px-3 py-2">
        {parseError && <p role="alert" className="mb-1.5 text-xs text-danger">{parseError}</p>}
        <div className="flex items-center gap-2 rounded-md border border-border bg-bg px-2.5 focus-within:border-accent/70 focus-within:ring-1 focus-within:ring-accent/40">
          <span className="select-none font-mono text-[12px] text-accent">db{db}&gt;</span>
          <input
            ref={inputRef}
            value={input}
            onChange={(e) => {
              setInput(e.target.value);
              if (parseError) setParseError(null);
            }}
            onKeyDown={onKeyDown}
            aria-label="Valkey command"
            placeholder={profile ? "GET key" : "Choose a connection first"}
            spellCheck={false}
            autoComplete="off"
            autoCapitalize="off"
            className="h-8 min-w-0 flex-1 bg-transparent font-mono text-[12px] text-fg outline-none placeholder:text-muted/60"
          />
          <span className="hidden items-center gap-1 text-[11px] text-muted sm:inline-flex">
            <Kbd>↑</Kbd>
            <Kbd>↓</Kbd> history
          </span>
          <Button size="sm" variant="primary" onClick={submit} disabled={!input.trim()} aria-label="Send command">
            <CornerDownLeft className="size-3.5" />
            Send
          </Button>
        </div>
      </div>

      <ConfirmDialog
        open={confirming !== undefined && confirming.status === "confirm"}
        title={confirmProfile?.environment === "prod" ? "Confirm write on a production Valkey" : "Confirm write command"}
        danger
        confirmText="Run command"
        requireTyping={confirmProfile?.environment === "prod" ? confirmProfile.name : undefined}
        onCancel={() => {
          if (confirming) patch(confirming.id, { status: "cancelled" });
          setConfirmFor(null);
        }}
        onConfirm={() => {
          const e = confirming;
          setConfirmFor(null);
          if (e) void send(e, true);
        }}
        body={
          <>
            <p>Connection: {confirmProfile?.name ?? "unknown"}</p>
            <p>{confirming?.reason ?? "This command changes data."}</p>
            <pre className="selectable overflow-auto rounded-md border border-border bg-bg p-2 font-mono text-[12px]">{confirming ? formatArgv(confirming.argv) : ""}</pre>
          </>
        }
      />
    </div>
  );
}
