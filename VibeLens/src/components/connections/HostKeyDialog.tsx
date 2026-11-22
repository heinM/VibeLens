import { Fingerprint } from "lucide-react";
import { useQueryClient } from "@tanstack/react-query";
import type { ConnectionProfile, PresentedHostKey } from "../../ipc/types";
import { Button, Dialog } from "../ui";
import { useHostKeyPrompt } from "./hostKeyPrompt";

/** The fingerprint an SSH server presented, for the user to compare before anything is sent to it. */
export function HostKeyTrustDialog({
  hostKey,
  connectionName,
  trustLabel,
  onTrust,
  onCancel,
}: {
  hostKey: PresentedHostKey | null;
  connectionName?: string | null;
  trustLabel: string;
  onTrust: () => void;
  onCancel: () => void;
}) {
  return (
    <Dialog
      open={hostKey !== null}
      onClose={onCancel}
      title="Check this SSH host key"
      footer={
        <>
          <Button variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button variant="primary" onClick={onTrust}>
            {trustLabel}
          </Button>
        </>
      }
    >
      {hostKey && (
        <div className="space-y-3 text-[13px] leading-snug">
          <p>
            VibeLens has no pinned key for <span className="selectable font-mono">{hostKey.host}:{hostKey.port}</span>
            {connectionName ? <> (connection &ldquo;{connectionName}&rdquo;)</> : null}. It stopped before sending a user name, key or password. The server presented:
          </p>
          <div data-testid="host-key-fingerprint" className="flex items-center gap-2 rounded-md border border-border bg-panel-2/50 px-2.5 py-2">
            <Fingerprint className="size-4 shrink-0 text-muted" />
            <span className="selectable break-all font-mono text-[12px] text-fg">{hostKey.fingerprint}</span>
          </div>
          <p className="text-muted">
            Compare it with the server&rsquo;s real key before you continue. On the server, <span className="selectable font-mono">ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub</span> prints it; otherwise ask whoever runs it. If
            the two differ, cancel: something between you and the server may be intercepting the connection.
          </p>
          <p className="text-muted">Once trusted, VibeLens connects only to a server that presents this key.</p>
        </div>
      )}
    </Dialog>
  );
}

/** Mounted once in App: the trust question raised by `useConnect`. */
export function HostKeyPrompt() {
  const pending = useHostKeyPrompt((s) => s.pending);
  const answer = useHostKeyPrompt((s) => s.answer);
  const qc = useQueryClient();
  const name = pending ? qc.getQueryData<ConnectionProfile[]>(["profiles"])?.find((p) => p.id === pending.profileId)?.name : null;
  return <HostKeyTrustDialog hostKey={pending?.key ?? null} connectionName={name} trustLabel="Trust and connect" onTrust={() => answer(true)} onCancel={() => answer(false)} />;
}
