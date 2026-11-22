//! # vibelens-core
//!
//! Everything VibeLens does that isn't UI. No Tauri dependency: the desktop shell in
//! `src-tauri` is a thin IPC adapter over this crate, which keeps the engines testable
//! headless (`cargo test -p vibelens-core`).
//!
//! Layout (ports & adapters):
//! - [`model`]  — serializable domain types, mirrored in `src/ipc/types.ts`
//! - [`ports`]  — traits between modules
//! - adapters   — [`pg`], [`mysql`], [`valkey`], [`aws`], [`vault`], [`profiles`], [`tunnel`], [`workspace`]
//! - services   — [`connections::ConnectionManager`], [`agent::AgentService`]
//! - [`sql`]    — statement classification used by the agent policy and the prod write-guard

pub mod agent;
#[cfg(feature = "iceberg")]
pub mod athena;
pub mod aws;
pub mod cleartext;
pub mod connections;
pub mod error;
pub mod iceberg;
pub mod import;
pub mod model;
pub mod mysql;
pub mod pg;
pub mod ports;
pub mod profiles;
pub mod redshift;
pub mod remote;
pub mod sql;
pub mod tls;
pub mod token;
pub mod tunnel;
pub mod valkey;
pub mod vault;
pub mod workspace;

pub use error::{CoreError, ErrorPayload, Result};

/// The rule for a test whose outside service is missing (`VG_TEST_REQUIRE`): one file, shared
/// with the integration tests in `tests/`.
#[cfg(test)]
#[path = "../tests/services/mod.rs"]
mod test_services;
