//! The Redshift Data API loop: submit, poll, page results, cancel, map errors. See [`super`] for
//! the public API; everything here is private to the module.

use std::error::Error as StdError;
use std::fmt::Debug;
use std::future::Future;
use std::time::{Duration, Instant};

use aws_sdk_redshiftdata::config::{
    BehaviorVersion, Region, retry::RetryConfig, timeout::TimeoutConfig,
};
use aws_sdk_redshiftdata::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_redshiftdata::operation::describe_statement::DescribeStatementOutput;
use aws_sdk_redshiftdata::types::StatusString;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::cells::{Raw, convert_cell, display_type};
use super::check;
use super::{RedshiftTiming, with_target};
use crate::error::{CoreError, ErrorPayload};
use crate::model::*;

/// Retries (after the first attempt) for throttling and transient failures.
const MAX_RETRIES: u32 = 3;
/// Error codes that mean "slow down and try again".
const THROTTLE_CODES: &[&str] = &[
    "ThrottlingException",
    "Throttling",
    "ThrottledException",
    "TooManyRequestsException",
    "RequestLimitExceeded",
    "SlowDown",
];
/// Error text from Redshift is cut to this many bytes (R6): it can quote whole statements and
/// values, and it travels to the UI and, redacted, to the agent.
pub(super) const MAX_ERROR_BYTES: usize = 4 * 1024;
/// Shown in the Data API's statement list and CloudTrail, so a statement can be traced to the app.
const STATEMENT_NAME: &str = "vibelens";

/// Why a statement stopped without finishing.
pub(super) enum Stop {
    Err(CoreError),
    /// The event receiver went away: nobody is listening any more.
    Gone,
}

impl From<CoreError> for Stop {
    fn from(e: CoreError) -> Self {
        Stop::Err(e)
    }
}

pub(super) type Run<T> = std::result::Result<T, Stop>;

impl Stop {
    pub(super) fn into_core(self) -> CoreError {
        match self {
            Stop::Err(e) => e,
            Stop::Gone => CoreError::Cancelled,
        }
    }
}

/// How a script ended.
pub(super) enum End {
    Finished,
    Failed {
        statement_index: Option<usize>,
        error: CoreError,
    },
    Gone,
}

impl End {
    pub(super) fn into_event(self, elapsed_ms: u64) -> Option<QueryEvent> {
        match self {
            End::Finished => Some(QueryEvent::Finished { elapsed_ms }),
            End::Failed {
                statement_index,
                error,
            } => Some(QueryEvent::Failed {
                statement_index,
                error: ErrorPayload::from(&error),
            }),
            End::Gone => None,
        }
    }
}

pub(super) fn build_client(
    region: &str,
    creds: &aws_credential_types::provider::SharedCredentialsProvider,
    endpoint_url: Option<&str>,
) -> aws_sdk_redshiftdata::Client {
    let mut b = aws_sdk_redshiftdata::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(region.trim().to_string()))
        .credentials_provider(creds.clone())
        // Retries are ours (see `Session::call`): the SDK's would hide how many requests we sent
        // and could not be told to stop when the user cancels.
        .retry_config(RetryConfig::disabled())
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(Duration::from_secs(10))
                .operation_attempt_timeout(Duration::from_secs(60))
                .build(),
        );
    if let Some(url) = endpoint_url {
        b = b.endpoint_url(url);
    }
    aws_sdk_redshiftdata::Client::from_conf(b.build())
}

pub(super) struct Session<'a> {
    pub client: &'a aws_sdk_redshiftdata::Client,
    pub opts: &'a RedshiftOptions,
    pub timing: &'a RedshiftTiming,
}

