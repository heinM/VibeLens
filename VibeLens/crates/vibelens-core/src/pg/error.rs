//! Mapping from driver/pool errors to [`CoreError`]. Messages never include credentials:
//! `tokio_postgres` errors do not carry the password, and we never format the `Config`.

use deadpool::managed::PoolError;
use tokio_postgres::error::{DbError, SqlState};

use crate::error::CoreError;

/// Flattens an error and its `source()` chain into `a: b: c`.
pub(super) fn error_chain(e: &(dyn std::error::Error + 'static)) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(s) = cur {
        let msg = s.to_string();
        if !out.contains(&msg) {
            out.push_str(": ");
            out.push_str(&msg);
        }
        cur = s.source();
    }
    out
}

fn db_message(db: &DbError) -> String {
    let mut m = db.message().to_string();
    if let Some(tokio_postgres::error::ErrorPosition::Original(p)) = db.position() {
        m.push_str(&format!(" (position {p})"));
    }
    if let Some(d) = db.detail() {
        m.push_str(&format!("\nDETAIL: {d}"));
    }
    if let Some(h) = db.hint() {
        m.push_str(&format!("\nHINT: {h}"));
    }
    m
}

fn has_timeout_cause(e: &(dyn std::error::Error + 'static)) -> bool {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cur {
        if let Some(io) = c.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::TimedOut
        {
            return true;
        }
        cur = c.source();
    }
    false
}

/// A query refused by capitec/pg-proxy: `P0001` (raise_exception) with one of its own messages
/// ("Query blocked by SQL Engineer: ...", "Query blocked in shunt mode: ..."). A user's own
/// `RAISE EXCEPTION` with a different text stays a plain database error.
pub(super) fn is_pgproxy_block(db: &DbError) -> bool {
    *db.code() == SqlState::RAISE_EXCEPTION && db.message().starts_with("Query blocked ")
}

/// Maps a `tokio_postgres` error to the closest [`CoreError`].
pub(super) fn map_pg_error(e: &tokio_postgres::Error) -> CoreError {
    if let Some(db) = e.as_db_error() {
        let code = db.code();
        let msg = db_message(db);
        if *code == SqlState::INVALID_PASSWORD
            || *code == SqlState::INVALID_AUTHORIZATION_SPECIFICATION
        {
            return CoreError::Auth(msg);
        }
        if is_pgproxy_block(db) {
            return CoreError::PolicyDenied(msg);
        }
        if *code == SqlState::QUERY_CANCELED {
            return if db.message().contains("statement timeout") {
                CoreError::Timeout(msg)
            } else {
                CoreError::Cancelled
            };
        }
        return CoreError::Database(msg);
    }
    let chain = error_chain(e);
    if has_timeout_cause(e) {
        return CoreError::Timeout(chain);
    }
    if e.is_closed() {
        return CoreError::NotConnected(chain);
    }
    CoreError::Database(chain)
}

/// The connection itself is gone (closed by the server, a proxy or the network), as opposed to
/// the server rejecting a statement. Nothing the caller sent can have been executed when this
/// happens before the first statement ran, so the run may be retried on a fresh connection.
pub(super) fn is_connection_lost(e: &tokio_postgres::Error) -> bool {
    if let Some(db) = e.as_db_error() {
        // Class 08 (connection exception), admin shutdown / crash, idle_session_timeout.
        let c = db.code().code();
        return c.starts_with("08") || matches!(c, "57P01" | "57P02" | "57P05");
    }
    if e.is_closed() {
        return true;
    }
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cur {
        if let Some(io) = c.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind::*;
            return matches!(
                io.kind(),
                BrokenPipe | ConnectionReset | ConnectionAborted | NotConnected | UnexpectedEof
            );
        }
        cur = c.source();
    }
    false
}

/// Maps a pool checkout error.
pub(super) fn map_pool_error(e: PoolError<CoreError>) -> CoreError {
    match e {
        PoolError::Backend(e) => e,
        PoolError::Timeout(t) => CoreError::Timeout(match t {
            deadpool::managed::TimeoutType::Wait => {
                "no free database connection became available".into()
            }
            deadpool::managed::TimeoutType::Create => {
                "timed out establishing a database connection".into()
            }
            deadpool::managed::TimeoutType::Recycle => {
                "timed out checking a pooled database connection".into()
            }
        }),
        PoolError::Closed => CoreError::NotConnected("the connection pool is closed".into()),
        other => CoreError::Database(format!("connection pool: {other}")),
    }
}
