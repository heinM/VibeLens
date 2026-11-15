//! Athena result typing (pure, no AWS calls).
//!
//! `GetQueryResults` returns every cell as an optional string (`Datum.VarCharValue`) plus a
//! `ColumnInfo.Type` per column. This module turns those into the same [`Cell`] encoding the
//! Postgres engine uses (see `model::query::Cell`): small integers and floats become JSON
//! numbers; `bigint` and `decimal` stay strings (JS numbers lose precision past 2^53);
//! `boolean` becomes a bool; timestamps are normalised to RFC3339 when that is unambiguous;
//! `json` is embedded when it parses; `varbinary` becomes `{"$bytes": base64, "len": n}`.
//! Nested types (`array`, `row`, `map`) arrive as Athena's own text rendering and stay strings.

use base64::Engine as _;
use chrono::{FixedOffset, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use serde_json::{Map, Number, Value};

use crate::model::Cell;

/// The lower-cased leading type word of an Athena type string: `decimal(12,2)` → `decimal`,
/// `timestamp(3) with time zone` → `timestamp`, `array(varchar)` → `array`.
pub(crate) fn base_type(ty: &str) -> String {
    ty.trim()
        .to_ascii_lowercase()
        .split(|c: char| c == '(' || c == '<' || c.is_whitespace())
        .next()
        .unwrap_or("")
        .to_string()
}

/// The type name shown to the user. Athena reports `decimal` without its parameters in
/// `ColumnInfo.Type` (they live in `Precision` / `Scale`), so put them back.
pub(crate) fn display_type(ty: &str, precision: i32, scale: i32) -> String {
    let ty = ty.trim();
    if ty.eq_ignore_ascii_case("decimal") && precision > 0 {
        return format!("decimal({precision},{})", scale.max(0));
    }
    ty.to_string()
}

/// Converts one Athena cell. `None` (a missing `VarCharValue`) is SQL NULL.
pub(crate) fn convert_cell(athena_type: &str, value: Option<&str>) -> Cell {
    let Some(v) = value else {
        return Cell::Null;
    };
    let text = || Cell::String(v.to_string());
    match base_type(athena_type).as_str() {
        "tinyint" | "smallint" | "integer" | "int" => v
            .trim()
            .parse::<i64>()
            .map(Cell::from)
            .unwrap_or_else(|_| text()),
        "real" | "float" | "double" => v
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Cell::Number)
            // NaN / Infinity have no JSON form: keep Athena's text.
            .unwrap_or_else(text),
        "boolean" => match v.trim().to_ascii_lowercase().as_str() {
            "true" => Cell::Bool(true),
            "false" => Cell::Bool(false),
            _ => text(),
        },
        "date" => match NaiveDate::parse_from_str(v.trim(), "%Y-%m-%d") {
            Ok(d) => Cell::String(d.format("%Y-%m-%d").to_string()),
            Err(_) => text(),
        },
        "timestamp" => timestamp(
            v,
            athena_type.to_ascii_lowercase().contains("with time zone"),
        ),
        "json" => serde_json::from_str::<Value>(v).unwrap_or_else(|_| text()),
        "varbinary" => varbinary(v).unwrap_or_else(text),
        // bigint, decimal, varchar, char, uuid, ipaddress, time, interval, array, row, map,
        // struct, and anything newer than this table: Athena's text, untouched.
        _ => text(),
    }
}

/// Whether `row` is the header row Athena puts first in the first page of a query's results:
/// every cell spells the column's name (or label), position by position.
pub(crate) fn is_header_row(columns: &[(String, Option<String>)], row: &[Option<&str>]) -> bool {
    if columns.is_empty() || row.len() != columns.len() {
        return false;
    }
    columns
        .iter()
        .zip(row)
        .all(|((name, label), cell)| match cell {
            Some(c) => c == name || label.as_deref() == Some(*c),
            None => false,
        })
}

