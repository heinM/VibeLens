//! Arrow → [`Cell`] encoding for the local (DataFusion) engine.
//!
//! The rules are the same as `pg::codec` so the grid renders Iceberg and Postgres alike:
//!
//! | Arrow | Cell |
//! |---|---|
//! | `Int8/16/32`, `UInt8/16/32`, `Float16/32/64` | JSON number (`NaN`/`±Infinity` → string) |
//! | `Int64`, `UInt64`, `Decimal32/64/128/256` | **string** (JS numbers lose precision past 2^53) |
//! | `Utf8`, `LargeUtf8`, `Utf8View` | string |
//! | `Binary`, `LargeBinary`, `BinaryView`, `FixedSizeBinary` | `{"$bytes": base64, "len": n}` |
//! | `FixedSizeBinary(16)` flagged as a UUID extension | canonical `8-4-4-4-12` string |
//! | `Date32/64` | `YYYY-MM-DD` |
//! | `Timestamp(unit, tz)` | RFC 3339 (in the column's zone; naive timestamps have no offset) |
//! | `Time32/64` | `HH:MM:SS[.ffffff]` |
//! | `Duration`, `Interval*` | Arrow's display text |
//! | `Struct`, `List`, `LargeList`, `FixedSizeList` | JSON object / array, recursively |
//! | `Map` | array of `{"key", "value"}` objects (keys need not be strings) |
//! | `Dictionary`, `RunEndEncoded` | the decoded value |
//! | `Null` / null slot | `null` |

use std::str::FromStr;

use base64::Engine as _;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, SecondsFormat, Utc};
use datafusion::arrow::array::timezone::Tz;
use datafusion::arrow::array::{
    Array, AsArray, BooleanArray, FixedSizeBinaryArray, GenericBinaryArray, GenericListArray,
    GenericStringArray, MapArray, OffsetSizeTrait, PrimitiveArray, RecordBatch, StructArray,
};
use datafusion::arrow::datatypes::{
    ArrowPrimitiveType, DataType, Date32Type, Date64Type, Decimal128Type, Decimal256Type, Field,
    Float16Type, Float32Type, Float64Type, Int8Type, Int16Type, Int32Type, Int64Type, TimeUnit,
    TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType,
    TimestampSecondType, UInt8Type, UInt16Type, UInt32Type, UInt64Type,
};
use datafusion::arrow::util::display::{ArrayFormatter, FormatOptions};
use serde_json::{Map, Number, Value};

use crate::model::{Cell, ColumnMeta};

/// Nested values deeper than this fall back to Arrow's display text (guards the recursion).
const MAX_DEPTH: usize = 48;

/// Field-metadata keys that mark a `FixedSizeBinary(16)` as a UUID.
const UUID_EXTENSION_KEYS: [&str; 2] = ["ARROW:extension:name", "iceberg.type"];
const UUID_EXTENSION_VALUES: [&str; 2] = ["arrow.uuid", "uuid"];

/// Converts every row of `batch` to cells (row-major).
pub(crate) fn batch_to_rows(batch: &RecordBatch) -> Vec<Vec<Cell>> {
    let schema = batch.schema();
    let fields = schema.fields();
    (0..batch.num_rows())
        .map(|row| {
            batch
                .columns()
                .iter()
                .enumerate()
                .map(|(c, col)| cell_at(col.as_ref(), row, fields.get(c).map(|f| f.as_ref())))
                .collect()
        })
        .collect()
}

/// Column metadata for the grid: SQL-ish type names, `type_oid` 0 (there are no Postgres OIDs).
pub(crate) fn columns_of(schema: &datafusion::arrow::datatypes::Schema) -> Vec<ColumnMeta> {
    schema
        .fields()
        .iter()
        .map(|f| ColumnMeta {
            name: f.name().clone(),
            type_name: type_name(f.data_type()),
            type_oid: 0,
        })
        .collect()
}

