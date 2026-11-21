//! The Athena execution loop: submit, poll, page results, map errors. See [`super`] for the
//! public API; everything here is private to the module.

use std::error::Error as StdError;
use std::fmt::Debug;
use std::future::Future;
use std::time::{Duration, Instant};

use aws_sdk_athena::config::{BehaviorVersion, Region, retry::RetryConfig, timeout::TimeoutConfig};
use aws_sdk_athena::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_athena::types::{
    QueryExecution, QueryExecutionContext, QueryExecutionState, ResultConfiguration, StatementType,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::cells::{convert_cell, display_type, is_header_row};
use super::scan;
use super::{AthenaConfig, AthenaTiming, cost_estimate_usd};
use crate::error::{CoreError, ErrorPayload, Result};
use crate::model::*;

/// Rows requested per `GetQueryResults` page (the API maximum).
const RESULTS_PAGE: i32 = 1000;
/// Retries (after the first attempt) for throttling and transient failures.
const MAX_RETRIES: u32 = 3;
/// Error codes that mean "slow down and try again".
const THROTTLE_CODES: &[&str] = &[
    "TooManyRequestsException",
    "ThrottlingException",
    "Throttling",
    "ThrottledException",
    "RequestLimitExceeded",
    "SlowDown",
];

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

type Run<T> = std::result::Result<T, Stop>;

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
    cfg: &AthenaConfig,
    creds: &aws_credential_types::provider::SharedCredentialsProvider,
    endpoint_url: Option<&str>,
) -> aws_sdk_athena::Client {
    let mut b = aws_sdk_athena::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(cfg.region.trim().to_string()))
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
    aws_sdk_athena::Client::from_conf(b.build())
}

/// Reject anything that is not provably a read. Fails closed: statements the PostgreSQL-dialect
/// classifier cannot parse are admitted only by the narrow text check in [`scan`].
pub(super) fn check_read_only(statements: &[String]) -> Result<()> {
    check_read_only_as(statements, "Athena")
}

/// [`check_read_only`] for any Trino-dialect engine (Athena, Trino); `engine` names it in the
/// refusal.
pub(crate) fn check_read_only_as(statements: &[String], engine: &str) -> Result<()> {
    for (i, stmt) in statements.iter().enumerate() {
        let c = crate::sql::classify(stmt);
        let ok = match c.overall {
            crate::sql::StatementClass::ReadOnly => c.parse_ok,
            // Trino-only syntax the PostgreSQL parser rejects, or `SHOW` / `DESCRIBE` forms it
            // parses but does not recognise. A parsed query the classifier flagged (a call to a
            // side-effecting function) stays denied.
            crate::sql::StatementClass::Unknown => {
                (!c.parse_ok || scan::is_show_or_describe(stmt)) && scan::provably_read_only(stmt)
            }
            _ => false,
        };
        if !ok {
            let which = if statements.len() > 1 {
                format!("statement {} of {}: ", i + 1, statements.len())
            } else {
                String::new()
            };
            return Err(CoreError::PolicyDenied(format!(
                "read-only run: {which}{} is not a plain query. {engine} has no read-only transactions, \
                 so only SELECT, WITH, SHOW and DESCRIBE statements are submitted",
                c.summary
            )));
        }
    }
    Ok(())
}

pub(super) struct Session<'a> {
    pub client: &'a aws_sdk_athena::Client,
    pub cfg: &'a AthenaConfig,
    pub timing: &'a AthenaTiming,
}

