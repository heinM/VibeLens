import { useId } from "react";
import type { TlsMode } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Button, Input } from "../../ui";
import { FormField, Notice, RadioCards, Switch, invalidRing } from "../../ui/formKit";
import { isAwsHost } from "../awsHelpers";
import type { SectionProps } from "./types";
import { isLoopbackHost } from "../remoteEngineForm";

/** MySQL refuses (in Rust) a password to another machine or an IAM token without verified TLS; say so up front. */
function MysqlTlsRule({ tlsMode, host, tunneled, iam }: { tlsMode: TlsMode; host: string; tunneled: boolean; iam: boolean }) {
  if (tlsMode === "verifyFull") return null;
  const remote = tunneled || !isLoopbackHost(host);
  if (!iam && !remote) return null;
  return (
    <Notice tone="danger">
      {iam
        ? "RDS IAM sends its token with the cleartext plugin, so MySQL connections with RDS IAM need Verify full (turn on the Amazon RDS CA bundle)."
        : "MySQL connections send a password to another machine (or through an SSH tunnel) only with Verify full: without verification, whoever is in the path can obtain a reusable credential. Turn on Verify full with the server's CA file, or the Amazon RDS CA bundle."}
    </Notice>
  );
}

export function TlsSection({ profile, patch, err }: SectionProps) {
  const uid = useId();
  const tls = profile.tls;
  const prod = profile.environment === "prod";
  const setTls = (next: Partial<typeof tls>) => patch((p) => ({ ...p, tls: { ...p.tls, ...next } }));
  const awsHost = isAwsHost(profile.host);

  return (
    <div className="space-y-4">
      <RadioCards<TlsMode>
        ariaLabel="TLS mode"
        value={tls.mode}
        onChange={(mode) => setTls({ mode })}
        options={[
          {
            value: "verifyFull",
            title: "Verify full",
            badge: <span className="rounded border border-ok/40 bg-ok/10 px-1 text-[10px] font-medium leading-4 text-ok">Recommended</span>,
            description: "Encrypts the connection and checks the server's certificate chain and hostname.",
          },
          {
            value: "require",
            title: "Require, no verification",
            description: "Encrypts, but accepts any certificate. Anyone on the network path can impersonate the server.",
            selectedClass: "border-warn/60 bg-warn/[0.06]",
          },
          {
            value: "disable",
            title: "Disable",
            description: "Plain text, including credentials and data. Use only on localhost or an already-encrypted link.",
            selectedClass: prod ? "border-danger/70 bg-danger/[0.07]" : "border-warn/60 bg-warn/[0.06]",
          },
        ]}
      />
      {profile.engine === "mysql" && <MysqlTlsRule tlsMode={tls.mode} host={profile.host} tunneled={!!profile.tunnel} iam={profile.auth.kind === "rdsIam"} />}
      {tls.mode === "require" && <Notice tone="warn">Certificates won&rsquo;t be checked. Prefer Verify full with the right CA, even on private networks.</Notice>}
      {tls.mode === "disable" && (
        <Notice tone={prod ? "danger" : "warn"}>
          {prod ? "This is a production connection and traffic will be unencrypted. Credentials and customer data can be read on the network." : "Traffic, including credentials, is not encrypted."}
        </Notice>
      )}

      {tls.mode !== "disable" && (
        <>
          <FormField label="CA certificate file" htmlFor={`${uid}-ca`} optional error={err("tls.caCertPath")} hint="PEM file with the CA that signed the server certificate. Needed for private CAs.">
            <Input
              id={`${uid}-ca`}
              value={tls.caCertPath ?? ""}
              spellCheck={false}
              autoComplete="off"
              placeholder="/path/to/ca.pem"
              className={cn("font-mono", err("tls.caCertPath") && invalidRing)}
              onChange={(e) => setTls({ caCertPath: e.target.value || null })}
            />
          </FormField>
          <Switch
            checked={tls.useRdsBundle}
            onChange={(useRdsBundle) => setTls({ useRdsBundle })}
            label="Trust the Amazon RDS CA bundle"
            description="RDS and Aurora certificates chain to Amazon CAs that operating systems don't trust by default."
          />
          {awsHost && !tls.useRdsBundle && (
            <Notice
              tone="info"
              action={
                <Button size="sm" onClick={() => setTls({ useRdsBundle: true, mode: tls.mode === "disable" ? "verifyFull" : tls.mode })}>
                  Trust bundle
                </Button>
              }
            >
              This host ends in <span className="font-mono">amazonaws.com</span>. Verifying it normally needs the Amazon RDS CA bundle.
            </Notice>
          )}
        </>
      )}
      {tls.mode === "disable" && awsHost && (
        <Notice
          tone="info"
          action={
            <Button size="sm" onClick={() => setTls({ mode: "verifyFull", useRdsBundle: true })}>
              Use Verify full
            </Button>
          }
        >
          AWS-hosted databases and caches normally require TLS.
        </Notice>
      )}
    </div>
  );
}