/// Human-readable type name (`bigint`, `decimal(12, 2)`, `timestamp with time zone`, `list<string>`, …).
pub(crate) fn type_name(dt: &DataType) -> String {
    match dt {
        DataType::Null => "null".into(),
        DataType::Boolean => "boolean".into(),
        DataType::Int8 => "tinyint".into(),
        DataType::Int16 => "smallint".into(),
        DataType::Int32 => "int".into(),
        DataType::Int64 => "bigint".into(),
        DataType::UInt8 => "unsigned tinyint".into(),
        DataType::UInt16 => "unsigned smallint".into(),
        DataType::UInt32 => "unsigned int".into(),
        DataType::UInt64 => "unsigned bigint".into(),
        DataType::Float16 | DataType::Float32 => "float".into(),
        DataType::Float64 => "double".into(),
        DataType::Decimal32(p, s)
        | DataType::Decimal64(p, s)
        | DataType::Decimal128(p, s)
        | DataType::Decimal256(p, s) => {
            format!("decimal({p}, {s})")
        }
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => "string".into(),
        DataType::Binary | DataType::LargeBinary | DataType::BinaryView => "binary".into(),
        DataType::FixedSizeBinary(n) => format!("fixed({n})"),
        DataType::Date32 | DataType::Date64 => "date".into(),
        DataType::Time32(_) | DataType::Time64(_) => "time".into(),
        DataType::Timestamp(unit, tz) => {
            let base = match unit {
                TimeUnit::Second => "timestamp(0)",
                TimeUnit::Millisecond => "timestamp(3)",
                TimeUnit::Microsecond => "timestamp",
                TimeUnit::Nanosecond => "timestamp(9)",
            };
            if tz.is_some() {
                format!("{base} with time zone")
            } else {
                base.into()
            }
        }
        DataType::Duration(_) => "duration".into(),
        DataType::Interval(_) => "interval".into(),
        DataType::List(f)
        | DataType::LargeList(f)
        | DataType::FixedSizeList(f, _)
        | DataType::ListView(f)
        | DataType::LargeListView(f) => {
            format!("list<{}>", type_name(f.data_type()))
        }
        DataType::Struct(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|f| format!("{}: {}", f.name(), type_name(f.data_type())))
                .collect();
            format!("struct<{}>", inner.join(", "))
        }
        DataType::Map(entries, _) => match entries.data_type() {
            DataType::Struct(kv) if kv.len() == 2 => {
                format!(
                    "map<{}, {}>",
                    type_name(kv[0].data_type()),
                    type_name(kv[1].data_type())
                )
            }
            other => format!("map<{}>", type_name(other)),
        },
        DataType::Dictionary(_, value) => type_name(value),
        DataType::RunEndEncoded(_, value) => type_name(value.data_type()),
        other => other.to_string().to_lowercase(),
    }
}

/// The cell for `array[i]`. `field` (when known) lets us recognise UUID columns.
pub(crate) fn cell_at(array: &dyn Array, i: usize, field: Option<&Field>) -> Cell {
    cell(array, i, field, 0)
}

