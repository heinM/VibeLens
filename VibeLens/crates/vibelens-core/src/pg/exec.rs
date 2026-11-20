//! Statement execution: splitting, read-only enforcement, streaming, cancellation.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::{TryStreamExt, pin_mut};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_postgres::types::ToSql;
use tokio_postgres::{CancelToken, Client, SimpleQueryMessage};
use tokio_util::sync::CancellationToken;

use super::PgHandle;
use super::codec::CellValue;
use super::connect::{Object, Shared};
use super::error::{is_connection_lost, map_pg_error, map_pool_error};
use crate::error::{CoreError, ErrorPayload, Result};
use crate::model::*;

/// `progress` value meaning "no statement started yet".
const NO_STATEMENT: usize = usize::MAX;
const TX_CLEAN: u8 = 0;
/// A multi-command string ran through the simple protocol; we cannot tell what it left behind.
const TX_MAYBE: u8 = 1;
/// The script issued its own `BEGIN` and has not ended it.
const TX_OPEN: u8 = 2;

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
    /// A read-only run's statement left the read-only transaction. The connection is not to be
    /// trusted any more and is discarded after a best-effort `ROLLBACK`.
    Escaped {
        statement_index: usize,
        error: CoreError,
    },
    Gone,
    /// The pooled connection turned out to be dead before any statement ran (closed by the
    /// server, a proxy, a NAT gateway or a laptop sleep). Nothing was executed and nothing was
    /// streamed, so the run is retried once on a fresh connection.
    Stale(CoreError),
    /// Finished, but the session may hold a transaction that cannot be ended because the
    /// endpoint (pg-proxy shunt mode) refuses `ROLLBACK`: do not reuse the connection.
    DoneDiscard,
    /// A read-only run's statement returned more rows than the cap. The server is still
    /// producing the rest of that result, and anything else asked of this connection (the
    /// after-statement check, the next statement, the final `ROLLBACK`) would queue behind all
    /// of it. Nothing more was run: the connection is cancelled and closed, never reused.
    Truncated {
        statement_index: usize,
    },
}

/// How many times a run is retried after [`Outcome::Stale`].
const STALE_RETRIES: usize = 1;

enum Stop {
    Failed(CoreError),
    Gone,
}

impl From<tokio_postgres::Error> for Stop {
    fn from(e: tokio_postgres::Error) -> Self {
        Stop::Failed(map_pg_error(&e))
    }
}

async fn emit(tx: &mpsc::Sender<QueryEvent>, ev: QueryEvent) -> std::result::Result<(), Stop> {
    tx.send(ev).await.map_err(|_| Stop::Gone)
}

/// Splits a script and drops statements that contain nothing but whitespace / `;`.
pub(super) fn split_script(sql: &str) -> Vec<String> {
    crate::sql::split_statements(sql)
        .into_iter()
        .filter(|s| s.chars().any(|c| !c.is_whitespace() && c != ';'))
        .collect()
}

// ---- lexical helpers -----------------------------------------------------------------------

/// Whitespace as the PostgreSQL scanner defines it: `[ \t\n\r\f\v]`. Rust's
/// `u8::is_ascii_whitespace` leaves out `\v` (0x0B), which the server skips.
fn is_pg_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0C | 0x0B)
}

/// A `--` comment runs to the next `\n` **or `\r`** in PostgreSQL's scanner. Treating only `\n`
/// as its end let `--x\rCOMMIT` hide a `COMMIT` from this module while the server executed it.
fn is_line_end(c: u8) -> bool {
    c == b'\n' || c == b'\r'
}

/// Index just past the block comment that starts at `i` (`b[i..]` begins with `/*`); comments
/// nest. An unterminated comment runs to the end of the text, as it does on the server.
fn skip_block_comment(b: &[u8], mut i: usize) -> usize {
    let mut depth = 1;
    i += 2;
    while i < b.len() && depth > 0 {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            depth += 1;
            i += 2;
        } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
            depth -= 1;
            i += 2;
        } else {
            i += 1;
        }
    }
    i
}

