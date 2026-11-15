//! Apache Iceberg connection + metadata types. Mirrored in `src/ipc/types.ts`.
//!
//! Snapshot ids and sequence numbers are i64 in Iceberg; they cross IPC as **strings**
//! (JS numbers lose precision past 2^53), same rule as Postgres int8.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::profile::AwsTarget;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IcebergOptions {
    pub catalog: IcebergCatalog,
    #[serde(default)]
    pub storage: IcebergStorage,
    #[serde(default)]
    pub query_engine: IcebergQueryEngine,
    /// Namespace used for unqualified table names in SQL (e.g. `analytics`; nested: `a.b`).
    pub default_namespace: Option<String>,
}

/// Where table metadata lives.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum IcebergCatalog {
    /// Iceberg REST catalog spec (Polaris, Lakekeeper, Nessie, Gravitino, Unity's Iceberg endpoint, …).
    Rest {
        uri: String,
        warehouse: Option<String>,
        auth: RestAuth,
        /// Extra HTTP headers sent with every catalog request (non-secret).
        #[serde(default)]
        headers: BTreeMap<String, String>,
    },
    /// AWS Glue Data Catalog.
    Glue {
        aws: AwsTarget,
        catalog_id: Option<String>,
        warehouse: Option<String>,
    },
    /// Amazon S3 Tables (table buckets).
    S3Tables {
        aws: AwsTarget,
        table_bucket_arn: String,
    },
    /// Iceberg SQL/JDBC catalog. `uri`: `sqlite:///abs/path.db` or `postgres://user@host/db`
    /// (password, if any, in the vault under [`super::vault_keys::iceberg_secret`]).
    Sql {
        uri: String,
        warehouse: String,
        catalog_name: String,
    },
    /// Hive Metastore over Thrift (no Kerberos). Only in builds with the `iceberg-hms` feature.
    Hms { address: String, warehouse: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RestAuth {
    None,
    /// Static bearer token, stored in the vault under [`super::vault_keys::iceberg_secret`].
    Bearer,
    /// OAuth2 client-credentials. Client secret in the vault under [`super::vault_keys::iceberg_secret`].
    OAuth2 {
        client_id: String,
        scope: Option<String>,
        server_uri: Option<String>,
    },
}

/// How table data files (Parquet) are read/written by the **local** engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum IcebergStorage {
    /// Use the catalog's AWS identity for S3 (Glue / S3 Tables), or credentials vended by a REST catalog.
    #[default]
    Auto,
    /// S3 with a specific AWS identity; `endpoint` for S3-compatible stores (MinIO, Ceph, R2).
    S3 {
        aws: Option<AwsTarget>,
        endpoint: Option<String>,
        region: Option<String>,
        path_style: bool,
    },
    /// S3-compatible store with static keys. Access key id + secret in the vault under
    /// [`super::vault_keys::s3_access_key_id`] / [`super::vault_keys::s3_secret_access_key`].
    S3Static {
        endpoint: Option<String>,
        region: Option<String>,
        path_style: bool,
    },
    /// Local filesystem warehouse (`file://`), for development.
    Local,
}

/// Where SQL runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum IcebergQueryEngine {
    /// Embedded Apache DataFusion: reads data files directly. SELECT, time travel, INSERT (append).
    #[default]
    Local,
    /// Amazon Athena (Glue / S3 Tables only): server-side, full DML, Lake Formation aware, billed per TB scanned.
    Athena {
        workgroup: String,
        /// `s3://bucket/prefix/` for results; `None` = the workgroup's configured location.
        output_location: Option<String>,
        /// Defaults to the catalog's AWS identity.
        aws: Option<AwsTarget>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IcebergNamespace {
    /// Namespace levels, e.g. `["analytics", "daily"]`.
    pub levels: Vec<String>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum IcebergObjectKind {
    Table,
    View,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IcebergTableRef {
    pub namespace: Vec<String>,
    pub name: String,
    pub kind: IcebergObjectKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IcebergField {
    pub id: i32,
    pub name: String,
    /// Iceberg type rendered as text: `long`, `decimal(12, 2)`, `timestamptz`, `struct<…>`, `list<…>`, `map<…>`.
    pub type_name: String,
    pub required: bool,
    pub doc: Option<String>,
    /// Struct fields, list element (`element`) or map key/value (`key`, `value`).
    #[serde(default)]
    pub children: Vec<IcebergField>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IcebergPartitionField {
    pub source_id: i32,
    pub field_id: i32,
    pub name: String,
    /// `identity`, `bucket[16]`, `truncate[10]`, `year`, `month`, `day`, `hour`, `void`.
    pub transform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IcebergSortField {
    pub source_id: i32,
    pub transform: String,
    /// `asc` | `desc`
    pub direction: String,
    /// `nulls-first` | `nulls-last`
    pub null_order: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IcebergSnapshot {
    /// i64 as string.
    pub snapshot_id: String,
    pub parent_snapshot_id: Option<String>,
    pub sequence_number: Option<String>,
    pub timestamp_ms: i64,
    /// `append` | `overwrite` | `delete` | `replace`
    pub operation: Option<String>,
    /// Snapshot summary: `added-data-files`, `added-records`, `total-records`, `total-files-size`, …
    #[serde(default)]
    pub summary: BTreeMap<String, String>,
    pub manifest_list: Option<String>,
    pub schema_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IcebergRef {
    pub name: String,
    pub snapshot_id: String,
    /// `branch` | `tag`
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IcebergTableInfo {
    pub namespace: Vec<String>,
    pub name: String,
    pub kind: IcebergObjectKind,
    pub location: String,
    pub metadata_location: Option<String>,
    pub format_version: u8,
    pub table_uuid: Option<String>,
    pub last_updated_ms: Option<i64>,
    pub current_snapshot_id: Option<String>,
    pub current_schema_id: i32,
    /// Fields of the current schema.
    pub fields: Vec<IcebergField>,
    /// Number of historical schemas (schema evolution).
    pub schema_count: usize,
    pub partition_spec: Vec<IcebergPartitionField>,
    pub sort_order: Vec<IcebergSortField>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
    /// Newest first.
    pub snapshots: Vec<IcebergSnapshot>,
    #[serde(default)]
    pub refs: Vec<IcebergRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExpireSnapshotsResult {
    pub expired: u64,
    pub remaining: u64,
}
