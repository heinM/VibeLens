//! Trino through its client REST protocol.
//!
//! `POST {url}/v1/statement` with the SQL as the body, then `GET nextUri` until an answer has
//! none. Columns come from `columns`, rows from `data`, the end state and `processedBytes` from
//! `stats`, failures from `error`. One statement per request; a script runs its statements in
//! order and stops at the first failure. Cancel (and timeout) is `DELETE` on the last
//! `nextUri`.
//!
//! # Behaviour worth knowing
//!
//! * **`nextUri` is checked.** It must have the scheme, host and port of the configured URL, or
//!   the statement fails before it is requested. The usual cause is a TLS-terminating load
//!   balancer in front of a coordinator without `http-server.process-forwarded=true`.
//! * **Session headers from the server are ignored.** `X-Trino-Set-Session`,
//!   `X-Trino-Set-Catalog`, `X-Trino-Started-Transaction-Id` and the like are never read, so
//!   each statement starts from the profile. Statements that only make sense with them (`USE`,
//!   `SET SESSION`, `START TRANSACTION`, …) are refused before anything is sent.
//! * **Retries.** `GET nextUri` is retried on HTTP 502, 503 and 504 (Trino documents this) up to
//!   3 times with jittered backoff. `POST /v1/statement` is never retried: it is not idempotent.
//! * **Writes.** A statement Trino reports with an `updateType` and `updateCount` (INSERT,
//!   DELETE, MERGE, CTAS, …) reports `rows_affected` and no result set.

use std::time::{Duration, Instant};

use base64::Engine as _;
use reqwest::Method;
use secrecy::SecretString;
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::RemoteTiming;
use super::common::{
    End, RowBuf, Run, Script, Stop, backoff, cap_cell, collect_script, elapsed_ms, emit,
    sleep_or_cancel, stream_script, which,
};
use super::http::{Body, HttpClient, HttpLimits, MAX_ERROR_TEXT, bounded, same_endpoint, shown};
use crate::error::{CoreError, Result};
use crate::model::*;

/// Retries (after the first attempt) of `GET nextUri` on 502, 503 and 504.
const MAX_RETRIES: u32 = 3;

#[derive(Debug, Clone)]
pub struct TrinoConfig {
    /// Coordinator base URL.
    pub url: String,
    /// `X-Trino-Catalog`.
    pub catalog: String,
    /// `X-Trino-Schema`: the connection's default namespace.
    pub schema: Option<String>,
    /// `X-Trino-User`.
    pub user: String,
    pub auth: HttpEngineAuth,
    pub ca_cert_path: Option<String>,
}

pub struct TrinoEngine {
    cfg: TrinoConfig,
    client: HttpClient,
    timing: RemoteTiming,
}

impl TrinoEngine {
    /// `secret` is the password (basic) or token (bearer) from the vault; ignored for `none`.
    pub fn new(cfg: TrinoConfig, secret: Option<SecretString>) -> Result<Self> {
        Self::with_limits(cfg, secret, HttpLimits::default())
    }

    /// [`new`](Self::new) with other HTTP limits (tests).
    pub fn with_limits(
        cfg: TrinoConfig,
        secret: Option<SecretString>,
        limits: HttpLimits,
    ) -> Result<Self> {
        let client = HttpClient::new(
            "Trino",
            &cfg.url,
            &cfg.auth,
            secret,
            cfg.ca_cert_path.as_deref(),
            limits,
        )?;
        Ok(Self {
            cfg,
            client,
            timing: RemoteTiming::default(),
        })
    }

    /// Overrides polling and retry pacing.
    #[must_use]
    pub fn with_timing(mut self, timing: RemoteTiming) -> Self {
        self.timing = timing;
        self
    }

    /// Same event contract as `PgHandle::execute_stream`. `opts.read_only` → reject anything
    /// that is not provably a read before submitting; Trino has no read-only transactions.
    pub async fn execute_stream(
        &self,
        query_id: String,
        sql: &str,
        opts: ExecOptions,
        cancel: CancellationToken,
        tx: mpsc::Sender<QueryEvent>,
    ) -> Result<()> {
        stream_script(self, query_id, sql, opts, cancel, tx).await
    }

    /// Bounded, materialised execution: the result of the last statement that returns rows
    /// (or of the last statement when none does). Errors are returned as `Err`.
    pub async fn execute_collect(&self, sql: &str, opts: ExecOptions) -> Result<QueryResult> {
        collect_script(self, sql, opts).await
    }

