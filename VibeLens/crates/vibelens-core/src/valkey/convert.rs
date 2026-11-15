//! Pure conversions between RESP3 [`redis::Value`]s and the JSON / model types the UI consumes.
//!
//! Nothing here touches the network, so everything is unit-tested without a server.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use redis::Value;
use serde_json::{Map, Number, json};

use crate::error::{CoreError, Result};
use crate::model::KeyType;

/// Bytes → JSON: a JSON string when valid UTF-8, otherwise `{"$bytes": "<base64>", "len": n}`.
pub(super) fn bytes_to_json(bytes: &[u8]) -> serde_json::Value {
    match std::str::from_utf8(bytes) {
        Ok(s) => serde_json::Value::String(s.to_owned()),
        Err(_) => json!({ "$bytes": B64.encode(bytes), "len": bytes.len() }),
    }
}

/// Lossy display string for bytes (key names, hash fields).
pub(super) fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// An object key for bytes: the text when valid UTF-8, else `base64:<data>` so distinct binary
/// fields never collapse into the same lossy name.
pub(super) fn object_key_from_bytes(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => format!("base64:{}", B64.encode(bytes)),
    }
}

/// JSON number for a double; non-finite values (which JSON cannot carry) become strings.
pub(super) fn double_to_json(d: f64) -> serde_json::Value {
    match Number::from_f64(d) {
        Some(n) => serde_json::Value::Number(n),
        None if d.is_nan() => serde_json::Value::String("nan".into()),
        None if d.is_sign_negative() => serde_json::Value::String("-inf".into()),
        None => serde_json::Value::String("inf".into()),
    }
}

/// Bytes of any string-like reply (`BulkString`, `SimpleString`, `Okay`, `VerbatimString`).
pub(super) fn value_bytes(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::BulkString(b) => Some(b.clone()),
        Value::SimpleString(s) => Some(s.clone().into_bytes()),
        Value::Okay => Some(b"OK".to_vec()),
        Value::VerbatimString { text, .. } => Some(text.clone().into_bytes()),
        Value::Attribute { data, .. } => value_bytes(data),
        _ => None,
    }
}

/// Lossy text of a string-like reply.
pub(super) fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::Int(i) => Some(i.to_string()),
        Value::Double(d) => Some(d.to_string()),
        other => value_bytes(other).map(|b| lossy(&b)),
    }
}

/// Integer view of a reply (integers, or numeric strings).
pub(super) fn value_i64(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        other => value_text(other)?.trim().parse().ok(),
    }
}

/// Float view of a reply (doubles, integers, or numeric strings such as RESP2 scores).
pub(super) fn value_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Double(d) => Some(*d),
        Value::Int(i) => Some(*i as f64),
        other => {
            let t = value_text(other)?;
            match t.trim().to_ascii_lowercase().as_str() {
                "inf" | "+inf" => Some(f64::INFINITY),
                "-inf" => Some(f64::NEG_INFINITY),
                "nan" => Some(f64::NAN),
                s => s.parse().ok(),
            }
        }
    }
}

/// Elements of an array-like reply (`Array` or `Set`); `None` for anything else.
pub(super) fn value_items(v: &Value) -> Option<&[Value]> {
    match v {
        Value::Array(a) | Value::Set(a) => Some(a),
        Value::Attribute { data, .. } => value_items(data),
        _ => None,
    }
}

/// A map key rendered as text: strings as themselves, everything else as compact JSON.
fn map_key_to_string(k: &Value) -> String {
    match k {
        Value::BulkString(b) => object_key_from_bytes(b),
        Value::SimpleString(s) => s.clone(),
        Value::Okay => "OK".into(),
        Value::VerbatimString { text, .. } => text.clone(),
        Value::Int(i) => i.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Nil => "null".into(),
        other => value_to_json(other).to_string(),
    }
}

