import { useId, useState } from "react";
import type { IcebergCatalog, RestAuth } from "../../../ipc/types";
import { cn } from "../../../lib/cn";
import { Input } from "../../ui";
import { FormField, Notice, SegmentedControl, invalidRing } from "../../ui/formKit";
import { AwsIdentityPicker } from "../AwsIdentityPicker";
import { SecretField } from "../SecretField";
import { useSecretStatus } from "../useSecretStatus";
import { blankCatalog, blankRestAuth, CATALOG_KINDS, headerRows, headersFromRows, secretKind, sqlFlavour, sqliteUriFromPath, sqlitePathFromUri, urlHasInlinePassword, regionAfterArnEdit, type CatalogKind, type HeaderRow } from "../icebergForm";
import { Group, HeaderEditor } from "./IcebergShared";
import type { SectionProps } from "./types";

/** Replaces the Authentication section for Iceberg connections: how VibeLens finds tables. */
export function IcebergCatalogSection(props: SectionProps) {
  const { profile, patchIceberg } = props;
  const ice = profile.iceberg;
  const uid = useId();
  if (!ice) return null;
  const c = ice.catalog;
  const setCatalog = (catalog: IcebergCatalog) => patchIceberg((o) => ({ ...o, catalog }));

  return (
    <div className="space-y-5">
      <Group title="Catalog" description="The service that knows which tables exist and where their metadata lives.">
        <SegmentedControl<CatalogKind>
          ariaLabel="Catalog type"
          value={c.kind}
          onChange={(kind) => patchIceberg((o) => ({ ...o, catalog: blankCatalog(kind, o.catalog, o.catalog.kind === "glue" || o.catalog.kind === "s3Tables" ? o.catalog.aws.region : "") }))}
          options={CATALOG_KINDS.map((k) => ({ value: k.value, label: k.label }))}
        />
      </Group>
      {c.kind === "rest" && <RestCatalog {...props} catalog={c} setCatalog={setCatalog} uid={uid} />}
      {c.kind === "glue" && <GlueCatalog {...props} catalog={c} setCatalog={setCatalog} uid={uid} />}
      {c.kind === "s3Tables" && <S3TablesCatalog {...props} catalog={c} setCatalog={setCatalog} uid={uid} />}
      {c.kind === "sql" && <SqlCatalog {...props} catalog={c} setCatalog={setCatalog} uid={uid} />}
      {c.kind === "hms" && <HmsCatalog {...props} catalog={c} setCatalog={setCatalog} uid={uid} />}
    </div>
  );
}

type Sub<K extends IcebergCatalog["kind"]> = SectionProps & { catalog: Extract<IcebergCatalog, { kind: K }>; setCatalog: (c: IcebergCatalog) => void; uid: string };

const mono = (invalid: boolean | undefined) => cn("font-mono", invalid && invalidRing);

/* ------------------------------------------------------------------ REST */

