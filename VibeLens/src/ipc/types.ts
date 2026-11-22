// Mirror of crates/vibelens-core/src/model/*.rs (serde camelCase). Change both together.

export type ProfileId = string;
export type EngineKind = "postgres" | "valkey" | "iceberg" | "redshift" | "mysql";
export type Environment = "dev" | "test" | "staging" | "prod";
export type AiDataAccess = "none" | "schemaOnly" | "resultsWithApproval" | "results";
export type TlsMode = "disable" | "require" | "verifyFull";

export interface TlsConfig {
  mode: TlsMode;
  caCertPath: string | null;
  useRdsBundle: boolean;
}

export type AwsIdentity =
  | { kind: "profile"; profileName: string }
  | { kind: "sso"; startUrl: string; ssoRegion: string; accountId: string; roleName: string }
  | { kind: "defaultChain" };

export interface AwsTarget {
  identity: AwsIdentity;
  region: string;
}

export type AuthMethod =
  | { kind: "none" }
  | { kind: "password"; username: string }
  | { kind: "rdsIam"; username: string; aws: AwsTarget }
  | { kind: "secretsManager"; secretArn: string; aws: AwsTarget }
  /** Bearer token sent as the password: capitec/pg-proxy JWT/OIDC, Azure Entra. No command = pasted token in the keychain. */
  | { kind: "accessToken"; username: string; command: string | null }
  | { kind: "elastiCacheIam"; username: string; cacheName: string; serverless: boolean; aws: AwsTarget };

export type SshAuth = { kind: "agent" } | { kind: "privateKey"; path: string } | { kind: "password" };

export type TunnelConfig = {
  kind: "ssh";
  host: string;
  port: number;
  username: string;
  auth: SshAuth;
  /** The pinned host key (`SHA256:<base64 without padding>`). Only this key is connected to; null until the user trusts or pastes one. */
  knownHostFingerprint: string | null;
};

/** The host key an SSH server presented when none was pinned for it. Nothing was sent to that server. */
export interface PresentedHostKey {
  host: string;
  port: number;
  /** `SHA256:<base64 without padding>`, what `ssh-keygen -lf` prints. */
  fingerprint: string;
}

export type PgProxyMode = "blocking" | "nonBlocking" | "shunt";

/** The endpoint is a capitec/pg-proxy (PGProxy). */
export interface PgProxyOptions {
  mode: PgProxyMode;
  /** Health listener base URL, e.g. http://pgproxy:8080 — Test checks GET /ready. */
  healthUrl: string | null;
}

export interface PgOptions {
  connectTimeoutSecs: number;
  statementTimeoutMs: number | null;
  /** Idle seconds before pooled server sessions are released (the connection stays open). 0 = never. */
  idleDisconnectSecs: number;
  maxPoolSize: number;
  applicationName: string;
  pgproxy?: PgProxyOptions | null;
}

export interface ValkeyOptions {
  cluster: boolean;
  keyDelimiter: string;
  scanCount: number;
  connectTimeoutSecs: number;
  idleDisconnectSecs: number;
}

export interface ConnectionProfile {
  id: ProfileId; // "" when creating; backend assigns
  name: string;
  engine: EngineKind;
  host: string;
  port: number;
  database: string | null;
  auth: AuthMethod;
  tls: TlsConfig;
  tunnel: TunnelConfig | null;
  environment: Environment;
  aiDataAccess: AiDataAccess;
  color: string | null;
  pg: PgOptions;
  valkey: ValkeyOptions;
  createdAt: string | null;
  updatedAt: string | null;
  /** Required when engine === "iceberg"; host/port are unused then. */
  iceberg?: IcebergOptions | null;
  /** Required when engine === "redshift"; host/port/auth/tls/tunnel are unused then. */
  redshift?: RedshiftOptions | null;
  /** MySQL settings (Rust always has them; `#[serde(default)]`). Never send null: omit it instead. */
  mysql?: MysqlOptions;
}

/** MySQL connection settings (crates/vibelens-core/src/model/mysql.rs). A MySQL profile also uses
 *  host, port, database (required), auth (password | rdsIam), tls and tunnel. */
export interface MysqlOptions {
  /** 1 to 600 seconds. */
  connectTimeoutSecs: number;
  /** Per-statement limit; on expiry the statement is stopped with KILL QUERY. null = none. */
  statementTimeoutMs: number | null;
  /** 0 = never. */
  idleDisconnectSecs: number;
  /** 1 to 32. */
  maxPoolSize: number;
}