    /// Headers every request carries. Nothing the server sent back is added.
    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut h = vec![
            ("x-trino-user", self.cfg.user.trim().to_string()),
            ("x-trino-source", "vibelens".to_string()),
            ("x-trino-catalog", self.cfg.catalog.trim().to_string()),
        ];
        if let Some(s) = self
            .cfg
            .schema
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            h.push(("x-trino-schema", s.to_string()));
        }
        h
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
        let deadline = opts.timeout_ms.map(|ms| t0 + Duration::from_millis(ms));
        let mut q = Query::default();
        // Not raced against cancellation: abandoning the submission could leave a query running
        // that we have no URL for. The request timeout bounds it.
        let url = self.client.url("/v1/statement").map_err(Stop::Err)?;
        let resp = self
            .client
            .send(
                Method::POST,
                &url,
                &self.headers(),
                Some(Body::Text(stmt.to_string())),
            )
            .await?;
        if !resp.is_success() {
            return Err(self.client.status_error("the statement", &resp).into());
        }
        let first = parse(&resp.body, self.client.label())?;
        let res = self
            .drive(first, &mut q, idx, stmt, opts, t0, deadline, cancel, tx)
            .await;
        // Whatever ended the statement early, do not leave the query running on the cluster.
        let early = matches!(
            res,
            Err(Stop::Gone | Stop::Err(CoreError::Cancelled | CoreError::Timeout(_)))
        ) || matches!(res, Ok(Done::Truncated));
        if early && let Some(next) = q.next_uri.as_ref() {
            self.cancel_best_effort(next).await;
        }
        res.map(|_| ())
    }

    #[allow(clippy::too_many_arguments)]
    async fn drive(
        &self,
        mut page: Value,
        q: &mut Query,
        idx: usize,
        stmt: &str,
        opts: &ExecOptions,
        t0: Instant,
        deadline: Option<Instant>,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> Run<Done> {
        let mut buf = RowBuf::new(idx, opts.page_size);
        let mut delay = self.timing.poll_initial;
        let mut truncated = false;
        loop {
            q.absorb_meta(&page);
            if let Some(err) = page.get("error").filter(|e| !e.is_null()) {
                return Err(CoreError::Database(self.failure_message(err)).into());
            }
            // Checked before anything else is done with the page. A refused `nextUri` leaves
            // the query to Trino, which abandons it when the client stops polling
            // (`query.client.timeout`): there is no other URL it could be cancelled through.
            q.next_uri = self.next_uri(&page)?;
            let had_data = self.absorb_rows(&page, q, idx, opts, &mut buf, tx).await?;
            if q.row_count >= opts.max_rows && q.more_rows_possible(&page) {
                truncated = true;
            }
            if truncated {
                break;
            }
            let Some(next) = q.next_uri.clone() else {
                break;
            };
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(self.timeout_error(opts).into());
            }
            if had_data {
                delay = self.timing.poll_initial;
            } else {
                // Trino long-polls on its side; a short pause keeps a misbehaving server from
                // turning this into a busy loop.
                sleep_or_cancel(delay, cancel).await?;
                delay = (delay * 2).min(self.timing.poll_max);
            }
            page = self.get(&next, deadline, opts, cancel).await?;
        }
        buf.flush(tx).await?;
        if !truncated
            && let Some(state) = q.state.as_deref()
            && state != "FINISHED"
        {
            return Err(CoreError::Database(format!(
                "Trino query ended in state {}",
                bounded(state, 64)
            ))
            .into());
        }
        emit(
            tx,
            QueryEvent::Stats {
                statement_index: idx,
                engine: "trino".to_string(),
                bytes_scanned: q.processed_bytes,
                cost_usd_estimate: None,
                engine_query_id: q.id.clone(),
            },
        )
        .await?;
        let command_tag = q
            .update_type
            .clone()
            .or_else(|| crate::athena::command_tag(stmt));
        emit(
            tx,
            QueryEvent::StatementDone {
                statement_index: idx,
                command_tag,
                rows_affected: q.update_type.as_ref().and(q.update_count),
                row_count: q.row_count,
                truncated,
                elapsed_ms: elapsed_ms(t0),
            },
        )
        .await?;
        Ok(if truncated {
            Done::Truncated
        } else {
            Done::Complete
        })
    }

    /// Sends `Columns` the first time they appear and the page's rows. Returns whether the page
    /// carried anything new.
    async fn absorb_rows(
        &self,
        page: &Value,
        q: &mut Query,
        idx: usize,
        opts: &ExecOptions,
        buf: &mut RowBuf,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> Run<bool> {
        let mut new = false;
        if !q.columns_seen
            && let Some(cols) = page.get("columns").and_then(Value::as_array)
        {
            q.columns_seen = true;
            new = true;
            // A write (INSERT, CTAS, DELETE, …) reports its count as a one-row result: report
            // it as `rows_affected` instead, as the other engines do.
            q.write = page.get("updateType").is_some_and(Value::is_string);
            if !q.write {
                let mut columns = Vec::with_capacity(cols.len());
                for c in cols {
                    let name = c.get("name").and_then(Value::as_str).unwrap_or("");
                    let ty = c.get("type").and_then(Value::as_str).unwrap_or("unknown");
                    q.types.push(ty.to_string());
                    columns.push(ColumnMeta {
                        name: bounded(name, 1024),
                        type_name: bounded(ty, 1024),
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
            }
        }
        let Some(rows) = page.get("data").and_then(Value::as_array) else {
            return Ok(new);
        };
        if rows.is_empty() {
            return Ok(new);
        }
        if q.write {
            return Ok(true);
        }
        for row in rows {
            if q.row_count >= opts.max_rows {
                q.dropped_rows = true;
                break;
            }
            let values = row.as_array().map(Vec::as_slice).unwrap_or_default();
            let cells = q
                .types
                .iter()
                .enumerate()
                .map(|(i, t)| trino_cell(t, values.get(i).cloned().unwrap_or(Value::Null)))
                .collect();
            buf.push(cells, tx).await?;
            q.row_count += 1;
        }
        Ok(true)
    }

    /// `GET nextUri`, after the endpoint check, raced against cancellation and the deadline,
    /// retried on 502, 503 and 504.
    async fn get(
        &self,
        next: &url::Url,
        deadline: Option<Instant>,
        opts: &ExecOptions,
        cancel: &CancellationToken,
    ) -> Run<Value> {
        let mut attempt = 0u32;
        let headers = self.headers();
        loop {
            let send = self.client.send(Method::GET, next, &headers, None);
            let resp = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(Stop::Err(CoreError::Cancelled)),
                _ = until(deadline) => return Err(self.timeout_error(opts).into()),
                r = send => r?,
            };
            if matches!(resp.status, 502..=504) && attempt < MAX_RETRIES {
                let wait = backoff(self.timing.retry_base, attempt);
                tracing::debug!(
                    "Trino answered HTTP {}; retry {} in {wait:?}",
                    resp.status,
                    attempt + 1
                );
                attempt += 1;
                sleep_or_cancel(wait, cancel).await?;
                continue;
            }
            if !resp.is_success() {
                return Err(self.client.status_error("a result request", &resp).into());
            }
            return Ok(parse(&resp.body, self.client.label())?);
        }
    }

    async fn cancel_best_effort(&self, next: &url::Url) {
        let headers = self.headers();
        let del = self.client.send(Method::DELETE, next, &headers, None);
        // Best effort: a failure here must not mask the reason the statement ended.
        match tokio::time::timeout(self.timing.cancel_timeout, del).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!("Trino cancel failed: {e}"),
            Err(_) => tracing::warn!("Trino cancel timed out"),
        }
    }

    fn timeout_error(&self, opts: &ExecOptions) -> CoreError {
        CoreError::Timeout(format!(
            "Trino query still running after {} ms; cancelled it",
            opts.timeout_ms.unwrap_or_default()
        ))
    }

    /// `Trino query failed: <errorName>: <message>`, redacted and bounded. The Java stack in
    /// `failureInfo` is dropped.
    fn failure_message(&self, err: &Value) -> String {
        let name = err.get("errorName").and_then(Value::as_str);
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|m| !m.is_empty());
        let text = match (name, msg) {
            (Some(n), Some(m)) => format!("{n}: {m}"),
            (None, Some(m)) => m.to_string(),
            (Some(n), None) => n.to_string(),
            (None, None) => "no reason given".to_string(),
        };
        format!("Trino query failed: {}", self.client.redact(&text))
    }

    /// The `nextUri` of `page`, when it is on the configured endpoint.
    fn next_uri(&self, page: &Value) -> Result<Option<url::Url>> {
        let Some(raw) = page.get("nextUri").and_then(Value::as_str) else {
            return Ok(None);
        };
        let next = url::Url::parse(raw).map_err(|_| {
            CoreError::Other("Trino returned a nextUri that is not a valid URL".into())
        })?;
        if !same_endpoint(self.client.base(), &next) {
            return Err(CoreError::Other(format!(
                "Trino returned a nextUri on {}, which is not the configured endpoint {}. \
                 VibeLens does not follow it, because the credential would be sent there. \
                 Behind a load balancer that terminates TLS, set \
                 http-server.process-forwarded=true on the coordinator so it builds its URLs \
                 from the address the client used",
                self.client.redact(&shown(&next)),
                shown(self.client.base())
            )));
        }
        Ok(Some(next))
    }
}

