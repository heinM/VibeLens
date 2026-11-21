//! Amazon Athena query engine for Iceberg on Glue / S3 Tables (OWNER: athena engineer).
//!
//! StartQueryExecution → poll GetQueryExecution (backoff 100ms → 1s) → page GetQueryResults
//! (1000 rows/page) into `QueryEvent::Rows`. Types from `ResultSetMetadata.ColumnInfo` map to the
//! same Cell encoding as Postgres (bigint/decimal → strings). Before `StatementDone`, emit
//! `QueryEvent::Stats { engine: "athena", bytes_scanned, cost_usd_estimate, engine_query_id }`.
//! Cancellation → StopQueryExecution. Multiple statements run sequentially (Athena runs one per call).
//! S3 Tables via Athena uses catalog `s3tablescatalog/<bucket-name>`.
//!
//! # Behaviour worth knowing
//!
//! * **Event contract** (same as `PgHandle::execute_stream`): `Started` first, then per
//!   statement `Columns`, `Rows`*, `Stats`, `StatementDone`, and exactly one of `Finished` /
//!   `Failed` last. `Stats` is sent only for statements that succeeded.
//! * **Header row.** Athena puts the column names in the first row of a query's first result
//!   page. It is dropped only when its cells spell the column names, so a `SHOW` or `DESCRIBE`
//!   that returns no header loses nothing.
//! * **`read_only`.** Athena has no read-only transactions, so the check *is* the enforcement:
//!   every statement must be provably a read before any is submitted (see
//!   [`engine::check_read_only`]). Anything unparsed or ambiguous is refused.
//! * **Writes.** `INSERT` / `UPDATE` / `DELETE` / `MERGE` report `rows_affected` from
//!   `GetQueryResults.UpdateCount` (Athena reports it for Iceberg tables). DDL is not billed, so
//!   its cost estimate is 0.
//! * **Timeouts.** `ExecOptions.timeout_ms` bounds queue plus run time per statement; on expiry
//!   the query is stopped and the statement fails with `CoreError::Timeout`.
//! * **Retries.** Throttling (`TooManyRequestsException` and friends) and transient transport
//!   failures are retried up to 3 times with jittered exponential backoff. All three calls are
//!   idempotent (`StartQueryExecution` carries a `ClientRequestToken`).

mod cells;
mod engine;
mod scan;

// Trino speaks the same dialect: the remote Trino engine shares the read-only check, the
// command tags and the cell typing.
pub(crate) use cells::{base_type, convert_cell};
pub(crate) use engine::check_read_only_as;
pub(crate) use scan::{command_tag, scan as scan_words};

use std::time::{Duration, Instant};

use aws_credential_types::provider::SharedCredentialsProvider;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::{CoreError, Result};
use crate::model::*;

/// Athena list price: US$5 per TB scanned, billed bytes rounded **up to the next MB**, 10 MB
/// minimum per query. AWS bills data volumes in binary units (TB = 2^40 bytes), as used here.
/// An estimate at list price, never a bill: regional prices, capacity reservations and
/// discounts differ.
pub fn cost_estimate_usd(bytes_scanned: u64) -> f64 {
    const MB: u64 = 1024 * 1024;
    const MIN: u64 = 10 * MB;
    let billable = (bytes_scanned.div_ceil(MB) * MB).max(MIN) as f64;
    billable / 1_099_511_627_776.0 * 5.0
}

#[derive(Debug, Clone)]
pub struct AthenaConfig {
    pub region: String,
    pub workgroup: String,
    pub output_location: Option<String>,
    /// Athena catalog: `AwsDataCatalog` (Glue) or `s3tablescatalog/<bucket>` (S3 Tables).
    pub catalog: String,
    /// Default database (= Iceberg namespace) for unqualified names.
    pub database: Option<String>,
}

/// Pacing knobs. The defaults are the production values; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct AthenaTiming {
    /// First `GetQueryExecution` poll delay; doubles each poll up to [`poll_max`](Self::poll_max).
    pub poll_initial: Duration,
    pub poll_max: Duration,
    /// Base of the throttling retry backoff (`base * 2^attempt`, plus up to 50% jitter).
    pub retry_base: Duration,
}

impl Default for AthenaTiming {
    fn default() -> Self {
        Self {
            poll_initial: Duration::from_millis(100),
            poll_max: Duration::from_secs(1),
            retry_base: Duration::from_millis(250),
        }
    }
}

pub struct AthenaEngine {
    cfg: AthenaConfig,
    creds: SharedCredentialsProvider,
    endpoint_url: Option<String>,
    timing: AthenaTiming,
    client: aws_sdk_athena::Client,
}