// ---------- import ----------
export type ImportSource = "dataGrip" | "dBeaver" | "azureDataStudio" | "pgAdmin" | "pgService";

export interface ImportCandidate {
  source: ImportSource;
  sourceFile: string;
  group: string | null;
  profile: ConnectionProfile;
  warnings: string[];
  needsSecret: boolean;
  duplicateOf: ProfileId | null;
}

export interface SkippedEntry {
  name: string;
  reason: string;
}

export interface ImportReport {
  source: ImportSource;
  path: string;
  candidates: ImportCandidate[];
  skipped: SkippedEntry[];
  error: string | null;
}

export interface ImportSaveResult {
  saved: ConnectionProfile[];
  failed: { name: string; error: string }[];
}

// ---------- iceberg ----------
export type RestAuth =
  | { kind: "none" }
  | { kind: "bearer" } // token in keychain: saveProfile(profile, icebergSecret)
  | { kind: "oAuth2"; clientId: string; scope: string | null; serverUri: string | null }; // secret in keychain

export type IcebergCatalog =
  | { kind: "rest"; uri: string; warehouse: string | null; auth: RestAuth; headers: Record<string, string> }
  | { kind: "glue"; aws: AwsTarget; catalogId: string | null; warehouse: string | null }
  | { kind: "s3Tables"; aws: AwsTarget; tableBucketArn: string }
  | { kind: "sql"; uri: string; warehouse: string; catalogName: string }
  | { kind: "hms"; address: string; warehouse: string };

export type IcebergStorage =
  | { kind: "auto" }
  | { kind: "s3"; aws: AwsTarget | null; endpoint: string | null; region: string | null; pathStyle: boolean }
  | { kind: "s3Static"; endpoint: string | null; region: string | null; pathStyle: boolean } // keys in keychain
  | { kind: "local" };

/** Authentication of the Trino / Livy HTTP engines. The password or token is in the vault (`engineSecret`). */
export type HttpEngineAuth = { kind: "none" } | { kind: "basic"; username: string } | { kind: "bearer" };

export type IcebergQueryEngine =
  | { kind: "local" }
  | { kind: "athena"; workgroup: string; outputLocation: string | null; aws: AwsTarget | null }
  | { kind: "trino"; url: string; catalog: string; user: string; auth: HttpEngineAuth; caCertPath: string | null }
  | {
      kind: "spark";
      url: string;
      catalog: string;
      auth: HttpEngineAuth;
      caCertPath: string | null;
      proxyUser: string | null;
      sessionConf: Record<string, string>;
      sessionStartTimeoutSecs: number | null;
    };

export interface IcebergOptions {
  catalog: IcebergCatalog;
  storage: IcebergStorage;
  queryEngine: IcebergQueryEngine;
  defaultNamespace: string | null;
}

export interface IcebergNamespace {
  levels: string[];
  properties: Record<string, string>;
}
export type IcebergObjectKind = "table" | "view";
export interface IcebergTableRef {
  namespace: string[];
  name: string;
  kind: IcebergObjectKind;
}
export interface IcebergField {
  id: number;
  name: string;
  typeName: string;
  required: boolean;
  doc: string | null;
  children: IcebergField[];
}
export interface IcebergPartitionField {
  sourceId: number;
  fieldId: number;
  name: string;
  transform: string;
}
export interface IcebergSortField {
  sourceId: number;
  transform: string;
  direction: string;
  nullOrder: string;
}
/** snapshotId / parentSnapshotId / sequenceNumber are i64 → strings. */
export interface IcebergSnapshot {
  snapshotId: string;
  parentSnapshotId: string | null;
  sequenceNumber: string | null;
  timestampMs: number;
  operation: string | null;
  summary: Record<string, string>;
  manifestList: string | null;
  schemaId: number | null;
}
export interface IcebergRef {
  name: string;
  snapshotId: string;
  kind: string;
}
export interface IcebergTableInfo {
  namespace: string[];
  name: string;
  kind: IcebergObjectKind;
  location: string;
  metadataLocation: string | null;
  formatVersion: number;
  tableUuid: string | null;
  lastUpdatedMs: number | null;
  currentSnapshotId: string | null;
  currentSchemaId: number;
  fields: IcebergField[];
  schemaCount: number;
  partitionSpec: IcebergPartitionField[];
  sortOrder: IcebergSortField[];
  properties: Record<string, string>;
  snapshots: IcebergSnapshot[]; // newest first
  refs: IcebergRef[];
}
export interface ExpireSnapshotsResult {
  expired: number;
  remaining: number;
}

