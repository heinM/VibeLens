import { useId } from "react";
import type { RedshiftAuth, RedshiftTarget } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Input } from "../../ui";
import { FormField, Notice, SegmentedControl, invalidRing } from "../../ui/formKit";
import { AwsIdentityPicker } from "../AwsIdentityPicker";
import { regionAfterArnEdit } from "../icebergForm";
import { authAllowed, blankRedshiftAuth, blankTarget } from "../redshiftForm";
import { Group } from "./IcebergShared";
import type { SectionProps } from "./types";

const mono = (invalid: boolean) => cn("font-mono", invalid && invalidRing);

/**
 * Replaces Authentication, Network and TLS for Redshift: which cluster or workgroup, which database,
 * the AWS identity that signs Data API calls and how that identity becomes a database user.
 */
export function RedshiftSection({ profile, patchRedshift, err }: SectionProps) {
  const uid = useId();
  const o = profile.redshift;
  if (!o) return null;
  const t = o.target;
  const a = o.auth;
  const setTarget = (target: RedshiftTarget) => patchRedshift((x) => ({ ...x, target }));
  const setAuth = (auth: RedshiftAuth) => patchRedshift((x) => ({ ...x, auth }));
  const scoped = (f: string) => err(`rs.${f}`);

  return (
    <div className="space-y-5">
      <Group title="Warehouse" description="The provisioned cluster or Serverless workgroup, and the database statements run in.">
        <SegmentedControl<RedshiftTarget["kind"]>
          ariaLabel="Redshift deployment"
          value={t.kind}
          onChange={(k) => setTarget(blankTarget(k, t))}
          options={[
            { value: "provisioned", label: "Provisioned cluster" },
            { value: "serverless", label: "Serverless workgroup" },
          ]}
        />
        <div className="grid gap-3 sm:grid-cols-[1fr_minmax(0,12rem)]">
          {t.kind === "provisioned" ? (
            <FormField label="Cluster identifier" htmlFor={`${uid}-cluster`} error={err("rs.clusterIdentifier")} hint="The name in the Redshift console, not the endpoint.">
              <Input id={`${uid}-cluster`} data-autofocus value={t.clusterIdentifier} spellCheck={false} autoComplete="off" placeholder="analytics-cluster" className={mono(!!err("rs.clusterIdentifier"))} onChange={(e) => setTarget({ ...t, clusterIdentifier: e.target.value })} />
            </FormField>
          ) : (
            <FormField label="Workgroup" htmlFor={`${uid}-wg`} error={err("rs.workgroupName")} hint="The Serverless workgroup name, not the endpoint.">
              <Input id={`${uid}-wg`} data-autofocus value={t.workgroupName} spellCheck={false} autoComplete="off" placeholder="analytics" className={mono(!!err("rs.workgroupName"))} onChange={(e) => setTarget({ ...t, workgroupName: e.target.value })} />
            </FormField>
          )}
          <FormField label="Database" htmlFor={`${uid}-db`} error={err("rs.database")}>
            <Input id={`${uid}-db`} value={o.database} spellCheck={false} autoComplete="off" placeholder="dev" className={mono(!!err("rs.database"))} onChange={(e) => patchRedshift((x) => ({ ...x, database: e.target.value }))} />
          </FormField>
        </div>
      </Group>

      <Group title="AWS identity" description="Signs every Data API call. Nothing else is stored: there is no database password on this computer.">
        <AwsIdentityPicker idPrefix={`${uid}-aws`} target={o.aws} err={scoped} onChange={(aws) => patchRedshift((x) => ({ ...x, aws }))} regionHint="Region of the cluster or workgroup." />
      </Group>

      <Group title="Sign-in" description="Which database user the statements run as inside Redshift.">
        <SegmentedControl<RedshiftAuth["kind"]>
          ariaLabel="Redshift sign-in"
          value={a.kind}
          onChange={(k) => setAuth(blankRedshiftAuth(k, a))}
          options={[
            { value: "iamIdentity", label: "IAM identity" },
            { value: "dbUser", label: "Database user", disabled: !authAllowed("dbUser", t.kind), title: authAllowed("dbUser", t.kind) ? undefined : "Provisioned clusters only" },
            { value: "secret", label: "Secrets Manager" },
          ]}
        />
        {a.kind === "iamIdentity" && (
          <p className="text-[12px] leading-snug text-muted">
            Your IAM identity is the database user (<span className="font-mono">IAM:user</span> or <span className="font-mono">IAMR:role</span>). Needs{" "}
            <span className="font-mono">{t.kind === "provisioned" ? "redshift:GetClusterCredentialsWithIAM" : "redshift-serverless:GetCredentials"}</span>.
          </p>
        )}
        {a.kind === "dbUser" && (
          <FormField label="Database user" htmlFor={`${uid}-dbuser`} error={err("rs.dbUser")} hint="The Data API fetches temporary credentials for this user. Needs redshift:GetClusterCredentials scoped to it.">
            <Input id={`${uid}-dbuser`} value={a.dbUser} spellCheck={false} autoComplete="off" placeholder="analyst" className={cn("max-w-72", mono(!!err("rs.dbUser")))} onChange={(e) => setAuth({ ...a, dbUser: e.target.value })} />
          </FormField>
        )}
        {a.kind === "secret" && (
          <FormField label="Secret ARN" htmlFor={`${uid}-arn`} error={err("rs.secretArn")} hint="The Data API reads the user name and password from this secret. VibeLens only passes the ARN and never sees the password.">
            <Input
              id={`${uid}-arn`}
              value={a.secretArn}
              spellCheck={false}
              autoComplete="off"
              placeholder="arn:aws:secretsmanager:eu-west-1:123456789012:secret:redshift-analyst-AbCdEf"
              className={mono(!!err("rs.secretArn"))}
              onChange={(e) => {
                const secretArn = e.target.value;
                patchRedshift((x) => ({ ...x, auth: { kind: "secret", secretArn }, aws: { ...x.aws, region: regionAfterArnEdit(x.aws.region, x.auth.kind === "secret" ? x.auth.secretArn : "", secretArn) } }));
              }}
            />
          </FormField>
        )}
        {t.kind === "serverless" && <p className="text-[11.5px] leading-snug text-muted">A named database user works with provisioned clusters only.</p>}
      </Group>

      <Notice tone="info">
        <span className="block font-medium text-fg">Through the Redshift Data API</span>
        <span className="mt-0.5 block">
          Calls go to the AWS API in this region over HTTPS, so a cluster in a private subnet needs no tunnel or VPN. Each statement commits on its own (BEGIN, COMMIT and ROLLBACK are
          refused), results are limited to 100 MB per statement, and a statement runs for at most 24 hours. The identity needs the <span className="font-mono">redshift-data:*</span>{" "}
          permissions it uses (ExecuteStatement, DescribeStatement, GetStatementResult, CancelStatement, ListDatabases, ListSchemas, ListTables, DescribeTable) plus{" "}
          <span className="font-mono">{a.kind === "dbUser" ? "redshift:GetClusterCredentials" : a.kind === "secret" ? "secretsmanager:GetSecretValue" : t.kind === "provisioned" ? "redshift:GetClusterCredentialsWithIAM" : "redshift-serverless:GetCredentials"}</span>
          {a.kind === "secret" ? " on the secret" : ""}.
        </span>
      </Notice>
    </div>
  );
}
