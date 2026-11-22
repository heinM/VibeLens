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

/// `crate::sql::classify(sql)`, corrected for Trino and Spark by the engine's own read-only
/// check, in both directions that make things safer or truthful, never towards "write is a
/// read":
///
/// * The PostgreSQL parser cannot read much of their syntax (backticks, `VERSION AS OF`, …)
///   and calls it `Unknown`; when the engine check accepts it, it is `ReadOnly`.
/// * The PostgreSQL parser calls some statements reads that the engine refuses
///   (`TABLE(pg.system.query(…))`, `java_method(…)`, `MAP … USING 'sh …'`); those become
///   `Unknown`, so the production write guard asks before they run.
///
/// A statement the PostgreSQL classifier calls a write stays a write.
pub fn classify_on(engine: Option<&IcebergQueryEngine>, sql: &str) -> SqlClassification {
    classify_on_explained(engine, sql).0
}

/// [`classify_on`], plus the engine's reason when it turned a PostgreSQL `ReadOnly` into
/// `Unknown`.
pub fn classify_on_explained(
    engine: Option<&IcebergQueryEngine>,
    sql: &str,
) -> (SqlClassification, Option<String>) {
    let mut c = crate::sql::classify(sql);
    let Some(verdict) = engine.and_then(|e| engine_read_only_check(e, sql)) else {
        return (c, None);
    };
    match (c.overall, verdict) {
        (StatementClass::Unknown, Ok(())) => {
            c.overall = StatementClass::ReadOnly;
            c.statements = vec![StatementClass::ReadOnly; c.statements.len()];
            (c, None)
        }
        (StatementClass::ReadOnly, Err(reason)) => {
            c.overall = StatementClass::Unknown;
            c.statements = vec![StatementClass::Unknown; c.statements.len()];
            c.summary = format!("{} (refused by the engine's read-only check)", c.summary);
            (c, Some(reason))
        }
        _ => (c, None),
    }
}

/// The engine's own read-only check on `sql`: `None` when the engine has none (local, Athena)
/// or the script is empty, `Some(Err(reason))` when it refuses.
#[cfg(feature = "iceberg")]
fn engine_read_only_check(
    engine: &IcebergQueryEngine,
    sql: &str,
) -> Option<std::result::Result<(), String>> {
    let statements = crate::sql::split_statements(sql);
    if statements.is_empty() {
        return None;
    }
    let r = match engine {
        IcebergQueryEngine::Trino { .. } => check_trino_read_only(&statements),
        IcebergQueryEngine::Spark { .. } => check_spark_read_only(&statements),
        IcebergQueryEngine::Local | IcebergQueryEngine::Athena { .. } => return None,
    };
    Some(r.map_err(|e| match e {
        crate::error::CoreError::PolicyDenied(m) => m,
        other => other.to_string(),
    }))
}

#[cfg(not(feature = "iceberg"))]
fn engine_read_only_check(
    _engine: &IcebergQueryEngine,
    _sql: &str,
) -> Option<std::result::Result<(), String>> {
    None
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

    /// Re-audit item 6: the PostgreSQL classifier calls these reads; the engine refuses them.
    /// Without the downgrade the production write guard let them run unconfirmed with the
    /// read-only toggle off.
    #[test]
    fn a_postgres_read_the_engine_refuses_becomes_unknown() {
        let spark = IcebergQueryEngine::Spark {
            url: "https://l".into(),
            catalog: "lake".into(),
            auth: HttpEngineAuth::None,
            ca_cert_path: None,
            proxy_user: None,
            session_conf: Default::default(),
            session_start_timeout_secs: None,
        };
        let trino = IcebergQueryEngine::Trino {
            url: "https://t".into(),
            catalog: "lake".into(),
            user: "u".into(),
            auth: HttpEngineAuth::None,
            ca_cert_path: None,
        };
        for (engine, s) in [
            (
                &trino,
                "SELECT * FROM TABLE(pg.system.query(query => 'DELETE FROM accounts'))",
            ),
            (
                &spark,
                "SELECT java_method('java.lang.Runtime', 'getRuntime')",
            ),
            (&spark, "SELECT reflect('a', 'b')"),
        ] {
            let (c, why) = classify_on_explained(Some(engine), s);
            assert_eq!(c.overall, StatementClass::Unknown, "{s}");
            // The engine's reason comes with it when the PostgreSQL classifier said "read".
            if crate::sql::classify(s).overall == StatementClass::ReadOnly {
                assert!(why.is_some(), "{s}");
            }
        }
        // Without an engine check nothing changes.
        assert_eq!(
            classify_on(None, "SELECT java_method('a', 'b')").overall,
            StatementClass::ReadOnly
        );
        // Plain reads stay reads.
        assert_eq!(
            classify_on(Some(&trino), "SELECT 1").overall,
            StatementClass::ReadOnly
        );
    }
}