// ---------- redshift ----------
export type RedshiftTarget = { kind: "provisioned"; clusterIdentifier: string } | { kind: "serverless"; workgroupName: string };
export type RedshiftAuth = { kind: "iamIdentity" } | { kind: "dbUser"; dbUser: string } | { kind: "secret"; secretArn: string };
export interface RedshiftOptions {
  target: RedshiftTarget;
  database: string;
  aws: AwsTarget;
  auth: RedshiftAuth;
}
export interface RedshiftTableRef {
  schema: string;
  name: string;
  /** TABLE, VIEW, ... as Redshift reports it. */
  kind: string;
}
export interface RedshiftColumn {
  name: string;
  typeName: string;
  nullable: boolean;
  length: number | null;
  precision: number | null;
  scale: number | null;
  defaultValue: string | null;
}

export type ConnectionState = "disconnected" | "connecting" | "connected" | "error";

export interface ConnectionStatus {
  profileId: ProfileId;
  engine: EngineKind;
  state: ConnectionState;
  since: string | null;
  serverVersion: string | null;
  detail: string | null;
  tunneled: boolean;
}

export interface SecretStatus {
  hasPassword: boolean;
  hasSshPassword: boolean;
  hasSshPassphrase: boolean;
  hasIcebergSecret: boolean;
  hasS3Keys: boolean;
  /** Trino / Spark password or token (`profile:{id}:engine-secret`). Optional: not in the design contract,
   *  so an older or partial backend may not send it (then the wizard assumes nothing is stored). */
  hasEngineSecret?: boolean;
}

export interface TestResult {
  ok: boolean;
  serverVersion: string | null;
  latencyMs: number;
  message: string;
  /** Set when the test stopped because the SSH host's key is not pinned. A test never stores it. */
  hostKey: PresentedHostKey | null;
}

// ---------- errors ----------
export interface ErrorPayload {
  code: string; // see CoreError::code(); plus "confirm_required" from the prod write-guard
  message: string;
  /** With code "unknown_host_key": the key the server presented, for the trust dialog. */
  hostKey?: PresentedHostKey | null;
}

// ---------- query ----------
export interface ExecOptions {
  readOnly: boolean;
  maxRows: number;
  pageSize: number;
  timeoutMs: number | null;
}

export interface ColumnMeta {
  name: string;
  typeName: string;
  typeOid: number;
}

/**
 * A value over 1 MiB (`MAX_CELL_BYTES` in `model/query.rs`) is not loaded in full: its cell is this
 * marker. `bytes` is the size of the whole value; `preview` is its start (text for `text` and `json`,
 * where the JSON is cut and so not parseable; base64 for `bytes`; empty for `other`). The marker is
 * not the value: never write it back, compare with it in SQL, or copy or export it as if complete.
 * Use `isTruncatedCell` / `containsTruncated` from `components/grid/cellFormat`.
 */
export interface TruncatedCell {
  $truncated: { kind: "text" | "bytes" | "json" | "other"; bytes: number; preview: string };
}

/** int8/numeric/money arrive as strings; bytea as {$bytes, len}; json as embedded JSON; an oversized value as a `TruncatedCell`. */
export type Cell = null | boolean | number | string | { $bytes: string; len: number } | TruncatedCell | unknown[] | Record<string, unknown>;

export type QueryEvent =
  | { type: "started"; queryId: string; statementCount: number }
  | { type: "columns"; statementIndex: number; columns: ColumnMeta[] }
  | { type: "rows"; statementIndex: number; rows: Cell[][] }
  | {
      type: "statementDone";
      statementIndex: number;
      commandTag: string | null;
      rowsAffected: number | null;
      rowCount: number;
      truncated: boolean;
      elapsedMs: number;
    }
  | {
      type: "stats";
      statementIndex: number;
      engine: string;
      bytesScanned: number | null;
      costUsdEstimate: number | null;
      engineQueryId: string | null;
    }
  | { type: "finished"; elapsedMs: number }
  | { type: "failed"; statementIndex: number | null; error: ErrorPayload };

