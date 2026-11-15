// Mirror of crates/vibelens-core/src/model/*.rs (serde camelCase). Change both together.

export type ProfileId = string;
export type EngineKind = "postgres" | "valkey" | "iceberg";
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
  knownHostFingerprint: string | null;
};

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

export type IcebergQueryEngine =
  | { kind: "local" }
  | { kind: "athena"; workgroup: string; outputLocation: string | null; aws: AwsTarget | null };

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
}

export interface TestResult {
  ok: boolean;
  serverVersion: string | null;
  latencyMs: number;
  message: string;
}

// ---------- errors ----------
export interface ErrorPayload {
  code: string; // see CoreError::code(); plus "confirm_required" from the prod write-guard
  message: string;
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

/** int8/numeric/money arrive as strings; bytea as {$bytes, len}; json as embedded JSON. */
export type Cell = null | boolean | number | string | { $bytes: string; len: number } | unknown[] | Record<string, unknown>;

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
export type AgentEvent =
  | { type: "turnStarted"; turnId: string; providerId: string; model: string }
  | { type: "textDelta"; text: string }
  | { type: "toolCallProposed"; callId: string; tool: string; arguments: unknown; risk: ToolRisk; reason: string | null }
  | { type: "approvalRequired"; callId: string; tool: string; arguments: unknown; reason: string; preview: string | null }
  | { type: "toolStarted"; callId: string }
  | { type: "toolFinished"; callId: string; ok: boolean; summary: string; elapsedMs: number }
  | { type: "fileWritten"; path: string }
  | { type: "turnFinished"; stopReason: string; usage: Usage | null }
  | { type: "failed"; error: ErrorPayload };

export interface AppInfo {
  version: string;
  configDir: string;
  workspaceDir: string;
  vaultBackend: "keychain" | "memory";
}
