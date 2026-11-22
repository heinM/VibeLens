//! Redshift result typing (pure, no AWS calls).
//!
//! `GetStatementResult` returns each cell as a typed `Field` (`longValue`, `doubleValue`,
//! `stringValue`, `booleanValue`, `blobValue` or `isNull`) and each column's Redshift type name
//! in `ColumnMetadata.typeName`. This module turns them into the [`Cell`] encoding the Postgres
//! engine uses (see `model::query::Cell`):
//!
//! * `int2` / `int4` → JSON numbers; `int8` and `numeric` → **strings** (JS numbers lose
//!   precision past 2^53). A long value of a type this table does not know also stays a string.
//! * `float4` / `float8` → numbers (NaN and infinities, which JSON cannot hold, as text).
//! * `bool` → bool; `timestamp` → ISO 8601 without offset; `timestamptz` → RFC 3339 in UTC.
//! * `super` → embedded JSON when it parses (Redshift's semi-structured type), else text.
//! * Binary → `{"$bytes": base64, "len": n}`.
//! * A value larger than [`MAX_CELL_BYTES`] becomes a [`truncated_cell`] marker.

use base64::Engine as _;
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{Map, Number, Value};

use crate::model::{Cell, MAX_CELL_BYTES, TruncatedKind, truncated_cell};

/// A cell as the Data API delivers it, independent of the SDK type so it can be tested alone.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Raw<'a> {
    Null,
    Bool(bool),
    Long(i64),
    Double(f64),
    Str(&'a str),
    Blob(&'a [u8]),
    /// A field kind newer than the SDK.
    Unknown,
}

impl<'a> From<&'a aws_sdk_redshiftdata::types::Field> for Raw<'a> {
    fn from(f: &'a aws_sdk_redshiftdata::types::Field) -> Self {
        use aws_sdk_redshiftdata::types::Field;
        match f {
            Field::IsNull(true) => Raw::Null,
            // `isNull: false` carries no value; nothing else can be said about it.
            Field::IsNull(false) => Raw::Unknown,
            Field::BooleanValue(b) => Raw::Bool(*b),
            Field::LongValue(n) => Raw::Long(*n),
            Field::DoubleValue(d) => Raw::Double(*d),
            Field::StringValue(s) => Raw::Str(s),
            Field::BlobValue(b) => Raw::Blob(b.as_ref()),
            _ => Raw::Unknown,
        }
    }
}

/// Lower-cased base type: `numeric(12,2)` → `numeric`, `character varying` → `character`.
fn base_type(ty: &str) -> String {
    ty.trim()
        .to_ascii_lowercase()
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_string()
}

fn is_small_int(base: &str) -> bool {
    matches!(
        base,
        "int2" | "int4" | "smallint" | "integer" | "int" | "oid" | "xid"
    )
}

fn is_float(base: &str) -> bool {
    matches!(base, "float4" | "float8" | "real" | "double" | "float")
}

/// Type name shown to the user: `numeric(12,2)` when Redshift reports precision and scale
/// separately, else the reported name.
pub(crate) fn display_type(ty: &str, precision: i32, scale: i32) -> String {
    let t = ty.trim();
    if matches!(base_type(t).as_str(), "numeric" | "decimal") && !t.contains('(') && precision > 0 {
        return format!("{t}({precision},{})", scale.max(0));
    }
    t.to_string()
}

/// Converts one cell of a column whose Redshift type is `ty`.
pub(crate) fn convert_cell(ty: &str, raw: Raw<'_>) -> Cell {
    let base = base_type(ty);
    match raw {
        Raw::Null | Raw::Unknown => Cell::Null,
        Raw::Bool(b) => Cell::Bool(b),
        Raw::Long(n) if is_small_int(&base) => Cell::from(n),
        // int8, numeric and anything not known to be small: a string keeps every digit.
        Raw::Long(n) => Cell::String(n.to_string()),
        Raw::Double(d) => Number::from_f64(d)
            .map(Cell::Number)
            .unwrap_or_else(|| Cell::String(float_text(d))),
        Raw::Blob(b) => bytes_cell(b),
        Raw::Str(s) if s.len() > MAX_CELL_BYTES => {
            let kind = if base == "super" {
                TruncatedKind::Json
            } else {
                TruncatedKind::Text
            };
            truncated_cell(kind, s.len(), s.as_bytes())
        }
        Raw::Str(s) => string_cell(&base, s),
    }
}

fn float_text(d: f64) -> String {
    if d.is_nan() {
        "NaN".into()
    } else if d > 0.0 {
        "Infinity".into()
    } else {
        "-Infinity".into()
    }
}

fn bytes_cell(b: &[u8]) -> Cell {
    if b.len() > MAX_CELL_BYTES {
        return truncated_cell(TruncatedKind::Bytes, b.len(), b);
    }
    let mut m = Map::new();
    m.insert(
        "$bytes".into(),
        Value::String(base64::engine::general_purpose::STANDARD.encode(b)),
    );
    m.insert("len".into(), Value::from(b.len()));
    Value::Object(m)
}