impl AthenaEngine {
    pub fn new(cfg: AthenaConfig, creds: SharedCredentialsProvider) -> Self {
        let client = engine::build_client(&cfg, &creds, None);
        Self {
            cfg,
            creds,
            endpoint_url: None,
            timing: AthenaTiming::default(),
            client,
        }
    }

    /// Sends every request to `url` instead of the regional Athena endpoint (tests, VPC
    /// endpoints, LocalStack).
    #[must_use]
    pub fn with_endpoint_url(mut self, url: impl Into<String>) -> Self {
        let url = url.into();
        self.client = engine::build_client(&self.cfg, &self.creds, Some(&url));
        self.endpoint_url = Some(url);
        self
    }

    /// Overrides polling and retry pacing.
    #[must_use]
    pub fn with_timing(mut self, timing: AthenaTiming) -> Self {
        self.timing = timing;
        self
    }

    fn session(&self) -> engine::Session<'_> {
        engine::Session {
            client: &self.client,
            cfg: &self.cfg,
            timing: &self.timing,
        }
    }

    /// Same event contract as `PgHandle::execute_stream`. `opts.read_only` → reject non-queries
    /// (via `crate::sql::classify`) before submitting; Athena has no read-only transactions.
    ///
    /// Emits `Started` first and exactly one of `Finished` / `Failed` last (unless the receiver
    /// disappears). Returns `Err` only if `Started` could not be delivered.
    pub async fn execute_stream(
        &self,
        query_id: String,
        sql: &str,
        opts: ExecOptions,
        cancel: CancellationToken,
        tx: mpsc::Sender<QueryEvent>,
    ) -> Result<()> {
        let statements = crate::sql::split_statements(sql);
        tx.send(QueryEvent::Started {
            query_id,
            statement_count: statements.len(),
        })
        .await
        .map_err(|_| CoreError::Other("query result receiver was dropped".into()))?;
        let t0 = Instant::now();
        let end = self.session().run(&statements, &opts, &cancel, &tx).await;
        if let Some(terminal) = end.into_event(engine::elapsed_ms(t0)) {
            // If the receiver vanished since the last event there is nobody to tell.
            let _ = tx.send(terminal).await;
        }
        Ok(())
    }

    /// Bounded, materialised execution: the result of the last statement that returns rows
    /// (or of the last statement when none does). Errors are returned as `Err`.
    pub async fn execute_collect(&self, sql: &str, opts: ExecOptions) -> Result<QueryResult> {
        let statements = crate::sql::split_statements(sql);
        if statements.is_empty() {
            return Ok(QueryResult::default());
        }
        let (tx, mut rx) = mpsc::channel::<QueryEvent>(32);
        let cancel = CancellationToken::new();
        let session = self.session();
        let runner = async {
            let end = session.run(&statements, &opts, &cancel, &tx).await;
            drop(tx);
            end
        };
        let collector = async {
            let mut collected = engine::Collector::default();
            while let Some(ev) = rx.recv().await {
                collected.push(ev);
            }
            collected
        };
        let (end, collected) = tokio::join!(runner, collector);
        match end {
            engine::End::Finished => collected
                .into_result()
                .ok_or_else(|| CoreError::Other("no statement produced a result".into())),
            engine::End::Failed { error, .. } => Err(error),
            engine::End::Gone => Err(CoreError::Other("result collector went away".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TB: f64 = 1_099_511_627_776.0;

    #[test]
    fn cost_has_a_ten_megabyte_minimum() {
        let min = 10.0 * 1024.0 * 1024.0 / TB * 5.0;
        assert_eq!(cost_estimate_usd(0), min);
        assert_eq!(cost_estimate_usd(1), min);
        assert_eq!(cost_estimate_usd(10 * 1024 * 1024), min);
        assert!(cost_estimate_usd(10 * 1024 * 1024 + 1) > min);
    }

    #[test]
    fn billed_bytes_round_up_to_the_next_megabyte() {
        let mb = 1024 * 1024;
        let eleven = 11.0 * mb as f64 / TB * 5.0;
        assert_eq!(cost_estimate_usd(10 * mb + 1), eleven);
        assert_eq!(cost_estimate_usd(11 * mb), eleven);
    }

    #[test]
    fn cost_is_five_dollars_per_tebibyte() {
        assert!((cost_estimate_usd(1_099_511_627_776) - 5.0).abs() < 1e-12);
        assert!((cost_estimate_usd(2 * 1_099_511_627_776) - 10.0).abs() < 1e-12);
        // About half a cent for a gigabyte.
        let gb = cost_estimate_usd(1 << 30);
        assert!((gb - 5.0 / 1024.0).abs() < 1e-9, "{gb}");
    }
}