impl Session<'_> {
    /// Runs `statements` in order in `database`, streaming events. `Started` and the terminal
    /// event belong to the caller. Every check that can refuse the script runs before the first
    /// request.
    pub(super) async fn run(
        &self,
        statements: &[String],
        database: &str,
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> End {
        let checked = check::check_no_transaction_control(statements).and_then(|()| {
            if opts.read_only {
                check::check_read_only(statements)
            } else {
                Ok(())
            }
        });
        if let Err(error) = checked {
            return End::Failed {
                statement_index: None,
                error,
            };
        }
        for (idx, stmt) in statements.iter().enumerate() {
            match self
                .run_statement(idx, stmt, database, opts, cancel, tx)
                .await
            {
                Ok(()) => {}
                Err(Stop::Gone) => return End::Gone,
                Err(Stop::Err(error)) => {
                    return End::Failed {
                        statement_index: Some(idx),
                        error,
                    };
                }
            }
        }
        End::Finished
    }

    async fn run_statement(
        &self,
        idx: usize,
        stmt: &str,
        database: &str,
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> Run<()> {
        if cancel.is_cancelled() {
            return Err(Stop::Err(CoreError::Cancelled));
        }
        let t0 = Instant::now();
        let id = self.start(stmt, database, cancel).await?;
        let res = self.drive(&id, idx, stmt, opts, t0, cancel, tx).await;
        // Whatever ended the statement early, stop it on the server too (R8): a cancelled or
        // timed-out statement must not keep running, holding locks or committing later.
        if matches!(
            res,
            Err(Stop::Gone | Stop::Err(CoreError::Cancelled | CoreError::Timeout(_)))
        ) {
            self.cancel_best_effort(&id).await;
        }
        res
    }

    async fn start(&self, stmt: &str, database: &str, cancel: &CancellationToken) -> Run<String> {
        // One token per submission, reused across retries: if a throttled-looking failure
        // actually reached Redshift, the retry returns the same statement instead of running
        // it a second time.
        let token = uuid::Uuid::new_v4().to_string();
        let out = self
            .call(cancel, "ExecuteStatement", false, || {
                let req = self
                    .client
                    .execute_statement()
                    .sql(stmt)
                    .client_token(&token)
                    .statement_name(STATEMENT_NAME);
                with_target!(req, self.opts, database).send()
            })
            .await?;
        match out.id() {
            Some(id) if !id.is_empty() => Ok(id.to_string()),
            _ => Err(CoreError::Aws(
                "Redshift Data API ExecuteStatement returned no statement id".into(),
            )
            .into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn drive(
        &self,
        id: &str,
        idx: usize,
        stmt: &str,
        opts: &ExecOptions,
        t0: Instant,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> Run<()> {
        let deadline = opts.timeout_ms.map(|ms| t0 + Duration::from_millis(ms));
        let desc = self
            .wait_done(id, deadline, opts.timeout_ms, cancel)
            .await?;

        let sum = if desc.has_result_set() == Some(true) {
            self.read_results(id, idx, opts, cancel, tx).await?
        } else {
            // `ResultRows` is the affected-row count for writes and -1 when Redshift has none.
            let n = u64::try_from(desc.result_rows()).ok();
            ResultSummary {
                rows_affected: n.filter(|n| *n > 0 || is_row_write(stmt)),
                ..ResultSummary::default()
            }
        };

        let query_id = match desc.redshift_query_id() {
            n if n > 0 => n.to_string(),
            _ => id.to_string(),
        };
        emit(
            tx,
            QueryEvent::Stats {
                statement_index: idx,
                engine: "redshift".to_string(),
                bytes_scanned: None,
                cost_usd_estimate: None,
                engine_query_id: Some(query_id),
            },
        )
        .await?;
        emit(
            tx,
            QueryEvent::StatementDone {
                statement_index: idx,
                command_tag: check::command_tag(stmt),
                rows_affected: sum.rows_affected,
                row_count: sum.row_count,
                truncated: sum.truncated,
                elapsed_ms: elapsed_ms(t0),
            },
        )
        .await
    }

    /// Polls `DescribeStatement` (100 ms doubling to 1 s) until the statement leaves
    /// SUBMITTED / PICKED / STARTED.
    async fn wait_done(
        &self,
        id: &str,
        deadline: Option<Instant>,
        timeout_ms: Option<u64>,
        cancel: &CancellationToken,
    ) -> Run<DescribeStatementOutput> {
        let mut delay = self.timing.poll_initial;
        loop {
            let out = self
                .call(cancel, "DescribeStatement", true, || {
                    self.client.describe_statement().id(id).send()
                })
                .await?;
            match out.status() {
                Some(StatusString::Finished) => return Ok(out),
                Some(StatusString::Failed) => {
                    return Err(CoreError::Database(failure_message(out.error())).into());
                }
                // Cancelled on the server, by someone else or by an earlier attempt.
                Some(StatusString::Aborted) => return Err(CoreError::Cancelled.into()),
                Some(StatusString::Submitted | StatusString::Picked | StatusString::Started)
                | None => {}
                Some(other) => {
                    return Err(CoreError::Aws(format!(
                        "Redshift reported an unknown statement status '{}'",
                        bounded(other.as_str(), 64)
                    ))
                    .into());
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(CoreError::Timeout(format!(
                    "Redshift statement still running after {} ms; cancelled it",
                    timeout_ms.unwrap_or_default()
                ))
                .into());
            }
            sleep_or_cancel(delay, cancel).await?;
            delay = (delay * 2).min(self.timing.poll_max);
        }
    }

    /// Pages `GetStatementResult`, sending `Columns` once and `Rows` in chunks of `page_size`
    /// rows or [`PAGE_BYTE_BUDGET`] bytes, whichever comes first. Stops at `max_rows`.
    async fn read_results(
        &self,
        id: &str,
        idx: usize,
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> Run<ResultSummary> {
        let page_size = opts.page_size.max(1);
        let mut sum = ResultSummary::default();
        let mut types: Vec<String> = Vec::new();
        let mut buf: Vec<Vec<Cell>> = Vec::new();
        let mut buf_bytes = 0usize;
        let mut token: Option<String> = None;
        let mut first = true;

        loop {
            let out = self
                .call(cancel, "GetStatementResult", true, || {
                    self.client
                        .get_statement_result()
                        .id(id)
                        .set_next_token(token.clone())
                        .send()
                })
                .await?;
            if first {
                let meta = out.column_metadata();
                let mut columns = Vec::with_capacity(meta.len());
                for m in meta {
                    let ty = m.type_name().unwrap_or("").to_string();
                    columns.push(ColumnMeta {
                        name: m.label().or(m.name()).unwrap_or("").to_string(),
                        type_name: display_type(&ty, m.precision(), m.scale()),
                        type_oid: 0,
                    });
                    types.push(ty);
                }
                emit(
                    tx,
                    QueryEvent::Columns {
                        statement_index: idx,
                        columns,
                    },
                )
                .await?;
                first = false;
            }

            for record in out.records() {
                if sum.row_count >= opts.max_rows {
                    sum.truncated = true;
                    break;
                }
                let row: Vec<Cell> = types
                    .iter()
                    .enumerate()
                    .map(|(i, t)| match record.get(i) {
                        Some(f) => convert_cell(t, Raw::from(f)),
                        None => Cell::Null,
                    })
                    .collect();
                buf_bytes += row.iter().map(cell_weight).sum::<usize>();
                buf.push(row);
                sum.row_count += 1;
                if buf.len() >= page_size || buf_bytes >= PAGE_BYTE_BUDGET {
                    buf_bytes = 0;
                    emit(
                        tx,
                        QueryEvent::Rows {
                            statement_index: idx,
                            rows: std::mem::take(&mut buf),
                        },
                    )
                    .await?;
                }
            }
            token = out
                .next_token()
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            if sum.truncated || token.is_none() {
                break;
            }
            if sum.row_count >= opts.max_rows {
                // More pages exist but we are at the cap.
                sum.truncated = true;
                break;
            }
        }
        if !buf.is_empty() {
            emit(
                tx,
                QueryEvent::Rows {
                    statement_index: idx,
                    rows: buf,
                },
            )
            .await?;
        }
        Ok(sum)
    }

    async fn cancel_best_effort(&self, id: &str) {
        let stop = self.client.cancel_statement().id(id).send();
        // Best effort: a failure here must not mask the reason the statement ended.
        match tokio::time::timeout(Duration::from_secs(10), stop).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(
                "Redshift CancelStatement failed: {}",
                bounded(&DisplayErrorContext(&e).to_string(), MAX_ERROR_BYTES)
            ),
            Err(_) => tracing::warn!("Redshift CancelStatement timed out"),
        }
    }

    /// One SDK call with retries for throttling and transient failures (up to
    /// [`MAX_RETRIES`], exponential backoff with jitter). With `race_cancel`, a cancellation
    /// abandons an in-flight request; without it (submission, where abandoning could leave a
    /// statement running that we have no id for) cancellation is checked between attempts.
    pub(super) async fn call<T, E, R, F, Fut>(
        &self,
        cancel: &CancellationToken,
        op: &'static str,
        race_cancel: bool,
        mut f: F,
    ) -> Run<T>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<T, SdkError<E, R>>>,
        E: StdError + ProvideErrorMetadata + 'static,
        R: Debug + 'static,
    {
        let mut attempt = 0u32;
        loop {
            let res = if race_cancel {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(Stop::Err(CoreError::Cancelled)),
                    r = f() => r,
                }
            } else {
                f().await
            };
            match res {
                Ok(v) => return Ok(v),
                Err(e) if attempt < MAX_RETRIES && is_retryable(&e) => {
                    let wait = backoff(self.timing.retry_base, attempt);
                    tracing::debug!(
                        "Redshift {op} throttled or failed transiently; retry {} in {wait:?}",
                        attempt + 1
                    );
                    attempt += 1;
                    sleep_or_cancel(wait, cancel).await?;
                }
                Err(e) => return Err(Stop::Err(map_sdk_error(op, &e))),
            }
        }
    }
}

#[derive(Default)]
struct ResultSummary {
    row_count: usize,
    truncated: bool,
    rows_affected: Option<u64>,
}

/// Statements whose affected-row count is worth reporting even when it is 0.
fn is_row_write(stmt: &str) -> bool {
    matches!(
        check::command_tag(stmt).as_deref(),
        Some("INSERT" | "UPDATE" | "DELETE" | "MERGE" | "COPY")
    )
}

async fn emit(tx: &mpsc::Sender<QueryEvent>, ev: QueryEvent) -> Run<()> {
    tx.send(ev).await.map_err(|_| Stop::Gone)
}

pub(super) fn elapsed_ms(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
}

async fn sleep_or_cancel(d: Duration, cancel: &CancellationToken) -> Run<()> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(Stop::Err(CoreError::Cancelled)),
        _ = tokio::time::sleep(d) => Ok(()),
    }
}

/// `base * 2^attempt` plus up to 50% jitter.
fn backoff(base: Duration, attempt: u32) -> Duration {
    let exp = base.saturating_mul(1u32 << attempt.min(8));
    let r = (uuid::Uuid::new_v4().as_u128() % 1000) as u32;
    exp + exp.mul_f64(f64::from(r) / 2000.0)
}

fn is_retryable<E, R>(e: &SdkError<E, R>) -> bool
where
    E: ProvideErrorMetadata,
{
    if e.code().is_some_and(|c| THROTTLE_CODES.contains(&c)) {
        return true;
    }
    match e {
        SdkError::TimeoutError(_) => true,
        SdkError::DispatchFailure(d) => d.is_timeout() || d.is_io(),
        _ => false,
    }
}

/// `s` cut to at most `max` bytes at a character boundary, with a marker when cut.
pub(super) fn bounded(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [cut, {} bytes in all]", &s[..end], s.len())
}

/// `CoreError::Aws("Redshift Data API <op> failed: <Code>: <message>")`, or `Auth` when the
/// failure came from our credentials provider. Never includes request payloads or credentials,
/// and the message is bounded.
pub(super) fn map_sdk_error<E, R>(op: &str, err: &SdkError<E, R>) -> CoreError
where
    E: StdError + ProvideErrorMetadata + 'static,
    R: Debug + 'static,
{
    let mut cur: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(CoreError::Auth(m)) = e.downcast_ref::<CoreError>() {
            return CoreError::Auth(m.clone());
        }
        cur = e.source();
    }
    let text = match (err.code(), err.message()) {
        (Some(code), Some(msg)) => format!("Redshift Data API {op} failed: {code}: {msg}"),
        (Some(code), None) => format!("Redshift Data API {op} failed: {code}"),
        _ => format!(
            "Redshift Data API {op} failed: {}",
            DisplayErrorContext(err)
        ),
    };
    CoreError::Aws(bounded(&text, MAX_ERROR_BYTES))
}

/// `DescribeStatement.Error`, bounded.
fn failure_message(error: Option<&str>) -> String {
    match error.map(str::trim).filter(|s| !s.is_empty()) {
        Some(e) => bounded(&format!("Redshift statement failed: {e}"), MAX_ERROR_BYTES),
        None => "Redshift statement failed (no reason given)".to_string(),
    }
}

/// Folds an event stream into the result of the last row-returning statement.
#[derive(Default)]
pub(super) struct Collector {
    current: QueryResult,
    last_rows: Option<QueryResult>,
    last_any: Option<QueryResult>,
}

impl Collector {
    pub(super) fn push(&mut self, ev: QueryEvent) {
        match ev {
            QueryEvent::Columns { columns, .. } => {
                self.current = QueryResult {
                    columns,
                    ..QueryResult::default()
                }
            }
            QueryEvent::Rows { rows, .. } => self.current.rows.extend(rows),
            QueryEvent::StatementDone {
                command_tag,
                rows_affected,
                row_count,
                truncated,
                elapsed_ms,
                ..
            } => {
                let mut done = std::mem::take(&mut self.current);
                done.command_tag = command_tag;
                done.rows_affected = rows_affected;
                done.row_count = row_count;
                done.truncated = truncated;
                done.elapsed_ms = elapsed_ms;
                if done.columns.is_empty() {
                    self.last_any = Some(done);
                } else {
                    self.last_rows = Some(done);
                }
            }
            _ => {}
        }
    }

    pub(super) fn into_result(self) -> Option<QueryResult> {
        self.last_rows.or(self.last_any)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_cuts_at_a_character_boundary() {
        assert_eq!(bounded("short", 10), "short");
        let s = "é".repeat(10); // 20 bytes
        let b = bounded(&s, 5);
        assert!(b.starts_with("éé…"), "{b}");
        assert!(b.contains("20 bytes"));
    }

    #[test]
    fn failure_messages() {
        assert_eq!(
            failure_message(None),
            "Redshift statement failed (no reason given)"
        );
        assert!(failure_message(Some("ERROR: relation \"x\" does not exist")).contains("relation"));
        assert!(failure_message(Some(&"x".repeat(10_000))).len() < MAX_ERROR_BYTES + 100);
    }

    #[test]
    fn row_writes_report_zero_rows_affected() {
        assert!(is_row_write("insert into t values (1)"));
        assert!(is_row_write("COPY t FROM 's3://b/k' IAM_ROLE default"));
        assert!(!is_row_write("CREATE TABLE t (a int)"));
        assert!(!is_row_write("SELECT 1"));
    }
}