/// Converts any RESP3 reply into JSON for the console.
///
/// - strings: UTF-8 → string, otherwise `{"$bytes", "len"}`
/// - `Map` → object (non-string keys are stringified), `Set`/`Array` → array
/// - `Attribute` → its data (attributes are out-of-band metadata and are dropped)
/// - `BigNumber` → string (precision), non-finite doubles → string
/// - `Push` → `{"$push": kind, "data": [...]}`; `ServerError` → `{"$error": "CODE message"}`
pub(super) fn value_to_json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Nil => J::Null,
        Value::Int(i) => J::Number((*i).into()),
        Value::BulkString(b) => bytes_to_json(b),
        Value::Array(items) | Value::Set(items) => {
            J::Array(items.iter().map(value_to_json).collect())
        }
        Value::SimpleString(s) => J::String(s.clone()),
        Value::Okay => J::String("OK".into()),
        Value::Map(entries) => {
            let mut obj = Map::with_capacity(entries.len());
            for (k, val) in entries {
                obj.insert(map_key_to_string(k), value_to_json(val));
            }
            J::Object(obj)
        }
        Value::Attribute { data, .. } => value_to_json(data),
        Value::Double(d) => double_to_json(*d),
        Value::Boolean(b) => J::Bool(*b),
        Value::VerbatimString { text, .. } => J::String(text.clone()),
        Value::BigNumber(n) => J::String(n.to_string()),
        Value::Push { kind, data } => json!({
            "$push": format!("{kind:?}"),
            "data": data.iter().map(value_to_json).collect::<Vec<_>>(),
        }),
        Value::ServerError(e) => {
            let detail = e.details().unwrap_or("");
            json!({ "$error": format!("{} {}", e.code(), detail).trim_end() })
        }
        // `Value` is non-exhaustive; render anything a future crate version adds as debug text
        // instead of failing the whole command.
        #[allow(unreachable_patterns)]
        other => J::String(format!("{other:?}")),
    }
}

/// Field/value pairs from a `Map` reply or a flat `[k1, v1, k2, v2, ...]` array (RESP2 style,
/// still used by HSCAN and stream fields under RESP3). Entries with an odd tail are dropped.
pub(super) fn pairs_from_value(v: &Value) -> Vec<(Vec<u8>, Value)> {
    match v {
        Value::Map(entries) => entries
            .iter()
            .map(|(k, val)| {
                (
                    value_bytes(k).unwrap_or_else(|| map_key_to_string(k).into_bytes()),
                    val.clone(),
                )
            })
            .collect(),
        Value::Array(items) | Value::Set(items) => items
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                (
                    value_bytes(&c[0]).unwrap_or_else(|| map_key_to_string(&c[0]).into_bytes()),
                    c[1].clone(),
                )
            })
            .collect(),
        Value::Attribute { data, .. } => pairs_from_value(data),
        _ => Vec::new(),
    }
}

/// Text pairs, for `CONFIG GET` style replies.
pub(super) fn text_pairs(v: &Value) -> Vec<(String, String)> {
    pairs_from_value(v)
        .into_iter()
        .map(|(k, val)| (lossy(&k), value_text(&val).unwrap_or_default()))
        .collect()
}

/// `TYPE` reply → [`KeyType`]. Module types (other than ReJSON) are `Unknown`.
pub(super) fn parse_key_type(s: &str) -> KeyType {
    match s.to_ascii_lowercase().as_str() {
        "string" => KeyType::String,
        "hash" => KeyType::Hash,
        "list" => KeyType::List,
        "set" => KeyType::Set,
        "zset" => KeyType::Zset,
        "stream" => KeyType::Stream,
        "rejson-rl" => KeyType::Json,
        _ => KeyType::Unknown,
    }
}

// ----------------------------------------------------------------------------------------
// SCAN cursors
// ----------------------------------------------------------------------------------------

/// Decoded scan position. `node` is always 0 outside cluster mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScanCursor {
    pub node: usize,
    pub cursor: u64,
}

impl ScanCursor {
    pub const START: ScanCursor = ScanCursor { node: 0, cursor: 0 };

    /// Standalone: the plain decimal cursor. Cluster: `"<nodeIndex>:<nodeCursor>"`.
    pub fn encode(self, cluster: bool) -> String {
        if cluster {
            format!("{}:{}", self.node, self.cursor)
        } else {
            self.cursor.to_string()
        }
    }

    /// Inverse of [`ScanCursor::encode`]. `""` and `"0"` mean "start". Anything malformed is an
    /// `Invalid` error rather than a silent restart, so a UI bug cannot loop forever.
    pub fn decode(s: &str, cluster: bool) -> Result<ScanCursor> {
        let s = s.trim();
        if s.is_empty() || s == "0" {
            return Ok(Self::START);
        }
        let bad = || CoreError::Invalid(format!("invalid scan cursor {s:?}"));
        if cluster {
            let (node, cur) = s.split_once(':').ok_or_else(bad)?;
            Ok(ScanCursor {
                node: node.parse().map_err(|_| bad())?,
                cursor: cur.parse().map_err(|_| bad())?,
            })
        } else {
            Ok(ScanCursor {
                node: 0,
                cursor: s.parse().map_err(|_| bad())?,
            })
        }
    }
}

// ----------------------------------------------------------------------------------------
// INFO parsing
// ----------------------------------------------------------------------------------------