fn string_cell(base: &str, s: &str) -> Cell {
    let text = || Cell::String(s.to_string());
    match base {
        _ if is_small_int(base) => s
            .trim()
            .parse::<i64>()
            .map(Cell::from)
            .unwrap_or_else(|_| text()),
        _ if is_float(base) => s
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Cell::Number)
            .unwrap_or_else(text),
        "bool" | "boolean" => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "t" => Cell::Bool(true),
            "false" | "f" => Cell::Bool(false),
            _ => text(),
        },
        "timestamp" => match NaiveDateTime::parse_from_str(s.trim(), "%Y-%m-%d %H:%M:%S%.f") {
            Ok(_) => Cell::String(s.trim().replacen(' ', "T", 1)),
            Err(_) => text(),
        },
        "timestamptz" => timestamptz(s).unwrap_or_else(text),
        "super" => serde_json::from_str::<Value>(s).unwrap_or_else(|_| text()),
        // int8, numeric, varchar, bpchar, date, time, timetz, varbyte (as hex text), geometry,
        // hllsketch, intervals, and anything newer: Redshift's own text.
        _ => text(),
    }
}

/// `2024-01-31 12:34:56.789+00` (the server's text form) → `2024-01-31T12:34:56.789Z`.
fn timestamptz(s: &str) -> Option<Cell> {
    let s = s.trim();
    // `+00` has no minutes, which chrono's `%#z` accepts.
    let dt = DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f%#z").ok()?;
    Some(Cell::String(
        dt.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::AutoSi, true),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nulls_and_unknown_fields_are_null() {
        for t in ["int8", "varchar", "super", "whatever"] {
            assert_eq!(convert_cell(t, Raw::Null), Cell::Null);
            assert_eq!(convert_cell(t, Raw::Unknown), Cell::Null);
        }
        assert_eq!(convert_cell("varchar", Raw::Str("")), json!(""));
    }

    #[test]
    fn small_integers_are_numbers_and_int8_and_numeric_are_strings() {
        assert_eq!(convert_cell("int2", Raw::Long(-5)), json!(-5));
        assert_eq!(
            convert_cell("int4", Raw::Long(2147483647)),
            json!(2147483647)
        );
        assert_eq!(
            convert_cell("int8", Raw::Long(9007199254740993)),
            json!("9007199254740993")
        );
        assert_eq!(
            convert_cell("numeric", Raw::Str("1234.50")),
            json!("1234.50")
        );
        assert_eq!(
            convert_cell("numeric(38,0)", Raw::Long(12)),
            json!("12"),
            "a numeric delivered as a long is still a string"
        );
        assert_eq!(convert_cell("mystery", Raw::Long(1)), json!("1"));
        assert_eq!(convert_cell("int4", Raw::Str("42")), json!(42));
        assert_eq!(convert_cell("int4", Raw::Str("n/a")), json!("n/a"));
    }

    #[test]
    fn floats_and_booleans() {
        assert_eq!(convert_cell("float8", Raw::Double(1.5)), json!(1.5));
        assert_eq!(convert_cell("float4", Raw::Double(f64::NAN)), json!("NaN"));
        assert_eq!(
            convert_cell("float8", Raw::Double(f64::NEG_INFINITY)),
            json!("-Infinity")
        );
        assert_eq!(convert_cell("float8", Raw::Str("2.5")), json!(2.5));
        assert_eq!(convert_cell("bool", Raw::Bool(true)), json!(true));
        assert_eq!(convert_cell("bool", Raw::Str("f")), json!(false));
    }

    #[test]
    fn times() {
        assert_eq!(
            convert_cell("timestamp", Raw::Str("2024-01-31 12:34:56.789")),
            json!("2024-01-31T12:34:56.789")
        );
        assert_eq!(
            convert_cell("timestamptz", Raw::Str("2024-01-31 12:34:56+02")),
            json!("2024-01-31T10:34:56Z")
        );
        assert_eq!(
            convert_cell("timestamptz", Raw::Str("2024-01-31 12:34:56.5+00")),
            json!("2024-01-31T12:34:56.500Z")
        );
        assert_eq!(
            convert_cell("date", Raw::Str("2024-02-29")),
            json!("2024-02-29")
        );
        assert_eq!(convert_cell("timestamp", Raw::Str("soon")), json!("soon"));
    }

    #[test]
    fn super_is_embedded_and_bytes_are_encoded() {
        assert_eq!(
            convert_cell("super", Raw::Str(r#"{"a":[1,2]}"#)),
            json!({"a": [1, 2]})
        );
        assert_eq!(convert_cell("super", Raw::Str("{broken")), json!("{broken"));
        assert_eq!(
            convert_cell("varbyte", Raw::Blob(b"hi")),
            json!({"$bytes": "aGk=", "len": 2})
        );
    }

    #[test]
    fn oversized_values_become_markers() {
        let big = "x".repeat(MAX_CELL_BYTES + 5);
        let c = convert_cell("varchar", Raw::Str(&big));
        assert!(crate::model::is_truncated_cell(&c));
        assert_eq!(c["$truncated"]["kind"], "text");
        assert_eq!(c["$truncated"]["bytes"], MAX_CELL_BYTES + 5);
        let c = convert_cell("super", Raw::Str(&big));
        assert_eq!(c["$truncated"]["kind"], "json");
        let blob = vec![0u8; MAX_CELL_BYTES + 1];
        let c = convert_cell("varbyte", Raw::Blob(&blob));
        assert_eq!(c["$truncated"]["kind"], "bytes");
    }

    #[test]
    fn display_types() {
        assert_eq!(display_type("numeric", 12, 2), "numeric(12,2)");
        assert_eq!(display_type("numeric(5,1)", 5, 1), "numeric(5,1)");
        assert_eq!(display_type("varchar", 256, 0), "varchar");
        assert_eq!(display_type("int8", 19, 0), "int8");
    }
}