fn timestamp(v: &str, with_tz: bool) -> Cell {
    let raw = || Cell::String(v.to_string());
    let s = v.trim();
    if !with_tz {
        // Naive local timestamp: ISO 8601 with a `T`, no offset (as the Postgres engine does).
        return match NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f") {
            Ok(_) => Cell::String(s.replacen(' ', "T", 1)),
            Err(_) => raw(),
        };
    }
    // `2024-01-31 12:34:56.789 UTC`, `... +02:00`, or `... Europe/Paris`.
    let Some((local, zone)) = s.rsplit_once(' ') else {
        return raw();
    };
    let Ok(naive) = NaiveDateTime::parse_from_str(local, "%Y-%m-%d %H:%M:%S%.f") else {
        return raw();
    };
    let offset = match zone {
        "UTC" | "UT" | "GMT" | "Z" => FixedOffset::east_opt(0),
        z => parse_offset(z),
    };
    // A region id (`Europe/Paris`) cannot be resolved without a tz database, and guessing an
    // offset would be wrong across DST: keep what Athena said.
    let Some(offset) = offset else {
        return raw();
    };
    match offset.from_local_datetime(&naive).single() {
        Some(dt) => Cell::String(
            dt.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::AutoSi, true),
        ),
        None => raw(),
    }
}

