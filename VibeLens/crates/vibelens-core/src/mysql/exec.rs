//! Statement execution: splitting, read-only enforcement, streaming with bounds, cancellation.
//!
//! Every way a run can end early (cancel, timeout, the receiver going away, the run future
//! being dropped, a read-only result cut at the row cap) asks the server to stop the statement
//! with `KILL QUERY` from a separate connection and closes the connection; it is never returned
//! to the pool, so a late kill cannot reach someone else's query.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use mysql_async::Conn;
use mysql_async::consts::StatusFlags;
use mysql_async::prelude::Queryable;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::MysqlHandle;
use super::cells::{cell, describe};
use super::check::{check_no_delimiter, check_read_only, command_tag, split_script};
use super::connect::{Object, Shared, close_hard};
use super::error::{is_connection_lost, map_error};
use crate::error::{CoreError, ErrorPayload, Result};
use crate::model::*;

/// `progress` value meaning "no statement started yet".
const NO_STATEMENT: usize = usize::MAX;
/// How many times a run is retried when the pooled connection was dead before anything ran.
const STALE_RETRIES: usize = 1;

/// How a run ended (the terminal event is sent by the caller).
pub(super) enum End {
    Finished,
    Failed {
        statement_index: Option<usize>,
        error: CoreError,
    },
    /// The result receiver went away; nobody is listening for a terminal event.
    Gone,
}

enum Outcome {
    Done,
    Failed {
        statement_index: Option<usize>,
        error: CoreError,
    },
    /// A read-only run's statement left the read-only transaction: the session is not trusted
    /// any more and the connection is closed.
    Escaped {
        statement_index: usize,
        error: CoreError,
    },
    /// A result was cut at the row cap while the server was still sending it (every
    /// read-only statement, and the last statement of any run). Nothing more runs on the
    /// connection: the statement is killed and the connection closed.
    Truncated {
        statement_index: usize,
    },
    /// A statement ran past its time limit.
    TimedOut {
        statement_index: usize,
    },
    Gone,
    /// The pooled connection was dead before any statement ran.
    Stale(CoreError),
}

enum Stop {
    Failed(CoreError),
    Gone,
}

impl From<mysql_async::Error> for Stop {
    fn from(e: mysql_async::Error) -> Self {
        Stop::Failed(map_error(&e))
    }
}

async fn emit(tx: &mpsc::Sender<QueryEvent>, ev: QueryEvent) -> std::result::Result<(), Stop> {
    tx.send(ev).await.map_err(|_| Stop::Gone)
}