impl Session<'_> {
    /// Runs `statements` in order, streaming events. `Started` and the terminal event belong to
    /// the caller.
    pub(super) async fn run(
        &self,
        statements: &[String],
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> End {
        if opts.read_only
            && let Err(error) = check_read_only(statements)
        {
            return End::Failed {
                statement_index: None,
                error,
            };
        }
        for (idx, stmt) in statements.iter().enumerate() {
            match self.run_statement(idx, stmt, opts, cancel, tx).await {
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
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> Run<()> {
        if cancel.is_cancelled() {
            return Err(Stop::Err(CoreError::Cancelled));
        }
        let t0 = Instant::now();
        let id = self.start(stmt, cancel).await?;
        let res = self.drive(&id, idx, stmt, opts, t0, cancel, tx).await;
        // Whatever ended the statement early, don't leave a query running (and billing) behind.
        if matches!(
            res,
            Err(Stop::Gone | Stop::Err(CoreError::Cancelled | CoreError::Timeout(_)))
        ) {
            self.stop_best_effort(&id).await;
        }
        res
    }

    async fn start(&self, stmt: &str, cancel: &CancellationToken) -> Run<String> {
        // One token per submission, reused across retries: if a throttled-looking failure
        // actually reached Athena, the retry returns the same execution instead of a second one.
        let token = uuid::Uuid::new_v4().to_string();
        let mut ctx = QueryExecutionContext::builder().catalog(self.cfg.catalog.trim());
        if let Some(db) = self
            .cfg
            .database
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            ctx = ctx.database(db);
        }
        let ctx = ctx.build();
        let result_cfg = self
            .cfg
            .output_location
            .as_deref()
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .map(|o| ResultConfiguration::builder().output_location(o).build());
        let out = self
            .call(cancel, "StartQueryExecution", false, || {
                let mut req = self
                    .client
                    .start_query_execution()
                    .query_string(stmt)
                    .client_request_token(&token)
                    .query_execution_context(ctx.clone());
                if !self.cfg.workgroup.trim().is_empty() {
                    req = req.work_group(self.cfg.workgroup.trim());
                }
                if let Some(rc) = &result_cfg {
                    req = req.result_configuration(rc.clone());
                }
                req.send()
            })
            .await?;
        match out.query_execution_id() {
            Some(id) if !id.is_empty() => Ok(id.to_string()),
            _ => Err(CoreError::Aws(
                "Athena StartQueryExecution returned no query execution id".into(),
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
        let qe = self
            .wait_done(id, deadline, opts.timeout_ms, cancel)
            .await?;
        let stype = qe.statement_type().cloned();
        let is_ddl = matches!(stype, Some(StatementType::Ddl));

        let sum = if is_ddl {
            // DDL has no result set to read.
            ResultSummary::default()
        } else {
            self.read_results(id, idx, stmt, opts, cancel, tx).await?
        };

        let bytes = qe
            .statistics()
            .and_then(|s| s.data_scanned_in_bytes())
            .map(|b| u64::try_from(b).unwrap_or(0));
        let cost = if is_ddl {
            Some(0.0)
        } else {
            bytes.map(cost_estimate_usd)
        };
        emit(
            tx,
            QueryEvent::Stats {
                statement_index: idx,
                engine: "athena".to_string(),
                bytes_scanned: bytes,
                cost_usd_estimate: cost,
                engine_query_id: Some(id.to_string()),
            },
        )
        .await?;
        emit(
            tx,
            QueryEvent::StatementDone {
                statement_index: idx,
                command_tag: scan::command_tag(stmt),
                rows_affected: sum.rows_affected,
                row_count: sum.row_count,
                truncated: sum.truncated,
                elapsed_ms: elapsed_ms(t0),
            },
        )
        .await
    }

    /// Polls `GetQueryExecution` (100 ms doubling to 1 s) until the query leaves QUEUED/RUNNING.
    async fn wait_done(
        &self,
        id: &str,
        deadline: Option<Instant>,
        timeout_ms: Option<u64>,
        cancel: &CancellationToken,
    ) -> Run<QueryExecution> {
        let mut delay = self.timing.poll_initial;
        loop {
            let out = self
                .call(cancel, "GetQueryExecution", true, || {
                    self.client
                        .get_query_execution()
                        .query_execution_id(id)
                        .send()
                })
                .await?;
            let qe = out.query_execution().cloned().ok_or_else(|| {
                CoreError::Aws("Athena GetQueryExecution returned no query execution".into())
            })?;
            let status = qe.status();
            match status.and_then(|s| s.state()) {
                Some(QueryExecutionState::Succeeded) => return Ok(qe),
                Some(QueryExecutionState::Failed) => {
                    return Err(CoreError::Database(failure_message(&qe)).into());
                }
                Some(QueryExecutionState::Cancelled) => return Err(CoreError::Cancelled.into()),
                Some(QueryExecutionState::Queued | QueryExecutionState::Running) | None => {}
                Some(other) => {
                    return Err(CoreError::Aws(format!(
                        "Athena reported an unknown query state '{}'",
                        other.as_str()
                    ))
                    .into());
                }
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(CoreError::Timeout(format!(
                    "Athena query still running after {} ms; stopped it",
                    timeout_ms.unwrap_or_default()
                ))
                .into());
            }
            sleep_or_cancel(delay, cancel).await?;
            delay = (delay * 2).min(self.timing.poll_max);
        }
    }

    /// Pages `GetQueryResults`, sending `Columns` once and `Rows` in `page_size` chunks.
    async fn read_results(
        &self,
        id: &str,
        idx: usize,
        stmt: &str,
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> Run<ResultSummary> {
        let page_size = opts.page_size.max(1);
        let is_write = scan::is_row_write(stmt);
        let mut sum = ResultSummary::default();
        let mut types: Vec<String> = Vec::new();
        let mut names: Vec<(String, Option<String>)> = Vec::new();
        let mut buf: Vec<Vec<Cell>> = Vec::new();
        let mut token: Option<String> = None;
        let mut first = true;

        loop {
            let out = self
                .call(cancel, "GetQueryResults", true, || {
                    self.client
                        .get_query_results()
                        .query_execution_id(id)
                        .max_results(RESULTS_PAGE)
                        .set_next_token(token.clone())
                        .send()
                })
                .await?;
            let mut skip = 0;
            if first {
                if is_write {
                    // Athena reports the affected-row count for Iceberg DML here.
                    sum.rows_affected = out.update_count().and_then(|n| u64::try_from(n).ok());
                    return Ok(sum);
                }
                let meta = out.result_set().and_then(|r| r.result_set_metadata());
                let infos = meta.map(|m| m.column_info()).unwrap_or_default();
                if infos.is_empty() {
                    // No tabular result (SHOW-less utility, DDL-like): a positive update count
                    // is the only thing worth reporting.
                    sum.rows_affected = out
                        .update_count()
                        .and_then(|n| u64::try_from(n).ok())
                        .filter(|n| *n > 0);
                    return Ok(sum);
                }
                let mut columns = Vec::with_capacity(infos.len());
                for info in infos {
                    types.push(info.r#type().to_string());
                    names.push((info.name().to_string(), info.label().map(str::to_string)));
                    columns.push(ColumnMeta {
                        name: info.name().to_string(),
                        type_name: display_type(info.r#type(), info.precision(), info.scale()),
                        type_oid: 0,
                    });
                }
                emit(
                    tx,
                    QueryEvent::Columns {
                        statement_index: idx,
                        columns,
                    },
                )
                .await?;
                // The first row of the first page is the header, for queries that have one.
                if let Some(row0) = out.result_set().and_then(|r| r.rows().first()) {
                    let cells: Vec<Option<&str>> =
                        row0.data().iter().map(|d| d.var_char_value()).collect();
                    if is_header_row(&names, &cells) {
                        skip = 1;
                    }
                }
                first = false;
            }

            let rows = out.result_set().map(|r| r.rows()).unwrap_or_default();
            for row in rows.iter().skip(skip) {
                if sum.row_count >= opts.max_rows {
                    sum.truncated = true;
                    break;
                }
                buf.push(
                    types
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            convert_cell(t, row.data().get(i).and_then(|d| d.var_char_value()))
                        })
                        .collect(),
                );
                sum.row_count += 1;
                if buf.len() >= page_size {
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
            token = out.next_token().map(str::to_string);
            if sum.truncated {
                break;
            }
            if token.is_none() {
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

    async fn stop_best_effort(&self, id: &str) {
        let stop = self
            .client
            .stop_query_execution()
            .query_execution_id(id)
            .send();
        // Best effort: a failure here must not mask the reason the statement ended.
        match tokio::time::timeout(Duration::from_secs(10), stop).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(
                "Athena StopQueryExecution failed: {}",
                DisplayErrorContext(&e)
            ),
            Err(_) => tracing::warn!("Athena StopQueryExecution timed out"),
        }
    }

    /// One SDK call with retries for throttling and transient failures (up to
    /// [`MAX_RETRIES`], exponential backoff with jitter). With `race_cancel`, a cancellation
    /// abandons an in-flight request; without it (used for submission, where abandoning could
    /// leave an unknown running query behind) cancellation is checked between attempts.
    async fn call<T, E, R, F, Fut>(
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
                        "Athena {op} throttled or failed transiently; retry {} in {wait:?}",
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
        SdkError::ResponseError(_) | SdkError::ServiceError(_) => false,
        _ => false,
    }
}

/// `CoreError::Aws("Athena <op> failed: <Code>: <message>")`, or `Auth` when the failure came
/// from our credentials provider. Never includes request payloads or credentials.
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
    match (err.code(), err.message()) {
        (Some(code), Some(msg)) => CoreError::Aws(format!("Athena {op} failed: {code}: {msg}")),
        (Some(code), None) => CoreError::Aws(format!("Athena {op} failed: {code}")),
        _ => CoreError::Aws(format!("Athena {op} failed: {}", DisplayErrorContext(err))),
    }
}

/// `StateChangeReason`, plus `AthenaError.ErrorMessage` when it adds something.
fn failure_message(qe: &QueryExecution) -> String {
    let status = qe.status();
    let reason = status
        .and_then(|s| s.state_change_reason())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let detail = status
        .and_then(|s| s.athena_error())
        .and_then(|a| a.error_message())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (reason, detail) {
        (Some(r), Some(d)) if !r.contains(d) => format!("Athena query failed: {r} ({d})"),
        (Some(r), _) => format!("Athena query failed: {r}"),
        (None, Some(d)) => format!("Athena query failed: {d}"),
        (None, None) => "Athena query failed (no reason given)".to_string(),
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