/// Parses `INFO` output (`# Section` headers and `key:value` lines) into a flat sorted map.
pub(super) fn parse_info(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// `db3:keys=10,expires=0,avg_ttl=0` entries of the INFO map → `{3: 10}`.
pub(super) fn parse_keyspace(info: &BTreeMap<String, String>) -> BTreeMap<u8, u64> {
    let mut out = BTreeMap::new();
    for (k, v) in info {
        let Some(idx) = k.strip_prefix("db").and_then(|n| n.parse::<u8>().ok()) else {
            continue;
        };
        let keys = v
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .find(|(name, _)| *name == "keys")
            .and_then(|(_, n)| n.parse::<u64>().ok());
        if let Some(keys) = keys {
            out.insert(idx, keys);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::{PushKind, VerbatimFormat};

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn bytes_utf8_vs_binary() {
        assert_eq!(bytes_to_json(b"hello"), json!("hello"));
        assert_eq!(bytes_to_json("héllo ✓".as_bytes()), json!("héllo ✓"));
        let v = bytes_to_json(&[0xff, 0xfe, 0x00, 0x41]);
        assert_eq!(v, json!({ "$bytes": "//4AQQ==", "len": 4 }));
        assert_eq!(bytes_to_json(b""), json!(""));
    }

    #[test]
    fn scalars() {
        assert_eq!(value_to_json(&Value::Nil), json!(null));
        assert_eq!(value_to_json(&Value::Int(-7)), json!(-7));
        assert_eq!(value_to_json(&Value::Okay), json!("OK"));
        assert_eq!(
            value_to_json(&Value::SimpleString("PONG".into())),
            json!("PONG")
        );
        assert_eq!(value_to_json(&Value::Boolean(true)), json!(true));
        assert_eq!(value_to_json(&Value::Double(1.5)), json!(1.5));
        assert_eq!(value_to_json(&Value::Double(f64::INFINITY)), json!("inf"));
        assert_eq!(
            value_to_json(&Value::Double(f64::NEG_INFINITY)),
            json!("-inf")
        );
        assert_eq!(value_to_json(&Value::Double(f64::NAN)), json!("nan"));
        assert_eq!(
            value_to_json(&Value::VerbatimString {
                format: VerbatimFormat::Text,
                text: "line1\nline2".into()
            }),
            json!("line1\nline2")
        );
    }

    #[test]
    fn big_number_is_a_string() {
        let v = value_to_json(&Value::BigNumber(
            "123456789012345678901234567890".parse().unwrap(),
        ));
        assert_eq!(v, json!("123456789012345678901234567890"));
    }

    #[test]
    fn collections() {
        let arr = Value::Array(vec![
            Value::Int(1),
            bulk("a"),
            Value::Nil,
            Value::Array(vec![bulk("x")]),
        ]);
        assert_eq!(value_to_json(&arr), json!([1, "a", null, ["x"]]));
        let set = Value::Set(vec![bulk("m1"), bulk("m2")]);
        assert_eq!(value_to_json(&set), json!(["m1", "m2"]));
    }

    #[test]
    fn map_with_non_string_keys_is_stringified() {
        let m = Value::Map(vec![
            (bulk("name"), bulk("vg")),
            (Value::Int(42), Value::Boolean(false)),
            (Value::SimpleString("s".into()), Value::Double(0.5)),
            (Value::Boolean(true), Value::Nil),
            (Value::BulkString(vec![0xff, 0x01]), Value::Int(1)),
            (
                Value::Array(vec![Value::Int(1), Value::Int(2)]),
                Value::Int(9),
            ),
        ]);
        assert_eq!(
            value_to_json(&m),
            json!({ "name": "vg", "42": false, "s": 0.5, "true": null, "base64:/wE=": 1, "[1,2]": 9 })
        );
    }

    #[test]
    fn nested_map_in_array_like_hello() {
        let hello = Value::Map(vec![
            (bulk("server"), bulk("redis")),
            (bulk("proto"), Value::Int(3)),
            (bulk("modules"), Value::Array(vec![])),
        ]);
        assert_eq!(
            value_to_json(&hello),
            json!({ "server": "redis", "proto": 3, "modules": [] })
        );
    }

    #[test]
    fn attribute_unwraps_to_data() {
        let v = Value::Attribute {
            data: Box::new(Value::Int(5)),
            attributes: vec![(bulk("ttl"), Value::Int(1))],
        };
        assert_eq!(value_to_json(&v), json!(5));
    }

    #[test]
    fn push_and_error() {
        let p = Value::Push {
            kind: PushKind::Message,
            data: vec![bulk("chan"), bulk("payload")],
        };
        let j = value_to_json(&p);
        assert_eq!(j["data"], json!(["chan", "payload"]));
        assert!(j["$push"].as_str().is_some());

        let e = redis::parse_redis_value(b"-NOPERM this user has no permissions\r\n").unwrap();
        assert!(matches!(e, Value::ServerError(_)));
        let j = value_to_json(&e);
        assert!(j["$error"].as_str().unwrap().starts_with("NOPERM"));
        // errors nested in arrays survive as values
        assert!(value_to_json(&Value::Array(vec![Value::Int(1), e]))[1]["$error"].is_string());
    }

    #[test]
    fn pairs_from_map_and_flat_array() {
        let m = Value::Map(vec![(bulk("a"), bulk("1")), (bulk("b"), bulk("2"))]);
        let f = Value::Array(vec![
            bulk("a"),
            bulk("1"),
            bulk("b"),
            bulk("2"),
            bulk("dangling"),
        ]);
        let pm = pairs_from_value(&m);
        let pf = pairs_from_value(&f);
        assert_eq!(pm.len(), 2);
        assert_eq!(pf.len(), 2);
        assert_eq!(pm[0].0, b"a");
        assert_eq!(value_text(&pf[1].1).as_deref(), Some("2"));
        assert_eq!(
            text_pairs(&m),
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
    }

    #[test]
    fn numeric_views() {
        assert_eq!(value_i64(&Value::Int(3)), Some(3));
        assert_eq!(value_i64(&bulk(" 12 ")), Some(12));
        assert_eq!(value_i64(&bulk("x")), None);
        assert_eq!(value_f64(&bulk("1.25")), Some(1.25));
        assert_eq!(value_f64(&bulk("-inf")), Some(f64::NEG_INFINITY));
        assert_eq!(value_f64(&Value::Double(2.0)), Some(2.0));
    }

    #[test]
    fn key_types() {
        assert_eq!(parse_key_type("string"), KeyType::String);
        assert_eq!(parse_key_type("zset"), KeyType::Zset);
        assert_eq!(parse_key_type("stream"), KeyType::Stream);
        assert_eq!(parse_key_type("ReJSON-RL"), KeyType::Json);
        assert_eq!(parse_key_type("MBbloom--"), KeyType::Unknown);
        assert_eq!(parse_key_type("none"), KeyType::Unknown);
    }

    #[test]
    fn cursor_roundtrip_standalone() {
        let c = ScanCursor {
            node: 0,
            cursor: 18446744073709551615,
        };
        assert_eq!(c.encode(false), "18446744073709551615");
        assert_eq!(
            ScanCursor::decode("18446744073709551615", false).unwrap(),
            c
        );
        assert_eq!(ScanCursor::decode("0", false).unwrap(), ScanCursor::START);
        assert_eq!(ScanCursor::decode("", false).unwrap(), ScanCursor::START);
    }

    #[test]
    fn cursor_roundtrip_cluster() {
        let c = ScanCursor {
            node: 2,
            cursor: 4711,
        };
        assert_eq!(c.encode(true), "2:4711");
        assert_eq!(ScanCursor::decode("2:4711", true).unwrap(), c);
        assert_eq!(ScanCursor::decode("0", true).unwrap(), ScanCursor::START);
        assert_eq!(ScanCursor::decode("0:0", true).unwrap(), ScanCursor::START);
    }

    #[test]
    fn cursor_rejects_garbage() {
        for bad in ["abc", "-1", "1.5", "99999999999999999999999"] {
            assert!(
                matches!(ScanCursor::decode(bad, false), Err(CoreError::Invalid(_))),
                "{bad}"
            );
        }
        for bad in ["5", "a:1", "1:b", "1:2:3", ":"] {
            assert!(
                matches!(ScanCursor::decode(bad, true), Err(CoreError::Invalid(_))),
                "{bad}"
            );
        }
        // a cluster cursor is not a valid standalone cursor, and vice versa
        assert!(ScanCursor::decode("1:2", false).is_err());
    }

    #[test]
    fn info_parsing() {
        let text = "# Server\r\nredis_version:7.0.15\r\nredis_mode:standalone\r\n\r\n# Keyspace\r\ndb0:keys=5,expires=1,avg_ttl=100\r\ndb14:keys=1200,expires=0,avg_ttl=0\r\n";
        let info = parse_info(text);
        assert_eq!(
            info.get("redis_version").map(String::as_str),
            Some("7.0.15")
        );
        assert_eq!(
            info.get("redis_mode").map(String::as_str),
            Some("standalone")
        );
        let ks = parse_keyspace(&info);
        assert_eq!(ks.get(&0), Some(&5));
        assert_eq!(ks.get(&14), Some(&1200));
        assert_eq!(ks.len(), 2);
    }

    #[test]
    fn info_value_with_colon_keeps_remainder() {
        let info = parse_info("config_file:C:\\conf\\redis.conf\r\n");
        assert_eq!(
            info.get("config_file").map(String::as_str),
            Some("C:\\conf\\redis.conf")
        );
    }
}