fn elapsed_ms(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Tags whose `affected_rows` means rows changed.
fn counts_rows(tag: &str) -> bool {
    matches!(tag, "INSERT" | "UPDATE" | "DELETE" | "REPLACE" | "LOAD")
}

/// The session is inside a read-only transaction, as the server reported with the last
/// statement's OK packet.
fn in_read_only_tx(conn: &Conn) -> bool {
    conn.last_ok_packet().is_some_and(|ok| {
        let f = ok.status_flags();
        f.contains(StatusFlags::SERVER_STATUS_IN_TRANS)
            && f.contains(StatusFlags::SERVER_STATUS_IN_TRANS_READONLY)
    })
}

/// The session is inside a transaction (explicit, or `autocommit = 0` with work done).
fn in_tx(conn: &Conn) -> bool {
    conn.last_ok_packet().is_some_and(|ok| {
        ok.status_flags()
            .contains(StatusFlags::SERVER_STATUS_IN_TRANS)
    })
}

/// Runs one statement and streams its result. `Ok(true)` means the result was cut off at
/// `opts.max_rows` with rows still to come.
async fn run_one(
    conn: &mut Conn,
    stmt: &str,
    idx: usize,
    opts: &ExecOptions,
    tx: &mpsc::Sender<QueryEvent>,
) -> std::result::Result<bool, Stop> {
    let t0 = Instant::now();
    let mut result = conn.query_iter(stmt).await?;
    let columns = result.columns().filter(|c| !c.is_empty());
    let Some(columns) = columns else {
        let affected = result.affected_rows();
        result.drop_result().await?;
        let tag = command_tag(stmt);
        let rows_affected = tag.as_deref().is_some_and(counts_rows).then_some(affected);
        emit(
            tx,
            QueryEvent::StatementDone {
                statement_index: idx,
                command_tag: tag,
                rows_affected,
                row_count: 0,
                truncated: false,
                elapsed_ms: elapsed_ms(t0),
            },
        )
        .await?;
        return Ok(false);
    };
    let (meta, shapes) = describe(&columns);
    emit(
        tx,
        QueryEvent::Columns {
            statement_index: idx,
            columns: meta,
        },
    )
    .await?;
    let page_size = opts.page_size.max(1);
    let mut page: Vec<Vec<Cell>> = Vec::with_capacity(page_size.min(1024));
    let mut page_bytes = 0usize;
    let mut row_count = 0usize;
    let mut truncated = false;
    // Rows of the first result set only; `next()` returns `None` where it ends.
    while row_count <= opts.max_rows {
        let Some(row) = result.next().await? else {
            break;
        };
        if row_count >= opts.max_rows {
            // More rows exist than we hold: stop reading. The rest is still on the wire.
            truncated = true;
            break;
        }
        let values = row.unwrap_raw();
        let mut cells = Vec::with_capacity(shapes.len());
        for (i, (shape, width)) in shapes.iter().enumerate() {
            cells.push(cell(values.get(i).and_then(Option::as_ref), *shape, *width));
        }
        page_bytes += cells.iter().map(cell_weight).sum::<usize>();
        page.push(cells);
        row_count += 1;
        if page.len() >= page_size || page_bytes >= PAGE_BYTE_BUDGET {
            page_bytes = 0;
            emit(
                tx,
                QueryEvent::Rows {
                    statement_index: idx,
                    rows: std::mem::take(&mut page),
                },
            )
            .await?;
        }
    }
    if !truncated {
        // Further result sets (a CALL's) are read and discarded; their rows are never kept.
        result.drop_result().await?;
    } else {
        drop(result);
    }
    if !page.is_empty() {
        emit(
            tx,
            QueryEvent::Rows {
                statement_index: idx,
                rows: page,
            },
        )
        .await?;
    }
    emit(
        tx,
        QueryEvent::StatementDone {
            statement_index: idx,
            command_tag: Some(
                command_tag(stmt)
                    .filter(|t| t != "WITH" && t != "TABLE" && t != "VALUES")
                    .unwrap_or_else(|| "SELECT".into()),
            ),
            rows_affected: None,
            row_count,
            truncated,
            elapsed_ms: elapsed_ms(t0),
        },
    )
    .await?;
    Ok(truncated)
}

/// Everything `run_script` needs besides the connection and the statements.
struct ScriptCtx<'a> {
    opts: &'a ExecOptions,
    timeout_ms: Option<u64>,
    tx: &'a mpsc::Sender<QueryEvent>,
    progress: &'a AtomicUsize,
    /// Refuse non-reads by their text before sending (always `true` outside tests).
    lexical_read_only: bool,
}

fn left_read_only_tx() -> CoreError {
    CoreError::PolicyDenied("statement left the read-only transaction".into())
}

