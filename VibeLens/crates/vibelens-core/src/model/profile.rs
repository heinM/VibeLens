use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

pub type ProfileId = String;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum EngineKind {
    Postgres,
    Valkey,
    /// Apache Iceberg tables via a catalog (REST, Glue, S3 Tables, SQL, HMS). Options in `iceberg`.
    Iceberg,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum Environment {
    #[default]
    Dev,
    Test,
    Staging,
    Prod,
}

/// What the AI agent is allowed to *see* from this connection. Enforced in the
/// agent tool layer before anything is sent to an LLM provider.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum AiDataAccess {
    /// Agent tools refuse to touch this connection at all.
    None,
    /// Catalog metadata only (tables, columns, indexes, key names/types). No row data or values.
    #[default]
    SchemaOnly,
    /// Row data / values may be sent, but every such tool call needs explicit approval.
    ResultsWithApproval,
    /// Row data / values may be sent for read-only calls without approval.
    Results,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum TlsMode {
    Disable,
    /// Encrypt but do not verify the certificate. Insecure; shown with a warning in the UI.
    Require,
    /// Verify chain and hostname (default). Uses OS roots + optional CA file + optional RDS bundle.
    #[default]
    VerifyFull,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct TlsConfig {
    pub mode: TlsMode,
    /// Extra PEM CA bundle to trust (e.g. corporate CA).
    pub ca_cert_path: Option<String>,
    /// Also trust the bundled Amazon RDS global CA bundle (RDS CAs are not in OS trust stores).
    #[serde(default)]
    pub use_rds_bundle: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AwsIdentity {
    /// A named profile from ~/.aws/config (covers SSO profiles, assume-role chains, credential_process).
    Profile { profile_name: String },
    /// An Identity Center session VibeLens logged into itself.
    Sso {
        start_url: String,
        sso_region: String,
        account_id: String,
        role_name: String,
    },
    /// The SDK default chain (env vars, instance metadata, etc.).
    DefaultChain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AwsTarget {
    pub identity: AwsIdentity,
    /// Region of the *resource* (RDS instance, secret, cache), not necessarily the SSO region.
    pub region: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AuthMethod {
    None,
    /// Password lives in the OS keychain under [`vault_keys::password`].
    Password {
        username: String,
    },
    /// RDS/Aurora IAM database authentication: a 15-minute SigV4 token minted per new physical connection.
    RdsIam {
        username: String,
        aws: AwsTarget,
    },
    /// Username/password read from Secrets Manager at connect time, held in memory only.
    SecretsManager {
        secret_arn: String,
        aws: AwsTarget,
    },
    /// A bearer access token sent as the password (the server must ask for a cleartext password,
    /// so use TLS). Covers capitec/pg-proxy JWT/OIDC auth (Keycloak, Entra ID) and Microsoft
    /// Entra authentication for Azure Database for PostgreSQL.
    ///
    /// `command` (optional) is run through the login shell for every new physical connection and
    /// must print the token (or a JSON object with `accessToken` / `access_token` / `token`) on
    /// stdout, e.g. `az account get-access-token --resource-type oss-rdbms --query accessToken -o tsv`.
    /// Without a command the token is read from the keychain under [`vault_keys::password`].
    /// A JWT's `exp` claim is honoured: an expired stored token is refused with a clear message
    /// and command output is cached until shortly before it expires.
    AccessToken {
        /// Sent as the startup user name. pg-proxy ignores it (identity comes from the token);
        /// Azure needs the Entra principal name. Empty means `vibelens`.
        username: String,
        #[serde(default)]
        command: Option<String>,
    },
    /// ElastiCache (Valkey/Redis OSS 7+) IAM auth. `cache_name` is the replication group id or serverless cache name.
    ElastiCacheIam {
        username: String,
        cache_name: String,
        serverless: bool,
        aws: AwsTarget,
    },
}

impl AuthMethod {
    /// Reads its secret from the keychain under [`vault_keys::password`]: a password, or an
    /// access token that is pasted rather than produced by a command.
    pub fn uses_stored_password(&self) -> bool {
        match self {
            AuthMethod::Password { .. } => true,
            AuthMethod::AccessToken { command, .. } => {
                command.as_deref().is_none_or(|c| c.trim().is_empty())
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SshAuth {
    /// Use the running ssh-agent (SSH_AUTH_SOCK / Windows OpenSSH agent pipe).
    #[default]
    Agent,
    /// Private key file. Passphrase (if any) lives in the keychain under [`vault_keys::ssh_passphrase`].
    PrivateKey { path: String },
    /// Password lives in the keychain under [`vault_keys::ssh_password`].
    Password,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TunnelConfig {
    Ssh {
        host: String,
        port: u16,
        username: String,
        auth: SshAuth,
        /// SHA256 host-key fingerprint pinned on first successful connect (TOFU). `None` until first connect.
        known_host_fingerprint: Option<String>,
    },
}

/// How a capitec/pg-proxy (PGProxy) in front of the database is configured (its `PROXY_MODE`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum PgProxyMode {
    /// Inefficient queries are rejected (`P0001 Query blocked by SQL Engineer: ...`).
    #[default]
    Blocking,
    /// Issues are logged; every query runs.
    NonBlocking,
    /// Bad SELECTs go to a slow node; writes **and transaction control** (`BEGIN`, `ROLLBACK`, ...)
    /// are refused. VibeLens then enforces read-only runs with `default_transaction_read_only`
    /// instead of `BEGIN READ ONLY`, and resets sessions without `ROLLBACK`.
    Shunt,
}

/// The connection goes through capitec/pg-proxy (<https://github.com/capitec/pg-proxy>).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PgProxyOptions {
    pub mode: PgProxyMode,
    /// Base URL of the proxy's health listener (`HEALTH_CHECK_ADDR`, port 8080 by default), e.g.
    /// `http://pgproxy.internal:8080`. When set, Test connection checks `GET /ready` first so a
    /// proxy whose backend is down is told apart from a proxy that cannot be reached.
    pub health_url: Option<String>,
}

impl PgProxyOptions {
    /// The proxy refuses `BEGIN`/`ROLLBACK`/`COMMIT`, so nothing may rely on them.
    pub fn forbids_transactions(&self) -> bool {
        self.mode == PgProxyMode::Shunt
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct PgOptions {
    /// Generous default so scale-to-zero (Aurora Serverless v2 min 0 ACU, Neon) can resume.
    pub connect_timeout_secs: u64,
    pub statement_timeout_ms: Option<u64>,
    /// After this many idle seconds the pooled server sessions are closed so serverless compute
    /// can pause (`0` = never). The connection itself stays open (status, tunnel, caches): the
    /// next query opens a fresh session transparently.
    pub idle_disconnect_secs: u64,
    pub max_pool_size: usize,
    pub application_name: String,
    /// Set when the endpoint is a capitec/pg-proxy rather than Postgres itself.
    pub pgproxy: Option<PgProxyOptions>,
}

impl Default for PgOptions {
    fn default() -> Self {
        Self {
            connect_timeout_secs: 30,
            statement_timeout_ms: None,
            idle_disconnect_secs: 300,
            max_pool_size: 4,
            application_name: "VibeLens".into(),
            pgproxy: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ValkeyOptions {
    pub cluster: bool,
    pub key_delimiter: String,
    pub scan_count: u32,
    pub connect_timeout_secs: u64,
    pub idle_disconnect_secs: u64,
}

impl Default for ValkeyOptions {
    fn default() -> Self {
        Self {
            cluster: false,
            key_delimiter: ":".into(),
            scan_count: 500,
            connect_timeout_secs: 15,
            idle_disconnect_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProfile {
    pub id: ProfileId,
    pub name: String,
    pub engine: EngineKind,
    pub host: String,
    pub port: u16,
    /// Postgres default database. Ignored for Valkey.
    pub database: Option<String>,
    pub auth: AuthMethod,
    #[serde(default)]
    pub tls: TlsConfig,
    pub tunnel: Option<TunnelConfig>,
    #[serde(default)]
    pub environment: Environment,
    #[serde(default)]
    pub ai_data_access: AiDataAccess,
    pub color: Option<String>,
    #[serde(default)]
    pub pg: PgOptions,
    #[serde(default)]
    pub valkey: ValkeyOptions,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    /// Required when `engine == Iceberg`; ignored otherwise. `host`/`port` are unused for Iceberg.
    #[serde(default)]
    pub iceberg: Option<super::iceberg::IcebergOptions>,
}

/// The Postgres database a call runs against: the requested one, else the profile's configured
/// database, else `postgres` (blank names count as unset).
///
/// This is the only place that rule lives. The connection manager opens its pool with the
/// result and the agent's approval card shows it, so the two cannot disagree.
pub fn effective_database(configured: Option<&str>, requested: Option<&str>) -> String {
    let named = |d: Option<&str>| d.map(str::trim).filter(|d| !d.is_empty()).map(String::from);
    named(requested)
        .or_else(|| named(configured))
        .unwrap_or_else(|| "postgres".to_string())
}

impl ConnectionProfile {
    /// [`effective_database`] for this profile.
    pub fn effective_database(&self, requested: Option<&str>) -> String {
        effective_database(self.database.as_deref(), requested)
    }

    /// Keychain entries this profile's current auth, tunnel and Iceberg settings never read.
    /// Anything here is left over from an earlier configuration and should be deleted.
    pub fn unused_vault_keys(&self) -> Vec<String> {
        use super::iceberg::{IcebergCatalog, IcebergStorage, RestAuth};

        let id = &self.id;
        let ssh = self
            .tunnel
            .as_ref()
            .map(|TunnelConfig::Ssh { auth, .. }| auth);
        let iceberg = self.iceberg.as_ref();
        let uses_iceberg_secret = iceberg.is_some_and(|o| match &o.catalog {
            IcebergCatalog::Rest { auth, .. } => !matches!(auth, RestAuth::None),
            IcebergCatalog::Sql { .. } => true,
            IcebergCatalog::Glue { .. }
            | IcebergCatalog::S3Tables { .. }
            | IcebergCatalog::Hms { .. } => false,
        });
        let uses_s3_keys =
            iceberg.is_some_and(|o| matches!(o.storage, IcebergStorage::S3Static { .. }));

        let mut unused = Vec::new();
        if !self.auth.uses_stored_password() {
            unused.push(vault_keys::password(id));
        }
        if !matches!(ssh, Some(SshAuth::Password)) {
            unused.push(vault_keys::ssh_password(id));
        }
        if !matches!(ssh, Some(SshAuth::PrivateKey { .. })) {
            unused.push(vault_keys::ssh_passphrase(id));
        }
        if !uses_iceberg_secret {
            unused.push(vault_keys::iceberg_secret(id));
        }
        if !uses_s3_keys {
            unused.push(vault_keys::s3_access_key_id(id));
            unused.push(vault_keys::s3_secret_access_key(id));
        }
        unused
    }
}

/// Credentials ready to hand to a driver. Lives in memory only; never serialized.
#[derive(Debug, Clone)]
pub struct ResolvedCredentials {
    pub username: Option<String>,
    pub password: Option<SecretString>,
    /// When the password stops being valid *for new connections* (IAM tokens: ~15 min).
    pub expires_at: Option<DateTime<Utc>>,
    /// Secrets Manager RDS secrets may carry these; the profile's values win if set.
    pub host_override: Option<String>,
    pub port_override: Option<u16>,
    pub database_override: Option<String>,
}

impl ResolvedCredentials {
    pub fn anonymous() -> Self {
        Self {
            username: None,
            password: None,
            expires_at: None,
            host_override: None,
            port_override: None,
            database_override: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatus {
    pub profile_id: ProfileId,
    pub engine: EngineKind,
    pub state: ConnectionState,
    pub since: Option<DateTime<Utc>>,
    pub server_version: Option<String>,
    pub detail: Option<String>,
    pub tunneled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub ok: bool,
    pub server_version: Option<String>,
    pub latency_ms: u64,
    pub message: String,
}

/// Keychain entry names. Service name is always [`vault_keys::SERVICE`].
pub mod vault_keys {
    pub const SERVICE: &str = "com.heinvandermerwe.vibelens";
    /// The one item holding all secrets on macOS (see `vault::BundledVault`).
    pub const BUNDLE: &str = "vault-bundle";
    pub fn password(profile_id: &str) -> String {
        format!("profile:{profile_id}:password")
    }
    pub fn ssh_password(profile_id: &str) -> String {
        format!("profile:{profile_id}:ssh-password")
    }
    pub fn ssh_passphrase(profile_id: &str) -> String {
        format!("profile:{profile_id}:ssh-passphrase")
    }
    pub fn llm_api_key(provider_id: &str) -> String {
        format!("llm:{provider_id}:api-key")
    }
    /// Iceberg REST bearer token / OAuth2 client secret / SQL-catalog database password.
    pub fn iceberg_secret(profile_id: &str) -> String {
        format!("profile:{profile_id}:iceberg-secret")
    }
    pub fn s3_access_key_id(profile_id: &str) -> String {
        format!("profile:{profile_id}:s3-access-key-id")
    }
    pub fn s3_secret_access_key(profile_id: &str) -> String {
        format!("profile:{profile_id}:s3-secret-access-key")
    }
}
