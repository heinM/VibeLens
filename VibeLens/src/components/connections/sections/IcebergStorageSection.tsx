import { useId } from "react";
import { Lock } from "lucide-react";
import type { IcebergStorage } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Button, Input } from "../../ui";
import { FormField, Notice, RadioCards, Switch, invalidRing } from "../../ui/formKit";
import { blankStorage, catalogAws, isAwsCatalog, type StorageKind } from "../icebergForm";
import { useSecretStatus } from "../useSecretStatus";
import type { S3KeysEdit } from "../draft";
import { Group, OptionalAwsIdentity } from "./IcebergShared";
import type { SectionProps } from "./types";

const mono = (invalid: boolean | undefined) => cn("font-mono", invalid && invalidRing);

/** Where data files are read from. Most connections can leave this on Automatic. */
export function IcebergStorageSection({ profile, patchIceberg, err, draft, setS3Keys, original, editing }: SectionProps) {
  const uid = useId();
  const ice = profile.iceberg;
  const secrets = useSecretStatus(editing ? original?.id : null);
  if (!ice) return null;
  const s = ice.storage;
  const cAws = catalogAws(ice.catalog);
  const setStorage = (storage: IcebergStorage) => patchIceberg((o) => ({ ...o, storage }));
  const hasStoredKeys = editing && original?.iceberg?.storage.kind === "s3Static" && (secrets.data?.hasS3Keys ?? false);
  const autoText =
    ice.catalog.kind === "rest"
      ? "Uses the storage settings and credentials the catalog hands out with each table (vended credentials). VibeLens never falls back to the AWS credentials on this computer."
      : isAwsCatalog(ice.catalog)
        ? "Reads data files with the catalog's AWS identity."
        : "Reads local paths from disk. For s3:// tables choose one of the S3 options below.";

  return (
    <div className="space-y-5">
      <Group title="Storage" description="How VibeLens reads the data files and metadata that tables point to.">
        <RadioCards<StorageKind>
          ariaLabel="Storage"
          value={s.kind}
          onChange={(k) => setStorage(blankStorage(k, ice.catalog, s))}
          options={[
            { value: "auto", title: "Automatic", badge: <span className="rounded border border-border px-1 text-[10px] font-medium leading-4 text-muted">Default</span>, description: autoText },
            { value: "s3", title: "Amazon S3 with an AWS identity", description: "Pick the profile or SSO role to read the bucket. Add an endpoint for private S3 gateways or VPC endpoints." },
            { value: "s3Static", title: "S3-compatible with access keys", description: "MinIO, Ceph, Cloudflare R2, Backblaze and similar. Keys are stored in the OS keychain." },
            { value: "local", title: "Local filesystem", description: "Table locations are paths on this computer (file:// or /data/...)." },
          ]}
        />
      </Group>

      {s.kind === "s3" && (
        <Group title="AWS identity">
          <OptionalAwsIdentity
            value={s.aws}
            catalogAws={cAws}
            onChange={(aws) => setStorage({ ...s, aws })}
            err={err}
            idPrefix={`${uid}-aws`}
            errPrefix="ice.storage."
            regionHint="Region used to sign requests. The bucket's own region can be set below."
          />
        </Group>
      )}

      {(s.kind === "s3" || s.kind === "s3Static") && (
        <Group title="Endpoint" description={s.kind === "s3" ? "Only needed when the bucket isn't reached at the regular AWS address." : undefined}>
          <FormField label="Endpoint" htmlFor={`${uid}-ep`} optional error={err("ice.storage.endpoint")} hint="Overrides any endpoint the catalog suggests, which is often an internal address your computer can't resolve.">
            <Input id={`${uid}-ep`} value={s.endpoint ?? ""} spellCheck={false} autoComplete="off" placeholder="https://minio.example.com:9000" className={mono(!!err("ice.storage.endpoint"))} onChange={(e) => setStorage({ ...s, endpoint: e.target.value || null })} />
          </FormField>
          <FormField label="Bucket region" htmlFor={`${uid}-rg`} optional error={err("ice.storage.region")} hint="Blank: the catalog's region if it gives one, else us-east-1, which MinIO and most S3-compatible stores expect. AWS buckets in other regions need their region here.">
            <Input id={`${uid}-rg`} value={s.region ?? ""} spellCheck={false} autoComplete="off" placeholder="us-east-1" className={cn("max-w-56", mono(!!err("ice.storage.region")))} onChange={(e) => setStorage({ ...s, region: e.target.value || null })} />
          </FormField>
          <Switch checked={s.pathStyle} onChange={(pathStyle) => setStorage({ ...s, pathStyle })} label="Path-style addressing" description="Use https://host/bucket/key instead of https://bucket.host/key. MinIO and most self-hosted stores need it." />
        </Group>
      )}

      {s.kind === "s3Static" && (
        <Group title="Access keys">
          <S3KeysField edit={draft.s3Keys} onChange={setS3Keys} hasStored={hasStoredKeys} error={err("ice.storage.keys")} idPrefix={uid} />
        </Group>
      )}

      {s.kind === "local" && <Notice tone="info">Reads files straight from disk. Athena can&rsquo;t be used with local storage.</Notice>}
      {s.kind === "auto" && ice.catalog.kind === "rest" && (
        <Notice tone="info">Automatic only works when the catalog hands out storage settings. Local and self-hosted catalogs (for example the Apache REST fixture with MinIO) usually don't: choose <b>S3-compatible with access keys</b> and set the endpoint, or reads go to AWS S3. Your settings always win over what the catalog returns.</Notice>
      )}
    </div>
  );
}