function RestCatalog({ catalog: c, setCatalog, uid, err, draft, setIcebergSecret, original, editing }: Sub<"rest">) {
  const [rows, setRows] = useState<HeaderRow[]>(() => headerRows(c.headers));
  const secrets = useSecretStatus(editing ? original?.id : null);
  const hasStored = editing && !!original?.iceberg && secretKind(original.iceberg.catalog) === secretKind(c) && (secrets.data?.hasIcebergSecret ?? false);
  const setAuth = (auth: RestAuth) => setCatalog({ ...c, auth });
  const o = c.auth.kind === "oAuth2" ? c.auth : null;

  return (
    <>
      <Group title="Connection">
        <FormField label="Catalog URI" htmlFor={`${uid}-uri`} error={err("ice.catalog.uri")} hint="The REST endpoint of Polaris, Lakekeeper, Nessie, Gravitino or any server implementing the Iceberg REST spec.">
          <Input id={`${uid}-uri`} data-autofocus value={c.uri} spellCheck={false} autoComplete="off" placeholder="https://catalog.example.com/api/catalog" className={mono(!!err("ice.catalog.uri"))} onChange={(e) => setCatalog({ ...c, uri: e.target.value })} />
        </FormField>
        <FormField label="Warehouse" htmlFor={`${uid}-wh`} optional hint="Some servers need it to pick a catalog (Polaris: the catalog name).">
          <Input id={`${uid}-wh`} value={c.warehouse ?? ""} spellCheck={false} autoComplete="off" className="font-mono" onChange={(e) => setCatalog({ ...c, warehouse: e.target.value || null })} />
        </FormField>
      </Group>

      <Group title="Sign-in">
        <SegmentedControl<RestAuth["kind"]>
          ariaLabel="REST authentication"
          value={c.auth.kind}
          onChange={(k) => setAuth(blankRestAuth(k))}
          options={[
            { value: "none", label: "None" },
            { value: "bearer", label: "Bearer token" },
            { value: "oAuth2", label: "OAuth2 client credentials" },
          ]}
        />
        {c.auth.kind === "none" && <p className="text-[12px] leading-snug text-muted">The server accepts anonymous requests. Fine for a local catalog; hosted ones almost always want a token.</p>}
        {c.auth.kind === "bearer" && (
          <SecretField
            label="Bearer token"
            edit={draft.icebergSecret}
            onChange={setIcebergSecret}
            hasStored={hasStored}
            hint="Sent as Authorization: Bearer on every request. Saved to the OS keychain, never shown again."
          />
        )}
        {o && (
          <div className="space-y-3">
            <p className="text-[12px] leading-snug text-muted">VibeLens exchanges the client ID and secret for a short-lived token and refreshes it when it expires.</p>
            <div className="grid gap-3 sm:grid-cols-2">
              <FormField label="Client ID" htmlFor={`${uid}-cid`} error={err("ice.catalog.clientId")}>
                <Input id={`${uid}-cid`} value={o.clientId} spellCheck={false} autoComplete="off" className={mono(!!err("ice.catalog.clientId"))} onChange={(e) => setAuth({ ...o, clientId: e.target.value })} />
              </FormField>
              <FormField label="Scope" htmlFor={`${uid}-scope`} optional>
                <Input id={`${uid}-scope`} value={o.scope ?? ""} spellCheck={false} autoComplete="off" placeholder="PRINCIPAL_ROLE:ALL" className="font-mono" onChange={(e) => setAuth({ ...o, scope: e.target.value || null })} />
              </FormField>
            </div>
            <SecretField label="Client secret" edit={draft.icebergSecret} onChange={setIcebergSecret} hasStored={hasStored} />
            <FormField label="Token server URI" htmlFor={`${uid}-ts`} optional error={err("ice.catalog.serverUri")} hint="Defaults to the catalog's own /v1/oauth/tokens endpoint.">
              <Input id={`${uid}-ts`} value={o.serverUri ?? ""} spellCheck={false} autoComplete="off" placeholder="https://login.example.com/oauth/token" className={mono(!!err("ice.catalog.serverUri"))} onChange={(e) => setAuth({ ...o, serverUri: e.target.value || null })} />
            </FormField>
          </div>
        )}
        {err("ice.catalog.secret") && (
          <span role="alert" className="block text-[11px] text-danger">
            {err("ice.catalog.secret")}
          </span>
        )}
      </Group>

      <Group title="Extra headers" description="Sent with every catalog request.">
        <HeaderEditor
          rows={rows}
          error={err("ice.catalog.headers")}
          onChange={(next) => {
            setRows(next);
            setCatalog({ ...c, headers: headersFromRows(next) });
          }}
        />
      </Group>
    </>
  );
}

/* ------------------------------------------------------------------ Glue */

