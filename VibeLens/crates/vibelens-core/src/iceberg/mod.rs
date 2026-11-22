//! Apache Iceberg (OWNER: iceberg-core engineer).
//!
//! One [`IcebergHandle`] per connection profile. It owns:
//! - a catalog (`iceberg::Catalog`): REST (OAuth2/bearer/none), Glue, S3 Tables, SQL (sqlx: sqlite/
//!   postgres), HMS (feature `iceberg-hms`). AWS catalogs and S3 FileIO get credentials from
//!   [`crate::aws::AwsService::credentials_provider`] (SSO profiles, Identity Center, default chain).
//!   Temporary credentials are refreshed by rebuilding the catalog/FileIO before they expire.
//! - a query engine: **Local** = DataFusion `SessionContext` with a lazy catalog provider registered
//!   as `iceberg` (default namespace = default schema; tables are loaded on first use, not at
//!   connect: see `engine.rs` for why `IcebergCatalogProvider::try_new` is not used); **Athena** =
//!   [`crate::athena::AthenaEngine`] (Glue / S3 Tables only).
//!
//! Time travel (local engine): `SELECT * FROM ns."table@<snapshot-id | branch | tag | RFC3339>"`
//! resolves to the table as of that snapshot; `ns."table$snapshots"` / `"table$manifests"` /
//! `"table$history"` are the metadata tables; nested namespaces are `"a.b".table`.
//!
//! Cell encoding is identical to Postgres (`model::query::Cell`): Int64/Decimal/UInt64 → strings,
//! binary → `{"$bytes", "len"}`, timestamps → RFC3339, structs/lists/maps → JSON.
//!
//! Read-only runs (`ExecOptions::read_only`): reject anything that isn't a query (classified with
//! `crate::sql::classify`) *before* execution: DataFusion has no transactions to enforce it.

use std::sync::Arc;

use crate::aws::AwsService;
use crate::ports::SecretVault;

#[cfg(feature = "iceberg")]
mod catalog;
#[cfg(feature = "iceberg")]
mod codec;
#[cfg(feature = "iceberg")]
mod engine;
#[cfg(feature = "iceberg")]
mod handle;
#[cfg(feature = "iceberg")]
mod meta;
#[cfg(feature = "iceberg")]
mod refresh;

#[cfg(feature = "iceberg")]
pub use handle::IcebergHandle;

#[cfg(not(feature = "iceberg"))]
mod disabled;
#[cfg(not(feature = "iceberg"))]
pub use disabled::IcebergHandle;

/// Services an Iceberg connection needs (AWS identity + keychain for REST/S3 secrets).
#[derive(Clone)]
pub struct IcebergDeps {
    pub aws: Arc<AwsService>,
    pub vault: Arc<dyn SecretVault>,
}