impl Script for TrinoEngine {
    async fn run(
        &self,
        statements: &[String],
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> End {
        // Both checks run over the whole script before anything is sent.
        if let Err(error) = check_session_statements(statements) {
            return End::Failed {
                statement_index: None,
                error,
            };
        }
        if opts.read_only
            && let Err(error) = crate::athena::check_read_only_as(statements, "Trino")
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
}

/// How a statement ended when it did not fail.
enum Done {
    Complete,
    /// Stopped at `max_rows` while the query could still return more.
    Truncated,
}

/// What is known about the running query.
#[derive(Default)]
struct Query {
    id: Option<String>,
    next_uri: Option<url::Url>,
    state: Option<String>,
    processed_bytes: Option<u64>,
    update_type: Option<String>,
    update_count: Option<u64>,
    columns_seen: bool,
    write: bool,
    types: Vec<String>,
    row_count: usize,
    /// A page held more rows than `max_rows` allowed.
    dropped_rows: bool,
}

impl Query {
    fn absorb_meta(&mut self, page: &Value) {
        if let Some(id) = page.get("id").and_then(Value::as_str)
            && is_query_id(id)
        {
            self.id = Some(id.to_string());
        }
        let stats = page.get("stats");
        if let Some(s) = stats.and_then(|s| s.get("state")).and_then(Value::as_str) {
            self.state = Some(s.to_string());
        }
        if let Some(b) = stats
            .and_then(|s| s.get("processedBytes"))
            .and_then(Value::as_u64)
        {
            self.processed_bytes = Some(b);
        }
        if let Some(t) = page.get("updateType").and_then(Value::as_str) {
            self.update_type = Some(bounded(t, 64));
        }
        if let Some(n) = page.get("updateCount").and_then(Value::as_u64) {
            self.update_count = Some(n);
        }
    }

    /// Whether the query may still produce rows past what was read.
    fn more_rows_possible(&self, page: &Value) -> bool {
        self.dropped_rows || page.get("nextUri").is_some_and(Value::is_string)
    }
}

/// Trino query ids look like `20240131_123456_00001_abcde`. Anything else is not shown.
fn is_query_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

/// Resolves when `deadline` passes; never without one.
async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending().await,
    }
}