async fn run_script(conn: &mut Conn, statements: &[String], ctx: &ScriptCtx<'_>) -> Outcome {
    let ScriptCtx {
        opts,
        timeout_ms,
        tx,
        progress,
        lexical_read_only,
    } = *ctx;
    let fail = |statement_index: Option<usize>, error: CoreError| Outcome::Failed {
        statement_index,
        error,
    };
    let setup_failed = |e: &mysql_async::Error| {
        if is_connection_lost(e) {
            Outcome::Stale(map_error(e))
        } else {
            fail(None, map_error(e))
        }
    };
    // Server-side limit for SELECTs (the client-side one below covers every statement), always
    // set so a value left by an earlier run cannot stick.
    let met = timeout_ms.map_or_else(|| "DEFAULT".to_string(), |ms| ms.to_string());
    if let Err(e) = conn
        .query_drop(format!("SET SESSION max_execution_time = {met}"))
        .await
    {
        return setup_failed(&e);
    }
    if opts.read_only {
        if let Err(e) = conn.query_drop("START TRANSACTION READ ONLY").await {
            return setup_failed(&e);
        }
        if !in_read_only_tx(conn) {
            return fail(
                None,
                CoreError::Other("could not open the read-only transaction".into()),
            );
        }
    }
    let limit = timeout_ms.map(Duration::from_millis);
    for (idx, stmt) in statements.iter().enumerate() {
        progress.store(idx, Ordering::SeqCst);
        // R5: decided before the statement is sent.
        if opts.read_only
            && lexical_read_only
            && let Err(why) = check_read_only(stmt)
        {
            return fail(
                Some(idx),
                CoreError::PolicyDenied(format!("not run in read-only mode: {why}")),
            );
        }
        let run = run_one(conn, stmt, idx, opts, tx);
        let result = match limit {
            Some(d) => match tokio::time::timeout(d, run).await {
                Ok(r) => r,
                Err(_) => {
                    return Outcome::TimedOut {
                        statement_index: idx,
                    };
                }
            },
            None => run.await,
        };
        match result {
            Ok(true) if opts.read_only || idx + 1 == statements.len() => {
                return Outcome::Truncated {
                    statement_index: idx,
                };
            }
            // A cut result in the middle of a writable script: the rest of it is read and
            // discarded before the next statement, so the script goes on in the same session.
            Ok(_) => {}
            Err(Stop::Failed(e)) => return fail(Some(idx), e),
            Err(Stop::Gone) => return Outcome::Gone,
        }
        if opts.read_only && !in_read_only_tx(conn) {
            return Outcome::Escaped {
                statement_index: idx,
                error: left_read_only_tx(),
            };
        }
    }
    if opts.read_only {
        if let Err(e) = conn.query_drop("ROLLBACK").await {
            return fail(None, map_error(&e));
        }
        return Outcome::Done;
    }
    if in_tx(conn) {
        let _ = conn.query_drop("ROLLBACK").await;
        return fail(
            None,
            CoreError::Invalid(
                "the script left a transaction open (START TRANSACTION, BEGIN or autocommit = 0) \
                 and never ended it; it was rolled back. Add COMMIT to keep the changes"
                    .into(),
            ),
        );
    }
    Outcome::Done
}

/// A pooled connection that goes back to the pool only when the run ended cleanly. If the
/// owning future is dropped first (agent tool timeout, aborted task), dropping this kills the
/// statement on the server and closes the connection.
struct PooledConn {
    obj: Option<Object>,
    shared: Arc<Shared>,
    id: u32,
}

impl PooledConn {
    fn new(obj: Object, shared: Arc<Shared>) -> Self {
        let id = obj.conn.id();
        Self {
            obj: Some(obj),
            shared,
            id,
        }
    }

    fn conn(&mut self) -> Option<&mut Conn> {
        self.obj.as_mut().map(|o| &mut o.conn)
    }

    /// Back to the pool once the session has been reset (in the background).
    fn release(mut self) {
        let Some(mut obj) = self.obj.take() else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    match shared.reset_session(&mut obj.conn).await {
                        Ok(()) => obj.reset_done = true,
                        Err(e) => {
                            tracing::debug!(error = %e, "discarding pooled MySQL connection");
                            let c = Object::take(obj);
                            close_hard(c.conn).await;
                        }
                    }
                });
            }
            Err(_) => drop(obj),
        }
    }

    /// Out of the pool and closed; nothing is running on it.
    async fn discard(mut self) {
        if let Some(obj) = self.obj.take() {
            close_hard(Object::take(obj).conn).await;
        }
    }

    /// Stops whatever runs on the connection (`KILL QUERY` from another connection), then
    /// closes it.
    async fn kill_and_discard(mut self) {
        if let Some(obj) = self.obj.take() {
            let c = Object::take(obj);
            self.shared.kill_query(self.id).await;
            close_hard(c.conn).await;
        }
    }
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        let Some(obj) = self.obj.take() else { return };
        let c = Object::take(obj);
        let shared = Arc::clone(&self.shared);
        let id = self.id;
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                // Keep the connection until the kill has been delivered, so the server stops the
                // statement rather than finishing it for nobody.
                rt.spawn(async move {
                    shared.kill_query(id).await;
                    close_hard(c.conn).await;
                });
            }
            Err(_) => drop(c),
        }
    }
}