// ---------- schema ----------
export interface DatabaseInfo {
  name: string;
  isTemplate: boolean;
  allowConnections: boolean;
  sizeBytes: number | null;
}
export interface SchemaInfo {
  name: string;
  owner: string | null;
  isSystem: boolean;
}
export type ObjectKind =
  | "table"
  | "partitionedTable"
  | "view"
  | "materializedView"
  | "foreignTable"
  | "sequence"
  | "function"
  | "procedure";
export interface SchemaObject {
  schema: string;
  name: string;
  kind: ObjectKind;
  comment: string | null;
  estimatedRows: number | null;
  signature: string | null;
  /** A partition of another table: still listed, but the tree nests it under its parent. */
  isPartition: boolean;
  parentSchema: string | null;
  parentName: string | null;
}
export interface PartitionInfo {
  schema: string;
  name: string;
  /** e.g. `FOR VALUES FROM ('2024-01-01') TO ('2024-02-01')`. */
  bound: string;
  isDefault: boolean;
  /** Itself partitioned: expands to its own partitions. */
  isPartitioned: boolean;
  estimatedRows: number | null;
  sizeBytes: number | null;
}
/** pg_partman `part_config` settings; every field can be absent (partman 4 vs 5, unset columns). */
export interface PartmanConfig {
  control: string | null;
  /** `1 day`, or a number for id-based sets. */
  partitionInterval: string | null;
  premake: number | null;
  retention: string | null;
  retentionKeepTable: boolean | null;
  /** `on` / `off`. */
  automaticMaintenance: string | null;
}
export interface PartitionListing {
  /** e.g. `RANGE (created_at)`. */
  partitionKey: string | null;
  /** Direct children ordered by bound, the default partition last. */
  partitions: PartitionInfo[];
  /** More partitions exist than were returned. */
  truncated: boolean;
  /** Null when pg_partman is absent, does not manage the table, or cannot be read. */
  partman: PartmanConfig | null;
}
export interface ColumnInfo {
  name: string;
  dataType: string;
  nullable: boolean;
  default: string | null;
  isPrimaryKey: boolean;
  ordinal: number;
  comment: string | null;
}
export interface IndexInfo {
  name: string;
  definition: string;
  isUnique: boolean;
  isPrimary: boolean;
}
export interface ForeignKeyInfo {
  name: string;
  columns: string[];
  referencedSchema: string;
  referencedTable: string;
  referencedColumns: string[];
}
export interface TableDescription {
  schema: string;
  name: string;
  kind: ObjectKind;
  comment: string | null;
  estimatedRows: number | null;
  columns: ColumnInfo[];
  indexes: IndexInfo[];
  foreignKeys: ForeignKeyInfo[];
}

/** Whole-schema catalog, capped at maxTables (truncated tells you so). */
export interface SchemaMetadata {
  database: string;
  serverVersion: string | null;
  tables: TableDescription[];
  truncated: boolean;
}

// ---------- sql classification ----------
export type StatementClass = "readOnly" | "session" | "write" | "ddl" | "unknown";
export interface SqlClassification {
  statements: StatementClass[];
  overall: StatementClass;
  parseOk: boolean;
  summary: string;
}

// ---------- valkey ----------
export interface ValkeyServerInfo {
  version: string | null;
  mode: string | null;
  role: string | null;
  usedMemoryHuman: string | null;
  extra: Record<string, string>;
}
export interface ValkeyDbInfo {
  index: number;
  keys: number | null;
}
export type KeyType = "string" | "hash" | "list" | "set" | "zset" | "stream" | "json" | "unknown";
export interface KeyEntry {
  key: string;
  keyType: KeyType | null;
}
export interface ScanPage {
  keys: KeyEntry[];
  cursor: string;
  done: boolean;
}
export interface ValkeyValue {
  key: string;
  keyType: KeyType;
  ttlMs: number;
  size: number | null;
  value: unknown;
  truncated: boolean;
}
export type CommandOutcome =
  | { status: "ok"; result: unknown; elapsedMs: number }
  | { status: "requiresConfirmation"; reason: string };

// ---------- aws ----------
export interface AwsProfileSummary {
  name: string;
  kind: "sso" | "static" | "assumeRole" | "process" | "other";
  region: string | null;
  ssoStartUrl: string | null;
  ssoAccountId: string | null;
  ssoRoleName: string | null;
}
export interface DeviceLoginStart {
  loginId: string;
  userCode: string;
  verificationUri: string;
  verificationUriComplete: string | null;
  expiresAt: string;
  intervalSecs: number;
}
export interface SsoSessionInfo {
  startUrl: string;
  region: string;
  expiresAt: string | null;
}
export interface SsoAccount {
  accountId: string;
  accountName: string | null;
  email: string | null;
}
export interface SsoRole {
  accountId: string;
  roleName: string;
}