/// The statement as the server's scanner sees it, with every comment replaced by one space:
/// `--` comments (ended by `\n` or `\r`), nested `/* */` comments, and nothing inside quoted
/// strings (`'..'`, `E'..'` with backslash escapes), quoted identifiers or dollar quotes.
/// Classification (transaction control, command tag) is done on this text, never on the raw one.
pub(super) fn normalise(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && !is_line_end(b[i]) {
                i += 1;
            }
            out.push(b' ');
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = skip_block_comment(b, i);
            out.push(b' ');
        } else if c == b'\'' || c == b'"' {
            let escapes = c == b'\''
                && i > 0
                && matches!(b[i - 1], b'e' | b'E')
                && (i < 2 || !ident(b[i - 2]));
            let start = i;
            i += 1;
            while i < b.len() {
                if escapes && b[i] == b'\\' {
                    i += 2;
                } else if b[i] == c {
                    if b.get(i + 1) == Some(&c) {
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            out.extend_from_slice(&b[start..i.min(b.len())]);
        } else if c == b'$' && (i == 0 || !ident(b[i - 1])) {
            // `$tag$ ... $tag$` (tag may be empty); `$1` is a parameter, not a quote.
            let mut j = i + 1;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] >= 0x80) {
                j += 1;
            }
            if b.get(j) == Some(&b'$') && !b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                let tag = &b[i..=j];
                let end = b[j + 1..]
                    .windows(tag.len())
                    .position(|w| w == tag)
                    .map_or(b.len(), |p| j + 1 + p + tag.len());
                out.extend_from_slice(&b[i..end]);
                i = end;
            } else {
                out.push(c);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    // Only whole UTF-8 sequences are copied or replaced by ASCII, so nothing is ever lossy.
    String::from_utf8_lossy(&out).into_owned()
}

/// First `max` alphabetic words of a statement, upper-cased, skipping whitespace, `(`, `;` and
/// SQL comments (block comments nest, line comments end at `\n` or `\r`).
pub(super) fn leading_words(sql: &str, max: usize) -> Vec<String> {
    let b = sql.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() && out.len() < max {
        let c = b[i];
        if is_pg_space(c) || c == b'(' || c == b';' {
            i += 1;
        } else if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < b.len() && !is_line_end(b[i]) {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i = skip_block_comment(b, i);
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(sql[start..i].to_ascii_uppercase());
        } else {
            break;
        }
    }
    out
}

/// The command tag, derived from the statement text (the driver does not expose the server's tag).
pub(super) fn command_tag(sql: &str, has_columns: bool) -> Option<String> {
    let w = leading_words(sql, 8);
    let first = w.first()?.as_str();
    if has_columns && matches!(first, "SELECT" | "WITH" | "VALUES" | "TABLE") {
        return Some("SELECT".into());
    }
    let tag = match first {
        "CREATE" | "ALTER" | "DROP" => {
            const MODIFIERS: [&str; 10] = [
                "OR",
                "REPLACE",
                "UNIQUE",
                "TEMP",
                "TEMPORARY",
                "UNLOGGED",
                "GLOBAL",
                "LOCAL",
                "RECURSIVE",
                "TRUSTED",
            ];
            let mut rest = w[1..].iter().filter(|x| !MODIFIERS.contains(&x.as_str()));
            match rest.next().map(String::as_str) {
                Some(a @ ("MATERIALIZED" | "FOREIGN" | "EVENT" | "ACCESS" | "LARGE")) => {
                    match rest.next() {
                        Some(b) => format!("{first} {a} {b}"),
                        None => format!("{first} {a}"),
                    }
                }
                Some(a) => format!("{first} {a}"),
                None => first.to_string(),
            }
        }
        "TRUNCATE" => "TRUNCATE TABLE".into(),
        "REFRESH" => "REFRESH MATERIALIZED VIEW".into(),
        "LOCK" => "LOCK TABLE".into(),
        "START" => "START TRANSACTION".into(),
        other => other.to_string(),
    };
    Some(tag)
}

/// Tags whose server-side count is "rows affected".
fn counts_rows(tag: &str) -> bool {
    matches!(
        tag,
        "INSERT" | "UPDATE" | "DELETE" | "MERGE" | "COPY" | "FETCH" | "MOVE" | "SELECT"
    )
}

/// Statements that would end or reconfigure the transaction we use to enforce read-only mode.
fn is_tx_control(sql: &str) -> bool {
    let w = leading_words(&normalise(sql), 3);
    let first = w.first().map(String::as_str).unwrap_or("");
    let second = w.get(1).map(String::as_str).unwrap_or("");
    match first {
        "BEGIN" | "START" | "COMMIT" | "END" | "ROLLBACK" | "ABORT" => true,
        "PREPARE" => second == "TRANSACTION",
        "SET" => {
            second == "TRANSACTION"
                || (second == "SESSION" && w.get(2).map(String::as_str) == Some("CHARACTERISTICS"))
        }
        _ => false,
    }
}

/// Does this statement (run in autocommit) open or close an explicit transaction?
fn tx_effect(sql: &str) -> Option<u8> {
    let w = leading_words(&normalise(sql), 5);
    // `COMMIT [WORK | TRANSACTION] AND CHAIN` and `ROLLBACK [WORK | TRANSACTION] TO ...`.
    let mut rest = w.iter().skip(1).map(String::as_str);
    let mut next = rest.next();
    if matches!(next, Some("WORK" | "TRANSACTION")) {
        next = rest.next();
    }
    let after = rest.next();
    match (w.first().map(String::as_str)?, w.get(1).map(String::as_str)) {
        ("BEGIN" | "START", _) => Some(TX_OPEN),
        ("COMMIT" | "END" | "ABORT", Some("PREPARED")) | ("ROLLBACK", Some("PREPARED")) => None,
        ("ROLLBACK", _) if next == Some("TO") => None,
        ("COMMIT" | "END" | "ROLLBACK" | "ABORT", _)
            if next == Some("AND") && after == Some("CHAIN") =>
        {
            Some(TX_OPEN)
        }
        ("COMMIT" | "END" | "ROLLBACK" | "ABORT", _) => Some(TX_CLEAN),
        ("PREPARE", Some("TRANSACTION")) => Some(TX_CLEAN),
        _ => None,
    }
}

fn is_multi_command_error(e: &tokio_postgres::Error) -> bool {
    e.as_db_error()
        .is_some_and(|d| d.message().contains("multiple commands"))
}

// ---- running -------------------------------------------------------------------------------

async fn rollback(client: &Client) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(5), client.batch_execute("ROLLBACK")).await,
        Ok(Ok(()))
    )
}

fn column_meta(st: &tokio_postgres::Statement) -> Vec<ColumnMeta> {
    st.columns()
        .iter()
        .map(|c| ColumnMeta {
            name: c.name().to_string(),
            type_name: c.type_().name().to_string(),
            type_oid: c.type_().oid(),
        })
        .collect()
}