/** Access key ID + secret, saved as a pair. Never shows a stored value. */
function S3KeysField({ edit, onChange, hasStored, error, idPrefix }: { edit: S3KeysEdit; onChange: (e: S3KeysEdit) => void; hasStored: boolean; error?: string; idPrefix: string }) {
  if (hasStored && edit.mode === "keep") {
    return (
      <div className="flex items-center gap-2">
        <div className="flex h-8 min-w-0 flex-1 items-center gap-2 rounded-md border border-border bg-panel-2/50 px-2.5 text-[13px] text-muted">
          <Lock className="size-3.5 shrink-0" />
          <span className="truncate font-mono">•••• key pair stored in keychain</span>
        </div>
        <Button size="sm" onClick={() => onChange({ mode: "set", accessKeyId: "", secretAccessKey: "" })}>
          Replace
        </Button>
        <Button size="sm" variant="ghost" onClick={() => onChange({ mode: "remove", accessKeyId: "", secretAccessKey: "" })}>
          Remove
        </Button>
      </div>
    );
  }
  if (edit.mode === "remove") {
    return (
      <div className="flex items-center gap-2">
        <div className="flex h-8 min-w-0 flex-1 items-center rounded-md border border-warn/40 bg-warn/10 px-2.5 text-[12px] text-warn">Will be removed from the keychain when you save.</div>
        <Button size="sm" variant="ghost" onClick={() => onChange({ mode: "keep", accessKeyId: "", secretAccessKey: "" })}>
          Undo
        </Button>
      </div>
    );
  }
  return (
    <div className="space-y-2">
      <div className="grid gap-3 sm:grid-cols-2">
        <FormField label="Access key ID" htmlFor={`${idPrefix}-ak`}>
          <Input id={`${idPrefix}-ak`} type="password" autoComplete="new-password" spellCheck={false} value={edit.accessKeyId} className={error ? invalidRing : undefined} onChange={(e) => onChange({ ...edit, mode: "set", accessKeyId: e.target.value })} />
        </FormField>
        <FormField label="Secret access key" htmlFor={`${idPrefix}-sk`}>
          <Input id={`${idPrefix}-sk`} type="password" autoComplete="new-password" spellCheck={false} value={edit.secretAccessKey} className={error ? invalidRing : undefined} onChange={(e) => onChange({ ...edit, mode: "set", secretAccessKey: e.target.value })} />
        </FormField>
      </div>
      {error ? (
        <span role="alert" className="block text-[11px] text-danger">
          {error}
        </span>
      ) : (
        <span className="block text-[11px] text-muted">Saved to the OS keychain. They are never written to disk or shown again.</span>
      )}
      {hasStored && (
        <Button size="sm" variant="ghost" onClick={() => onChange({ mode: "keep", accessKeyId: "", secretAccessKey: "" })}>
          Keep existing
        </Button>
      )}
    </div>
  );
}