// ---------- workspace ----------
export interface WorkspaceFile {
  path: string;
  size: number;
  modified: string | null;
}
/** One file `ws_import_files` left out. `name` is the file name only. */
export interface ImportSkip {
  name: string;
  reason: string;
}
export interface WorkspaceImportOutcome {
  /** Workspace-relative paths created, in the order picked. */
  imported: string[];
  skipped: ImportSkip[];
}

// ---------- agent ----------
export type ProviderKind = "anthropic" | "openAiCompatible" | "portkey";
export type PortkeyApiFormat = "chatCompletions" | "anthropicMessages";
export type PortkeyRouting =
  | { kind: "modelCatalog" }
  | { kind: "provider"; slug: string }
  | { kind: "virtualKey"; virtualKey: string }
  | { kind: "config"; config: string };
export interface PortkeyOptions {
  apiFormat: PortkeyApiFormat;
  routing: PortkeyRouting;
  /** `_user` in x-portkey-metadata; empty = "vibelens". */
  user: string | null;
}
export interface ProviderConfig {
  id: string;
  label: string;
  kind: ProviderKind;
  baseUrl: string;
  model: string;
  maxTokens: number;
  temperature: number | null;
  extraHeaders: Record<string, string>;
  requiresApiKey: boolean;
  /** Only for kind === "portkey". */
  portkey?: PortkeyOptions | null;
  /**
   * The model's context window in tokens (local models). The history sent each turn is cut to
   * fit after leaving room for the output and the system prompt. null = the agent's default.
   */
  contextWindow?: number | null;
}
export interface ProviderSummary {
  config: ProviderConfig;
  hasApiKey: boolean;
}
export interface OpenFile {
  path: string;
  contents: string;
}
export interface AgentContext {
  profileId: string | null;
  database: string | null;
  openFile: OpenFile | null;
  selectedText: string | null;
}
export type ToolRisk = "safe" | "needsApproval" | "denied";
export type ApprovalDecision = { decision: "approve" } | { decision: "reject"; reason: string | null };
export interface Usage {
  inputTokens: number;
  outputTokens: number;
}
/**
 * Where an agent call that needs approval will run. Rust builds it from the stored profile and
 * the same database resolution execution uses, never from text the model wrote.
 */
export interface ApprovalTarget {
  connection: string;
  engine: EngineKind;
  environment: Environment;
  /** Postgres: the database the statement runs against. */
  database: string | null;
  /** Postgres: the connection's default database (what `database` is when the model names none). */
  defaultDatabase: string | null;
  /** Valkey: the logical database number. */
  valkeyDb: number | null;
  /**
   * Where the statement really runs, resolved by Rust from the stored profile: Redshift
   * `Redshift · cluster · database · region · auth`; Trino / Spark engine, host:port, catalog, namespace,
   * acting user. Plain text: rendered as text, never as markup.
   */
  detail?: string | null;
}
/**
 * Where an executed run_sql call ran, resolved by Rust from the stored profile (never from text
 * the model wrote). No result rows.
 */
export interface SqlRun {
  profileId: string;
  /** Postgres / Redshift database it ran against; null for Iceberg. */
  database: string | null;
  /** The statement exactly as executed. */
  sql: string;
  /** True when it ran as a read-only call. */
  readOnly: boolean;
}
export type AgentEvent =
  | { type: "turnStarted"; turnId: string; providerId: string; model: string }
  | { type: "textDelta"; text: string }
  | { type: "toolCallProposed"; callId: string; tool: string; arguments: unknown; risk: ToolRisk; reason: string | null }
  | { type: "approvalRequired"; callId: string; tool: string; arguments: unknown; reason: string; preview: string | null; target?: ApprovalTarget | null }
  | { type: "toolStarted"; callId: string }
  | { type: "toolFinished"; callId: string; ok: boolean; summary: string; elapsedMs: number; sqlRun?: SqlRun | null }
  | { type: "fileWritten"; path: string }
  | { type: "turnFinished"; stopReason: string; usage: Usage | null }
  | { type: "failed"; error: ErrorPayload };

export interface AppInfo {
  version: string;
  configDir: string;
  workspaceDir: string;
  vaultBackend: "keychain" | "memory";
}
