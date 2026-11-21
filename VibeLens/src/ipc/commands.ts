// Typed wrappers over every Tauri command in src-tauri/src/lib.rs.
// Components MUST go through these (or the hooks built on them), never raw `invoke`.
import { Channel, invoke } from "@tauri-apps/api/core";
import type * as T from "./types";

/** Narrow an unknown thrown value into an ErrorPayload. */
export function asError(e: unknown): T.ErrorPayload {
  if (e && typeof e === "object" && "code" in e && "message" in e) return e as T.ErrorPayload;
  return { code: "other", message: e instanceof Error ? e.message : String(e) };
}

export const api = {
  // app
  appInfo: () => invoke<T.AppInfo>("app_info"),
  sqlClassify: (sql: string) => invoke<T.SqlClassification>("sql_classify", { sql }),
  /** Number of tabs with unsaved changes; Rust guards window close / quit while it is > 0. */
  appSetUnsaved: (count: number) => invoke<void>("app_set_unsaved", { count }),

  // profiles
  listProfiles: () => invoke<T.ConnectionProfile[]>("list_profiles"),
  /** password / sshSecret: undefined = keep, "" = delete, value = store in OS keychain */
  /** icebergSecret: REST bearer token / OAuth2 client secret / SQL-catalog DB password.
   *  s3Keys: static S3 keys for storage kind "s3Static".
   *  engineSecret: Trino / Spark (Livy) password or bearer token. Same keep/delete semantics as password. */
  saveProfile: (
    profile: T.ConnectionProfile,
    password?: string,
    sshSecret?: string,
    icebergSecret?: string,
    s3Keys?: { accessKeyId: string; secretAccessKey: string },
    forgetHostKey?: boolean,
    engineSecret?: string,
  ) =>
    invoke<T.ConnectionProfile>("save_profile", {
      forgetHostKey,
      profile,
      password: password ?? null,
      sshSecret: sshSecret ?? null,
      icebergSecret: icebergSecret ?? null,
      s3AccessKeyId: s3Keys?.accessKeyId ?? null,
      s3SecretAccessKey: s3Keys?.secretAccessKey ?? null,
      engineSecret: engineSecret ?? null,
    }),
  deleteProfile: (id: string) => invoke<void>("delete_profile", { id }),
  /** Never stores anything. An unpinned SSH host comes back as `ok: false` with `hostKey` set. */
  /** `engineSecret`: a Trino / Spark password or token typed in the form; without it the stored one is used. */
  testConnection: (profile: T.ConnectionProfile, password?: string, engineSecret?: string) =>
    invoke<T.TestResult>("test_connection", { profile, password: password ?? null, engineSecret: engineSecret ?? null }),
  /**
   * Pins the host key an SSH server presented, on the saved profile. Call it only from the user's
   * "trust" action. Rust refuses when the profile's tunnel no longer points at that host and port,
   * or already has a different pin.
   */
  trustHostKey: (profileId: string, key: T.PresentedHostKey) =>
    invoke<T.ConnectionProfile>("trust_host_key", { profileId, host: key.host, port: key.port, fingerprint: key.fingerprint }),

  // import from other tools (no secrets are ever imported)
  importScan: () => invoke<T.ImportReport[]>("import_scan"),
  /** Native open dialog; null when cancelled. */
  importChooseFile: () => invoke<T.ImportReport | null>("import_choose_file"),
  importText: (text: string) => invoke<T.ImportReport>("import_text", { text }),
  importSave: (selections: { profile: T.ConnectionProfile; source: T.ImportSource | null }[]) =>
    invoke<T.ImportSaveResult>("import_save", { selections }),

  /** Which keychain entries exist for a profile (never the secrets). */
  profileSecretStatus: (id: string) => invoke<T.SecretStatus>("profile_secret_status", { id }),

  // connections
  connect: (profileId: string) => invoke<T.ConnectionStatus>("connect", { profileId }),
  disconnect: (profileId: string) => invoke<void>("disconnect", { profileId }),
  connectionStatus: () => invoke<T.ConnectionStatus[]>("connection_status"),

  // postgres
  pgListDatabases: (profileId: string) => invoke<T.DatabaseInfo[]>("pg_list_databases", { profileId }),
  pgListSchemas: (profileId: string, database: string | null) =>
    invoke<T.SchemaInfo[]>("pg_list_schemas", { profileId, database }),
  pgListObjects: (profileId: string, database: string | null, schema: string) =>
    invoke<T.SchemaObject[]>("pg_list_objects", { profileId, database, schema }),
  pgListPartitions: (profileId: string, database: string | null, schema: string, table: string) =>
    invoke<T.PartitionListing>("pg_list_partitions", { profileId, database, schema, table }),
  pgDescribeTable: (profileId: string, database: string | null, schema: string, table: string) =>
    invoke<T.TableDescription>("pg_describe_table", { profileId, database, schema, table }),
  /**
   * Streams QueryEvents to `onEvent`. Resolves with the queryId as soon as execution starts.
   * Rejects with code "confirm_required" on prod writes unless `confirmed` is true.
   */
  pgExecute: (args: {
    profileId: string;
    database: string | null;
    sql: string;
    options?: Partial<T.ExecOptions>;
    confirmed?: boolean;
    onEvent: (ev: T.QueryEvent) => void;
  }) => {
    const ch = new Channel<T.QueryEvent>();
    ch.onmessage = args.onEvent;
    const options: T.ExecOptions = { readOnly: false, maxRows: 100_000, pageSize: 500, timeoutMs: null, ...args.options };
    return invoke<string>("pg_execute", {
      profileId: args.profileId,
      database: args.database,
      sql: args.sql,
      options,
      confirmed: args.confirmed ?? false,
      onEvent: ch,
    });
  },
  pgCancel: (queryId: string) => invoke<void>("pg_cancel", { queryId }),
  /** Tables + columns + indexes + FKs for a database (optionally one schema). Cache it: see lib/schemaCache.ts. */
  pgSchemaMetadata: (profileId: string, database: string | null, schema?: string | null, maxTables?: number) =>
    invoke<T.SchemaMetadata>("pg_schema_metadata", { profileId, database, schema: schema ?? null, maxTables: maxTables ?? null }),

  // iceberg
  iceListNamespaces: (profileId: string, parent: string[] | null = null) =>
    invoke<T.IcebergNamespace[]>("ice_list_namespaces", { profileId, parent }),
  iceListTables: (profileId: string, namespace: string[]) => invoke<T.IcebergTableRef[]>("ice_list_tables", { profileId, namespace }),
  iceDescribeTable: (profileId: string, namespace: string[], name: string) =>
    invoke<T.IcebergTableInfo>("ice_describe_table", { profileId, namespace, name }),
  /** Catalog shaped like Postgres SchemaMetadata (schema = namespace joined with "."), for autocomplete. */
  iceSchemaMetadata: (profileId: string, namespace: string[] | null = null, maxTables?: number) =>
    invoke<T.SchemaMetadata>("ice_schema_metadata", { profileId, namespace, maxTables: maxTables ?? null }),
  /** Same streaming contract as pgExecute; cancel with pgCancel(queryId). */
  iceExecute: (args: {
    profileId: string;
    sql: string;
    options?: Partial<T.ExecOptions>;
    confirmed?: boolean;
    onEvent: (ev: T.QueryEvent) => void;
  }) => {
    const ch = new Channel<T.QueryEvent>();
    ch.onmessage = args.onEvent;
    const options: T.ExecOptions = { readOnly: false, maxRows: 100_000, pageSize: 500, timeoutMs: null, ...args.options };
    return invoke<string>("ice_execute", { profileId: args.profileId, sql: args.sql, options, confirmed: args.confirmed ?? false, onEvent: ch });
  },
  /** Always rejects with code "confirm_required" unless confirmed: it permanently removes time-travel history. */
  iceExpireSnapshots: (profileId: string, namespace: string[], name: string, opts: { olderThanMs?: number; retainLast?: number; confirmed?: boolean }) =>
    invoke<T.ExpireSnapshotsResult>("ice_expire_snapshots", {
      profileId,
      namespace,
      name,
      olderThanMs: opts.olderThanMs ?? null,
      retainLast: opts.retainLast ?? null,
      confirmed: opts.confirmed ?? false,
    }),

  // redshift (Data API). `database` null = the profile's database.
  rsListDatabases: (profileId: string) => invoke<string[]>("rs_list_databases", { profileId }),
  rsListSchemas: (profileId: string, database: string | null) => invoke<string[]>("rs_list_schemas", { profileId, database }),
  rsListTables: (profileId: string, database: string | null, schema: string) =>
    invoke<T.RedshiftTableRef[]>("rs_list_tables", { profileId, database, schema }),
  rsDescribeTable: (profileId: string, database: string | null, schema: string, name: string) =>
    invoke<T.RedshiftColumn[]>("rs_describe_table", { profileId, database, schema, name }),
  /** Catalog in the Postgres SchemaMetadata shape, for autocomplete. */
  rsSchemaMetadata: (profileId: string, database: string | null, maxTables?: number) =>
    invoke<T.SchemaMetadata>("rs_schema_metadata", { profileId, database, maxTables: maxTables ?? null }),
  /** Same streaming contract as iceExecute; cancel with pgCancel(queryId). */
  rsExecute: (args: {
    profileId: string;
    database: string | null;
    sql: string;
    options?: Partial<T.ExecOptions>;
    confirmed?: boolean;
    onEvent: (ev: T.QueryEvent) => void;
  }) => {
    const ch = new Channel<T.QueryEvent>();
    ch.onmessage = args.onEvent;
    const options: T.ExecOptions = { readOnly: false, maxRows: 100_000, pageSize: 500, timeoutMs: null, ...args.options };
    return invoke<string>("rs_execute", { profileId: args.profileId, sql: args.sql, database: args.database, options, confirmed: args.confirmed ?? false, onEvent: ch });
  },

  // files
  /** Native Save dialog, then writes the file. Resolves to the saved path, or null if cancelled. */
  exportFile: (suggestedName: string, contents: string) => invoke<string | null>("export_file", { suggestedName, contents }),

  // valkey
  vkServerInfo: (profileId: string) => invoke<T.ValkeyServerInfo>("vk_server_info", { profileId }),
  vkListDbs: (profileId: string) => invoke<T.ValkeyDbInfo[]>("vk_list_dbs", { profileId }),
  vkScan: (profileId: string, db: number, pattern = "*", cursor = "0", count?: number) =>
    invoke<T.ScanPage>("vk_scan", { profileId, db, pattern, cursor, count: count ?? null }),
  vkGetValue: (profileId: string, db: number, key: string, maxItems?: number) =>
    invoke<T.ValkeyValue>("vk_get_value", { profileId, db, key, maxItems: maxItems ?? null }),
  vkCommand: (profileId: string, db: number, argv: string[], allowWrite = false, confirmed = false) =>
    invoke<T.CommandOutcome>("vk_command", { profileId, db, argv, allowWrite, confirmed }),

  // aws
  awsListProfiles: () => invoke<T.AwsProfileSummary[]>("aws_list_profiles"),
  awsSsoStart: (startUrl: string, region: string) => invoke<T.DeviceLoginStart>("aws_sso_start", { startUrl, region }),
  awsSsoComplete: (loginId: string) => invoke<T.SsoSessionInfo>("aws_sso_complete", { loginId }),
  awsSsoSessions: () => invoke<T.SsoSessionInfo[]>("aws_sso_sessions"),
  awsSsoListAccounts: (startUrl: string) => invoke<T.SsoAccount[]>("aws_sso_list_accounts", { startUrl }),
  awsSsoListRoles: (startUrl: string, accountId: string) =>
    invoke<T.SsoRole[]>("aws_sso_list_roles", { startUrl, accountId }),

  // agent
  agentListProviders: () => invoke<T.ProviderSummary[]>("agent_list_providers"),
  /** apiKey: undefined = keep, "" = delete */
  agentSaveProvider: (config: T.ProviderConfig, apiKey?: string) =>
    invoke<void>("agent_save_provider", { config, apiKey: apiKey ?? null }),
  agentDeleteProvider: (id: string) => invoke<void>("agent_delete_provider", { id }),
  agentStartTurn: (args: {
    sessionId: string;
    providerId: string;
    prompt: string;
    context: T.AgentContext;
    onEvent: (ev: T.AgentEvent) => void;
  }) => {
    const ch = new Channel<T.AgentEvent>();
    ch.onmessage = args.onEvent;
    return invoke<string>("agent_start_turn", {
      sessionId: args.sessionId,
      providerId: args.providerId,
      prompt: args.prompt,
      context: args.context,
      onEvent: ch,
    });
  },
  agentResolveApproval: (sessionId: string, callId: string, decision: T.ApprovalDecision) =>
    invoke<void>("agent_resolve_approval", { sessionId, callId, decision }),
  agentCancel: (sessionId: string) => invoke<void>("agent_cancel", { sessionId }),
  agentReset: (sessionId: string) => invoke<void>("agent_reset", { sessionId }),

  // workspace
  wsList: () => invoke<T.WorkspaceFile[]>("ws_list"),
  wsRead: (path: string) => invoke<string>("ws_read", { path }),
  wsWrite: (path: string, contents: string) => invoke<void>("ws_write", { path, contents }),
  /** Native file picker, then copies the chosen files into the workspace. null = cancelled. Rust picks and checks every path. */
  wsImportFiles: () => invoke<T.WorkspaceImportOutcome | null>("ws_import_files"),
  /** Shows the workspace folder in the system file manager. */
  wsReveal: () => invoke<void>("ws_reveal"),
};

export type Api = typeof api;