fn cell(array: &dyn Array, i: usize, field: Option<&Field>, depth: usize) -> Cell {
    if i >= array.len() || array.is_null(i) {
        return Value::Null;
    }
    if depth > MAX_DEPTH {
        return display_cell(array, i);
    }
    match array.data_type() {
        DataType::Null => Value::Null,
        DataType::Boolean => Value::Bool(
            array
                .as_any()
                .downcast_ref::<BooleanArray>()
                .map(|a| a.value(i))
                .unwrap_or_default(),
        ),
        DataType::Int8 => int::<Int8Type>(array, i),
        DataType::Int16 => int::<Int16Type>(array, i),
        DataType::Int32 => int::<Int32Type>(array, i),
        DataType::UInt8 => int::<UInt8Type>(array, i),
        DataType::UInt16 => int::<UInt16Type>(array, i),
        DataType::UInt32 => int::<UInt32Type>(array, i),
        DataType::Int64 => string_of::<Int64Type>(array, i),
        DataType::UInt64 => string_of::<UInt64Type>(array, i),
        DataType::Float16 => prim::<Float16Type>(array, i)
            .map(|v| float(f64::from(v.to_f32())))
            .unwrap_or(Value::Null),
        DataType::Float32 => prim::<Float32Type>(array, i)
            .map(float32)
            .unwrap_or(Value::Null),
        DataType::Float64 => prim::<Float64Type>(array, i)
            .map(float)
            .unwrap_or(Value::Null),
        DataType::Decimal32(..) => decimal_string(array, i),
        DataType::Decimal64(..) => decimal_string(array, i),
        DataType::Decimal128(..) => array
            .as_any()
            .downcast_ref::<PrimitiveArray<Decimal128Type>>()
            .map(|a| Value::String(a.value_as_string(i)))
            .unwrap_or(Value::Null),
        DataType::Decimal256(..) => array
            .as_any()
            .downcast_ref::<PrimitiveArray<Decimal256Type>>()
            .map(|a| Value::String(a.value_as_string(i)))
            .unwrap_or(Value::Null),
        DataType::Utf8 => array
            .as_any()
            .downcast_ref::<GenericStringArray<i32>>()
            .map(|a| Value::String(a.value(i).to_string()))
            .unwrap_or(Value::Null),
        DataType::LargeUtf8 => array
            .as_any()
            .downcast_ref::<GenericStringArray<i64>>()
            .map(|a| Value::String(a.value(i).to_string()))
            .unwrap_or(Value::Null),
        DataType::Utf8View => Value::String(array.as_string_view().value(i).to_string()),
        DataType::Binary => array
            .as_any()
            .downcast_ref::<GenericBinaryArray<i32>>()
            .map(|a| bytes_cell(a.value(i)))
            .unwrap_or(Value::Null),
        DataType::LargeBinary => array
            .as_any()
            .downcast_ref::<GenericBinaryArray<i64>>()
            .map(|a| bytes_cell(a.value(i)))
            .unwrap_or(Value::Null),
        DataType::BinaryView => bytes_cell(array.as_binary_view().value(i)),
        DataType::FixedSizeBinary(width) => {
            let Some(a) = array.as_any().downcast_ref::<FixedSizeBinaryArray>() else {
                return Value::Null;
            };
            let raw = a.value(i);
            if *width == 16 && field.is_some_and(is_uuid_field) {
                uuid_string(raw)
                    .map(Value::String)
                    .unwrap_or_else(|| bytes_cell(raw))
            } else {
                bytes_cell(raw)
            }
        }
        DataType::Date32 => prim::<Date32Type>(array, i)
            .and_then(|days| {
                NaiveDate::from_num_days_from_ce_opt(days.checked_add(UNIX_EPOCH_DAYS_FROM_CE)?)
            })
            .map(|d| Value::String(d.format("%Y-%m-%d").to_string()))
            .unwrap_or_else(|| display_cell(array, i)),
        DataType::Date64 => prim::<Date64Type>(array, i)
            .and_then(DateTime::<Utc>::from_timestamp_millis)
            .map(|d| Value::String(d.date_naive().format("%Y-%m-%d").to_string()))
            .unwrap_or_else(|| display_cell(array, i)),
        DataType::Timestamp(unit, tz) => timestamp_cell(array, i, *unit, tz.as_deref()),
        DataType::Time32(_) | DataType::Time64(_) => time_cell(array, i),
        DataType::Duration(_) | DataType::Interval(_) => display_cell(array, i),
        DataType::Struct(_) => array
            .as_any()
            .downcast_ref::<StructArray>()
            .map(|a| struct_cell(a, i, depth))
            .unwrap_or(Value::Null),
        DataType::List(_) => array
            .as_any()
            .downcast_ref::<GenericListArray<i32>>()
            .map(|a| list_cell(a, i, depth))
            .unwrap_or(Value::Null),
        DataType::LargeList(_) => array
            .as_any()
            .downcast_ref::<GenericListArray<i64>>()
            .map(|a| list_cell(a, i, depth))
            .unwrap_or(Value::Null),
        DataType::FixedSizeList(..) => {
            let values = array.as_fixed_size_list().value(i);
            Value::Array(
                (0..values.len())
                    .map(|j| cell(values.as_ref(), j, None, depth + 1))
                    .collect(),
            )
        }
        DataType::Map(..) => array
            .as_any()
            .downcast_ref::<MapArray>()
            .map(|a| map_cell(a, i, depth))
            .unwrap_or(Value::Null),
        DataType::Dictionary(..) => match array.as_any_dictionary_opt() {
            Some(dict) => {
                let keys = dict.normalized_keys();
                match keys.get(i) {
                    Some(&k) => cell(dict.values().as_ref(), k, field, depth + 1),
                    None => Value::Null,
                }
            }
            None => display_cell(array, i),
        },
        _ => display_cell(array, i),
    }
}

