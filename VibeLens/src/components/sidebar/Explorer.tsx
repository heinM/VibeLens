// The navigator body for the focused connection: Postgres object tree, Iceberg catalog, Redshift
// schemas, MySQL databases or Valkey key tree.
import { useEffect, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { Eye, EyeOff, Plug, RefreshCw, Search, X } from "lucide-react";
import { asError } from "../../ipc/commands";
import { useUIStore } from "../../store/useUIStore";
import { useConnect, useConnectionStatus } from "../../hooks/useConnectionStatus";
import { useProfile } from "../../hooks/useProfiles";
import { Button, EmptyState, ErrorBox, IconButton, Spinner } from "../ui";
import { cn } from "../../lib/cn";
import { isSsoExpired } from "../../lib/ssoReauth";
import { SsoReauth } from "../connections/SsoReauth";
import { IcebergTree } from "./IcebergTree";
import { PgTree } from "./PgTree";
import { MysqlTree } from "./MysqlTree";
import { RedshiftTree } from "./RedshiftTree";
import { SectionHeader } from "./TreeRow";
import { ValkeyTree, normalisePattern } from "./ValkeyTree";

export function Explorer() {
  const qc = useQueryClient();
  const activeProfileId = useUIStore((s) => s.activeProfileId);
  const profile = useProfile(activeProfileId);
  const status = useConnectionStatus(activeProfileId);
  const connect = useConnect();
  const [showSystem, setShowSystem] = useState(false);
  const [filterText, setFilterText] = useState("");
  const [pattern, setPattern] = useState("*");

  // Debounce the key filter so typing doesn't fire a SCAN per keystroke.
  useEffect(() => {
    const t = window.setTimeout(() => setPattern(normalisePattern(filterText)), 350);
    return () => window.clearTimeout(t);
  }, [filterText]);
  useEffect(() => {
    setFilterText("");
    setPattern("*");
  }, [activeProfileId]);

  const state = status?.state ?? "disconnected";
  const isPg = profile?.engine === "postgres";
  const isIce = profile?.engine === "iceberg";
  const isRs = profile?.engine === "redshift";
  const isMy = profile?.engine === "mysql";
  const isVk = profile?.engine === "valkey";

  const refresh = () => {
    if (!profile) return;
    if (profile.engine === "postgres") void qc.invalidateQueries({ queryKey: ["pg", profile.id] });
    else if (profile.engine === "iceberg") void qc.invalidateQueries({ queryKey: ["ice", profile.id] });
    else if (profile.engine === "redshift") void qc.invalidateQueries({ queryKey: ["rs", profile.id] });
    else if (profile.engine === "mysql") void qc.invalidateQueries({ queryKey: ["my", profile.id] });
    else {
      void qc.resetQueries({ predicate: (q) => q.queryKey[0] === "vk" && q.queryKey[1] === profile.id && q.queryKey[3] === "scan" });
      void qc.invalidateQueries({ queryKey: ["vk", profile.id, "dbs"] });
    }
  };

  // An expired Identity Center session, from the last connect attempt or the status poll.
  const connectFailure = connect.error ? asError(connect.error).message : null;
  const ssoError = [connectFailure, state === "error" ? status?.detail : null].find((m) => isSsoExpired(m)) ?? null;

  let body;
  if (!profile) {
    body = (
      <EmptyState title="Nothing selected">Pick a connection above to browse its databases or keys.</EmptyState>
    );
  } else if (state === "connecting") {
    body = (
      <div className="flex h-full items-center justify-center gap-2 text-xs text-muted">
        <Spinner /> Connecting to {profile.name}…
      </div>
    );
  } else if (state !== "connected") {
    body = (
      <div className="flex h-full flex-col items-center justify-center gap-3 p-4 text-center">
        <div className="text-sm font-medium text-fg">{profile.name} is not connected</div>
        {state === "error" && status?.detail && (
          <div className="w-full text-left">
            <ErrorBox error={{ message: status.detail }} />
          </div>
        )}
        {connect.error && <ErrorBox error={asError(connect.error)} />}
        <SsoReauth profile={profile} error={ssoError} center onSignedIn={() => connect.mutate(profile.id)} />
        <Button variant={ssoError ? "secondary" : "primary"} loading={connect.isPending} onClick={() => connect.mutate(profile.id)}>
          <Plug className="size-4" /> Connect
        </Button>
      </div>
    );
  } else if (isPg) {
    body = <PgTree key={profile.id} profile={profile} showSystem={showSystem} />;
  } else if (isIce) {
    body = <IcebergTree key={profile.id} profile={profile} />;
  } else if (isMy) {
    body = <MysqlTree key={profile.id} profile={profile} showSystem={showSystem} />;
  } else if (isRs) {
    body = <RedshiftTree key={profile.id} profile={profile} showSystem={showSystem} />;
  } else {
    body = <ValkeyTree key={profile.id} profile={profile} pattern={pattern} />;
  }

  const connected = state === "connected" && !!profile;

  return (
    <div className="flex h-full min-h-0 flex-col bg-panel">
      <SectionHeader
        title={profile ? profile.name : "Explorer"}
        actions={
          connected && (
            <>
              {(isPg || isRs || isMy) && (
                <IconButton label={showSystem ? "Hide system objects" : "Show system objects"} onClick={() => setShowSystem((v) => !v)} className={cn(showSystem && "text-accent")}>
                  {showSystem ? <Eye className="size-3.5" /> : <EyeOff className="size-3.5" />}
                </IconButton>
              )}
              <IconButton label="Refresh" onClick={refresh}>
                <RefreshCw className="size-3.5" />
              </IconButton>
            </>
          )
        }
      />
      {connected && isVk && (
        <div className="shrink-0 border-b border-border px-2 py-1.5">
          <div className="flex h-7 items-center gap-1.5 rounded-md border border-border bg-bg px-2 focus-within:border-accent/70 focus-within:ring-1 focus-within:ring-accent/40">
            <Search className="size-3.5 shrink-0 text-muted" />
            <input
              value={filterText}
              onChange={(e) => setFilterText(e.target.value)}
              aria-label="Filter keys by pattern"
              placeholder="Filter keys (user:* or text)"
              spellCheck={false}
              className="min-w-0 flex-1 bg-transparent font-mono text-xs text-fg outline-none placeholder:font-sans placeholder:text-muted/70"
            />
            {filterText && (
              <button type="button" aria-label="Clear filter" onClick={() => setFilterText("")} className="text-muted hover:text-fg">
                <X className="size-3.5" />
              </button>
            )}
          </div>
        </div>
      )}
      <div className="min-h-0 flex-1">{body}</div>
    </div>
  );
}