/// Runs one prepared statement and streams its result. `Ok(true)` means the result was cut off
/// at `opts.max_rows`: the rest of it is still arriving on `client`, so whatever is sent next
/// on this connection waits until the server has produced every remaining row.
async fn run_prepared(
    client: &Client,
    sql: &str,
    st: &tokio_postgres::Statement,
    idx: usize,
    opts: &ExecOptions,
    tx: &mpsc::Sender<QueryEvent>,
) -> std::result::Result<bool, Stop> {
    let t0 = Instant::now();
    let no_params: [&(dyn ToSql + Sync); 0] = [];
    if st.columns().is_empty() {
        let n = client.execute_raw(st, no_params.iter().copied()).await?;
        let tag = command_tag(sql, false);
        let rows_affected = tag.as_deref().is_some_and(counts_rows).then_some(n);
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
    }

    let stream = client.query_raw(st, no_params.iter().copied()).await?;
    pin_mut!(stream);
    emit(
        tx,
        QueryEvent::Columns {
            statement_index: idx,
            columns: column_meta(st),
        },
    )
    .await?;
    let page_size = opts.page_size.max(1);
    let mut page: Vec<Vec<Cell>> = Vec::with_capacity(page_size.min(1024));
    let mut page_bytes = 0usize;
    let mut row_count = 0usize;
    let mut truncated = false;
    while let Some(row) = stream.try_next().await? {
        if row_count >= opts.max_rows {
            // More rows exist than we are willing to hold: stop reading. Dropping the stream does
            // not stop the server: the driver keeps reading and discarding the rest of the result
            // until the connection is cancelled or closed (the caller's job, see the return value).
            truncated = true;
            break;
        }
        let mut cells = Vec::with_capacity(row.len());
        for i in 0..row.len() {
            cells.push(row.try_get::<usize, CellValue>(i)?.0);
        }
        page_bytes += cells.iter().map(cell_weight).sum::<usize>();
        page.push(cells);
        row_count += 1;
        // A page of large cells goes out early, so no single message grows without limit.
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
    if !page.is_empty() {
        emit(
            tx,
            QueryEvent::Rows {
                statement_index: idx,
                rows: std::mem::take(&mut page),
            },
        )
        .await?;
    }
    let tag = command_tag(sql, true);
    // `rows_affected` is only known once the stream is exhausted (DML ... RETURNING).
    let rows_affected = if truncated
        || !tag
            .as_deref()
            .is_some_and(|t| counts_rows(t) && t != "SELECT")
    {
        None
    } else {
        stream.rows_affected()
    };
    emit(
        tx,
        QueryEvent::StatementDone {
            statement_index: idx,
            command_tag: tag,
            rows_affected,
            row_count,
            truncated,
            elapsed_ms: elapsed_ms(t0),
        },
    )
    .await?;
    Ok(truncated)
}

/// A text value from the simple-query protocol, under the same size limit as decoded values.
fn text_cell(s: &str) -> Cell {
    if s.len() > MAX_CELL_BYTES {
        truncated_cell(TruncatedKind::Text, s.len(), s.as_bytes())
    } else {
        Value::String(s.to_owned())
    }
}

/// Fallback for text that cannot be prepared (several commands in one string). Values are text.
/// Only the last result set that has columns is reported.
async fn run_simple(
    client: &Client,
    sql: &str,
    idx: usize,
    opts: &ExecOptions,
    tx: &mpsc::Sender<QueryEvent>,
) -> std::result::Result<(), Stop> {
    let t0 = Instant::now();
    let stream = client.simple_query_raw(sql).await?;
    pin_mut!(stream);
    let mut columns: Option<Vec<ColumnMeta>> = None;
    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let mut truncated = false;
    let mut affected: Option<u64> = None;
    let text_col = |name: &str| ColumnMeta {
        name: name.to_string(),
        type_name: "text".into(),
        type_oid: 25,
    };
    while let Some(msg) = stream.try_next().await? {
        match msg {
            SimpleQueryMessage::RowDescription(desc) => {
                columns = Some(desc.iter().map(|c| text_col(c.name())).collect());
                rows.clear();
                truncated = false;
            }
            SimpleQueryMessage::Row(r) => {
                if columns.is_none() {
                    columns = Some(r.columns().iter().map(|c| text_col(c.name())).collect());
                }
                if rows.len() >= opts.max_rows {
                    truncated = true;
                    break;
                }
                rows.push(
                    (0..r.len())
                        .map(|i| r.get(i).map_or(Value::Null, text_cell))
                        .collect(),
                );
            }
            SimpleQueryMessage::CommandComplete(n) => affected = Some(n),
            _ => {}
        }
    }
    let row_count = rows.len();
    let has_columns = columns.is_some();
    if let Some(columns) = columns {
        emit(
            tx,
            QueryEvent::Columns {
                statement_index: idx,
                columns,
            },
        )
        .await?;
        let page_size = opts.page_size.max(1);
        let mut page: Vec<Vec<Cell>> = Vec::new();
        let mut page_bytes = 0usize;
        for row in rows {
            page_bytes += row.iter().map(cell_weight).sum::<usize>();
            page.push(row);
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
    }
    emit(
        tx,
        QueryEvent::StatementDone {
            statement_index: idx,
            command_tag: None,
            rows_affected: if has_columns { None } else { affected },
            row_count,
            truncated,
            elapsed_ms: elapsed_ms(t0),
        },
    )
    .await
}

fn elapsed_ms(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Why a read-only run's after-statement check failed.
enum Verify {
    /// The session is no longer inside the read-only transaction this run opened.
    Left,
    /// The check itself could not be completed (connection trouble, server error).
    Broken(CoreError),
}

/// Defence in depth for read-only runs: after **every** statement, prove that the session is
/// still inside the very transaction `BEGIN READ ONLY` opened, and still read-only. The
/// transaction start time identifies that transaction (a `COMMIT` followed by anything starts a
/// new one, or none), so this holds whatever the lexical checks missed. The driver does not
/// expose the protocol's transaction status, hence the query.
async fn verify_read_only_tx(client: &Client, started: &str) -> std::result::Result<(), Verify> {
    let msgs = client
        .simple_query(
            "SELECT pg_catalog.current_setting('transaction_read_only'), \
             pg_catalog.transaction_timestamp()::text",
        )
        .await
        .map_err(|e| Verify::Broken(map_pg_error(&e)))?;
    let still_ours = msgs.iter().any(|m| {
        matches!(m, SimpleQueryMessage::Row(r)
            if r.get(0) == Some("on") && r.get(1) == Some(started))
    });
    if still_ours {
        Ok(())
    } else {
        Err(Verify::Left)
    }
}

/// [`verify_read_only_tx`] for runs without a transaction (pg-proxy shunt mode): every
/// statement runs in its own implicit transaction, made read-only by the session default. Prove
/// after each statement that the default still holds, so the next statement is read-only too.
async fn verify_read_only_session(client: &Client) -> std::result::Result<(), Verify> {
    let msgs = client
        .simple_query(
            "SELECT pg_catalog.current_setting('default_transaction_read_only'), \
             pg_catalog.current_setting('transaction_read_only')",
        )
        .await
        .map_err(|e| Verify::Broken(map_pg_error(&e)))?;
    let read_only = msgs.iter().any(|m| {
        matches!(m, SimpleQueryMessage::Row(r) if r.get(0) == Some("on") && r.get(1) == Some("on"))
    });
    if read_only { Ok(()) } else { Err(Verify::Left) }
}

/// `CALL` and `DO` can run `COMMIT` / `SET` inside one statement when they are not inside a
/// transaction block, i.e. they could switch a session-default read-only run to read-write and
/// write in the same statement. Refused in read-only runs that cannot use `BEGIN READ ONLY`.
fn is_procedural(sql: &str) -> bool {
    matches!(
        leading_words(&normalise(sql), 1)
            .first()
            .map(String::as_str),
        Some("CALL" | "DO")
    )
}

fn left_read_only_tx() -> CoreError {
    CoreError::PolicyDenied("statement left the read-only transaction".into())
}

/// The error for the statements a read-only script did not reach because statement
/// `truncated` (0-based) of `total` hit the row cap. Running them on another connection would
/// silently drop the `SET`s, temporary tables and cursors the script made on this one.
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

/// Everything `run_script` needs besides the connection and the statements.
struct ScriptCtx<'a> {
    opts: &'a ExecOptions,
    timeout_ms: Option<u64>,
    tx: &'a mpsc::Sender<QueryEvent>,
    progress: &'a AtomicUsize,
    txstate: &'a AtomicU8,
    /// Refuse transaction-control statements by their text (always `true` outside tests).
    lexical_tx_control: bool,
    /// The endpoint refuses `BEGIN`/`COMMIT`/`ROLLBACK` (pg-proxy shunt mode).
    no_transactions: bool,
}

async fn run_script(client: &Client, statements: &[String], ctx: &ScriptCtx<'_>) -> Outcome {
    let ScriptCtx {
        opts,
        timeout_ms,
        tx,
        progress,
        txstate,
        lexical_tx_control,
        no_transactions,
    } = *ctx;
    let fail = |statement_index: Option<usize>, error: CoreError| Outcome::Failed {
        statement_index,
        error,
    };
    // Before the first statement has run, a dead connection is retried rather than reported.
    let setup_failed = |e: &tokio_postgres::Error| {
        if is_connection_lost(e) {
            Outcome::Stale(map_pg_error(e))
        } else {
            fail(None, map_pg_error(e))
        }
    };
    // Read-only without a transaction: each statement's implicit transaction is read-only
    // because the session default says so (server-enforced), checked after every statement.
    let ro_session = opts.read_only && no_transactions;

    // Session setup. Read-only: one transaction for the whole script. The first query takes the
    // snapshot, after which `SET TRANSACTION READ WRITE` / `set_config('transaction_read_only')`
    // are rejected by the server; it also records the transaction's start time for
    // `verify_read_only_tx`. Otherwise the timeout is session-level (and always re-applied, so a
    // value leaked by an earlier script on this pooled connection cannot stick).
    let mut ro_tx_start: Option<String> = None;
    if ro_session {
        let t = timeout_ms.map_or_else(|| "DEFAULT".to_string(), |ms| ms.to_string());
        let setup = format!("SET default_transaction_read_only = on; SET statement_timeout = {t}");
        if let Err(e) = client.batch_execute(&setup).await {
            return setup_failed(&e);
        }
    } else if opts.read_only {
        let t = timeout_ms.map_or_else(|| "DEFAULT".to_string(), |ms| ms.to_string());
        let setup = format!(
            "BEGIN READ ONLY; SET LOCAL statement_timeout = {t}; \
             SELECT pg_catalog.transaction_timestamp()::text"
        );
        match client.simple_query(&setup).await {
            Ok(msgs) => {
                ro_tx_start = msgs.iter().rev().find_map(|m| match m {
                    SimpleQueryMessage::Row(r) => r.get(0).map(str::to_owned),
                    _ => None,
                });
                if ro_tx_start.is_none() {
                    return fail(
                        None,
                        CoreError::Other("could not open the read-only transaction".into()),
                    );
                }
            }
            Err(e) => return setup_failed(&e),
        }
    } else {
        // `SET ... = DEFAULT` rather than `RESET ...`: same effect, but pg-proxy skips `SET`
        // statements while it would send a `RESET` to its LLM analysis on every run.
        let setup = timeout_ms.map_or_else(
            || "SET statement_timeout = DEFAULT".to_string(),
            |ms| format!("SET statement_timeout = {ms}"),
        );
        if let Err(e) = client.batch_execute(&setup).await {
            return setup_failed(&e);
        }
    }

    for (idx, stmt) in statements.iter().enumerate() {
        progress.store(idx, Ordering::SeqCst);
        if opts.read_only && lexical_tx_control && is_tx_control(stmt) {
            return fail(
                Some(idx),
                CoreError::PolicyDenied(
                    "transaction control statements are not allowed in read-only mode".into(),
                ),
            );
        }
        if ro_session && is_procedural(stmt) {
            return fail(
                Some(idx),
                CoreError::PolicyDenied(
                    "CALL and DO cannot run in read-only mode through a pg-proxy in shunt mode: \
                     without a transaction they could switch the session to read-write"
                        .into(),
                ),
            );
        }
        let mut simple = false;
        let result = match client.prepare(stmt).await {
            Ok(st) => run_prepared(client, stmt, &st, idx, opts, tx).await,
            // Several commands in one string (the splitter could not separate them). Never done
            // in read-only mode: a simple-protocol string could contain its own COMMIT.
            Err(e) if !opts.read_only && is_multi_command_error(&e) => {
                simple = true;
                if txstate.load(Ordering::SeqCst) != TX_OPEN {
                    txstate.store(TX_MAYBE, Ordering::SeqCst);
                }
                run_simple(client, stmt, idx, opts, tx)
                    .await
                    .map(|()| false)
            }
            // Preparing has no side effects: a connection found dead here ran nothing yet.
            Err(e) if idx == 0 && is_connection_lost(&e) => {
                return Outcome::Stale(map_pg_error(&e));
            }
            Err(e) => Err(Stop::from(e)),
        };
        match result {
            // Before the after-statement check: the check is a query on this connection and
            // would not be answered until the server had sent the whole result.
            Ok(true) if opts.read_only => {
                return Outcome::Truncated {
                    statement_index: idx,
                };
            }
            Ok(_) => {
                // A multi-command string says nothing reliable about its first keyword.
                if !opts.read_only
                    && !simple
                    && let Some(state) = tx_effect(stmt)
                {
                    txstate.store(state, Ordering::SeqCst);
                }
            }
            Err(Stop::Failed(e)) => return fail(Some(idx), e),
            Err(Stop::Gone) => return Outcome::Gone,
        }
        if let Some(started) = &ro_tx_start {
            match verify_read_only_tx(client, started).await {
                Ok(()) => {}
                Err(Verify::Left) => {
                    return Outcome::Escaped {
                        statement_index: idx,
                        error: left_read_only_tx(),
                    };
                }
                Err(Verify::Broken(e)) => return fail(Some(idx), e),
            }
        }
        if ro_session {
            match verify_read_only_session(client).await {
                Ok(()) => {}
                Err(Verify::Left) => {
                    return Outcome::Escaped {
                        statement_index: idx,
                        error: left_read_only_tx(),
                    };
                }
                Err(Verify::Broken(e)) => return fail(Some(idx), e),
            }
        }
    }

    if ro_session {
        if let Err(e) = client
            .batch_execute("SET default_transaction_read_only = DEFAULT")
            .await
        {
            return fail(None, map_pg_error(&e));
        }
        return Outcome::Done;
    }
    if opts.read_only {
        if let Err(e) = client.batch_execute("ROLLBACK").await {
            return fail(None, map_pg_error(&e));
        }
        return Outcome::Done;
    }
    match txstate.load(Ordering::SeqCst) {
        TX_OPEN => fail(
            None,
            CoreError::Invalid("the script started a transaction (BEGIN) but never ended it; it was rolled back. Add COMMIT to keep the changes".into()),
        ),
        // The proxy refuses ROLLBACK, so whatever the multi-command string left cannot be ended.
        TX_MAYBE if no_transactions => Outcome::DoneDiscard,
        TX_MAYBE => {
            // Quiet clean-up after a simple-protocol script; "no transaction in progress" is only a warning.
            if rollback(client).await {
                txstate.store(TX_CLEAN, Ordering::SeqCst);
                Outcome::Done
            } else {
                fail(None, CoreError::NotConnected("connection lost while finishing the script".into()))
            }
        }
        _ => Outcome::Done,
    }
}

/// A pooled connection that is only allowed back into the pool when the run has ended cleanly.
///
/// If the owning future is dropped first (an agent tool call cancelled or timed out, a Tauri
/// task aborted), the connection may be mid-statement or inside a transaction the script opened.
/// Dropping the guard then takes the connection out of the pool for good and, best effort, asks
/// the server to cancel whatever it is still running.
struct PooledConn {
    obj: Option<Object>,
    shared: Arc<Shared>,
    token: CancelToken,
}

impl PooledConn {
    fn new(obj: Object, shared: Arc<Shared>) -> Self {
        let token = obj.cancel_token();
        Self {
            obj: Some(obj),
            shared,
            token,
        }
    }

    fn client(&self) -> Option<&Client> {
        self.obj.as_ref().map(|o| &o.client)
    }

    /// The run is over: the connection goes back to the pool once its session has been reset.
    /// The reset runs in the background, so it adds nothing to the caller's latency; the next
    /// checkout waits for it only if it arrives sooner. Advisory locks, temp tables and `SET`s
    /// are released now rather than whenever the connection is next used.
    fn release(mut self) {
        let Some(mut obj) = self.obj.take() else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    match shared.reset_session(&obj.client).await {
                        Ok(()) => obj.reset_done = true,
                        Err(e) => {
                            tracing::debug!(error = %e, "discarding pooled connection");
                            drop(Object::take(obj));
                        }
                    }
                });
            }
            // No runtime to run the reset on: the pool's recycle step will do it.
            Err(_) => drop(obj),
        }
    }

    /// Removes the connection from the pool and closes it.
    fn discard(mut self) {
        if let Some(obj) = self.obj.take() {
            drop(Object::take(obj));
        }
    }

    /// Ends a session-default read-only run (pg-proxy shunt mode); `true` when that worked.
    async fn clear_read_only_session(&self) -> bool {
        match self.client() {
            Some(c) => matches!(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    c.batch_execute("SET default_transaction_read_only = DEFAULT"),
                )
                .await,
                Ok(Ok(()))
            ),
            None => false,
        }
    }

    /// Best-effort `ROLLBACK`; `true` when the session is known to be idle afterwards.
    async fn rollback(&self) -> bool {
        match self.client() {
            Some(c) => rollback(c).await,
            None => false,
        }
    }
}

