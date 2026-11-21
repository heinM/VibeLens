import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Brain, CheckCircle2, Cog, Cpu, Database, HardDrive, KeyRound, Library, Lock, Network, Trash2, Warehouse } from "lucide-react";
import { api, asError } from "../../ipc/commands";
import type { ConnectionProfile, ErrorPayload, IcebergOptions, RedshiftOptions, TestResult } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { useUIStore } from "../../store/useUIStore";
import { Button, ConfirmDialog, Dialog, EmptyState, EnvBadge, ErrorBox, Spinner } from "../ui";
import { FocusScope } from "../ui/formKit";
import { aiAccessMeta } from "./aiAccess";
import { cleanProfile, icebergSecretArgs, initialDraft, missingIcebergSecrets, secretArgs, testPasswordArg, type Draft, type S3KeysEdit, type SecretEdit } from "./draft";
import { catalogSummary, engineSummary, normalizeIceberg, storageSummary } from "./icebergForm";
import { normalizeRedshift, redshiftTargetName } from "./redshiftForm";
import { RedshiftSection } from "./sections/RedshiftSection";
import { AdvancedSection } from "./sections/AdvancedSection";
import { AiAccessSection } from "./sections/AiAccessSection";
import { AuthSection } from "./sections/AuthSection";
import { GeneralSection } from "./sections/GeneralSection";
import { IcebergCatalogSection } from "./sections/IcebergCatalogSection";
import { IcebergEngineSection } from "./sections/IcebergEngineSection";
import { IcebergStorageSection } from "./sections/IcebergStorageSection";
import { NetworkSection } from "./sections/NetworkSection";
import { TlsSection } from "./sections/TlsSection";
import type { SectionProps } from "./sections/types";
import { useSecretStatus } from "./useSecretStatus";
import { errorCountBySection, firstErrorSection, validateProfile, type SectionId } from "./validation";

const NAV: { id: SectionId; label: string; icon: typeof Database }[] = [
  { id: "general", label: "General", icon: Database },
  { id: "auth", label: "Authentication", icon: KeyRound },
  { id: "warehouse", label: "Warehouse", icon: Warehouse },
  { id: "catalog", label: "Catalog", icon: Library },
  { id: "storage", label: "Storage", icon: HardDrive },
  { id: "queryEngine", label: "Query engine", icon: Cpu },
  { id: "network", label: "Network", icon: Network },
  { id: "tls", label: "TLS", icon: Lock },
  { id: "ai", label: "AI access", icon: Brain },
  { id: "advanced", label: "Advanced", icon: Cog },
];

/**
 * Sections shown per engine. Iceberg swaps Authentication for Catalog and drops Network, TLS and Advanced.
 * Redshift has one Warehouse section (target, database, AWS identity, sign-in): the Data API is reached
 * through AWS with IAM, so there is no host, port, password, TLS or tunnel to set.
 */
const ENGINE_SECTIONS: Record<ConnectionProfile["engine"], SectionId[]> = {
  postgres: ["general", "auth", "network", "tls", "ai", "advanced"],
  valkey: ["general", "auth", "network", "tls", "ai", "advanced"],
  iceberg: ["general", "catalog", "storage", "queryEngine", "ai"],
  redshift: ["general", "warehouse", "ai"],
};

type TestState =
  | { status: "idle" }
  | { status: "running" }
  | { status: "done"; result: TestResult }
  | { status: "error"; error: ErrorPayload };

/** Mounted once; renders nothing until `useUIStore().wizard.open`. */
export function ConnectionWizard() {
  const wizard = useUIStore((s) => s.wizard);
  if (!wizard.open) return null;
  return <WizardLoader key={wizard.editingProfileId ?? "new"} editingId={wizard.editingProfileId} />;
}