function GlueCatalog({ catalog: c, setCatalog, uid, err }: Sub<"glue">) {
  return (
    <>
      <Group title="AWS Glue Data Catalog" description="Tables registered in Glue. VibeLens signs requests with the identity below.">
        <AwsIdentityPicker idPrefix={`${uid}-aws`} target={c.aws} err={(f) => err(`ice.catalog.${f}`)} onChange={(aws) => setCatalog({ ...c, aws })} regionHint="Region of the Glue catalog." />
        <FormField label="Catalog ID" htmlFor={`${uid}-cid`} optional error={err("ice.catalog.catalogId")} hint="The AWS account that owns the catalog. Leave empty for the signed-in account; set it for cross-account access.">
          <Input id={`${uid}-cid`} value={c.catalogId ?? ""} inputMode="numeric" spellCheck={false} autoComplete="off" placeholder="123456789012" className={cn("max-w-56", mono(!!err("ice.catalog.catalogId")))} onChange={(e) => setCatalog({ ...c, catalogId: e.target.value || null })} />
        </FormField>
        <FormField label="Warehouse" htmlFor={`${uid}-wh`} error={err("ice.catalog.warehouse")} hint="Default S3 location for new tables. Existing tables keep their own location.">
          <Input id={`${uid}-wh`} value={c.warehouse ?? ""} spellCheck={false} autoComplete="off" placeholder="s3://my-lake/warehouse" className={mono(!!err("ice.catalog.warehouse"))} onChange={(e) => setCatalog({ ...c, warehouse: e.target.value || null })} />
        </FormField>
      </Group>
    </>
  );
}

/* ------------------------------------------------------------------ S3 Tables */

function S3TablesCatalog({ catalog: c, setCatalog, uid, err }: Sub<"s3Tables">) {
  return (
    <Group title="Amazon S3 Tables" description="A table bucket is its own Iceberg catalog; namespaces and tables live inside it.">
      <FormField label="Table bucket ARN" htmlFor={`${uid}-arn`} error={err("ice.catalog.tableBucketArn")} hint="arn:aws:s3tables:<region>:<account>:bucket/<name>. The region below is filled in from it.">
        <Input
          id={`${uid}-arn`}
          data-autofocus
          value={c.tableBucketArn}
          spellCheck={false}
          autoComplete="off"
          placeholder="arn:aws:s3tables:eu-west-1:123456789012:bucket/analytics"
          className={mono(!!err("ice.catalog.tableBucketArn"))}
          onChange={(e) => {
            const tableBucketArn = e.target.value;
            setCatalog({ ...c, tableBucketArn, aws: { ...c.aws, region: regionAfterArnEdit(c.aws.region, c.tableBucketArn, tableBucketArn) } });
          }}
        />
      </FormField>
      <AwsIdentityPicker idPrefix={`${uid}-aws`} target={c.aws} err={(f) => err(`ice.catalog.${f}`)} onChange={(aws) => setCatalog({ ...c, aws })} regionHint="Prefilled from the ARN." />
    </Group>
  );
}

/* ------------------------------------------------------------------ SQL */