impl MysqlHandle {
    /// Runs `statements` on one pooled connection, sending `Columns`/`Rows`/`StatementDone`
    /// events (but neither `Started` nor the terminal event).
    pub(super) async fn run(
        &self,
        statements: &[String],
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> End {
        self.run_checked(statements, opts, cancel, tx, true).await
    }

    /// [`run`](Self::run) with the pre-submit read-only check switchable, so tests can prove
    /// what the server-side read-only transaction and the after-statement check do on their
    /// own. Production code always passes `true`.
    async fn run_checked(
        &self,
        statements: &[String],
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
        lexical_read_only: bool,
    ) -> End {
        if statements.is_empty() {
            return End::Finished;
        }
        if let Err(e) = check_no_delimiter(statements) {
            return End::Failed {
                statement_index: None,
                error: CoreError::Invalid(e),
            };
        }
        let _guard = self.activity();
        let timeout_ms = opts
            .timeout_ms
            .or(self.shared.params.options.statement_timeout_ms)
            .filter(|ms| *ms > 0);
        let mut stale_retries = 0;
        loop {
            let obj: Object = tokio::select! {
                biased;
                _ = cancel.cancelled() => return End::Failed { statement_index: None, error: CoreError::Cancelled },
                r = self.pool.get() => match r {
                    Ok(c) => c,
                    Err(e) => return End::Failed { statement_index: None, error: super::map_pool_error(e) },
                },
            };
            let mut conn = PooledConn::new(obj, Arc::clone(&self.shared));
            let progress = AtomicUsize::new(NO_STATEMENT);
            let ctx = ScriptCtx {
                opts,
                timeout_ms,
                tx,
                progress: &progress,
                lexical_read_only,
            };
            let outcome = {
                let Some(c) = conn.conn() else {
                    return End::Failed {
                        statement_index: None,
                        error: CoreError::Other("pooled connection unavailable".into()),
                    };
                };
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    _ = tx.closed() => Some(Outcome::Gone),
                    o = run_script(c, statements, &ctx) => Some(o),
                }
            };
            let current = Some(progress.load(Ordering::SeqCst)).filter(|&i| i != NO_STATEMENT);
            return match outcome {
                None => {
                    conn.kill_and_discard().await;
                    End::Failed {
                        statement_index: current,
                        error: CoreError::Cancelled,
                    }
                }
                Some(Outcome::Stale(error)) => {
                    conn.discard().await;
                    if stale_retries < STALE_RETRIES {
                        stale_retries += 1;
                        tracing::info!(error = %error, "pooled MySQL connection was dead; retrying on a fresh one");
                        continue;
                    }
                    End::Failed {
                        statement_index: None,
                        error,
                    }
                }
                Some(Outcome::Done) => {
                    conn.release();
                    End::Finished
                }
                Some(Outcome::Truncated { statement_index }) => {
                    conn.kill_and_discard().await;
                    if statement_index + 1 < statements.len() {
                        End::Failed {
                            statement_index: Some(statement_index + 1),
                            error: not_run_after_truncation(
                                statement_index,
                                statements.len(),
                                opts.max_rows,
                            ),
                        }
                    } else {
                        End::Finished
                    }
                }
                Some(Outcome::TimedOut { statement_index }) => {
                    conn.kill_and_discard().await;
                    End::Failed {
                        statement_index: Some(statement_index),
                        error: CoreError::Timeout(format!(
                            "the statement ran longer than {} ms and was stopped on the server",
                            timeout_ms.unwrap_or_default()
                        )),
                    }
                }
                Some(Outcome::Gone) => {
                    conn.kill_and_discard().await;
                    End::Gone
                }
                Some(Outcome::Escaped {
                    statement_index,
                    error,
                }) => {
                    // Closing the connection rolls back whatever it holds.
                    conn.discard().await;
                    End::Failed {
                        statement_index: Some(statement_index),
                        error,
                    }
                }
                Some(Outcome::Failed {
                    statement_index,
                    error,
                }) => {
                    // The background reset (COM_RESET_CONNECTION) rolls back what the run left;
                    // a connection it cannot reset is closed.
                    conn.release();
                    End::Failed {
                        statement_index,
                        error,
                    }
                }
            };
        }
    }

