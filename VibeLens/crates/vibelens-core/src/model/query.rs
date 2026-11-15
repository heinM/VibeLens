use serde::{Deserialize, Serialize};

use crate::error::ErrorPayload;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ExecOptions {
    /// Run inside `BEGIN READ ONLY` (or `SET TRANSACTION READ ONLY`). Server-enforced.
    pub read_only: bool,
    /// Hard cap on rows materialised per statement; the rest are discarded and `truncated=true`.
    pub max_rows: usize,
    /// Rows per `QueryEvent::Rows` message (keeps IPC messages small).
    pub page_size: usize,
    /// Per-statement `statement_timeout`.
    pub timeout_ms: Option<u64>,
}

impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            read_only: false,
            max_rows: 100_000,
            page_size: 500,
            timeout_ms: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
    pub type_oid: u32,
}

/// Cell encoding rule (see docs/DESIGN.md §IPC): int2/int4/float → JSON number;
/// int8, numeric, money → JSON *string* (JS numbers lose precision past 2^53);
/// bytea → `{"$bytes": "<base64>", "len": n}`; json/jsonb → embedded JSON;
/// timestamps → RFC3339 strings; NULL → null; unknown types → text.
pub type Cell = serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum QueryEvent {
    Started {
        query_id: String,
        statement_count: usize,
    },
    Columns {
        statement_index: usize,
        columns: Vec<ColumnMeta>,
    },
    Rows {
        statement_index: usize,
        rows: Vec<Vec<Cell>>,
    },
    StatementDone {
        statement_index: usize,
        command_tag: Option<String>,
        rows_affected: Option<u64>,
        row_count: usize,
        truncated: bool,
        elapsed_ms: u64,
    },
    /// Engine-reported execution statistics for one statement (Iceberg: DataFusion / Athena).
    /// Sent before that statement's `StatementDone`; optional, Postgres never sends it.
    Stats {
        statement_index: usize,
        /// `datafusion` | `athena`
        engine: String,
        bytes_scanned: Option<u64>,
        /// Athena list-price estimate (`athena::cost_estimate_usd`); never a bill.
        cost_usd_estimate: Option<f64>,
        engine_query_id: Option<String>,
    },
    Finished {
        elapsed_ms: u64,
    },
    Failed {
        statement_index: Option<usize>,
        error: ErrorPayload,
    },
}

/// Bounded, fully-materialised result (agent tools, tests).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    pub row_count: usize,
    pub truncated: bool,
    pub command_tag: Option<String>,
    pub rows_affected: Option<u64>,
    pub elapsed_ms: u64,
}
