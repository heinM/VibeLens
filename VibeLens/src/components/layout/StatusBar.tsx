import { AlertTriangle, ShieldCheck } from "lucide-react";
import { useUIStore } from "../../store/useUIStore";
import { useAppInfo } from "../../hooks/useAppInfo";
import { useConnectionStatus } from "../../hooks/useConnectionStatus";
import { useProfile } from "../../hooks/useProfiles";
import { EnvBadge } from "../ui";
import { Chip, StatusDot, connectionLabel } from "../ui/wsKit";

export function StatusBar() {
  const activeProfileId = useUIStore((s) => s.activeProfileId);
  const activeDatabase = useUIStore((s) => s.activeDatabase);
  const profile = useProfile(activeProfileId);
  const status = useConnectionStatus(activeProfileId);
  const info = useAppInfo();
  const state = status?.state ?? "disconnected";
  const memoryVault = info.data?.vaultBackend === "memory";

  return (
    <footer className="flex h-7 shrink-0 items-center gap-3 border-t border-border bg-panel px-3 text-[11px] text-muted">
      {profile ? (
        <div className="flex min-w-0 items-center gap-2">
          <StatusDot state={state} />
          <span className="truncate font-medium text-fg">{profile.name}</span>
          <EnvBadge env={profile.environment} />
          <span>{connectionLabel[state]}</span>
          {activeDatabase && <span className="font-mono">{activeDatabase}</span>}
          {state === "connected" && status?.serverVersion && <span className="truncate font-mono" title={status.serverVersion}>{status.serverVersion}</span>}
          {status?.tunneled && state === "connected" && <Chip>SSH tunnel</Chip>}
        </div>
      ) : (
        <span>No connection selected</span>
      )}
      <div className="ml-auto flex items-center gap-3">
        {memoryVault ? (
          <Chip tone="warn" icon={<AlertTriangle className="size-3" />} title="No OS keychain is available on this system. Passwords and API keys live in memory and are forgotten when you quit.">
            Secrets not persisted
          </Chip>
        ) : (
          info.data && (
            <span className="inline-flex items-center gap-1" title="Secrets are stored in the OS keychain">
              <ShieldCheck className="size-3 text-ok" /> Keychain
            </span>
          )
        )}
        {info.data && <span className="font-mono">v{info.data.version}</span>}
      </div>
    </footer>
  );
}
