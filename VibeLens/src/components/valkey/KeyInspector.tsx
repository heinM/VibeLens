// Docked inspector for one Valkey key: header (name, type, TTL, size) and a body per data type.
import { useMemo, useState } from "react";
import { AlertTriangle, Check, Clock, Copy, RefreshCw, Terminal, X } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import type { KeyType, ValkeyValue } from "../../ipc/types";
import { asError } from "../../ipc/commands";
import { useValkeyValue } from "../../hooks/useValkey";
import { useConnectionStatus, useConnect } from "../../hooks/useConnectionStatus";
import { useProfile } from "../../hooks/useProfiles";
import { Button, EmptyState, ErrorBox, IconButton } from "../ui";
import { Chip, Segmented, SkeletonRows } from "../ui/wsKit";
import { copyToClipboard } from "../ui/clipboard";
import { cn } from "../../lib/cn";
import { JsonTree } from "../json/JsonTree";
import { BinaryView } from "../json/BinaryView";
import { useInspectorStore, type InspectorTarget } from "./inspectorStore";
import { VTable } from "./VTable";
import { elementText, hashEntries, humanBytes, humanTtl, isBytesValue, recreateCommands, streamEntries, zsetEntries } from "./keyCommands";

const typeTone: Record<KeyType, string> = {
  string: "border-sky-500/40 bg-sky-500/10 text-sky-600 dark:text-sky-300",
  hash: "border-violet-500/40 bg-violet-500/10 text-violet-600 dark:text-violet-300",
  list: "border-emerald-500/40 bg-emerald-500/10 text-emerald-600 dark:text-emerald-300",
  set: "border-amber-500/40 bg-amber-500/10 text-amber-600 dark:text-amber-300",
  zset: "border-rose-500/40 bg-rose-500/10 text-rose-600 dark:text-rose-300",
  stream: "border-cyan-500/40 bg-cyan-500/10 text-cyan-600 dark:text-cyan-300",
  json: "border-fuchsia-500/40 bg-fuchsia-500/10 text-fuchsia-600 dark:text-fuchsia-300",
  unknown: "border-border bg-panel-2 text-muted",
};

export function TypeBadge({ type, className }: { type: KeyType | null; className?: string }) {
  const t = type ?? "unknown";
  return <span className={cn("inline-flex h-5 items-center rounded border px-1.5 font-mono text-[10px] font-semibold leading-none", typeTone[t], className)}>{t}</span>;
}

const LOAD_MORE = 10_000;

function StringBody({ value }: { value: unknown }) {
  const [mode, setMode] = useState<"tree" | "raw">("tree");
  const parsed = useMemo(() => {
    if (typeof value !== "string") return null;
    const t = value.trimStart();
    if (t[0] !== "{" && t[0] !== "[") return null;
    try {
      const v: unknown = JSON.parse(value);
      return typeof v === "object" && v !== null ? v : null;
    } catch {
      return null;
    }
  }, [value]);

  if (isBytesValue(value)) return <div className="h-full p-3"><BinaryView base64={value.$bytes} len={value.len} /></div>;
  const text = typeof value === "string" ? value : elementText(value);
  return (
    <div className="flex h-full min-h-0 flex-col">
      {parsed !== null && (
        <div className="flex h-9 shrink-0 items-center gap-2 border-b border-border px-3">
          <Chip tone="accent">JSON</Chip>
          <Segmented
            label="String format"
            value={mode}
            onChange={setMode}
            options={[
              { value: "tree", label: "Tree" },
              { value: "raw", label: "Raw" },
            ]}
          />
        </div>
      )}
      <div className="min-h-0 flex-1">
        {parsed !== null && mode === "tree" ? (
          <JsonTree value={parsed} defaultExpandDepth={2} />
        ) : (
          <pre className="selectable h-full overflow-auto whitespace-pre-wrap break-words p-3 font-mono text-[12px] leading-5">{text}</pre>
        )}
      </div>
    </div>
  );
}

function Body({ v }: { v: ValkeyValue }) {
  switch (v.keyType) {
    case "string":
      return <StringBody value={v.value} />;
    case "hash": {
      const rows = hashEntries(v.value);
      return (
        <VTable
          rows={rows}
          columns={[
            { label: "Field", width: "minmax(120px,0.4fr)", cell: ([f]) => <span className="text-sky-700 dark:text-sky-300">{f}</span> },
            { label: "Value", width: "minmax(0,1fr)", cell: ([, x]) => <span title={elementText(x)}>{elementText(x)}</span> },
          ]}
        />
      );
    }
    case "list":
    case "set": {
      const rows = Array.isArray(v.value) ? v.value : [];
      return (
        <VTable
          rows={rows}
          columns={[
            ...(v.keyType === "list" ? [{ label: "#", width: "64px", cell: (_: unknown, i: number) => <span className="text-muted tabular-nums">{i}</span> }] : []),
            { label: v.keyType === "list" ? "Element" : "Member", width: "minmax(0,1fr)", cell: (x: unknown) => <span title={elementText(x)}>{elementText(x)}</span> },
          ]}
        />
      );
    }
    case "zset": {
      const rows = zsetEntries(v.value);
      return (
        <VTable
          rows={rows}
          columns={[
            { label: "Member", width: "minmax(0,1fr)", cell: (e) => <span title={elementText(e.member)}>{elementText(e.member)}</span> },
            { label: "Score", width: "140px", className: "text-right tabular-nums", cell: (e) => <span className="text-amber-700 dark:text-amber-300">{elementText(e.score)}</span> },
          ]}
        />
      );
    }
    case "stream": {
      const rows = streamEntries(v.value);
      return (
        <VTable
          rows={rows}
          columns={[
            { label: "ID", width: "170px", cell: (e) => <span className="text-muted">{e.id}</span> },
            {
              label: "Fields",
              width: "minmax(0,1fr)",
              cell: (e) => {
                const text = e.fields.map(([k, x]) => `${k}=${elementText(x)}`).join("  ");
                return <span title={text}>{text}</span>;
              },
            },
          ]}
        />
      );
    }
    default:
      return <JsonTree value={v.value} defaultExpandDepth={2} />;
  }
}

