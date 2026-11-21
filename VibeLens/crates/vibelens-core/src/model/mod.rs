//! Domain model. Everything here is `Serialize + Deserialize` with camelCase
//! fields so it mirrors `src/ipc/types.ts` one-to-one. Change both together.

pub mod agent;
pub mod aws;
pub mod iceberg;
pub mod profile;
pub mod query;
pub mod redshift;
pub mod schema;
pub mod valkey;
pub mod workspace;

pub use agent::*;
pub use aws::*;
pub use iceberg::*;
pub use profile::*;
pub use query::*;
pub use redshift::*;
pub use schema::*;
pub use valkey::*;
pub use workspace::*;
