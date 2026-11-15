import { useQuery } from "@tanstack/react-query";
import { api } from "../../ipc/commands";
import type { AwsIdentity, AwsTarget } from "../../ipc/types";
import { cn } from "../../lib/cn";
import { Input, Select, Spinner } from "../ui";
import { FormField, Notice, SegmentedControl, invalidRing } from "../ui/formKit";
import { COMMON_REGIONS, describeProfile, emptyIdentity } from "./awsHelpers";
import { SsoIdentity } from "./SsoIdentity";

/**
 * Shared by the RDS IAM, Secrets Manager and ElastiCache IAM auth modes:
 * the resource region plus the identity used to sign requests.
 */
export function AwsIdentityPicker({
  target,
  onChange,
  err,
  idPrefix,
  regionHint,
}: {
  target: AwsTarget;
  onChange: (t: AwsTarget) => void;
  err: (field: string) => string | undefined;
  idPrefix: string;
  regionHint?: string;
}) {
  const id = target.identity;
  const setIdentity = (identity: AwsIdentity) => onChange({ ...target, identity });
  const regionId = `${idPrefix}-region`;

  return (
    <div className="space-y-3 rounded-lg border border-border bg-panel-2/30 p-3">
      <div className="text-[12px] font-medium text-fg">AWS identity</div>
      <FormField label="Region" htmlFor={regionId} error={err("aws.region")} hint={regionHint ?? "Region of the database, cache or secret, not necessarily your SSO region."}>
        <Input
          id={regionId}
          list={`${idPrefix}-region-list`}
          value={target.region}
          spellCheck={false}
          placeholder="eu-west-1"
          className={cn("max-w-56 font-mono", err("aws.region") && invalidRing)}
          onChange={(e) => onChange({ ...target, region: e.target.value })}
        />
        <datalist id={`${idPrefix}-region-list`}>
          {COMMON_REGIONS.map((r) => (
            <option key={r} value={r} />
          ))}
        </datalist>
      </FormField>

      <SegmentedControl
        ariaLabel="AWS identity source"
        value={id.kind}
        onChange={(kind) => setIdentity(id.kind === kind ? id : emptyIdentity(kind, target.region))}
        options={[
          { value: "profile", label: "AWS profile" },
          { value: "sso", label: "IAM Identity Center" },
          { value: "defaultChain", label: "Default credential chain" },
        ]}
      />

      {id.kind === "profile" && (
        <ProfileIdentity
          value={id.profileName}
          onSelect={(profileName, region) => onChange({ identity: { kind: "profile", profileName }, region: target.region.trim() ? target.region : (region ?? "") })}
          err={err}
          idPrefix={idPrefix}
        />
      )}
      {id.kind === "sso" && <SsoIdentity value={id} onChange={setIdentity} err={err} idPrefix={idPrefix} />}
      {id.kind === "defaultChain" && (
        <p className="text-[12px] leading-snug text-muted">
          Uses whatever the AWS SDK finds: <span className="font-mono">AWS_PROFILE</span> or access-key environment variables, the default profile in <span className="font-mono">~/.aws</span>, then container or instance roles.
          Convenient, but which identity is used depends on how VibeLens was launched.
        </p>
      )}
    </div>
  );
}

function ProfileIdentity({
  value,
  onSelect,
  err,
  idPrefix,
}: {
  value: string;
  onSelect: (name: string, region: string | null) => void;
  err: (field: string) => string | undefined;
  idPrefix: string;
}) {
  const profiles = useQuery({ queryKey: ["aws-profiles"], queryFn: () => api.awsListProfiles(), refetchOnMount: "always" });
  const selected = profiles.data?.find((p) => p.name === value);

  if (profiles.isPending)
    return (
      <div className="flex items-center gap-2 text-[12px] text-muted">
        <Spinner className="size-3.5" /> Reading ~/.aws/config
      </div>
    );
  if (profiles.data && profiles.data.length === 0)
    return (
      <Notice tone="info">
        No profiles found. VibeLens reads named profiles from <span className="font-mono">~/.aws/config</span> and <span className="font-mono">~/.aws/credentials</span>. Run{" "}
        <span className="font-mono">aws configure sso</span> in a terminal, or use IAM Identity Center sign-in above.
      </Notice>
    );

  return (
    <FormField label="Profile" htmlFor={`${idPrefix}-profile`} error={err("aws.profile")} hint={selected ? describeProfile(selected) : undefined}>
      <Select
        id={`${idPrefix}-profile`}
        value={value}
        className={err("aws.profile") ? invalidRing : undefined}
        onChange={(e) => onSelect(e.target.value, profiles.data?.find((p) => p.name === e.target.value)?.region ?? null)}
      >
        <option value="">Choose a profile</option>
        {value && !selected && <option value={value}>{value} (not found in ~/.aws/config)</option>}
        {profiles.data?.map((p) => (
          <option key={p.name} value={p.name}>
            {p.name} — {describeProfile(p)}
          </option>
        ))}
      </Select>
    </FormField>
  );
}