    /// Runs one or more statements sequentially, streaming events into `tx`. Emits `Started`
    /// first and exactly one of `Finished` / `Failed` last (unless the receiver disappears).
    pub async fn execute_stream(
        &self,
        query_id: String,
        sql: &str,
        opts: ExecOptions,
        cancel: CancellationToken,
        tx: mpsc::Sender<QueryEvent>,
    ) -> Result<()> {
        let statements = split_script(sql);
        tx.send(QueryEvent::Started {
            query_id,
            statement_count: statements.len(),
        })
        .await
        .map_err(|_| CoreError::Other("query result receiver was dropped".into()))?;
        let t0 = Instant::now();
        let terminal = match self.run(&statements, &opts, &cancel, &tx).await {
            End::Finished => QueryEvent::Finished {
                elapsed_ms: elapsed_ms(t0),
            },
            End::Failed {
                statement_index,
                error,
            } => QueryEvent::Failed {
                statement_index,
                error: ErrorPayload::from(&error),
            },
            End::Gone => return Ok(()),
        };
        let _ = tx.send(terminal).await;
        Ok(())
    }

    /// Bounded, materialised execution: the result of the last statement that returns rows
    /// (or of the last statement when none does), stopped when `cancel` fires.
    pub async fn execute_collect(
        &self,
        sql: &str,
        opts: ExecOptions,
        cancel: CancellationToken,
    ) -> Result<QueryResult> {
        let statements = split_script(sql);
        if statements.is_empty() {
            return Ok(QueryResult::default());
        }
        let (tx, mut rx) = mpsc::channel::<QueryEvent>(32);
        let runner = async {
            let end = self.run(&statements, &opts, &cancel, &tx).await;
            drop(tx);
            end
        };
        let collector = async {
            let mut c = Collector::default();
            while let Some(ev) = rx.recv().await {
                c.push(ev);
            }
            c
        };
        let (end, collected) = tokio::join!(runner, collector);
        match end {
            End::Finished => collected
                .into_result()
                .ok_or_else(|| CoreError::Other("no statement produced a result".into())),
            End::Failed { error, .. } => Err(error),
            End::Gone => Err(CoreError::Other("result collector went away".into())),
        }
    }

    /// Runs one catalog query (text protocol, no read-only transaction needed: the text is ours)
    /// and returns its rows as text, bounded by `max_rows`.
    pub(super) async fn catalog_rows(
        &self,
        sql: &str,
        max_rows: usize,
    ) -> Result<Vec<Vec<Option<String>>>> {
        let (mut obj, _g) = self.conn().await?;
        let id = obj.conn.id();
        let fut = async {
            let mut out = Vec::new();
            let mut result = obj.conn.query_iter(sql).await.map_err(|e| map_error(&e))?;
            while let Some(row) = result.next().await.map_err(|e| map_error(&e))? {
                if out.len() >= max_rows {
                    return Ok::<_, CoreError>((out, true));
                }
                out.push(
                    row.unwrap_raw()
                        .into_iter()
                        .map(|v| match v {
                            Some(mysql_async::Value::Bytes(b)) => {
                                Some(String::from_utf8_lossy(&b).into_owned())
                            }
                            Some(mysql_async::Value::NULL) | None => None,
                            Some(other) => Some(other.as_sql(false)),
                        })
                        .collect(),
                );
            }
            result.drop_result().await.map_err(|e| map_error(&e))?;
            Ok((out, false))
        };
        match tokio::time::timeout(Duration::from_secs(60), fut).await {
            Ok(Ok((rows, false))) => Ok(rows),
            Ok(Ok((rows, true))) => {
                // Cut: the rest is still arriving; the connection does not go back.
                let c = Object::take(obj);
                self.shared.kill_query(id).await;
                close_hard(c.conn).await;
                Ok(rows)
            }
            Ok(Err(e)) => Err(e),
            Err(_) => {
                let c = Object::take(obj);
                self.shared.kill_query(id).await;
                close_hard(c.conn).await;
                Err(CoreError::Timeout(
                    "reading the MySQL catalog took longer than 60 seconds".into(),
                ))
            }
        }
    }
}

