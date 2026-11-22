//! MySQL text-protocol values → [`Cell`]s. Every value arrives as bytes (or NULL) with the
//! column's type, flags and character set; nothing here can panic on what a server sends.
//!
//! - integers that fit a JavaScript number exactly (all but `BIGINT`) → JSON numbers;
//!   `BIGINT`, `DECIMAL` → strings (as Postgres `int8`/`numeric`, no lost precision);
//! - `FLOAT`/`DOUBLE` → numbers when finite, else the text the server sent;
//! - `BIT(n)` → a string of `n` binary digits;
//! - `JSON` → the parsed value (text when it does not parse, e.g. nesting deeper than 128);
//! - dates and times → the server's text as it is (zero dates such as `0000-00-00` included);
//! - binary strings, `BLOB`, `GEOMETRY`, `VECTOR` and text that is not valid UTF-8 →
//!   `{"$bytes": base64, "len": n}`;
//! - anything larger than [`MAX_CELL_BYTES`] → a [`truncated_cell`] marker.

use base64::Engine as _;
use mysql_async::consts::{ColumnFlags, ColumnType};
use serde_json::Value;

use crate::model::{Cell, ColumnMeta, MAX_CELL_BYTES, TruncatedKind, truncated_cell};

/// The `binary` character set: the column holds bytes, not text.
const BINARY_CHARSET: u16 = 63;

/// What a column's values are, decided once per result from its metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    /// Integer that fits in an `f64` exactly (TINYINT … INT, YEAR, MEDIUMINT).
    SmallInt,
    /// BIGINT (signed or unsigned): string.
    BigInt,
    /// DECIMAL: string.
    Decimal,
    Float,
    Bit,
    Json,
    /// Text in the connection's character set (utf8mb4).
    Text,
    Bytes,
}

/// What the columns of a result are, plus the [`ColumnMeta`] the UI sees.
pub(crate) fn describe(columns: &[mysql_async::Column]) -> (Vec<ColumnMeta>, Vec<(Shape, u32)>) {
    columns
        .iter()
        .map(|c| {
            let ty = c.column_type();
            let flags = c.flags();
            let binary = c.character_set() == BINARY_CHARSET;
            let shape = shape_of(ty, binary);
            let meta = ColumnMeta {
                name: c.name_str().into_owned(),
                type_name: type_name(ty, flags, binary),
                type_oid: ty as u32,
            };
            (meta, (shape, c.column_length()))
        })
        .unzip()
}

pub(crate) fn shape_of(ty: ColumnType, binary: bool) -> Shape {
    use ColumnType::*;
    match ty {
        MYSQL_TYPE_TINY | MYSQL_TYPE_SHORT | MYSQL_TYPE_INT24 | MYSQL_TYPE_LONG
        | MYSQL_TYPE_YEAR => Shape::SmallInt,
        MYSQL_TYPE_LONGLONG => Shape::BigInt,
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => Shape::Decimal,
        MYSQL_TYPE_FLOAT | MYSQL_TYPE_DOUBLE => Shape::Float,
        MYSQL_TYPE_BIT => Shape::Bit,
        MYSQL_TYPE_JSON => Shape::Json,
        MYSQL_TYPE_GEOMETRY | MYSQL_TYPE_VECTOR => Shape::Bytes,
        MYSQL_TYPE_TINY_BLOB
        | MYSQL_TYPE_MEDIUM_BLOB
        | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_BLOB
        | MYSQL_TYPE_VAR_STRING
        | MYSQL_TYPE_STRING
        | MYSQL_TYPE_VARCHAR
            if binary =>
        {
            Shape::Bytes
        }
        _ => Shape::Text,
    }
}