function SqlCatalog({ catalog: c, setCatalog, uid, err, draft, setIcebergSecret, original, editing }: Sub<"sql">) {
  const flavour = sqlFlavour(c.uri);
  const secrets = useSecretStatus(editing ? original?.id : null);
  const hasStored = editing && !!original?.iceberg && secretKind(original.iceberg.catalog) === secretKind(c) && (secrets.data?.hasIcebergSecret ?? false);
  const pick = (f: "sqlite" | "postgres") => {
    if (f === flavour) return;
    setCatalog({ ...c, uri: f === "sqlite" ? "sqlite:" : "postgres://" });
  };
  return (
    <Group title="Catalog database" description="Iceberg's JDBC-style catalog keeps table pointers in a SQL database.">
      <SegmentedControl<"sqlite" | "postgres">
        ariaLabel="Database type"
        value={flavour === "postgres" ? "postgres" : "sqlite"}
        onChange={pick}
        options={[
          { value: "sqlite", label: "SQLite file" },
          { value: "postgres", label: "PostgreSQL" },
        ]}
      />
      {flavour !== "postgres" ? (
        <FormField label="SQLite file" htmlFor={`${uid}-uri`} error={err("ice.catalog.uri")} hint={<>Stored as <span className="font-mono">{sqliteUriFromPath(sqlitePathFromUri(c.uri)) || "sqlite:"}</span>. Created if it doesn&rsquo;t exist.</>}>
          <Input id={`${uid}-uri`} data-autofocus value={sqlitePathFromUri(c.uri)} spellCheck={false} autoComplete="off" placeholder="/data/iceberg/catalog.db" className={mono(!!err("ice.catalog.uri"))} onChange={(e) => setCatalog({ ...c, uri: sqliteUriFromPath(e.target.value) })} />
        </FormField>
      ) : (
        <>
          <FormField label="PostgreSQL URL" htmlFor={`${uid}-uri`} error={err("ice.catalog.uri")} hint="postgres://user@host:5432/database">
            <Input id={`${uid}-uri`} data-autofocus value={c.uri} spellCheck={false} autoComplete="off" placeholder="postgres://iceberg@db.internal:5432/catalog" className={mono(!!err("ice.catalog.uri"))} onChange={(e) => setCatalog({ ...c, uri: e.target.value })} />
          </FormField>
          {urlHasInlinePassword(c.uri) && <Notice tone="warn">The URL contains a password, which would be saved as plain text in your connection file. Remove it and enter it below so it goes to the keychain.</Notice>}
          <SecretField label="Database password" edit={draft.icebergSecret} onChange={setIcebergSecret} hasStored={hasStored} optional />
        </>
      )}
      <div className="grid gap-3 sm:grid-cols-2">
        <FormField label="Catalog name" htmlFor={`${uid}-name`} error={err("ice.catalog.catalogName")} hint="Part of every row key in the catalog database.">
          <Input id={`${uid}-name`} value={c.catalogName} spellCheck={false} autoComplete="off" className={mono(!!err("ice.catalog.catalogName"))} onChange={(e) => setCatalog({ ...c, catalogName: e.target.value })} />
        </FormField>
      </div>
      <FormField label="Warehouse" htmlFor={`${uid}-wh`} error={err("ice.catalog.warehouse")} hint="Where table data and metadata live: an s3:// path or a local folder.">
        <Input id={`${uid}-wh`} value={c.warehouse} spellCheck={false} autoComplete="off" placeholder="s3://my-lake/warehouse  or  /data/iceberg/warehouse" className={mono(!!err("ice.catalog.warehouse"))} onChange={(e) => setCatalog({ ...c, warehouse: e.target.value })} />
      </FormField>
    </Group>
  );
}

/* ------------------------------------------------------------------ HMS */

function HmsCatalog({ catalog: c, setCatalog, uid, err }: Sub<"hms">) {
  return (
    <Group title="Hive Metastore" description="Thrift connection to an existing metastore.">
      <Notice tone="info">Requires a VibeLens build with the iceberg-hms feature. Kerberos isn&rsquo;t supported.</Notice>
      <FormField label="Thrift address" htmlFor={`${uid}-addr`} error={err("ice.catalog.address")} hint="host:port, usually 9083.">
        <Input id={`${uid}-addr`} data-autofocus value={c.address} spellCheck={false} autoComplete="off" placeholder="metastore.internal:9083" className={mono(!!err("ice.catalog.address"))} onChange={(e) => setCatalog({ ...c, address: e.target.value })} />
      </FormField>
      <FormField label="Warehouse" htmlFor={`${uid}-wh`} error={err("ice.catalog.warehouse")} hint="Default location for new tables.">
        <Input id={`${uid}-wh`} value={c.warehouse} spellCheck={false} autoComplete="off" placeholder="s3://my-lake/warehouse" className={mono(!!err("ice.catalog.warehouse"))} onChange={(e) => setCatalog({ ...c, warehouse: e.target.value })} />
      </FormField>
    </Group>
  );
}