/// The error for the statements a read-only script did not reach because statement
/// `truncated` (0-based) of `total` hit the row cap.
fn not_run_after_truncation(truncated: usize, total: usize, max_rows: usize) -> CoreError {
    let (first, last) = (truncated + 2, total);
    let rest = if first == last {
        format!("statement {first} was not run")
    } else {
        format!("statements {first} to {last} were not run")
    };
    CoreError::Invalid(format!(
        "statement {} returned more than {max_rows} rows, so this read-only run stopped there and {rest}. Add a LIMIT to statement {}, or run the rest separately",
        truncated + 1,
        truncated + 1
    ))
}

/// Folds an event stream into the result of the last row-returning statement.
#[derive(Default)]
struct Collector {
    current: QueryResult,
    last_rows: Option<QueryResult>,
    last_any: Option<QueryResult>,
}

impl Collector {
    fn push(&mut self, ev: QueryEvent) {
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

    fn into_result(self) -> Option<QueryResult> {
        self.last_rows.or(self.last_any)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MysqlOptions, ResolvedCredentials, TlsConfig, TlsMode};
    use futures_util::FutureExt;
    use secrecy::SecretString;

    use crate::mysql::MysqlConnectParams;

    #[test]
    fn collector_prefers_the_last_row_returning_statement() {
        let mut c = Collector::default();
        c.push(QueryEvent::Columns {
            statement_index: 0,
            columns: vec![ColumnMeta {
                name: "a".into(),
                type_name: "int".into(),
                type_oid: 3,
            }],
        });
        c.push(QueryEvent::Rows {
            statement_index: 0,
            rows: vec![vec![serde_json::json!(1)]],
        });
        c.push(QueryEvent::StatementDone {
            statement_index: 0,
            command_tag: Some("SELECT".into()),
            rows_affected: None,
            row_count: 1,
            truncated: false,
            elapsed_ms: 1,
        });
        c.push(QueryEvent::StatementDone {
            statement_index: 1,
            command_tag: Some("INSERT".into()),
            rows_affected: Some(2),
            row_count: 0,
            truncated: false,
            elapsed_ms: 1,
        });
        let r = c.into_result().unwrap();
        assert_eq!(r.rows, vec![vec![serde_json::json!(1)]]);
    }

    // ---- database-backed (skipped without VG_TEST_MYSQL; see tests/services) -------------

    async fn handle() -> Option<Arc<MysqlHandle>> {
        let raw = std::env::var("VG_TEST_MYSQL").ok()?;
        let (mut host, mut port, mut user, mut pw, mut db) = (
            "127.0.0.1".to_string(),
            3306u16,
            "root".to_string(),
            String::new(),
            "mysql".to_string(),
        );
        for kv in raw.split_whitespace() {
            let (k, v) = kv.split_once('=')?;
            match k {
                "host" => host = v.into(),
                "port" => port = v.parse().ok()?,
                "user" => user = v.into(),
                "password" => pw = v.into(),
                "dbname" => db = v.into(),
                _ => {}
            }
        }
        let creds: crate::ports::CredentialFn = Arc::new(move || {
            let c = ResolvedCredentials {
                username: Some(user.clone()),
                password: Some(SecretString::from(pw.clone())),
                ..ResolvedCredentials::anonymous()
            };
            async move { Ok(c) }.boxed()
        });
        let params = MysqlConnectParams {
            host: host.clone(),
            port,
            tls_server_name: host,
            database: db,
            tls: TlsConfig {
                mode: TlsMode::Require,
                ..Default::default()
            },
            options: MysqlOptions {
                max_pool_size: 1,
                ..Default::default()
            },
            cleartext_plugin: false,
        };
        MysqlHandle::connect(params, creds).await.ok()
    }

    async fn run(h: &MysqlHandle, stmts: &[&str], read_only: bool, lexical: bool) -> End {
        let stmts: Vec<String> = stmts.iter().map(|s| s.to_string()).collect();
        let (tx, mut rx) = mpsc::channel(64);
        let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        let end = h
            .run_checked(
                &stmts,
                &ExecOptions {
                    read_only,
                    ..Default::default()
                },
                &CancellationToken::new(),
                &tx,
                lexical,
            )
            .await;
        drop(tx);
        let _ = drain.await;
        end
    }

    async fn value(h: &MysqlHandle, sql: &str) -> serde_json::Value {
        h.execute_collect(sql, ExecOptions::default(), CancellationToken::new())
            .await
            .expect(sql)
            .rows
            .first()
            .and_then(|r| r.first())
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    }

    /// The driver does not advertise CLIENT_MULTI_STATEMENTS, so a COM_QUERY holding two
    /// statements fails as a whole on the server; the second one never runs.
    #[tokio::test]
    async fn the_server_runs_no_second_statement_in_one_query() {
        let Some(h) = handle().await else {
            crate::test_services::skip("mysql", "VG_TEST_MYSQL is not set");
            return;
        };
        let t = format!("vg_unit_victim_{}", uuid::Uuid::new_v4().simple());
        value(&h, &format!("CREATE TABLE shop_{t} (a INT)")).await;
        // `SELECT 1` alone would be read-only; the second statement is what is being smuggled.
        let end = run(
            &h,
            &[&format!("SELECT 1; DROP TABLE shop_{t}")],
            false,
            true,
        )
        .await;
        match end {
            End::Failed { error, .. } => assert!(error.to_string().contains("1064"), "{error}"),
            _ => panic!("the server accepted two statements in one query"),
        }
        assert_eq!(
            value(
                &h,
                &format!(
                    "SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_NAME = 'shop_{t}'"
                )
            )
            .await,
            serde_json::json!("1"),
            "BIGINT counts arrive as strings"
        );
        value(&h, &format!("DROP TABLE shop_{t}")).await;
    }

    /// With the text check switched off, the server's read-only transaction still refuses a
    /// write, and a statement that ends the transaction is caught right after it ran: nothing
    /// after it runs and the session is closed, not reused.
    #[tokio::test]
    async fn the_transaction_and_the_after_statement_check_hold_without_the_text_check() {
        let Some(h) = handle().await else {
            crate::test_services::skip("mysql", "VG_TEST_MYSQL is not set");
            return;
        };
        let t = format!("vg_unit_ro_{}", uuid::Uuid::new_v4().simple());
        value(&h, &format!("CREATE TABLE {t} (a INT)")).await;
        match run(&h, &[&format!("INSERT INTO {t} VALUES (1)")], true, false).await {
            End::Failed { error, .. } => assert!(error.to_string().contains("1792"), "{error}"),
            _ => panic!("the read-only transaction accepted a write"),
        }
        for escape in ["COMMIT", "ROLLBACK", "COMMIT AND NO CHAIN"] {
            let before = value(&h, "SELECT CONNECTION_ID()").await;
            let end = run(
                &h,
                &[escape, &format!("INSERT INTO {t} VALUES (2)")],
                true,
                false,
            )
            .await;
            match end {
                End::Failed {
                    statement_index: Some(0),
                    error: CoreError::PolicyDenied(m),
                } => assert_eq!(m, "statement left the read-only transaction", "{escape}"),
                End::Failed { error, .. } => panic!("{escape}: wrong error {error:?}"),
                _ => panic!("{escape}: the run did not fail"),
            }
            let after = value(&h, "SELECT CONNECTION_ID()").await;
            assert_ne!(before, after, "{escape}: the session went back to the pool");
        }
        assert_eq!(
            value(&h, &format!("SELECT COUNT(*) FROM {t}")).await,
            serde_json::json!("0")
        );
        value(&h, &format!("DROP TABLE {t}")).await;
    }
}
