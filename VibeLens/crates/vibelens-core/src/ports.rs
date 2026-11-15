//! Ports (hexagonal architecture): the seams between modules. Adapters implement these;
//! the ConnectionManager and AgentService only ever depend on the traits.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use secrecy::SecretString;
use tokio_util::sync::CancellationToken;

use crate::error::Result;
use crate::model::*;

/// OS keychain (macOS Keychain Services / Windows Credential Manager) or in-memory for tests.
#[async_trait]
pub trait SecretVault: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<SecretString>>;
    async fn set(&self, key: &str, value: &SecretString) -> Result<()>;
    async fn delete(&self, key: &str) -> Result<()>;
}

/// Non-secret connection profiles (JSON file in the app config dir).
pub trait ProfileStore: Send + Sync {
    fn list(&self) -> Result<Vec<ConnectionProfile>>;
    fn get(&self, id: &str) -> Result<ConnectionProfile>;
    /// Assigns `id` if empty and stamps `created_at`/`updated_at`. Returns the stored profile.
    fn upsert(&self, profile: ConnectionProfile) -> Result<ConnectionProfile>;
    fn delete(&self, id: &str) -> Result<()>;
}

/// Turns a profile's [`AuthMethod`] into usable credentials (keychain, IAM token, Secrets Manager).
#[async_trait]
pub trait CredentialSource: Send + Sync {
    async fn resolve(&self, profile: &ConnectionProfile) -> Result<ResolvedCredentials>;
}

/// Called by a driver once per *physical* connection it opens. IAM tokens are only checked at
/// connection establishment, so minting per new connection is all "refresh" needs to mean.
pub type CredentialFn =
    Arc<dyn Fn() -> BoxFuture<'static, Result<ResolvedCredentials>> + Send + Sync>;

/// A live local port-forward. Dropping the handle tears the tunnel down.
pub struct TunnelHandle {
    pub local_addr: SocketAddr,
    pub cancel: CancellationToken,
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[async_trait]
pub trait TunnelProvider: Send + Sync {
    /// Opens `127.0.0.1:<ephemeral>` → `target_host:target_port` via the tunnel.
    /// `profile_id` is used to look up tunnel secrets in the vault.
    async fn open(
        &self,
        profile_id: &str,
        cfg: &TunnelConfig,
        target_host: &str,
        target_port: u16,
    ) -> Result<TunnelHandle>;
}

/// Everything the agent's tools are allowed to do, and nothing else.
/// Implemented by `connections::AppToolBackend`. Policy (approval, AiDataAccess) is enforced
/// by the agent layer *before* calling these; implementations still honour `read_only`.
#[async_trait]
pub trait ToolBackend: Send + Sync {
    fn profile(&self, profile_id: &str) -> Result<ConnectionProfile>;
    fn list_profiles(&self) -> Result<Vec<ConnectionProfile>>;
    async fn execute_sql(
        &self,
        profile_id: &str,
        database: Option<&str>,
        sql: &str,
        read_only: bool,
        max_rows: usize,
    ) -> Result<QueryResult>;
    async fn schema_metadata(
        &self,
        profile_id: &str,
        database: Option<&str>,
        schema_filter: Option<&str>,
        max_tables: usize,
    ) -> Result<SchemaMetadata>;
    async fn valkey_scan(
        &self,
        profile_id: &str,
        db: u8,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<KeyEntry>>;
    async fn valkey_get(&self, profile_id: &str, db: u8, key: &str) -> Result<ValkeyValue>;
    async fn valkey_is_write(&self, profile_id: &str, argv: &[String]) -> Result<bool>;
    async fn valkey_command(
        &self,
        profile_id: &str,
        db: u8,
        argv: Vec<String>,
        allow_write: bool,
    ) -> Result<CommandOutcome>;
    async fn list_files(&self) -> Result<Vec<WorkspaceFile>>;
    async fn read_file(&self, path: &str) -> Result<String>;
    async fn write_file(&self, path: &str, contents: &str) -> Result<()>;
    /// Full Iceberg table metadata (partition spec, sort order, snapshots) for `describe_table`.
    /// Default: not supported, so fakes and non-Iceberg backends need not implement it; callers
    /// fall back to `schema_metadata` on `CoreError::NotImplemented`.
    async fn describe_iceberg_table(
        &self,
        _profile_id: &str,
        _namespace: Vec<String>,
        _name: &str,
    ) -> Result<IcebergTableInfo> {
        Err(crate::error::CoreError::NotImplemented(
            "describe_iceberg_table",
        ))
    }
}

/// Convenience for drivers: a cancellation-aware unit of work.
pub type Cancel = CancellationToken;