fn parse(body: &[u8], label: &str) -> Result<Value> {
    serde_json::from_slice(body).map_err(|e| {
        CoreError::Other(format!(
            "{label} sent an answer that is not the expected JSON: {}",
            bounded(&e.to_string(), MAX_ERROR_TEXT)
        ))
    })
}

/// Statements that only change the client session. Each statement here starts from the
/// profile (the server's `X-Trino-Set-*` answers are ignored), so they would do nothing, or
/// worse, look like they did.
fn session_statement(stmt: &str) -> Option<&'static str> {
    let s = crate::athena::scan_words(stmt);
    let w: Vec<&str> = s.words.iter().map(String::as_str).take(3).collect();
    Some(match w.as_slice() {
        ["USE", ..] => "USE",
        ["SET", "SESSION", ..] => "SET SESSION",
        ["RESET", "SESSION", ..] => "RESET SESSION",
        ["SET", "ROLE", ..] => "SET ROLE",
        ["SET", "PATH", ..] => "SET PATH",
        ["SET", "TIME", "ZONE"] => "SET TIME ZONE",
        ["START", "TRANSACTION", ..] => "START TRANSACTION",
        ["COMMIT", ..] => "COMMIT",
        ["ROLLBACK", ..] => "ROLLBACK",
        ["PREPARE", ..] => "PREPARE",
        ["EXECUTE", ..] => "EXECUTE",
        ["DEALLOCATE", ..] => "DEALLOCATE",
        _ => return None,
    })
}

