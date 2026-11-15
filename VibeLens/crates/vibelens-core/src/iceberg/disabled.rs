//! `IcebergHandle` for builds without the `iceberg` feature: every entry point says so.

use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::IcebergDeps;
use crate::error::{CoreError, Result};
use crate::model::*;

fn off() -> CoreError {
    CoreError::Invalid("this build has no Apache Iceberg support (feature iceberg)".into())
}

/// Placeholder handle; cannot be constructed.
pub struct IcebergHandle {
    _private: (),
}

impl IcebergHandle {
    pub async fn connect(
        _profile: ConnectionProfile,
        _deps: IcebergDeps,
    ) -> Result<Arc<IcebergHandle>> {
        Err(off())
    }
    pub fn describe_target(&self) -> String {
        String::new()
    }
    pub async fn list_namespaces(
        &self,
        _parent: Option<Vec<String>>,
    ) -> Result<Vec<IcebergNamespace>> {
        Err(off())
    }
    pub async fn list_tables(&self, _namespace: Vec<String>) -> Result<Vec<IcebergTableRef>> {
        Err(off())
    }
    pub async fn describe_table(
        &self,
        _namespace: Vec<String>,
        _name: &str,
    ) -> Result<IcebergTableInfo> {
        Err(off())
    }
    pub async fn schema_metadata(
        &self,
        _namespace: Option<Vec<String>>,
        _max_tables: usize,
    ) -> Result<SchemaMetadata> {
        Err(off())
    }
    pub async fn execute_stream(
        &self,
        _query_id: String,
        _sql: &str,
        _opts: ExecOptions,
        _cancel: CancellationToken,
        _tx: mpsc::Sender<QueryEvent>,
    ) -> Result<()> {
        Err(off())
    }
    pub async fn execute_collect(&self, _sql: &str, _opts: ExecOptions) -> Result<QueryResult> {
        Err(off())
    }
    pub async fn expire_snapshots(
        &self,
        _namespace: Vec<String>,
        _name: &str,
        _older_than_ms: Option<i64>,
        _retain_last: Option<u32>,
    ) -> Result<ExpireSnapshotsResult> {
        Err(off())
    }
    pub fn idle_secs(&self) -> u64 {
        0
    }
    pub async fn close(&self) {}
}
