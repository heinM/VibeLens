use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInfo {
    pub name: String,
    pub is_template: bool,
    pub allow_connections: bool,
    pub size_bytes: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SchemaInfo {
    pub name: String,
    pub owner: Option<String>,
    pub is_system: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ObjectKind {
    Table,
    PartitionedTable,
    View,
    MaterializedView,
    ForeignTable,
    Sequence,
    Function,
    Procedure,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SchemaObject {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    pub comment: Option<String>,
    /// `pg_class.reltuples` estimate; -1/None when never analysed.
    pub estimated_rows: Option<i64>,
    /// For functions/procedures: identity arguments, e.g. `(integer, text)`.
    pub signature: Option<String>,
    /// The relation is a partition of another table (`pg_class.relispartition`). It is still
    /// listed, so completion and lookups see it; the tree nests it under its parent.
    #[serde(default)]
    pub is_partition: bool,
    /// Schema and name of the partitioned table this one belongs to, when `is_partition`.
    #[serde(default)]
    pub parent_schema: Option<String>,
    #[serde(default)]
    pub parent_name: Option<String>,
}

/// One direct child partition of a partitioned table (it may live in another schema).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PartitionInfo {
    pub schema: String,
    pub name: String,
    /// `pg_get_expr(relpartbound, oid)`, e.g. `FOR VALUES FROM ('2024-01-01') TO ('2024-02-01')`.
    pub bound: String,
    pub is_default: bool,
    /// The partition is itself partitioned (it has partitions of its own).
    pub is_partitioned: bool,
    /// `reltuples` estimate; None when never analysed.
    pub estimated_rows: Option<i64>,
    /// `pg_total_relation_size`; for a partitioned partition, the sum over its leaves.
    pub size_bytes: Option<i64>,
}

/// `pg_partman.part_config` settings of one parent table. Every field is optional: partman 4
/// and 5 have different column sets, and a managed table need not set them all.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PartmanConfig {
    pub control: Option<String>,
    /// As partman stores it: `1 day`, or a number for id-based sets.
    pub partition_interval: Option<String>,
    pub premake: Option<i64>,
    /// `90 days`, or a number of partitions/ids; None when partitions are kept forever.
    pub retention: Option<String>,
    pub retention_keep_table: Option<bool>,
    /// `on` / `off`.
    pub automatic_maintenance: Option<String>,
}

/// Partition details of one partitioned table, loaded when its tree node is expanded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PartitionListing {
    /// `pg_get_partkeydef`, e.g. `RANGE (created_at)`.
    pub partition_key: Option<String>,
    /// Direct children, ordered by bound with the default partition last.
    pub partitions: Vec<PartitionInfo>,
    /// More than the cap exist; `partitions` holds the first ones.
    pub truncated: bool,
    /// None when pg_partman is not installed, does not manage this table, or cannot be read.
    pub partman: Option<PartmanConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub is_primary_key: bool,
    pub ordinal: i32,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct IndexInfo {
    pub name: String,
    pub definition: String,
    pub is_unique: bool,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKeyInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_schema: String,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TableDescription {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    pub comment: Option<String>,
    pub estimated_rows: Option<i64>,
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
}

/// Compact catalog dump for the agent. Capped by `max_tables`; `truncated` says so.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SchemaMetadata {
    pub database: String,
    pub server_version: Option<String>,
    pub tables: Vec<TableDescription>,
    pub truncated: bool,
}