/// `+HH:MM` / `-HH:MM` (also `+HHMM`).
fn parse_offset(z: &str) -> Option<FixedOffset> {
    let (sign, rest) = match z.as_bytes().first()? {
        b'+' => (1, &z[1..]),
        b'-' => (-1, &z[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i32 = digits[..2].parse().ok()?;
    let m: i32 = digits[2..].parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    FixedOffset::east_opt(sign * (h * 3600 + m * 60))
}

/// Athena renders `varbinary` as space-separated lower-case hex bytes (`68 65 6c 6c 6f`).
fn varbinary(v: &str) -> Option<Cell> {
    let mut bytes = Vec::new();
    for tok in v.split_whitespace() {
        if tok.len() != 2 {
            return None;
        }
        bytes.push(u8::from_str_radix(tok, 16).ok()?);
    }
    let mut m = Map::new();
    m.insert(
        "$bytes".into(),
        Value::String(base64::engine::general_purpose::STANDARD.encode(&bytes)),
    );
    m.insert("len".into(), Value::from(bytes.len()));
    Some(Value::Object(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn null_is_null_for_every_type() {
        for t in [
            "bigint",
            "varchar",
            "json",
            "boolean",
            "array(varchar)",
            "weird",
        ] {
            assert_eq!(convert_cell(t, None), Cell::Null, "{t}");
        }
        // An empty string is a value, not NULL.
        assert_eq!(convert_cell("varchar", Some("")), json!(""));
    }

    #[test]
    fn small_integers_are_numbers_and_bigint_is_a_string() {
        assert_eq!(convert_cell("tinyint", Some("-5")), json!(-5));
        assert_eq!(convert_cell("smallint", Some("300")), json!(300));
        assert_eq!(
            convert_cell("integer", Some("2147483647")),
            json!(2147483647)
        );
        assert_eq!(
            convert_cell("bigint", Some("9007199254740993")),
            json!("9007199254740993")
        );
        assert_eq!(
            convert_cell("decimal(12,2)", Some("1234.50")),
            json!("1234.50")
        );
        assert_eq!(convert_cell("decimal", Some("0.10")), json!("0.10"));
        // Garbage in an integer column is kept as text rather than dropped.
        assert_eq!(convert_cell("integer", Some("n/a")), json!("n/a"));
    }

    #[test]
    fn floats_are_numbers_except_nan_and_infinity() {
        assert_eq!(convert_cell("double", Some("1.5")), json!(1.5));
        assert_eq!(convert_cell("double", Some("1.0E10")), json!(1.0e10));
        assert_eq!(convert_cell("real", Some("-0.25")), json!(-0.25));
        assert_eq!(convert_cell("double", Some("NaN")), json!("NaN"));
        assert_eq!(convert_cell("double", Some("Infinity")), json!("Infinity"));
        assert_eq!(
            convert_cell("double", Some("-Infinity")),
            json!("-Infinity")
        );
    }

    #[test]
    fn booleans() {
        assert_eq!(convert_cell("boolean", Some("true")), json!(true));
        assert_eq!(convert_cell("boolean", Some("false")), json!(false));
        assert_eq!(convert_cell("boolean", Some("maybe")), json!("maybe"));
    }

    #[test]
    fn dates_and_timestamps() {
        assert_eq!(
            convert_cell("date", Some("2024-02-29")),
            json!("2024-02-29")
        );
        assert_eq!(
            convert_cell("date", Some("2024-13-40")),
            json!("2024-13-40")
        );
        assert_eq!(
            convert_cell("timestamp", Some("2024-01-31 12:34:56.789")),
            json!("2024-01-31T12:34:56.789")
        );
        assert_eq!(
            convert_cell("timestamp(3)", Some("2024-01-31 12:34:56")),
            json!("2024-01-31T12:34:56")
        );
        assert_eq!(
            convert_cell(
                "timestamp(3) with time zone",
                Some("2024-01-31 12:34:56.789 UTC")
            ),
            json!("2024-01-31T12:34:56.789Z")
        );
        assert_eq!(
            convert_cell(
                "timestamp with time zone",
                Some("2024-01-31 12:34:56.000 +02:00")
            ),
            json!("2024-01-31T10:34:56Z")
        );
        assert_eq!(
            convert_cell(
                "timestamp with time zone",
                Some("2024-01-31 01:00:00.000 -05:30")
            ),
            json!("2024-01-31T06:30:00Z")
        );
        // A region id cannot be resolved offline; keep Athena's text.
        assert_eq!(
            convert_cell(
                "timestamp with time zone",
                Some("2024-07-01 12:00:00.000 Europe/Paris")
            ),
            json!("2024-07-01 12:00:00.000 Europe/Paris")
        );
        assert_eq!(
            convert_cell("timestamp", Some("not a time")),
            json!("not a time")
        );
    }

    #[test]
    fn json_is_embedded_when_valid() {
        assert_eq!(
            convert_cell("json", Some(r#"{"a":[1,2],"b":null}"#)),
            json!({"a": [1, 2], "b": null})
        );
        assert_eq!(convert_cell("json", Some("{broken")), json!("{broken"));
    }

    #[test]
    fn nested_types_stay_text() {
        assert_eq!(
            convert_cell("array(varchar)", Some("[a, b]")),
            json!("[a, b]")
        );
        assert_eq!(convert_cell("array", Some("[1, 2]")), json!("[1, 2]"));
        assert_eq!(
            convert_cell("row(x integer, y varchar)", Some("{x=1, y=z}")),
            json!("{x=1, y=z}")
        );
        assert_eq!(
            convert_cell("map(varchar, integer)", Some("{a=1}")),
            json!("{a=1}")
        );
        assert_eq!(convert_cell("varchar", Some("héllo")), json!("héllo"));
        assert_eq!(convert_cell("uuid", Some("x")), json!("x"));
        assert_eq!(convert_cell("some_future_type", Some("x")), json!("x"));
    }

    #[test]
    fn varbinary_becomes_bytes() {
        assert_eq!(
            convert_cell("varbinary", Some("68 65 6c 6c 6f")),
            json!({"$bytes": "aGVsbG8=", "len": 5})
        );
        assert_eq!(
            convert_cell("varbinary", Some("")),
            json!({"$bytes": "", "len": 0})
        );
        assert_eq!(convert_cell("varbinary", Some("zz")), json!("zz"));
    }

    #[test]
    fn base_and_display_types() {
        assert_eq!(base_type("Timestamp(3) WITH TIME ZONE"), "timestamp");
        assert_eq!(base_type("array(varchar)"), "array");
        assert_eq!(base_type(" bigint "), "bigint");
        assert_eq!(display_type("decimal", 12, 2), "decimal(12,2)");
        assert_eq!(display_type("decimal(5,1)", 5, 1), "decimal(5,1)");
        assert_eq!(display_type("varchar", 2147483647, 0), "varchar");
    }

    #[test]
    fn header_row_detection() {
        let cols = vec![
            ("id".to_string(), Some("id".to_string())),
            ("name".to_string(), None),
        ];
        assert!(is_header_row(&cols, &[Some("id"), Some("name")]));
        assert!(!is_header_row(&cols, &[Some("1"), Some("bob")]));
        assert!(!is_header_row(&cols, &[Some("id")]));
        assert!(!is_header_row(&cols, &[Some("id"), None]));
        assert!(!is_header_row(&[], &[]));
    }
}
