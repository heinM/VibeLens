//! Tauri IPC commands. Thin adapters: validate → call core → map errors to `ErrorPayload`.
//! Rule: no secret ever appears in a return value. Secrets only travel UI → Rust
//! (typed into a form) and are written straight to the keychain.

pub mod agent;
pub mod app;
pub mod aws;
pub mod connections;
pub mod files;
pub mod iceberg;
pub mod import;
pub mod pg;
pub mod profiles;
pub mod redshift;
pub mod valkey;
pub mod workspace;

use vibelens_core::ErrorPayload;

pub type CmdResult<T> = Result<T, ErrorPayload>;

pub fn err<E: Into<ErrorPayload>>(e: E) -> ErrorPayload {
    e.into()
}