function WizardLoader({ editingId }: { editingId: string | null }) {
  const closeWizard = useUIStore((s) => s.closeWizard);
  const profiles = useQuery({ queryKey: ["profiles"], queryFn: () => api.listProfiles(), refetchOnMount: "always" });

  if (editingId == null && !profiles.isPending) return <WizardForm existing={null} others={profiles.data ?? []} />;
  if (editingId == null) return <WizardForm existing={null} others={[]} />;

  const found = profiles.data?.find((p) => p.id === editingId) ?? null;
  if (found) return <WizardForm existing={found} others={profiles.data ?? []} />;

  return (
    <Dialog open onClose={closeWizard} title="Edit connection" width="max-w-md">
      <FocusScope>
        {profiles.isPending ? (
          <div className="flex h-32 items-center justify-center">
            <Spinner />
          </div>
        ) : profiles.isError ? (
          <ErrorBox error={asError(profiles.error)} />
        ) : (
          <EmptyState title="Connection not found">It may have been deleted. Close this dialog and pick another connection.</EmptyState>
        )}
      </FocusScope>
    </Dialog>
  );
}

function WizardForm({ existing, others }: { existing: ConnectionProfile | null; others: ConnectionProfile[] }) {
  const aiMode = useUIStore((s) => s.aiMode);
  const qc = useQueryClient();
  const closeWizard = useUIStore((s) => s.closeWizard);
  const activeProfileId = useUIStore((s) => s.activeProfileId);
  const setActiveTarget = useUIStore((s) => s.setActiveTarget);

  const [draft, setDraft] = useState<Draft>(() => initialDraft(existing));
  const [section, setSection] = useState<SectionId>("general");
  const [submitted, setSubmitted] = useState(false);
  const [test, setTest] = useState<TestState>({ status: "idle" });
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<ErrorPayload | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const confirmingRef = useRef(false);
  confirmingRef.current = confirmDelete;
  const testSeq = useRef(0);

  const editing = existing != null;
  const profile = draft.profile;
  const storedSecrets = useSecretStatus(editing ? existing?.id : null).data;
  const missing = useMemo(() => missingIcebergSecrets(existing, draft, storedSecrets), [existing, draft, storedSecrets]);
  const errors = useMemo(() => validateProfile(cleanProfile(profile), others, missing), [profile, others, missing]);
  const counts = useMemo(() => errorCountBySection(errors), [errors]);

  const touch = useCallback(() => {
    testSeq.current += 1;
    setTest((t) => (t.status === "idle" ? t : { status: "idle" }));
    setSaveError(null);
  }, []);
  const patch = useCallback(
    (fn: (p: ConnectionProfile) => ConnectionProfile) => {
      touch();
      setDraft((d) => ({ ...d, profile: fn(d.profile) }));
    },
    [touch],
  );
  const setPassword = useCallback(
    (password: SecretEdit) => {
      touch();
      setDraft((d) => ({ ...d, password }));
    },
    [touch],
  );
  const setSshSecret = useCallback(
    (sshSecret: SecretEdit) => {
      touch();
      setDraft((d) => ({ ...d, sshSecret }));
    },
    [touch],
  );

  const patchIceberg = useCallback(
    (fn: (o: IcebergOptions) => IcebergOptions) => patch((p) => (p.iceberg ? { ...p, iceberg: normalizeIceberg(fn(p.iceberg)) } : p)),
    [patch],
  );
  const setIcebergSecret = useCallback(
    (icebergSecret: SecretEdit) => {
      touch();
      setDraft((d) => ({ ...d, icebergSecret }));
    },
    [touch],
  );
  const setS3Keys = useCallback(
    (s3Keys: S3KeysEdit) => {
      touch();
      setDraft((d) => ({ ...d, s3Keys }));
    },
    [touch],
  );
  const setEngineSecret = useCallback(
    (engineSecret: SecretEdit) => {
      touch();
      setDraft((d) => ({ ...d, engineSecret }));
    },
    [touch],
  );
  const patchRedshift = useCallback(
    (fn: (o: RedshiftOptions) => RedshiftOptions) => patch((p) => (p.redshift ? { ...p, redshift: normalizeRedshift(fn(p.redshift)) } : p)),
    [patch],
  );

  const onClose = useCallback(() => {
    if (confirmingRef.current) return; // Esc belongs to the delete confirmation
    closeWizard();
  }, [closeWizard]);

  /** Returns true when valid; otherwise reveals errors and jumps to the first section with one. */
  const checkValid = (): boolean => {
    setSubmitted(true);
    const first = firstErrorSection(errors);
    if (first) {
      setSection(first);
      return false;
    }
    return true;
  };

  const runTest = async () => {
    if (!checkValid()) return;
    const seq = ++testSeq.current;
    setTest({ status: "running" });
    try {
      const result = await api.testConnection(cleanProfile(profile), testPasswordArg(draft));
      if (seq === testSeq.current) setTest({ status: "done", result });
    } catch (e) {
      if (seq === testSeq.current) setTest({ status: "error", error: asError(e) });
    }
  };

  const save = async () => {
    if (!checkValid()) return;
    setSaving(true);
    setSaveError(null);
    try {
      const { password, sshSecret } = secretArgs(existing, draft);
      const { icebergSecret, s3Keys, engineSecret } = icebergSecretArgs(existing, draft);
      const forgetHostKey = !!existing?.tunnel?.knownHostFingerprint && !profile.tunnel?.knownHostFingerprint;
      await api.saveProfile(cleanProfile(profile), password, sshSecret, icebergSecret, s3Keys, forgetHostKey, engineSecret);
      await qc.invalidateQueries({ queryKey: ["profiles"] });
      closeWizard();
    } catch (e) {
      setSaveError(asError(e));
      setSaving(false);
    }
  };

  const remove = async () => {
    if (!existing) return;
    setDeleting(true);
    setSaveError(null);
    try {
      await api.disconnect(existing.id).catch(() => undefined);
      await api.deleteProfile(existing.id);
      if (activeProfileId === existing.id) setActiveTarget(null);
      await qc.invalidateQueries({ queryKey: ["profiles"] });
      setConfirmDelete(false);
      closeWizard();
    } catch (e) {
      setConfirmDelete(false);
      setSaveError(asError(e));
      setDeleting(false);
    }
  };

  // Ctrl/Cmd+Enter saves from anywhere in the dialog.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && !confirmingRef.current) {
        e.preventDefault();
        void save();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const err = useCallback((field: string) => (submitted ? errors[field] : undefined), [submitted, errors]);
  const props: SectionProps = { draft, profile, editing, original: existing, patch, patchIceberg, patchRedshift, setPassword, setSshSecret, setIcebergSecret, setS3Keys, setEngineSecret, err };
  const visible = ENGINE_SECTIONS[profile.engine];
  const current: SectionId = visible.includes(section) ? section : "general";

  const ice = profile.iceberg;
  const summary: Record<SectionId, string> = {
    general: profile.name.trim() || "Untitled",
    catalog: ice ? catalogSummary(ice.catalog) : "",
    storage: ice ? storageSummary(ice.storage) : "",
    queryEngine: ice ? engineSummary(ice.queryEngine) : "",
    warehouse: profile.redshift ? redshiftTargetName(profile.redshift.target) || (profile.redshift.target.kind === "serverless" ? "Serverless" : "Provisioned") : "",
    auth: { none: "None", password: "Password", rdsIam: "RDS IAM", secretsManager: "Secrets Manager", elastiCacheIam: "ElastiCache IAM", accessToken: "Access token" }[profile.auth.kind],
    network: profile.tunnel ? "SSH tunnel" : "Direct",
    tls: { verifyFull: "Verify full", require: "No verification", disable: "Off" }[profile.tls.mode],
    ai: aiAccessMeta(profile.aiDataAccess).title,
    advanced: "",
  };

  const busy = saving || deleting;

  return (
    <>
      <Dialog
        open
        onClose={onClose}
        width="max-w-4xl"
        title={
          <span className="flex items-center gap-2">
            {editing ? "Edit connection" : "New connection"}
            {editing && <span className="font-normal text-muted">{existing.name}</span>}
            <EnvBadge env={profile.environment} />
          </span>
        }
        footer={
          <>
            <div className="mr-auto flex min-w-0 flex-1 items-center gap-3">
              {editing && (
                <Button variant="ghost" className="!text-danger" onClick={() => setConfirmDelete(true)} disabled={busy}>
                  <Trash2 className="size-3.5" /> Delete
                </Button>
              )}
              <TestResultView state={test} />
            </div>
            <Button onClick={() => void runTest()} loading={test.status === "running"} disabled={busy}>
              Test connection
            </Button>
            <Button variant="ghost" onClick={closeWizard} disabled={busy}>
              Cancel
            </Button>
            <Button variant="primary" onClick={() => void save()} loading={saving} disabled={deleting}>
              {editing ? "Save changes" : "Save connection"}
            </Button>
          </>
        }
      >
        <FocusScope>
          <div className="-m-4 flex h-[min(620px,72vh)] min-h-[360px]">
            <nav aria-label="Connection settings sections" className="flex w-48 shrink-0 flex-col gap-0.5 border-r border-border bg-bg/40 p-2">
              {NAV.filter((n) => visible.includes(n.id) && !(n.id === "ai" && aiMode === "classic")).map(({ id, label, icon: Icon }) => {
                const active = current === id;
                const n = submitted ? counts[id] : 0;
                return (
                  <button
                    key={id}
                    type="button"
                    aria-current={active ? "page" : undefined}
                    onClick={() => setSection(id)}
                    className={cn(
                      "flex h-9 items-center gap-2 rounded-md px-2 text-left text-[13px] outline-none focus-visible:ring-2 focus-visible:ring-accent/60",
                      active ? "bg-panel-2 text-fg" : "text-muted hover:bg-panel-2/60 hover:text-fg",
                    )}
                  >
                    <Icon className={cn("size-3.5 shrink-0", active && "text-accent")} />
                    <span className="min-w-0 flex-1">
                      <span className="block truncate leading-4">{label}</span>
                      {summary[id] && <span className="block truncate text-[10.5px] leading-3 text-muted">{summary[id]}</span>}
                    </span>
                    {n > 0 && (
                      <span className="grid size-4 shrink-0 place-items-center rounded-full bg-danger text-[10px] font-semibold text-white" title={`${n} to fix`}>
                        {n}
                      </span>
                    )}
                  </button>
                );
              })}
            </nav>
            <div className="min-w-0 flex-1 overflow-y-auto p-5" role="region" aria-label={NAV.find((n) => n.id === current)?.label}>
              <div className="mx-auto max-w-xl space-y-4">
                {current === "general" && <GeneralSection {...props} />}
                {current === "auth" && <AuthSection {...props} />}
                {current === "catalog" && <IcebergCatalogSection {...props} />}
                {current === "storage" && <IcebergStorageSection {...props} />}
                {current === "queryEngine" && <IcebergEngineSection {...props} />}
                {current === "warehouse" && <RedshiftSection {...props} />}
                {current === "network" && <NetworkSection {...props} />}
                {current === "tls" && <TlsSection {...props} />}
                {current === "ai" && <AiAccessSection {...props} />}
                {current === "advanced" && <AdvancedSection {...props} />}
                {saveError && <ErrorBox error={saveError} />}
              </div>
            </div>
          </div>
        </FocusScope>
      </Dialog>
      <ConfirmDialog
        open={confirmDelete}
        title="Delete connection"
        danger
        confirmText={deleting ? "Deleting" : "Delete connection"}
        requireTyping={profile.environment === "prod" && existing ? existing.name : undefined}
        onCancel={() => setConfirmDelete(false)}
        onConfirm={() => void remove()}
        body={
          <FocusScope className="space-y-3">
            <p data-autofocus tabIndex={-1} className="outline-none">
              Delete <strong className="font-semibold">{existing?.name}</strong>? Its saved passwords, tokens, keys and pinned host key are removed from this machine.
            </p>
            <p className="text-muted">Worksheets in your workspace folder are not touched.</p>
          </FocusScope>
        }
      />
    </>
  );
}

function TestResultView({ state }: { state: TestState }) {
  if (state.status === "idle") return null;
  if (state.status === "running")
    return (
      <span className="inline-flex items-center gap-2 text-xs text-muted" role="status">
        <Spinner className="size-3.5" /> Connecting
      </span>
    );
  if (state.status === "error") return <div className="max-h-16 min-w-0 flex-1 overflow-auto"><ErrorBox error={state.error} /></div>;
  const r = state.result;
  if (r.ok)
    return (
      <span className="inline-flex min-w-0 items-center gap-1.5 text-xs text-ok" role="status">
        <CheckCircle2 className="size-4 shrink-0" />
        <span className="truncate">
          Connected in {r.latencyMs} ms{r.serverVersion ? ` · ${r.serverVersion}` : ""}
        </span>
      </span>
    );
  return (
    <div className="max-h-16 min-w-0 flex-1 overflow-auto">
      <ErrorBox error={{ message: r.message || "The connection test failed." }} />
    </div>
  );
}