/// `int unsigned`, `varchar`, `blob`, `json`, … as the grid shows it.
fn type_name(ty: ColumnType, flags: ColumnFlags, binary: bool) -> String {
    use ColumnType::*;
    let base = match ty {
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "decimal",
        MYSQL_TYPE_TINY => "tinyint",
        MYSQL_TYPE_SHORT => "smallint",
        MYSQL_TYPE_INT24 => "mediumint",
        MYSQL_TYPE_LONG => "int",
        MYSQL_TYPE_LONGLONG => "bigint",
        MYSQL_TYPE_FLOAT => "float",
        MYSQL_TYPE_DOUBLE => "double",
        MYSQL_TYPE_NULL => "null",
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "timestamp",
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "date",
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "time",
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "datetime",
        MYSQL_TYPE_YEAR => "year",
        MYSQL_TYPE_BIT => "bit",
        MYSQL_TYPE_JSON => "json",
        MYSQL_TYPE_ENUM => "enum",
        MYSQL_TYPE_SET => "set",
        MYSQL_TYPE_GEOMETRY => "geometry",
        MYSQL_TYPE_VECTOR => "vector",
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            if binary {
                "blob"
            } else {
                "text"
            }
        }
        MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR => {
            if binary {
                "varbinary"
            } else {
                "varchar"
            }
        }
        MYSQL_TYPE_STRING => {
            if flags.contains(ColumnFlags::ENUM_FLAG) {
                "enum"
            } else if flags.contains(ColumnFlags::SET_FLAG) {
                "set"
            } else if binary {
                "binary"
            } else {
                "char"
            }
        }
        _ => "unknown",
    };
    let numeric = matches!(
        ty,
        MYSQL_TYPE_TINY
            | MYSQL_TYPE_SHORT
            | MYSQL_TYPE_INT24
            | MYSQL_TYPE_LONG
            | MYSQL_TYPE_LONGLONG
            | MYSQL_TYPE_DECIMAL
            | MYSQL_TYPE_NEWDECIMAL
            | MYSQL_TYPE_FLOAT
            | MYSQL_TYPE_DOUBLE
    );
    if numeric && flags.contains(ColumnFlags::UNSIGNED_FLAG) {
        format!("{base} unsigned")
    } else {
        base.to_string()
    }
}

fn bytes_cell(b: &[u8]) -> Cell {
    if b.len() > MAX_CELL_BYTES {
        return truncated_cell(TruncatedKind::Bytes, b.len(), b);
    }
    serde_json::json!({
        "$bytes": base64::engine::general_purpose::STANDARD.encode(b),
        "len": b.len(),
    })
}

/// Text, or bytes when it is not valid UTF-8 (a hostile server, or `SET NAMES latin1`): a
/// replacement character would show something the database does not hold.
fn text_cell(b: &[u8]) -> Cell {
    match std::str::from_utf8(b) {
        Ok(s) if b.len() > MAX_CELL_BYTES => truncated_cell(TruncatedKind::Text, s.len(), b),
        Ok(s) => Value::String(s.to_owned()),
        Err(_) => bytes_cell(b),
    }
}

/// `BIT(n)` arrives as big-endian bytes; shown as `n` binary digits (at least as many as the
/// bytes hold when the declared width is missing or wrong).
fn bit_cell(b: &[u8], width: u32) -> Cell {
    if b.len() > 8 {
        // BIT is at most 64 bits; anything longer is not a BIT value.
        return bytes_cell(b);
    }
    let mut v: u64 = 0;
    for &x in b {
        v = (v << 8) | u64::from(x);
    }
    let digits = format!("{v:b}");
    let width = usize::try_from(width).unwrap_or(64).clamp(1, 64);
    Value::String(if digits.len() >= width {
        digits
    } else {
        format!("{}{digits}", "0".repeat(width - digits.len()))
    })
}

