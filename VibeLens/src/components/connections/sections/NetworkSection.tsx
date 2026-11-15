import { useId } from "react";
import { useSecretStatus } from "../useSecretStatus";
import { Fingerprint } from "lucide-react";
import type { SshAuth, TunnelConfig } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Button, Input } from "../../ui";
import { FormField, Notice, NumberField, SegmentedControl, Switch, invalidRing } from "../../ui/formKit";
import { newSecret } from "../draft";
import { SecretField } from "../SecretField";
import type { SectionProps } from "./types";

const blankTunnel = (): TunnelConfig => ({ kind: "ssh", host: "", port: 22, username: "", auth: { kind: "agent" }, knownHostFingerprint: null });

export function NetworkSection({ profile, original, editing, patch, draft, setSshSecret, err }: SectionProps) {
  const uid = useId();
  const t = profile.tunnel;
  const setTunnel = (tunnel: TunnelConfig | null) => patch((p) => ({ ...p, tunnel }));
  const secrets = useSecretStatus(editing ? original?.id : null);
  const storedSsh = t?.auth.kind === "password" ? secrets.data?.hasSshPassword : secrets.data?.hasSshPassphrase;
  const hadSecret =
    editing && !!original?.tunnel && original.tunnel.auth.kind !== "agent" && original.tunnel.auth.kind === t?.auth.kind && (storedSsh ?? false);

  const setAuth = (kind: SshAuth["kind"]) => {
    if (!t || t.auth.kind === kind) return;
    const auth: SshAuth = kind === "privateKey" ? { kind, path: "" } : { kind } as SshAuth;
    setTunnel({ ...t, auth });
    setSshSecret(newSecret());
  };

  return (
    <div className="space-y-4">
      <Switch
        checked={!!t}
        onChange={(on) => setTunnel(on ? blankTunnel() : null)}
        label="Connect through an SSH tunnel"
        description="Reach a database in a private network via a bastion host."
      />

      {t && (
        <>
          <div className="grid grid-cols-[1fr_110px] gap-3">
            <FormField label="SSH host" htmlFor={`${uid}-host`} error={err("tunnel.host")}>
              <Input
                id={`${uid}-host`}
                value={t.host}
                spellCheck={false}
                autoComplete="off"
                placeholder="bastion.example.com"
                className={cn("font-mono", err("tunnel.host") && invalidRing)}
                onChange={(e) => setTunnel({ ...t, host: e.target.value })}
              />
            </FormField>
            <FormField label="Port" htmlFor={`${uid}-port`} error={err("tunnel.port")}>
              <NumberField id={`${uid}-port`} value={t.port} min={1} max={65535} invalid={!!err("tunnel.port")} onChange={(v) => setTunnel({ ...t, port: v ?? Number.NaN })} />
            </FormField>
          </div>
          <FormField label="Username" htmlFor={`${uid}-user`} error={err("tunnel.username")}>
            <Input
              id={`${uid}-user`}
              value={t.username}
              spellCheck={false}
              autoComplete="off"
              className={cn("max-w-72 font-mono", err("tunnel.username") && invalidRing)}
              onChange={(e) => setTunnel({ ...t, username: e.target.value })}
            />
          </FormField>

          <FormField label="Authentication">
            <SegmentedControl
              ariaLabel="SSH authentication"
              value={t.auth.kind}
              onChange={setAuth}
              options={[
                { value: "agent", label: "SSH agent" },
                { value: "privateKey", label: "Private key" },
                { value: "password", label: "Password" },
              ]}
            />
          </FormField>

          {t.auth.kind === "agent" && <p className="text-[12px] text-muted">Uses the keys loaded in your running ssh-agent (SSH_AUTH_SOCK, or the Windows OpenSSH agent).</p>}
          {t.auth.kind === "privateKey" && (
            <div className="space-y-3">
              <FormField label="Private key file" htmlFor={`${uid}-key`} error={err("tunnel.keyPath")} hint="Absolute path, for example ~/.ssh/id_ed25519.">
                <Input
                  id={`${uid}-key`}
                  value={t.auth.path}
                  spellCheck={false}
                  autoComplete="off"
                  className={cn("font-mono", err("tunnel.keyPath") && invalidRing)}
                  onChange={(e) => setTunnel({ ...t, auth: { kind: "privateKey", path: e.target.value } })}
                />
              </FormField>
              <SecretField label="Key passphrase" optional edit={draft.sshSecret} onChange={setSshSecret} hasStored={hadSecret} />
            </div>
          )}
          {t.auth.kind === "password" && <SecretField label="SSH password" edit={draft.sshSecret} onChange={setSshSecret} hasStored={hadSecret} />}

          <FormField label="Pinned host key" hint="VibeLens pins the server's host key the first time it connects and refuses to connect if it ever changes.">
            <div className="flex items-center gap-2">
              <div className="flex h-8 min-w-0 flex-1 items-center gap-2 rounded-md border border-border bg-panel-2/50 px-2.5 text-[12px]">
                <Fingerprint className="size-3.5 shrink-0 text-muted" />
                {t.knownHostFingerprint ? <span className="selectable truncate font-mono text-fg">{t.knownHostFingerprint}</span> : <span className="text-muted">Not pinned yet. Pinned on first connect.</span>}
              </div>
              <Button size="sm" disabled={!t.knownHostFingerprint} onClick={() => setTunnel({ ...t, knownHostFingerprint: null })}>
                Forget
              </Button>
            </div>
          </FormField>

          <Notice tone="info">AWS SSM Session Manager tunnels (no bastion host needed) are coming in V1.1.</Notice>
          {profile.engine === "valkey" && profile.valkey.cluster && (
            <Notice tone="warn">Valkey cluster mode doesn&rsquo;t work through an SSH tunnel in V1: cluster redirects point at node addresses the tunnel doesn&rsquo;t forward.</Notice>
          )}
        </>
      )}
    </div>
  );
}