function itemCount(v: ValkeyValue): number | null {
  if (Array.isArray(v.value)) return v.value.length;
  if (v.keyType === "hash") return hashEntries(v.value).length;
  return null;
}

export function KeyInspector({ target }: { target: InspectorTarget }) {
  const qc = useQueryClient();
  const close = useInspectorStore((s) => s.close);
  const profile = useProfile(target.profileId);
  const status = useConnectionStatus(target.profileId);
  const connect = useConnect();
  const [maxItems, setMaxItems] = useState<number | undefined>(undefined);
  const [copied, setCopied] = useState(false);
  const q = useValkeyValue(target.profileId, target.db, target.key, maxItems);
  const v = q.data;
  const err = q.error ? asError(q.error) : null;

  const copyCommand = async () => {
    if (!v) return;
    const text = recreateCommands(v);
    if (text && (await copyToClipboard(text))) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1400);
    }
  };

  const shown = v ? itemCount(v) : null;
  const canCopy = v ? recreateCommands(v) !== "" : false;

  return (
    <section aria-label={`Key ${target.key}`} className="flex h-full min-h-0 flex-col border-l border-border bg-bg">
      <header className="shrink-0 border-b border-border bg-panel px-3 py-2">
        <div className="flex items-start gap-2">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              {v && <TypeBadge type={v.keyType} />}
              <h3 className="selectable min-w-0 truncate font-mono text-[13px] font-semibold text-fg" title={target.key}>
                {target.key}
              </h3>
            </div>
            <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted">
              <span>
                {profile?.name ?? "Connection"} <span className="text-muted/60">/</span> db{target.db}
              </span>
              {v && (
                <>
                  <span className="inline-flex items-center gap-1">
                    <Clock className="size-3" /> {humanTtl(v.ttlMs)}
                  </span>
                  {v.size !== null && <span className="tabular-nums">{v.keyType === "string" ? humanBytes(v.size) : `${v.size.toLocaleString("en-US")} ${v.keyType === "hash" ? "fields" : v.keyType === "stream" ? "entries" : "items"}`}</span>}
                </>
              )}
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-0.5">
            <IconButton label="Refresh key" onClick={() => void qc.invalidateQueries({ queryKey: ["vk", target.profileId, target.db, "value", target.key] })}>
              <RefreshCw className={cn("size-3.5", q.isFetching && "animate-spin")} />
            </IconButton>
            <IconButton label="Close inspector" onClick={close}>
              <X className="size-4" />
            </IconButton>
          </div>
        </div>
        {v && (
          <div className="mt-2 flex items-center gap-2">
            <Button size="sm" variant="secondary" disabled={!canCopy} onClick={() => void copyCommand()} title={canCopy ? "Copy commands that recreate this key" : "This type cannot be expressed as a command"}>
              {copied ? <Check className="size-3.5 text-ok" /> : <Terminal className="size-3.5" />} Copy as command
            </Button>
            <Button size="sm" variant="ghost" onClick={() => void copyToClipboard(target.key)}>
              <Copy className="size-3.5" /> Copy key
            </Button>
          </div>
        )}
      </header>

      {v?.truncated && (
        <div className="flex shrink-0 items-center gap-2 border-b border-warn/30 bg-warn/10 px-3 py-1.5 text-xs text-warn">
          <AlertTriangle className="size-3.5 shrink-0" />
          <span className="min-w-0 flex-1">
            Showing {shown !== null ? `${shown.toLocaleString("en-US")} of ` : "part of "}
            {v.size !== null ? v.size.toLocaleString("en-US") : "many"} items.
          </span>
          {(maxItems ?? 0) < LOAD_MORE && (
            <Button size="sm" variant="secondary" onClick={() => setMaxItems(LOAD_MORE)}>
              Load up to {LOAD_MORE.toLocaleString("en-US")}
            </Button>
          )}
        </div>
      )}

      <div className="min-h-0 flex-1">
        {status?.state !== "connected" ? (
          <EmptyState title="Not connected">
            <div className="flex flex-col items-center gap-3">
              <span>Connect to {profile?.name ?? "this connection"} to read the key.</span>
              <Button variant="primary" size="sm" loading={connect.isPending} onClick={() => connect.mutate(target.profileId)}>
                Connect
              </Button>
            </div>
          </EmptyState>
        ) : q.isLoading ? (
          <SkeletonRows rows={8} />
        ) : err ? (
          <div className="p-3">
            <ErrorBox error={err} />
          </div>
        ) : v ? (
          v.ttlMs === -2 ? (
            <EmptyState title="This key no longer exists">It expired or was deleted after it was listed. Refresh the key list to update the tree.</EmptyState>
          ) : (
            <Body v={v} />
          )
        ) : null}
      </div>
    </section>
  );
}