/// Days from 0001-01-01 (CE) to 1970-01-01.
const UNIX_EPOCH_DAYS_FROM_CE: i32 = 719_163;

fn prim<T: ArrowPrimitiveType>(array: &dyn Array, i: usize) -> Option<T::Native> {
    array
        .as_any()
        .downcast_ref::<PrimitiveArray<T>>()
        .map(|a| a.value(i))
}

fn int<T>(array: &dyn Array, i: usize) -> Value
where
    T: ArrowPrimitiveType,
    T::Native: Into<i64>,
{
    prim::<T>(array, i)
        .map(|v| Value::from(v.into()))
        .unwrap_or(Value::Null)
}

fn string_of<T>(array: &dyn Array, i: usize) -> Value
where
    T: ArrowPrimitiveType,
    T::Native: ToString,
{
    prim::<T>(array, i)
        .map(|v| Value::String(v.to_string()))
        .unwrap_or(Value::Null)
}

fn decimal_string(array: &dyn Array, i: usize) -> Value {
    // Decimal32/64 are rare; the formatter prints the exact scaled value.
    display_cell(array, i)
}

fn float(v: f64) -> Value {
    if v.is_finite() {
        Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(v.to_string()))
    } else {
        Value::String(non_finite(v))
    }
}

fn float32(v: f32) -> Value {
    if v.is_finite() {
        // Shortest decimal repr, so 0.1f32 shows as 0.1 rather than 0.10000000149011612.
        let widened = v.to_string().parse::<f64>().unwrap_or(f64::from(v));
        Number::from_f64(widened)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(v.to_string()))
    } else {
        Value::String(non_finite(f64::from(v)))
    }
}

fn non_finite(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v.is_sign_positive() {
        "Infinity".into()
    } else {
        "-Infinity".into()
    }
}

fn bytes_cell(raw: &[u8]) -> Value {
    let mut m = Map::new();
    m.insert(
        "$bytes".into(),
        Value::String(base64::engine::general_purpose::STANDARD.encode(raw)),
    );
    m.insert("len".into(), Value::from(raw.len()));
    Value::Object(m)
}