impl Drop for PooledConn {
    fn drop(&mut self) {
        let Some(obj) = self.obj.take() else { return };
        let client = Object::take(obj);
        let shared = Arc::clone(&self.shared);
        let token = self.token.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                // Keep the client open until the cancel request has been delivered: the server
                // then interrupts the statement instead of finishing it for nobody.
                rt.spawn(async move {
                    shared.cancel_backend(token).await;
                    drop(client);
                });
            }
            Err(_) => drop(client),
        }
    }
}

impl PgHandle {
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

    /// [`run`](Self::run) with the lexical transaction-control refusal switchable, so tests can
    /// prove that the after-statement transaction check holds even when the lexer is fooled.
    /// Production code always passes `true`.
    async fn run_checked(
        &self,
        statements: &[String],
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
        lexical_tx_control: bool,
    ) -> End {
        if statements.is_empty() {
            return End::Finished;
        }
        let _guard = self.activity();
        let no_transactions = self.shared.no_transactions();
        let timeout_ms = opts
            .timeout_ms
            .or(self.shared.params.options.statement_timeout_ms);
        let mut stale_retries = 0;
        loop {
            let obj: Object = tokio::select! {
                biased;
                _ = cancel.cancelled() => return End::Failed { statement_index: None, error: CoreError::Cancelled },
                r = self.pool.get() => match r {
                    Ok(c) => c,
                    Err(e) => return End::Failed { statement_index: None, error: map_pool_error(e) },
                },
            };
            let conn = PooledConn::new(obj, Arc::clone(&self.shared));
            let progress = AtomicUsize::new(NO_STATEMENT);
            let txstate = AtomicU8::new(TX_CLEAN);

            let ctx = ScriptCtx {
                opts,
                timeout_ms,
                tx,
                progress: &progress,
                txstate: &txstate,
                lexical_tx_control,
                no_transactions,
            };
            let outcome = {
                let Some(client) = conn.client() else {
                    return End::Failed {
                        statement_index: None,
                        error: CoreError::Other("pooled connection unavailable".into()),
                    };
                };
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    // Nobody is listening any more: stop even if the server is still thinking.
                    _ = tx.closed() => Some(Outcome::Gone),
                    o = run_script(client, statements, &ctx) => Some(o),
                }
            };
            let current = Some(progress.load(Ordering::SeqCst)).filter(|&i| i != NO_STATEMENT);

            return match outcome {
                None => {
                    // The query future was cancelled mid-protocol: ask the server to stop it, then
                    // discard the connection so a late cancel can never hit someone else's query.
                    self.shared.cancel_backend(conn.token.clone()).await;
                    conn.discard();
                    End::Failed {
                        statement_index: current,
                        error: CoreError::Cancelled,
                    }
                }
                Some(Outcome::Stale(error)) => {
                    conn.discard();
                    if stale_retries < STALE_RETRIES {
                        stale_retries += 1;
                        tracing::info!(error = %error, "pooled Postgres connection was dead; retrying on a fresh one");
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
                Some(Outcome::DoneDiscard) => {
                    conn.discard();
                    End::Finished
                }
                Some(Outcome::Truncated { statement_index }) => {
                    // Dropping the guard asks the server to cancel the statement and closes the
                    // connection. The after-statement read-only check was skipped for this
                    // statement, and that is safe only because of this: nothing else ever runs
                    // on the session, and it never goes back to the pool. Do not turn this into
                    // a `release`.
                    drop(conn);
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
                Some(Outcome::Gone) => {
                    // Dropping the guard cancels whatever the server is still running.
                    drop(conn);
                    End::Gone
                }
                Some(Outcome::Escaped {
                    statement_index,
                    error,
                }) => {
                    // Do not trust this session any more: roll back what is left, then close it.
                    let _ = conn.rollback().await;
                    conn.discard();
                    End::Failed {
                        statement_index: Some(statement_index),
                        error,
                    }
                }
                Some(Outcome::Failed {
                    statement_index,
                    error,
                }) => {
                    // Every error path leaves the session idle or loses the connection. The pool's
                    // reset on the next checkout is the backstop for what this cannot see.
                    let tx_dirty = txstate.load(Ordering::SeqCst) != TX_CLEAN;
                    let clean = if opts.read_only && no_transactions && !tx_dirty {
                        conn.clear_read_only_session().await
                    } else if opts.read_only || tx_dirty {
                        conn.rollback().await
                    } else {
                        true
                    };
                    if clean {
                        conn.release();
                    } else {
                        conn.discard();
                    }
                    End::Failed {
                        statement_index,
                        error,
                    }
                }
            };
        }
    }

    /// Runs one or more statements sequentially, streaming events into `tx`.
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
        self.execute_statements(query_id, split_script(sql), opts, cancel, tx)
            .await
    }

    /// Like [`execute_stream`](Self::execute_stream) for statements the caller has already split
    /// (no splitting is done here). A statement that turns out to hold several commands is run
    /// through the simple protocol with text-valued cells, except in read-only mode where it is
    /// rejected.
    pub async fn execute_statements(
        &self,
        query_id: String,
        statements: Vec<String>,
        opts: ExecOptions,
        cancel: CancellationToken,
        tx: mpsc::Sender<QueryEvent>,
    ) -> Result<()> {
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
        // If the receiver vanished between the last event and now there is nobody to tell.
        let _ = tx.send(terminal).await;
        Ok(())
    }

    /// Bounded, materialised execution: the result of the last statement that returns rows
    /// (or of the last statement when none does). Errors are returned as `Err`.
    pub async fn execute_collect(&self, sql: &str, opts: ExecOptions) -> Result<QueryResult> {
        let statements = split_script(sql);
        if statements.is_empty() {
            return Ok(QueryResult::default());
        }
        let (tx, mut rx) = mpsc::channel::<QueryEvent>(32);
        let cancel = CancellationToken::new();
        let runner = async {
            let end = self.run(&statements, &opts, &cancel, &tx).await;
            drop(tx);
            end
        };
        let collector = async {
            let mut collected = Collector::default();
            while let Some(ev) = rx.recv().await {
                collected.push(ev);
            }
            collected
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

    #[test]
    fn leading_words_skip_comments_and_parens() {
        assert_eq!(
            leading_words("  -- hi\n /* a /* nested */ b */ select 1", 2),
            vec!["SELECT"]
        );
        assert_eq!(
            leading_words("(SELECT 1) UNION SELECT 2", 1),
            vec!["SELECT"]
        );
        assert_eq!(leading_words("", 2), Vec::<String>::new());
        assert_eq!(leading_words("$$ x $$", 2), Vec::<String>::new());
    }

    #[test]
    fn line_comments_end_at_cr_as_well_as_lf() {
        // The server ends a `--` comment at '\r'; so must every lexer here.
        assert_eq!(leading_words("--x\rCOMMIT", 1), vec!["COMMIT"]);
        assert_eq!(leading_words("--x\nCOMMIT", 1), vec!["COMMIT"]);
        assert_eq!(leading_words("--x\r\nCOMMIT", 1), vec!["COMMIT"]);
        assert_eq!(leading_words("--x COMMIT", 1), Vec::<String>::new());
        // '\v' is whitespace to the server's scanner.
        assert_eq!(leading_words("\x0bCOMMIT", 1), vec!["COMMIT"]);
        assert_eq!(
            leading_words("\x0c\x0b /* c */\x0bCOMMIT", 1),
            vec!["COMMIT"]
        );
    }

    #[test]
    fn cr_terminated_comments_cannot_hide_transaction_control() {
        for s in [
            // The reviewer's probe, split the way a '\n'-only splitter would split it.
            "--x\rCOMMIT /*\nSHOW x */;",
            "\n--x\rDROP TABLE probe_victim /*\nSHOW x */",
            "--x\rCOMMIT /*\nEXPLAIN SELECT 1 */;",
            "-- a\r-- b\r\n/* c */\rROLLBACK",
            "--\rBEGIN",
            "\x0bCOMMIT",
            "/* a */\x0b--x\rEND",
            "COMMIT--x\rAND CHAIN",
        ] {
            let first = leading_words(&normalise(s), 1);
            if matches!(first.first().map(String::as_str), Some("DROP")) {
                continue; // not transaction control, only here to pin the lexing
            }
            assert!(is_tx_control(s), "{s:?}");
            assert!(tx_effect(s).is_some(), "{s:?}");
        }
        assert_eq!(
            leading_words(
                &normalise("\n--x\rDROP TABLE probe_victim /*\nSHOW x */"),
                2
            ),
            vec!["DROP", "TABLE"]
        );
        // A comment-looking string is not a comment.
        assert!(!is_tx_control("SELECT '--\rCOMMIT'"));
    }

    #[test]
    fn normalise_strips_comments_like_the_server() {
        assert_eq!(normalise("a--x\rb"), "a \rb");
        assert_eq!(normalise("a--x\nb"), "a \nb");
        assert_eq!(normalise("a/* x /* y */ z */b"), "a b");
        assert_eq!(normalise("a/* never closed"), "a ");
        // Quotes protect their contents.
        assert_eq!(normalise("'--x\r/* y */'"), "'--x\r/* y */'");
        assert_eq!(normalise("\"a--b\"\"c\" -- d"), "\"a--b\"\"c\"  ");
        assert_eq!(
            normalise("'it''s -- not a comment' -- is"),
            "'it''s -- not a comment'  "
        );
        assert_eq!(
            normalise(r"E'\' -- still a string' -- c"),
            r"E'\' -- still a string'  "
        );
        assert_eq!(normalise("$$ -- x $$ -- y"), "$$ -- x $$  ");
        assert_eq!(normalise("$t$ $$ -- x $t$--y\rz"), "$t$ $$ -- x $t$ \rz");
        // `$1` is a parameter, not a dollar quote.
        assert_eq!(normalise("select $1 -- c\r, $2"), "select $1  \r, $2");
        // Multi-byte text survives.
        assert_eq!(normalise("'é' --x\rü"), "'é'  \rü");
    }

    #[test]
    fn command_tags() {
        assert_eq!(
            command_tag("insert into t values (1)", false).as_deref(),
            Some("INSERT")
        );
        assert_eq!(
            command_tag("INSERT INTO t VALUES (1) RETURNING id", true).as_deref(),
            Some("INSERT")
        );
        assert_eq!(
            command_tag("create table t (a int)", false).as_deref(),
            Some("CREATE TABLE")
        );
        assert_eq!(
            command_tag("CREATE OR REPLACE FUNCTION f() ...", false).as_deref(),
            Some("CREATE FUNCTION")
        );
        assert_eq!(
            command_tag("create unique index i on t(a)", false).as_deref(),
            Some("CREATE INDEX")
        );
        assert_eq!(
            command_tag("CREATE TEMP TABLE x()", false).as_deref(),
            Some("CREATE TABLE")
        );
        assert_eq!(
            command_tag("create materialized view v as select 1", false).as_deref(),
            Some("CREATE MATERIALIZED VIEW")
        );
        assert_eq!(
            command_tag("DROP FOREIGN TABLE f", false).as_deref(),
            Some("DROP FOREIGN TABLE")
        );
        assert_eq!(
            command_tag("WITH x AS (SELECT 1) SELECT * FROM x", true).as_deref(),
            Some("SELECT")
        );
        assert_eq!(
            command_tag("truncate t", false).as_deref(),
            Some("TRUNCATE TABLE")
        );
        assert_eq!(
            command_tag("show server_version", true).as_deref(),
            Some("SHOW")
        );
        assert_eq!(command_tag(";;", false), None);
    }

    #[test]
    fn transaction_control_detection() {
        for s in [
            "COMMIT",
            "commit;",
            "END",
            "ROLLBACK",
            "ROLLBACK TO SAVEPOINT x",
            "ABORT",
            "BEGIN",
            "START TRANSACTION",
            "PREPARE TRANSACTION 'x'",
            "SET TRANSACTION READ WRITE",
            "set session characteristics as transaction read write",
            "/* c */ COMMIT",
        ] {
            assert!(is_tx_control(s), "{s}");
        }
        for s in [
            "SELECT 1",
            "SET search_path = x",
            "SET LOCAL x = 1",
            "PREPARE p AS SELECT 1",
            "SAVEPOINT a",
            "EXPLAIN SELECT 1",
            "INSERT INTO t VALUES (1)",
        ] {
            assert!(!is_tx_control(s), "{s}");
        }
    }

    #[test]
    fn tx_effects() {
        assert_eq!(tx_effect("BEGIN"), Some(TX_OPEN));
        assert_eq!(tx_effect("start transaction"), Some(TX_OPEN));
        assert_eq!(tx_effect("COMMIT"), Some(TX_CLEAN));
        assert_eq!(tx_effect("ROLLBACK"), Some(TX_CLEAN));
        assert_eq!(tx_effect("ROLLBACK TO SAVEPOINT a"), None);
        assert_eq!(tx_effect("ROLLBACK WORK TO a"), None);
        assert_eq!(tx_effect("ROLLBACK TRANSACTION TO SAVEPOINT a"), None);
        assert_eq!(tx_effect("COMMIT AND CHAIN"), Some(TX_OPEN));
        assert_eq!(tx_effect("commit work and chain"), Some(TX_OPEN));
        assert_eq!(tx_effect("ROLLBACK AND CHAIN"), Some(TX_OPEN));
        assert_eq!(tx_effect("COMMIT AND NO CHAIN"), Some(TX_CLEAN));
        assert_eq!(tx_effect("SELECT 1"), None);
    }

    #[test]
    fn collector_prefers_last_row_returning_statement() {
        let mut c = Collector::default();
        c.push(QueryEvent::Columns {
            statement_index: 0,
            columns: vec![ColumnMeta {
                name: "a".into(),
                type_name: "int4".into(),
                type_oid: 23,
            }],
        });
        c.push(QueryEvent::Rows {
            statement_index: 0,
            rows: vec![vec![Value::from(1)]],
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
            rows_affected: Some(3),
            row_count: 0,
            truncated: false,
            elapsed_ms: 2,
        });
        let r = c.into_result().expect("result");
        assert_eq!(r.rows, vec![vec![Value::from(1)]]);
        assert_eq!(r.command_tag.as_deref(), Some("SELECT"));
    }

    #[test]
    fn collector_falls_back_to_last_statement() {
        let mut c = Collector::default();
        c.push(QueryEvent::StatementDone {
            statement_index: 0,
            command_tag: Some("CREATE TABLE".into()),
            rows_affected: None,
            row_count: 0,
            truncated: false,
            elapsed_ms: 1,
        });
        c.push(QueryEvent::StatementDone {
            statement_index: 1,
            command_tag: Some("INSERT".into()),
            rows_affected: Some(3),
            row_count: 0,
            truncated: false,
            elapsed_ms: 2,
        });
        let r = c.into_result().expect("result");
        assert_eq!(r.rows_affected, Some(3));
        assert_eq!(r.command_tag.as_deref(), Some("INSERT"));
    }

    // ---- database-backed (skipped without VG_TEST_PG; see tests/services) ----------------------

    use futures_util::FutureExt;
    use secrecy::SecretString;

    use crate::pg::PgConnectParams;

    async fn handle() -> Option<Arc<PgHandle>> {
        let raw = std::env::var("VG_TEST_PG").ok()?;
        let (mut host, mut port, mut user, mut pw, mut db) = (
            "127.0.0.1".to_string(),
            5432u16,
            "postgres".to_string(),
            String::new(),
            "postgres".to_string(),
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
        let params = PgConnectParams {
            host: host.clone(),
            port,
            tls_server_name: host,
            database: db,
            tls: TlsConfig {
                mode: TlsMode::Require,
                ..Default::default()
            },
            options: PgOptions {
                max_pool_size: 1,
                ..Default::default()
            },
        };
        PgHandle::connect(params, creds).await.ok()
    }

    async fn text(h: &PgHandle, sql: &str, read_only: bool) -> String {
        let r = h
            .execute_collect(
                sql,
                ExecOptions {
                    read_only,
                    ..Default::default()
                },
            )
            .await
            .expect("query");
        r.rows[0][0].as_str().unwrap_or_default().to_string()
    }

    /// With the lexical refusal switched off, a statement that really does leave the read-only
    /// transaction is still caught right after it ran, the rest of the script never runs, and
    /// the connection is thrown away.
    #[tokio::test]
    async fn after_statement_check_catches_what_the_lexer_misses() {
        let Some(h) = handle().await else {
            crate::test_services::skip("pg", "VG_TEST_PG is not set");
            return;
        };
        let table = format!("vg_unit_victim_{}", uuid::Uuid::new_v4().simple());
        h.execute_collect(
            &format!("CREATE TABLE {table} (a int)"),
            ExecOptions::default(),
        )
        .await
        .expect("create");
        let exists = format!("SELECT (to_regclass('{table}') IS NOT NULL)::text");
        let ro = ExecOptions {
            read_only: true,
            ..Default::default()
        };
        // Includes the chained forms: the new transaction is read-only too, so only its
        // different start time gives it away.
        for escape in [
            "COMMIT",
            "END",
            "ROLLBACK",
            "ABORT",
            "COMMIT AND CHAIN",
            "ROLLBACK AND CHAIN",
        ] {
            let before = text(&h, "SELECT pg_backend_pid()::text", false).await;
            let stmts = vec![escape.to_string(), format!("DROP TABLE {table}")];
            let (tx, _rx) = mpsc::channel(64);
            let end = h
                .run_checked(&stmts, &ro, &CancellationToken::new(), &tx, false)
                .await;
            match end {
                End::Failed {
                    statement_index: Some(0),
                    error: CoreError::PolicyDenied(m),
                } => assert_eq!(m, "statement left the read-only transaction", "{escape}"),
                End::Failed { error, .. } => panic!("{escape}: wrong error {error:?}"),
                _ => panic!("{escape}: run did not fail"),
            }
            assert_eq!(text(&h, &exists, false).await, "true", "{escape}");
            let after = text(&h, "SELECT pg_backend_pid()::text", false).await;
            assert_ne!(
                before, after,
                "{escape}: connection was returned to the pool"
            );
            // And the new connection is a normal, idle one.
            assert_eq!(
                text(&h, "SELECT current_setting('transaction_read_only')", true).await,
                "on"
            );
            assert_eq!(
                text(&h, "SELECT current_setting('transaction_read_only')", false).await,
                "off"
            );
        }
        h.execute_collect(&format!("DROP TABLE {table}"), ExecOptions::default())
            .await
            .expect("drop");
    }

    /// The after-statement check also holds when the server's *default* is read-only, so that
    /// "read-only is on" alone would not tell a fresh autocommit statement from our transaction.
    #[tokio::test]
    async fn after_statement_check_does_not_trust_read_only_alone() {
        let Some(h) = handle().await else {
            crate::test_services::skip("pg", "VG_TEST_PG is not set");
            return;
        };
        let obj = h.pool.get().await.expect("conn");
        let client: &Client = &obj;
        let msgs = client
            .simple_query("BEGIN READ ONLY; SELECT pg_catalog.transaction_timestamp()::text")
            .await
            .expect("begin");
        let started = msgs
            .iter()
            .rev()
            .find_map(|m| match m {
                SimpleQueryMessage::Row(r) => r.get(0).map(str::to_owned),
                _ => None,
            })
            .expect("timestamp");
        assert!(verify_read_only_tx(client, &started).await.is_ok());
        client
            .batch_execute("COMMIT; SET default_transaction_read_only = on")
            .await
            .expect("escape");
        let on = client
            .simple_query("SELECT current_setting('transaction_read_only')")
            .await
            .expect("show");
        assert!(
            on.iter()
                .any(|m| matches!(m, SimpleQueryMessage::Row(r) if r.get(0) == Some("on")))
        );
        assert!(matches!(
            verify_read_only_tx(client, &started).await,
            Err(Verify::Left)
        ));
        drop(Object::take(obj));
    }

    /// The pool's own reset: a connection that comes back dirty without going through the
    /// engine's clean-up (here: checked out directly and dropped) is made idle and pristine at
    /// the next checkout, including an open transaction, `SET`s and a session advisory lock.
    #[tokio::test]
    async fn recycle_resets_a_connection_returned_dirty() {
        let Some(h) = handle().await else {
            crate::test_services::skip("pg", "VG_TEST_PG is not set");
            return;
        };
        let key = i64::from(uuid::Uuid::new_v4().as_u128() as u32);
        let table = format!("vg_unit_dirty_{}", uuid::Uuid::new_v4().simple());
        let default_path = text(&h, "SELECT current_setting('search_path')", false).await;
        let pid = {
            let obj = h.pool.get().await.expect("conn");
            obj.batch_execute(&format!(
                "SELECT pg_advisory_lock({key}); BEGIN; CREATE TABLE {table} (a int); \
                 SET search_path = pg_catalog; PREPARE vg_unit_p AS SELECT 1"
            ))
            .await
            .expect("dirty the session");
            text_on(&obj, "SELECT pg_backend_pid()::text").await
            // `obj` goes back with a transaction open and no reset done.
        };
        let other = {
            let (c, conn) = tokio_postgres::connect(
                &std::env::var("VG_TEST_PG").unwrap_or_default(),
                tokio_postgres::NoTls,
            )
            .await
            .expect("raw");
            tokio::spawn(async move {
                let _ = conn.await;
            });
            c
        };
        let held: bool = other
            .query_one(&format!("SELECT pg_try_advisory_lock({key})"), &[])
            .await
            .expect("try")
            .get(0);
        assert!(!held, "precondition: the dirty connection holds the lock");

        let obj = h.pool.get().await.expect("recycled conn");
        assert_eq!(
            text_on(&obj, "SELECT pg_backend_pid()::text").await,
            pid,
            "reused"
        );
        assert_eq!(
            text_on(&obj, "SELECT current_setting('search_path')").await,
            default_path
        );
        assert_eq!(
            text_on(
                &obj,
                &format!("SELECT (to_regclass('{table}') IS NULL)::text")
            )
            .await,
            "true",
            "the leftover transaction must be rolled back"
        );
        assert_eq!(
            text_on(
                &obj,
                "SELECT count(*)::text FROM pg_prepared_statements WHERE name = 'vg_unit_p'"
            )
            .await,
            "0"
        );
        let free: bool = other
            .query_one(&format!("SELECT pg_try_advisory_lock({key})"), &[])
            .await
            .expect("try")
            .get(0);
        assert!(free, "advisory lock survived the recycle");
    }

    async fn text_on(c: &Client, sql: &str) -> String {
        for m in c.simple_query(sql).await.expect("query") {
            if let SimpleQueryMessage::Row(r) = m {
                return r.get(0).unwrap_or_default().to_string();
            }
        }
        String::new()
    }
}
