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

pub mod config;
pub mod secret;

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
pub use trino::{TrinoConfig, TrinoEngine, check_trino_read_only};

use std::time::Duration;

use crate::model::IcebergQueryEngine;
use crate::sql::{SqlClassification, StatementClass};

/// `crate::sql::classify(sql)`, corrected for Trino and Spark. The PostgreSQL parser cannot
/// read much of their syntax (backticks, `VERSION AS OF`, …) and calls it `Unknown`; when it
/// does, the engine's own read-only check decides, and a statement it accepts is a read. A
/// statement the PostgreSQL classifier calls a write stays a write: this only ever turns
/// `Unknown` into `ReadOnly`. Execution runs the engine check again (`read_only = true`).
pub fn classify_on(engine: Option<&IcebergQueryEngine>, sql: &str) -> SqlClassification {
    let mut c = crate::sql::classify(sql);
    if c.overall == StatementClass::Unknown && engine.is_some_and(|e| engine_says_read_only(e, sql))
    {
        c.overall = StatementClass::ReadOnly;
        c.statements = vec![StatementClass::ReadOnly; c.statements.len()];
    }
    c
}

#[cfg(feature = "iceberg")]
fn engine_says_read_only(engine: &IcebergQueryEngine, sql: &str) -> bool {
    let statements = crate::sql::split_statements(sql);
    if statements.is_empty() {
        return false;
    }
    match engine {
        IcebergQueryEngine::Trino { .. } => check_trino_read_only(&statements).is_ok(),
        IcebergQueryEngine::Spark { .. } => check_spark_read_only(&statements).is_ok(),
        IcebergQueryEngine::Local | IcebergQueryEngine::Athena { .. } => false,
    }
}

#[cfg(not(feature = "iceberg"))]
fn engine_says_read_only(_engine: &IcebergQueryEngine, _sql: &str) -> bool {
    false
}

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

#[cfg(all(test, feature = "iceberg"))]
mod tests {
    use super::*;
    use crate::model::HttpEngineAuth;

    /// Audit 2026-11-21, item 13 (the agent policy and the prod guard of `ice_execute` both use
    /// this): Unknown becomes ReadOnly only when the engine's own check passes, and a write
    /// never changes class.
    #[test]
    fn classification_is_corrected_only_from_unknown_to_read_only() {
        let spark = IcebergQueryEngine::Spark {
            url: "https://l".into(),
            catalog: "lake".into(),
            auth: HttpEngineAuth::None,
            ca_cert_path: None,
            proxy_user: None,
            session_conf: Default::default(),
            session_start_timeout_secs: None,
        };
        let s = "SELECT `id` FROM lake.shop.orders VERSION AS OF 1";
        assert_eq!(crate::sql::classify(s).overall, StatementClass::Unknown);
        assert_eq!(
            classify_on(Some(&spark), s).overall,
            StatementClass::ReadOnly
        );
        assert_eq!(classify_on(None, s).overall, StatementClass::Unknown);
        // Unknown to the PostgreSQL classifier and refused by the Spark check: stays Unknown.
        for s in [
            "SELECT '${env:SECRET}' FROM `t`",
            "SELECT * FROM text.`/etc/passwd`",
            "SELECT `a` FROM t -- c",
        ] {
            assert_ne!(
                classify_on(Some(&spark), s).overall,
                StatementClass::ReadOnly,
                "{s}"
            );
        }
        for s in ["DELETE FROM t", "DROP TABLE t", "INSERT INTO t VALUES (1)"] {
            assert!(
                classify_on(Some(&spark), s).overall >= StatementClass::Write,
                "{s}"
            );
        }
        assert_eq!(
            classify_on(Some(&IcebergQueryEngine::Local), s).overall,
            StatementClass::Unknown
        );
    }
}
