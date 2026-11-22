//! MySQL connection settings (MySQL 8.x, Aurora MySQL, RDS MySQL).
//!
//! A MySQL profile uses the profile's `host`, `port`, `database` (required: the default
//! database), `auth` (`password` or `rdsIam`), `tls` and `tunnel`, like Postgres. What is
//! specific to MySQL is here. Nothing in this struct is secret.

use serde::{Deserialize, Serialize};

/// Bounds the profile validator enforces on [`MysqlOptions`] (on save and on use).
pub const MYSQL_MAX_POOL_SIZE: usize = 32;
pub const MYSQL_MAX_CONNECT_TIMEOUT_SECS: u64 = 600;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct MysqlOptions {
    /// TCP connect + TLS + authentication, in seconds (1 to 600).
    pub connect_timeout_secs: u64,
    /// Per-statement time limit for runs without their own (`ExecOptions.timeout_ms`). On
    /// expiry the statement is stopped on the server with `KILL QUERY`.
    pub statement_timeout_ms: Option<u64>,
    /// After this many idle seconds the pooled server sessions are closed (`0` = never).
    pub idle_disconnect_secs: u64,
    /// Connections per database (1 to 32).
    pub max_pool_size: usize,
}

impl Default for MysqlOptions {
    fn default() -> Self {
        Self {
            connect_timeout_secs: 30,
            statement_timeout_ms: None,
            idle_disconnect_secs: 300,
            max_pool_size: 4,
        }
    }
}