fn uuid_string(raw: &[u8]) -> Option<String> {
    if raw.len() != 16 {
        return None;
    }
    let h = hex::encode(raw);
    Some(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

fn is_uuid_field(field: &Field) -> bool {
    UUID_EXTENSION_KEYS.iter().any(|k| {
        field
            .metadata()
            .get(*k)
            .is_some_and(|v| UUID_EXTENSION_VALUES.contains(&v.as_str()))
    })
}

fn display_cell(array: &dyn Array, i: usize) -> Value {
    match ArrayFormatter::try_new(array, &FormatOptions::default()) {
        Ok(f) => Value::String(f.value(i).to_string()),
        Err(_) => Value::Null,
    }
}

fn timestamp_cell(array: &dyn Array, i: usize, unit: TimeUnit, tz: Option<&str>) -> Value {
    let naive: Option<NaiveDateTime> = match unit {
        TimeUnit::Second => ts_naive::<TimestampSecondType>(array, i),
        TimeUnit::Millisecond => ts_naive::<TimestampMillisecondType>(array, i),
        TimeUnit::Microsecond => ts_naive::<TimestampMicrosecondType>(array, i),
        TimeUnit::Nanosecond => ts_naive::<TimestampNanosecondType>(array, i),
    };
    let Some(naive) = naive else {
        return display_cell(array, i);
    };
    match tz {
        // Zoned: the stored instant is UTC; render it in the column's zone (offset or IANA name).
        Some(tz_name) => {
            let utc = naive.and_utc();
            match Tz::from_str(tz_name) {
                Ok(tz) => Value::String(
                    utc.with_timezone(&tz)
                        .to_rfc3339_opts(SecondsFormat::AutoSi, true),
                ),
                Err(_) => Value::String(utc.to_rfc3339_opts(SecondsFormat::AutoSi, true)),
            }
        }
        None => Value::String(naive.format("%Y-%m-%dT%H:%M:%S%.f").to_string()),
    }
}

fn ts_naive<T: datafusion::arrow::datatypes::ArrowTimestampType>(
    array: &dyn Array,
    i: usize,
) -> Option<NaiveDateTime> {
    array
        .as_any()
        .downcast_ref::<PrimitiveArray<T>>()?
        .value_as_datetime(i)
}

fn time_cell(array: &dyn Array, i: usize) -> Value {
    use datafusion::arrow::datatypes::{
        Time32MillisecondType, Time32SecondType, Time64MicrosecondType, Time64NanosecondType,
    };
    let t = match array.data_type() {
        DataType::Time32(TimeUnit::Second) => prim::<Time32SecondType>(array, i)
            .and_then(|s| u32::try_from(s).ok())
            .and_then(|s| NaiveTime::from_num_seconds_from_midnight_opt(s, 0)),
        DataType::Time32(TimeUnit::Millisecond) => prim::<Time32MillisecondType>(array, i)
            .and_then(|ms| u32::try_from(ms).ok())
            .and_then(|ms| {
                NaiveTime::from_num_seconds_from_midnight_opt(ms / 1000, (ms % 1000) * 1_000_000)
            }),
        DataType::Time64(TimeUnit::Microsecond) => prim::<Time64MicrosecondType>(array, i)
            .and_then(|us| u64::try_from(us).ok())
            .and_then(|us| {
                NaiveTime::from_num_seconds_from_midnight_opt(
                    u32::try_from(us / 1_000_000).ok()?,
                    u32::try_from(us % 1_000_000).ok()? * 1000,
                )
            }),
        DataType::Time64(TimeUnit::Nanosecond) => prim::<Time64NanosecondType>(array, i)
            .and_then(|ns| u64::try_from(ns).ok())
            .and_then(|ns| {
                NaiveTime::from_num_seconds_from_midnight_opt(
                    u32::try_from(ns / 1_000_000_000).ok()?,
                    u32::try_from(ns % 1_000_000_000).ok()?,
                )
            }),
        _ => None,
    };
    match t {
        Some(t) => Value::String(t.format("%H:%M:%S%.f").to_string()),
        None => display_cell(array, i),
    }
}

fn struct_cell(a: &StructArray, i: usize, depth: usize) -> Value {
    let mut m = Map::new();
    for (field, col) in a.fields().iter().zip(a.columns()) {
        m.insert(
            field.name().clone(),
            cell(col.as_ref(), i, Some(field.as_ref()), depth + 1),
        );
    }
    Value::Object(m)
}

fn list_cell<O: OffsetSizeTrait>(a: &GenericListArray<O>, i: usize, depth: usize) -> Value {
    let values = a.value(i);
    let item_field = match a.data_type() {
        DataType::List(f) | DataType::LargeList(f) => Some(f.clone()),
        _ => None,
    };
    Value::Array(
        (0..values.len())
            .map(|j| cell(values.as_ref(), j, item_field.as_deref(), depth + 1))
            .collect(),
    )
}

fn map_cell(a: &MapArray, i: usize, depth: usize) -> Value {
    let entries = a.value(i);
    let (keys, values) = (entries.column(0), entries.column(1));
    let fields = entries.fields();
    Value::Array(
        (0..entries.len())
            .map(|j| {
                let mut m = Map::new();
                m.insert(
                    "key".into(),
                    cell(
                        keys.as_ref(),
                        j,
                        fields.first().map(|f| f.as_ref()),
                        depth + 1,
                    ),
                );
                m.insert(
                    "value".into(),
                    cell(
                        values.as_ref(),
                        j,
                        fields.get(1).map(|f| f.as_ref()),
                        depth + 1,
                    ),
                );
                Value::Object(m)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use datafusion::arrow::array::builder::{
        Int32Builder, ListBuilder, MapBuilder, StringBuilder as SB,
    };
    use datafusion::arrow::array::{
        ArrayRef, BinaryArray, BooleanArray, Date32Array, Date64Array, Decimal128Array,
        Decimal256Array, DictionaryArray, DurationSecondArray, FixedSizeBinaryArray, Float32Array,
        Float64Array, Int8Array, Int32Array, Int64Array, LargeStringArray, ListArray, NullArray,
        StringArray, StringBuilder, Time32SecondArray, Time64MicrosecondArray,
        TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
        TimestampSecondArray, UInt8Array, UInt64Array,
    };
    use datafusion::arrow::datatypes::{Fields, Int32Type, i256};
    use serde_json::json;

    use super::*;

    fn one(array: ArrayRef) -> Value {
        cell_at(array.as_ref(), 0, None)
    }

    #[test]
    fn small_ints_are_numbers_and_big_ints_are_strings() {
        assert_eq!(one(Arc::new(Int8Array::from(vec![-5]))), json!(-5));
        assert_eq!(
            one(Arc::new(Int32Array::from(vec![2_147_483_647]))),
            json!(2_147_483_647)
        );
        assert_eq!(one(Arc::new(UInt8Array::from(vec![200]))), json!(200));
        assert_eq!(
            one(Arc::new(Int64Array::from(vec![9_007_199_254_740_993]))),
            json!("9007199254740993")
        );
        assert_eq!(
            one(Arc::new(UInt64Array::from(vec![u64::MAX]))),
            json!("18446744073709551615")
        );
    }

    #[test]
    fn floats_round_trip_and_non_finite_become_strings() {
        assert_eq!(one(Arc::new(Float64Array::from(vec![1.5]))), json!(1.5));
        assert_eq!(one(Arc::new(Float32Array::from(vec![0.1f32]))), json!(0.1));
        assert_eq!(
            one(Arc::new(Float64Array::from(vec![f64::NAN]))),
            json!("NaN")
        );
        assert_eq!(
            one(Arc::new(Float64Array::from(vec![f64::INFINITY]))),
            json!("Infinity")
        );
        assert_eq!(
            one(Arc::new(Float32Array::from(vec![f32::NEG_INFINITY]))),
            json!("-Infinity")
        );
    }

    #[test]
    fn decimals_keep_every_digit() {
        let d = Decimal128Array::from(vec![123_456_789_012_345i128])
            .with_precision_and_scale(20, 2)
            .unwrap();
        assert_eq!(one(Arc::new(d)), json!("1234567890123.45"));
        let neg = Decimal128Array::from(vec![-5i128])
            .with_precision_and_scale(12, 2)
            .unwrap();
        assert_eq!(one(Arc::new(neg)), json!("-0.05"));
        let wide = Decimal256Array::from(vec![i256::from_i128(10_i128.pow(30))])
            .with_precision_and_scale(50, 4)
            .unwrap();
        assert_eq!(
            one(Arc::new(wide)),
            json!("100000000000000000000000000.0000")
        );
    }

    #[test]
    fn text_bool_and_null() {
        assert_eq!(
            one(Arc::new(StringArray::from(vec!["héllo"]))),
            json!("héllo")
        );
        assert_eq!(one(Arc::new(LargeStringArray::from(vec!["x"]))), json!("x"));
        assert_eq!(one(Arc::new(BooleanArray::from(vec![true]))), json!(true));
        assert_eq!(one(Arc::new(NullArray::new(1))), Value::Null);
        assert_eq!(one(Arc::new(Int32Array::from(vec![None]))), Value::Null);
        assert_eq!(
            one(Arc::new(StringArray::from(vec![None::<&str>]))),
            Value::Null
        );
    }

    #[test]
    fn binary_is_base64_with_length() {
        let v = one(Arc::new(BinaryArray::from(vec![
            &[0xde_u8, 0xad, 0xbe, 0xef][..],
        ])));
        assert_eq!(v, json!({"$bytes": "3q2+7w==", "len": 4}));
        let f = FixedSizeBinaryArray::try_from_iter(vec![vec![1u8, 2, 3]].into_iter()).unwrap();
        assert_eq!(one(Arc::new(f)), json!({"$bytes": "AQID", "len": 3}));
    }

    #[test]
    fn uuid_needs_the_extension_flag() {
        let raw = [
            0x12u8, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc,
            0xde, 0xf0,
        ];
        let arr = FixedSizeBinaryArray::try_from_iter(vec![raw.to_vec()].into_iter()).unwrap();
        let plain = Field::new("u", DataType::FixedSizeBinary(16), true);
        assert!(
            cell_at(&arr, 0, Some(&plain)).get("$bytes").is_some(),
            "no flag, so raw bytes"
        );
        let flagged = Field::new("u", DataType::FixedSizeBinary(16), true).with_metadata(
            HashMap::from([("ARROW:extension:name".to_string(), "arrow.uuid".to_string())]),
        );
        assert_eq!(
            cell_at(&arr, 0, Some(&flagged)),
            json!("12345678-9abc-def0-1234-56789abcdef0")
        );
    }

    #[test]
    fn dates() {
        assert_eq!(
            one(Arc::new(Date32Array::from(vec![0]))),
            json!("1970-01-01")
        );
        assert_eq!(
            one(Arc::new(Date32Array::from(vec![20_332]))),
            json!("2025-09-01")
        );
        assert_eq!(
            one(Arc::new(Date32Array::from(vec![-1]))),
            json!("1969-12-31")
        );
        assert_eq!(
            one(Arc::new(Date64Array::from(vec![86_400_000]))),
            json!("1970-01-02")
        );
    }

    #[test]
    fn timestamps_respect_unit_and_zone() {
        // 2026-10-01T00:00:00Z
        let secs = 1_790_812_800i64;
        let tz = Some(Arc::<str>::from("+00:00"));
        let us = TimestampMicrosecondArray::from(vec![secs * 1_000_000 + 123_456])
            .with_timezone_opt(tz.clone());
        assert_eq!(one(Arc::new(us)), json!("2026-10-01T00:00:00.123456Z"));
        let ms = TimestampMillisecondArray::from(vec![secs * 1000]).with_timezone_opt(tz.clone());
        assert_eq!(one(Arc::new(ms)), json!("2026-10-01T00:00:00Z"));
        let s = TimestampSecondArray::from(vec![secs]).with_timezone_opt(tz.clone());
        assert_eq!(one(Arc::new(s)), json!("2026-10-01T00:00:00Z"));
        let ns =
            TimestampNanosecondArray::from(vec![secs * 1_000_000_000 + 5]).with_timezone_opt(tz);
        assert_eq!(one(Arc::new(ns)), json!("2026-10-01T00:00:00.000000005Z"));
        // Non-UTC offset: same instant, shown in the column's zone.
        let plus2 = TimestampMicrosecondArray::from(vec![secs * 1_000_000])
            .with_timezone_opt(Some(Arc::<str>::from("+02:00")));
        assert_eq!(one(Arc::new(plus2)), json!("2026-10-01T02:00:00+02:00"));
        // Naive: no offset at all.
        let naive = TimestampMicrosecondArray::from(vec![secs * 1_000_000]);
        assert_eq!(one(Arc::new(naive)), json!("2026-10-01T00:00:00"));
    }

    #[test]
    fn times_and_durations() {
        assert_eq!(
            one(Arc::new(Time32SecondArray::from(vec![3_661]))),
            json!("01:01:01")
        );
        assert_eq!(
            one(Arc::new(Time64MicrosecondArray::from(vec![3_661_500_000]))),
            json!("01:01:01.500")
        );
        let d = one(Arc::new(DurationSecondArray::from(vec![90])));
        assert!(d.is_string());
    }

    #[test]
    fn structs_become_objects() {
        let a: ArrayRef = Arc::new(Int32Array::from(vec![7]));
        let b: ArrayRef = Arc::new(StringArray::from(vec!["ios"]));
        let c: ArrayRef = Arc::new(Int64Array::from(vec![42]));
        let s = StructArray::try_new(
            Fields::from(vec![
                Field::new("version", DataType::Int32, true),
                Field::new("device", DataType::Utf8, true),
                Field::new("big", DataType::Int64, true),
            ]),
            vec![a, b, c],
            None,
        )
        .unwrap();
        assert_eq!(
            one(Arc::new(s)),
            json!({"version": 7, "device": "ios", "big": "42"})
        );
    }

    #[test]
    fn lists_become_arrays_with_nulls() {
        let mut b = ListBuilder::new(SB::new());
        b.values().append_value("a");
        b.values().append_null();
        b.values().append_value("c");
        b.append(true);
        b.append(false);
        let list: ListArray = b.finish();
        assert_eq!(cell_at(&list, 0, None), json!(["a", null, "c"]));
        assert_eq!(cell_at(&list, 1, None), Value::Null);
        // nested list of ints holds numbers, not strings
        let mut ib = ListBuilder::new(Int32Builder::new());
        ib.values().append_value(1);
        ib.values().append_value(2);
        ib.append(true);
        assert_eq!(cell_at(&ib.finish(), 0, None), json!([1, 2]));
    }

    #[test]
    fn maps_become_arrays_of_key_value_objects() {
        let mut b = MapBuilder::new(None, SB::new(), SB::new());
        b.keys().append_value("ref");
        b.values().append_value("r1");
        b.keys().append_value("utm");
        b.values().append_value("mail");
        b.append(true).unwrap();
        let m = b.finish();
        assert_eq!(
            cell_at(&m, 0, None),
            json!([{"key": "ref", "value": "r1"}, {"key": "utm", "value": "mail"}])
        );
    }

    #[test]
    fn dictionary_values_are_decoded() {
        let mut b = datafusion::arrow::array::builder::StringDictionaryBuilder::<Int32Type>::new();
        b.append("x").unwrap();
        b.append("y").unwrap();
        b.append("x").unwrap();
        let d: DictionaryArray<Int32Type> = b.finish();
        assert_eq!(cell_at(&d, 2, None), json!("x"));
        assert_eq!(cell_at(&d, 1, None), json!("y"));
    }

    #[test]
    fn batches_convert_row_major() {
        let schema = Arc::new(datafusion::arrow::datatypes::Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2])),
                Arc::new(StringArray::from(vec![Some("a"), None])),
            ],
        )
        .unwrap();
        assert_eq!(
            batch_to_rows(&batch),
            vec![vec![json!("1"), json!("a")], vec![json!("2"), Value::Null]]
        );
        let cols = columns_of(&schema);
        assert_eq!(cols[0].type_name, "bigint");
        assert_eq!(cols[1].type_name, "string");
        assert_eq!(cols[0].type_oid, 0);
    }

    #[test]
    fn type_names_are_sql_like() {
        assert_eq!(type_name(&DataType::Decimal128(12, 2)), "decimal(12, 2)");
        assert_eq!(
            type_name(&DataType::Timestamp(
                TimeUnit::Microsecond,
                Some("+00:00".into())
            )),
            "timestamp with time zone"
        );
        assert_eq!(
            type_name(&DataType::Timestamp(TimeUnit::Microsecond, None)),
            "timestamp"
        );
        let list = DataType::List(Arc::new(Field::new("item", DataType::Utf8, true)));
        assert_eq!(type_name(&list), "list<string>");
        let st = DataType::Struct(Fields::from(vec![Field::new("a", DataType::Int32, true)]));
        assert_eq!(type_name(&st), "struct<a: int>");
    }

    #[allow(dead_code)]
    fn _unused(_: StringBuilder) {}
}