/// One value of a text-protocol row.
pub(crate) fn cell(value: Option<&mysql_async::Value>, shape: Shape, width: u32) -> Cell {
    let b: &[u8] = match value {
        None | Some(mysql_async::Value::NULL) => return Value::Null,
        Some(mysql_async::Value::Bytes(b)) => b,
        // The text protocol only produces bytes; anything else is shown as its SQL text.
        Some(other) => return Value::String(other.as_sql(false)),
    };
    match shape {
        Shape::Bytes => bytes_cell(b),
        Shape::Bit => bit_cell(b, width),
        Shape::Text => text_cell(b),
        Shape::SmallInt => std::str::from_utf8(b)
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .filter(|n| n.unsigned_abs() <= (1u64 << 53))
            .map_or_else(|| text_cell(b), Value::from),
        Shape::BigInt | Shape::Decimal => text_cell(b),
        Shape::Float => std::str::from_utf8(b)
            .ok()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| text_cell(b), Value::Number),
        Shape::Json => {
            if b.len() > MAX_CELL_BYTES {
                return truncated_cell(TruncatedKind::Json, b.len(), b);
            }
            serde_json::from_slice::<Value>(b).unwrap_or_else(|_| text_cell(b))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mysql_async::Value as V;
    use serde_json::json;

    fn c(v: &[u8], shape: Shape) -> Cell {
        cell(Some(&V::Bytes(v.to_vec())), shape, 0)
    }

    #[test]
    fn numbers_keep_their_precision() {
        assert_eq!(c(b"42", Shape::SmallInt), json!(42));
        assert_eq!(c(b"-8388608", Shape::SmallInt), json!(-8388608));
        assert_eq!(c(b"4294967295", Shape::SmallInt), json!(4294967295u64));
        assert_eq!(
            c(b"18446744073709551615", Shape::BigInt),
            json!("18446744073709551615")
        );
        assert_eq!(
            c(b"-9223372036854775808", Shape::BigInt),
            json!("-9223372036854775808")
        );
        let huge = format!("{}.{}", "9".repeat(65), "9".repeat(30));
        assert_eq!(c(huge.as_bytes(), Shape::Decimal), json!(huge));
        assert_eq!(c(b"1.5", Shape::Float), json!(1.5));
        // Not a finite number: shown as the server's text, never a panic.
        assert_eq!(c(b"inf", Shape::Float), json!("inf"));
        assert_eq!(c(b"nan", Shape::Float), json!("nan"));
        assert_eq!(c(b"12abc", Shape::SmallInt), json!("12abc"));
        assert_eq!(c(b"", Shape::SmallInt), json!(""));
    }

    #[test]
    fn odd_and_hostile_values_do_not_panic() {
        // Zero dates and invalid dates are shown as sent.
        assert_eq!(c(b"0000-00-00", Shape::Text), json!("0000-00-00"));
        assert_eq!(
            c(b"0000-00-00 00:00:00", Shape::Text),
            json!("0000-00-00 00:00:00")
        );
        // Text that is not UTF-8 becomes bytes rather than replacement characters.
        assert_eq!(
            c(&[b'a', 0xFF, 0xFE], Shape::Text),
            json!({"$bytes": "Yf/+", "len": 3})
        );
        assert_eq!(
            c(&[0xC3], Shape::SmallInt),
            json!({"$bytes": "ww==", "len": 1})
        );
        // BIT.
        assert_eq!(
            cell(Some(&V::Bytes(vec![0x05])), Shape::Bit, 4),
            json!("0101")
        );
        assert_eq!(
            cell(Some(&V::Bytes(vec![0x01, 0x00])), Shape::Bit, 10),
            json!("0100000000")
        );
        assert_eq!(cell(Some(&V::Bytes(vec![])), Shape::Bit, 1), json!("0"));
        assert_eq!(
            cell(Some(&V::Bytes(vec![0xFF; 9])), Shape::Bit, 64)["len"],
            json!(9)
        );
        assert_eq!(
            cell(Some(&V::Bytes(vec![0xFF; 8])), Shape::Bit, 0)
                .as_str()
                .map(str::len),
            Some(64)
        );
        // JSON: parsed, or text when it does not parse (too deep, invalid).
        assert_eq!(c(br#"{"a": [1, 2]}"#, Shape::Json), json!({"a": [1, 2]}));
        let deep = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
        assert!(c(deep.as_bytes(), Shape::Json).is_string());
        assert_eq!(c(b"{oops", Shape::Json), json!("{oops"));
        // GEOMETRY / binary.
        assert_eq!(c(&[0, 0, 0, 0, 1], Shape::Bytes)["len"], json!(5));
        // NULL.
        assert_eq!(cell(None, Shape::Text, 0), Value::Null);
        assert_eq!(cell(Some(&V::NULL), Shape::Json, 0), Value::Null);
    }

    #[test]
    fn big_values_are_cut() {
        let big = vec![b'x'; MAX_CELL_BYTES + 1];
        assert!(crate::model::is_truncated_cell(&c(&big, Shape::Text)));
        assert!(crate::model::is_truncated_cell(&c(&big, Shape::Bytes)));
        assert!(crate::model::is_truncated_cell(&c(&big, Shape::Json)));
        assert_eq!(c(&big, Shape::Bytes)["$truncated"]["kind"], json!("bytes"));
    }

    #[test]
    fn shapes_follow_type_and_charset() {
        use ColumnType::*;
        assert_eq!(shape_of(MYSQL_TYPE_BLOB, true), Shape::Bytes);
        assert_eq!(shape_of(MYSQL_TYPE_BLOB, false), Shape::Text);
        assert_eq!(shape_of(MYSQL_TYPE_VAR_STRING, true), Shape::Bytes);
        assert_eq!(shape_of(MYSQL_TYPE_GEOMETRY, false), Shape::Bytes);
        assert_eq!(shape_of(MYSQL_TYPE_LONGLONG, false), Shape::BigInt);
        assert_eq!(shape_of(MYSQL_TYPE_NEWDECIMAL, false), Shape::Decimal);
        assert_eq!(shape_of(MYSQL_TYPE_DATETIME, false), Shape::Text);
        assert_eq!(
            type_name(MYSQL_TYPE_LONG, ColumnFlags::UNSIGNED_FLAG, false),
            "int unsigned"
        );
        assert_eq!(
            type_name(MYSQL_TYPE_VAR_STRING, ColumnFlags::empty(), true),
            "varbinary"
        );
    }
}
