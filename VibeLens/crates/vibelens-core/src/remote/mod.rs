//! Remote query engines for Iceberg connections, reached over HTTP: Trino (client REST
//! protocol) and Spark SQL through Apache Livy. See `docs/REMOTE_ENGINES_AND_REDSHIFT.md`.
//!
//! Both have the shape of the Athena engine (`crate::athena`): `execute_stream` with the same
//! event contract as `PgHandle::execute_stream` (`Started`, then per statement `Columns`,
//! `Rows`*, `Stats`, `StatementDone`, and exactly one `Finished` or `Failed`), and
//! `execute_collect` for bounded, materialised results.
//!
//! # Rules every request follows
//!
//! All HTTP goes through [`http::HttpClient`]: no redirects, TLS always verified (OS roots plus
//! an optional PEM bundle), the credential attached only to requests for the configured
//! endpoint, bounded response bodies, secrets redacted from every message. A URL handed back by
//! the server (Trino's `nextUri`) is used only when it has the scheme, host and port of the
//! configured URL.
//!
//! # Read-only
//!
//! Neither engine has a read-only transaction, so the check is the enforcement and runs before
//! anything is sent: Trino shares Athena's check (same dialect); Spark has a stricter scan
//! ([`spark_scan`]) because a Spark `SELECT` can call Java methods and read files.

pub(crate) mod config;

#[cfg(feature = "iceberg")]
mod common;
#[cfg(feature = "iceberg")]
pub mod http;
#[cfg(feature = "iceberg")]
mod livy;
#[cfg(feature = "iceberg")]
mod spark_scan;
#[cfg(feature = "iceberg")]
mod trino;

#[cfg(feature = "iceberg")]
pub use http::HttpLimits;
#[cfg(feature = "iceberg")]
pub use livy::{LIVY_DEFAULT_ROW_CAP, SparkConfig, SparkEngine};
#[cfg(feature = "iceberg")]
pub use spark_scan::check_spark_read_only;
#[cfg(feature = "iceberg")]
pub use trino::{TrinoConfig, TrinoEngine};

use std::time::Duration;

/// Pacing knobs shared by both engines. The defaults are the production values; tests shrink
/// them.
#[derive(Debug, Clone, Copy)]
pub struct RemoteTiming {
    /// First poll delay while a statement has nothing new; doubles up to
    /// [`poll_max`](Self::poll_max).
    pub poll_initial: Duration,
    pub poll_max: Duration,
    /// Base of the retry backoff (`base * 2^attempt`, plus up to 50% jitter).
    pub retry_base: Duration,
    /// How long a best-effort cancel or session delete may take.
    pub cancel_timeout: Duration,
}

impl Default for RemoteTiming {
    fn default() -> Self {
        Self {
            poll_initial: Duration::from_millis(50),
            poll_max: Duration::from_secs(1),
            retry_base: Duration::from_millis(250),
            cancel_timeout: Duration::from_secs(10),
        }
    }
}