fn check_session_statements(statements: &[String]) -> Result<()> {
    for (i, stmt) in statements.iter().enumerate() {
        if let Some(kind) = session_statement(stmt) {
            return Err(CoreError::PolicyDenied(format!(
                "{}{kind} is not available on Trino here: every statement starts from the \
                 connection's catalog and namespace, and session changes the server sends back \
                 are not kept, so it would have no effect on the next statement. Qualify names \
                 as catalog.schema.table instead, and run each statement on its own (there are \
                 no multi-statement transactions)",
                which(i, statements.len())
            )));
        }
    }
    Ok(())
}

/// Converts one Trino value. Trino's JSON already types most values; what changes is what the
/// other engines change: `bigint` and `decimal` become strings (JS numbers lose precision past
/// 2^53), `varbinary` (base64 on the wire) becomes `{"$bytes", "len"}`, timestamps and `json`
/// are handled as for Athena (same type names). Nested values stay JSON. Oversized values become
/// markers.
fn trino_cell(ty: &str, v: Value) -> Cell {
    let base = crate::athena::base_type(ty);
    let cell = match v {
        Value::Null => Value::Null,
        Value::String(s) => match base.as_str() {
            "varbinary" => match base64::engine::general_purpose::STANDARD.decode(s.trim()) {
                Ok(bytes) => {
                    let mut m = Map::new();
                    m.insert("$bytes".into(), Value::String(s));
                    m.insert("len".into(), Value::from(bytes.len()));
                    Value::Object(m)
                }
                Err(_) => Value::String(s),
            },
            _ => crate::athena::convert_cell(ty, Some(&s)),
        },
        Value::Number(n) => match base.as_str() {
            "bigint" | "decimal" => Value::String(n.to_string()),
            _ => Value::Number(n),
        },
        other => other,
    };
    cap_cell(cell)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn session_statements_are_recognised() {
        for s in [
            "USE lake.shop",
            "set session query_max_run_time = '1h'",
            "RESET SESSION x",
            "SET ROLE admin",
            "SET PATH a",
            "SET TIME ZONE 'UTC'",
            "START TRANSACTION",
            "COMMIT",
            "ROLLBACK",
            "PREPARE q FROM SELECT 1",
            "EXECUTE q",
            "DEALLOCATE PREPARE q",
        ] {
            assert!(session_statement(s).is_some(), "{s}");
        }
        for s in ["SELECT 1", "SHOW SESSION", "INSERT INTO t VALUES (1)"] {
            assert!(session_statement(s).is_none(), "{s}");
        }
    }

    #[test]
    fn cells_follow_the_shared_encoding() {
        assert_eq!(
            trino_cell("bigint", json!(9007199254740993u64)),
            json!("9007199254740993")
        );
        assert_eq!(trino_cell("decimal(12,2)", json!("1.50")), json!("1.50"));
        assert_eq!(trino_cell("integer", json!(5)), json!(5));
        assert_eq!(trino_cell("double", json!("NaN")), json!("NaN"));
        assert_eq!(trino_cell("boolean", json!(true)), json!(true));
        assert_eq!(
            trino_cell("varbinary", json!("aGVsbG8=")),
            json!({"$bytes": "aGVsbG8=", "len": 5})
        );
        assert_eq!(
            trino_cell("timestamp(3)", json!("2024-01-31 12:34:56.789")),
            json!("2024-01-31T12:34:56.789")
        );
        assert_eq!(trino_cell("array(integer)", json!([1, 2])), json!([1, 2]));
        assert_eq!(trino_cell("json", json!("{\"a\":1}")), json!({"a": 1}));
        assert_eq!(trino_cell("varchar", Value::Null), Value::Null);
    }

    #[test]
    fn query_ids_are_validated_before_they_are_shown() {
        assert!(is_query_id("20240131_123456_00001_abcde"));
        assert!(!is_query_id(""));
        assert!(!is_query_id("a/b"));
        assert!(!is_query_id(&"a".repeat(200)));
    }
}
