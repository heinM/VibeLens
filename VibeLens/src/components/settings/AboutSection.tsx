import { useQuery } from "@tanstack/react-query";
import { api, asError } from "../../ipc/commands";
import { ErrorBox, Spinner } from "../ui";
import { CopyIconButton, Notice } from "../ui/formKit";

function Row({ label, value, mono, copy }: { label: string; value: string; mono?: boolean; copy?: boolean }) {
  return (
    <div className="grid grid-cols-[150px_1fr_auto] items-center gap-2 border-b border-border/70 py-2 last:border-b-0">
      <dt className="text-[12px] text-muted">{label}</dt>
      <dd className={mono ? "selectable min-w-0 break-all font-mono text-[12px]" : "selectable text-[13px]"}>{value}</dd>
      {copy ? <CopyIconButton text={value} label={`Copy ${label.toLowerCase()}`} /> : <span />}
    </div>
  );
}

export function AboutSection() {
  const info = useQuery({ queryKey: ["app-info"], queryFn: () => api.appInfo(), staleTime: Infinity });
  return (
    <div className="space-y-4">
      <h3 className="text-[13px] font-semibold">About VibeLens</h3>
      {info.isPending && <Spinner />}
      {info.isError && <ErrorBox error={asError(info.error)} />}
      {info.data && (
        <>
          <dl className="rounded-lg border border-border bg-bg/50 px-3">
            <Row label="Version" value={info.data.version} />
            <Row label="Config directory" value={info.data.configDir} mono copy />
            <Row label="Workspace directory" value={info.data.workspaceDir} mono copy />
            <Row label="Credential storage" value={info.data.vaultBackend === "keychain" ? "OS keychain" : "In memory (this session only)"} />
          </dl>
          {info.data.vaultBackend === "memory" && (
            <Notice tone="warn">
              No OS keychain is available on this platform, so passwords and API keys are kept in memory and are lost when VibeLens quits.
            </Notice>
          )}
        </>
      )}
    </div>
  );
}
