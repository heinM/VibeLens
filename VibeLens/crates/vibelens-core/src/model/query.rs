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
///
/// A value larger than [`MAX_CELL_BYTES`] is not decoded in full. Its cell is the marker built
/// by [`truncated_cell`]: `{"$truncated": {"kind": "text" | "bytes" | "json" | "other",
/// "bytes": <size of the whole value>, "preview": "<its first part>"}}`. The preview is text for
/// `text` and `json` (the JSON source, cut, so not parseable), base64 for `bytes`, and empty for
/// `other` (arrays, ranges and other types that have no readable prefix). **A marker is not the
/// value**: nothing may write it back, compare with it in SQL, or export it as if it were
/// complete.
pub type Cell = serde_json::Value;

/// Largest value that is decoded in full, in bytes as the database sends it. A larger one
/// becomes a [`truncated_cell`] holding at most this much of it.
///
/// This lowers peak memory, it does not bound it: the driver has already received the whole row
/// when a value is decoded. What it removes are the copies made afterwards (the decoded value,
/// its JSON form for IPC, the copy the UI keeps).
pub const MAX_CELL_BYTES: usize = 1024 * 1024;

/// A `QueryEvent::Rows` page is sent as soon as its cells weigh this much ([`cell_weight`]),
/// even when it holds fewer rows than `page_size`, so a page of large cells does not grow into
/// one enormous IPC message.
pub const PAGE_BYTE_BUDGET: usize = 4 * 1024 * 1024;

/// What a truncated value was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedKind {
    /// Text; the preview is its first part, cut at a character boundary.
    Text,
    /// Binary; the preview is base64 of its first bytes.
    Bytes,
    /// JSON; the preview is the first part of the JSON text.
    Json,
    /// Anything with no readable prefix (arrays, ranges, …); the preview is empty.
    Other,
}

/// The marker cell for a value of `total_bytes` bytes that was not decoded in full. `head` is
/// the start of the value; at most [`MAX_CELL_BYTES`] of it are kept.
pub fn truncated_cell(kind: TruncatedKind, total_bytes: usize, head: &[u8]) -> Cell {
    use base64::Engine as _;
    let head = &head[..head.len().min(MAX_CELL_BYTES)];
    let (kind, preview) = match kind {
        TruncatedKind::Text => ("text", text_prefix(head)),
        TruncatedKind::Json => ("json", text_prefix(head)),
        TruncatedKind::Bytes => (
            "bytes",
            base64::engine::general_purpose::STANDARD.encode(head),
        ),
        TruncatedKind::Other => ("other", String::new()),
    };
    serde_json::json!({ "$truncated": { "kind": kind, "bytes": total_bytes, "preview": preview } })
}

/// `head` as text, without the partial character a cut may have left at the end.
fn text_prefix(head: &[u8]) -> String {
    match std::str::from_utf8(head) {
        Ok(s) => s.to_owned(),
        // Cut inside a character: keep what is valid before it.
        Err(e) if e.error_len().is_none() => {
            String::from_utf8_lossy(&head[..e.valid_up_to()]).into_owned()
        }
        Err(_) => String::from_utf8_lossy(head).into_owned(),
    }
}

/// Whether `cell` is a [`truncated_cell`] marker.
pub fn is_truncated_cell(cell: &Cell) -> bool {
    cell.as_object()
        .is_some_and(|m| m.len() == 1 && m.get("$truncated").is_some_and(|t| t.is_object()))
}

/// Roughly how many bytes `cell` occupies, for [`PAGE_BYTE_BUDGET`]. Cheap and approximate.
pub fn cell_weight(cell: &Cell) -> usize {
    match cell {
        Cell::Null | Cell::Bool(_) | Cell::Number(_) => 8,
        Cell::String(s) => 8 + s.len(),
        Cell::Array(a) => 8 + a.iter().map(cell_weight).sum::<usize>(),
        Cell::Object(m) => {
            8 + m
                .iter()
                .map(|(k, v)| k.len() + cell_weight(v))
                .sum::<usize>()
        }
    }
}

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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_truncated_cell_says_what_was_cut_and_keeps_only_the_start() {
        let c = truncated_cell(TruncatedKind::Text, 50_000_000, b"hello");
        assert_eq!(
            c,
            json!({"$truncated": {"kind": "text", "bytes": 50_000_000, "preview": "hello"}})
        );
        assert!(is_truncated_cell(&c));

        // Never more than the limit, whatever is handed in.
        let big = vec![b'a'; MAX_CELL_BYTES + 10];
        let c = truncated_cell(TruncatedKind::Text, big.len(), &big);
        assert_eq!(
            c["$truncated"]["preview"].as_str().unwrap().len(),
            MAX_CELL_BYTES
        );

        let c = truncated_cell(TruncatedKind::Bytes, 9, &[0xDE, 0xAD, 0xBE]);
        assert_eq!(c["$truncated"]["kind"], "bytes");
        assert_eq!(c["$truncated"]["preview"], "3q2+");

        let c = truncated_cell(TruncatedKind::Json, 9, br#"{"a": [1, 2"#);
        assert_eq!(c["$truncated"]["kind"], "json");
        assert_eq!(c["$truncated"]["preview"], r#"{"a": [1, 2"#);

        let c = truncated_cell(TruncatedKind::Other, 9, b"\x00\x01");
        assert_eq!(
            c["$truncated"],
            json!({"kind": "other", "bytes": 9, "preview": ""})
        );
    }

    #[test]
    fn a_text_preview_never_ends_inside_a_character() {
        // "é" is two bytes; cut after the first byte of the third one.
        let s = "ééé".as_bytes();
        let c = truncated_cell(TruncatedKind::Text, 6, &s[..5]);
        assert_eq!(c["$truncated"]["preview"], "éé");
        // Bytes that are not UTF-8 at all are still shown, with replacement characters.
        let c = truncated_cell(TruncatedKind::Text, 3, &[b'a', 0xFF, b'b']);
        assert_eq!(c["$truncated"]["preview"], "a\u{FFFD}b");
    }

    #[test]
    fn ordinary_values_are_not_markers() {
        for v in [
            json!(null),
            json!("text"),
            json!({"$bytes": "AA==", "len": 1}),
            json!({"$truncated": true}),
            json!({"$truncated": {"kind": "text"}, "other": 1}),
            json!([{"$truncated": {}}]),
        ] {
            assert!(!is_truncated_cell(&v), "{v}");
        }
    }

    #[test]
    fn cell_weight_follows_the_size_of_the_value() {
        assert!(cell_weight(&json!(1)) < 16);
        assert!(cell_weight(&json!("x".repeat(1000))) >= 1000);
        assert!(cell_weight(&json!({"key": ["x".repeat(1000), "y".repeat(1000)]})) >= 2000);
    }
}
