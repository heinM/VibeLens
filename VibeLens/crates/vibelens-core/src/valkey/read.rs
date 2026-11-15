//! Typed value reads for the key inspector (`ValkeyHandle::get_value`).
//!
//! Every read is bounded: collections are capped at `max_items` (using `*SCAN` when the
//! collection is larger than the cap so the server never builds a huge reply), and strings at
//! [`MAX_STRING_BYTES`]. `truncated` tells the UI that more exists.

use redis::{Cmd, Value, cmd, pipe};
use serde_json::{Map, json};

use super::convert::*;
use super::session::Session;
use crate::error::{CoreError, Result};
use crate::model::{KeyType, ValkeyValue};

/// Largest string payload fetched (bytes). Beyond this the value is cut and flagged `truncated`.
pub(super) const MAX_STRING_BYTES: usize = 1024 * 1024;
/// Hard ceiling on `max_items`, whatever the caller asks for.
pub(super) const MAX_ITEMS_CEILING: usize = 100_000;
/// `COUNT` hint for `HSCAN`/`SSCAN` pages.
const SCAN_PAGE: usize = 256;

fn unexpected(what: &str, v: &Value) -> CoreError {
    CoreError::Valkey(format!("unexpected reply to {what}: {v:?}"))
}

fn int_at(replies: &[Value], i: usize, what: &str) -> Result<i64> {
    replies
        .get(i)
        .and_then(value_i64)
        .ok_or_else(|| CoreError::Valkey(format!("unexpected reply to {what}")))
}

pub(super) async fn get_value(
    s: &Session,
    db: u8,
    key: &str,
    max_items: usize,
) -> Result<ValkeyValue> {
    let max = max_items.clamp(1, MAX_ITEMS_CEILING);

    // TYPE and PTTL in one round trip. PTTL is -2 for a missing key, -1 for no expiry.
    let mut head = pipe();
    head.cmd("TYPE").arg(key).cmd("PTTL").arg(key);
    let head = s.pipeline(db, &head, None).await?;
    let type_name = head
        .first()
        .and_then(value_text)
        .ok_or_else(|| CoreError::Valkey("unexpected reply to TYPE".into()))?;
    let pttl = int_at(&head, 1, "PTTL")?;
    if type_name == "none" || pttl == -2 {
        return Err(CoreError::NotFound(format!("key {key:?} does not exist")));
    }
    let key_type = parse_key_type(&type_name);
    let ttl_ms = if pttl < 0 { -1 } else { pttl };

    let (value, size, truncated) = match key_type {
        KeyType::String => read_string(s, db, key).await?,
        KeyType::Hash => read_hash(s, db, key, max).await?,
        KeyType::List => read_list(s, db, key, max).await?,
        KeyType::Set => read_set(s, db, key, max).await?,
        KeyType::Zset => read_zset(s, db, key, max).await?,
        KeyType::Stream => read_stream(s, db, key, max).await?,
        KeyType::Json => read_json(s, db, key).await?,
        // Module types we cannot render: report the type instead of failing.
        KeyType::Unknown => (json!({ "unsupportedType": type_name }), None, false),
    };
    Ok(ValkeyValue {
        key: key.to_owned(),
        key_type,
        ttl_ms,
        size,
        value,
        truncated,
    })
}

type Read = (serde_json::Value, Option<u64>, bool);

async fn read_string(s: &Session, db: u8, key: &str) -> Result<Read> {
    let mut p = pipe();
    p.cmd("STRLEN")
        .arg(key)
        .cmd("GETRANGE")
        .arg(key)
        .arg(0)
        .arg(MAX_STRING_BYTES - 1);
    let r = s.pipeline(db, &p, None).await?;
    let len = int_at(&r, 0, "STRLEN")?.max(0) as u64;
    let mut bytes = r.get(1).and_then(value_bytes).unwrap_or_default();
    let truncated = len as usize > MAX_STRING_BYTES;
    if truncated {
        // The cut may land inside a multi-byte character; drop the partial tail so a text value
        // stays text instead of flipping to base64.
        if let Err(e) = std::str::from_utf8(&bytes)
            && e.error_len().is_none()
        {
            bytes.truncate(e.valid_up_to());
        }
    }
    Ok((bytes_to_json(&bytes), Some(len), truncated))
}

/// Pages `HSCAN`/`SSCAN`/... until `cap` items are gathered. Returns the raw elements
/// (`per_item` elements make one item) and whether more remain.
async fn scan_collect(
    s: &Session,
    db: u8,
    verb: &str,
    key: &str,
    cap: usize,
    per_item: usize,
) -> Result<(Vec<Value>, bool)> {
    let mut items: Vec<Value> = Vec::new();
    let mut cursor = String::from("0");
    loop {
        let mut c = cmd(verb);
        c.arg(key).arg(&cursor).arg("COUNT").arg(SCAN_PAGE);
        let reply = s.exec(db, &c, None, true).await?;
        let parts = value_items(&reply).ok_or_else(|| unexpected(verb, &reply))?;
        let [next, page] = parts else {
            return Err(unexpected(verb, &reply));
        };
        cursor = value_text(next).ok_or_else(|| unexpected(verb, &reply))?;
        let page = match page {
            // Some servers/versions return HSCAN pages as a map under RESP3.
            Value::Map(entries) => entries
                .iter()
                .flat_map(|(k, v)| [k.clone(), v.clone()])
                .collect(),
            other => value_items(other)
                .ok_or_else(|| unexpected(verb, &reply))?
                .to_vec(),
        };
        items.extend(page);
        if cursor == "0" {
            let more = items.len() > cap * per_item;
            items.truncate(cap * per_item);
            return Ok((items, more));
        }
        if items.len() >= cap * per_item {
            items.truncate(cap * per_item);
            return Ok((items, true));
        }
    }
}

fn pairs_to_object(pairs: Vec<(Vec<u8>, Value)>) -> serde_json::Value {
    let mut obj = Map::with_capacity(pairs.len());
    for (k, v) in pairs {
        let json = match value_bytes(&v) {
            Some(b) => bytes_to_json(&b),
            None => value_to_json(&v),
        };
        obj.insert(object_key_from_bytes(&k), json);
    }
    serde_json::Value::Object(obj)
}

async fn read_hash(s: &Session, db: u8, key: &str, max: usize) -> Result<Read> {
    let len = s.exec(db, cmd("HLEN").arg(key), None, true).await?;
    let len = value_i64(&len)
        .ok_or_else(|| unexpected("HLEN", &len))?
        .max(0) as u64;
    if len as usize <= max {
        let all = s.exec(db, cmd("HGETALL").arg(key), None, true).await?;
        return Ok((pairs_to_object(pairs_from_value(&all)), Some(len), false));
    }
    let (flat, _more) = scan_collect(s, db, "HSCAN", key, max, 2).await?;
    Ok((
        pairs_to_object(pairs_from_value(&Value::Array(flat))),
        Some(len),
        true,
    ))
}

async fn read_list(s: &Session, db: u8, key: &str, max: usize) -> Result<Read> {
    let mut p = pipe();
    p.cmd("LLEN")
        .arg(key)
        .cmd("LRANGE")
        .arg(key)
        .arg(0)
        .arg(max - 1);
    let r = s.pipeline(db, &p, None).await?;
    let len = int_at(&r, 0, "LLEN")?.max(0) as u64;
    let items = r
        .get(1)
        .and_then(value_items)
        .ok_or_else(|| CoreError::Valkey("unexpected reply to LRANGE".into()))?;
    Ok((
        serde_json::Value::Array(items.iter().map(element_to_json).collect()),
        Some(len),
        len as usize > max,
    ))
}

fn element_to_json(v: &Value) -> serde_json::Value {
    match value_bytes(v) {
        Some(b) => bytes_to_json(&b),
        None => value_to_json(v),
    }
}

async fn read_set(s: &Session, db: u8, key: &str, max: usize) -> Result<Read> {
    let card = s.exec(db, cmd("SCARD").arg(key), None, true).await?;
    let card = value_i64(&card)
        .ok_or_else(|| unexpected("SCARD", &card))?
        .max(0) as u64;
    if card as usize <= max {
        let all = s.exec(db, cmd("SMEMBERS").arg(key), None, true).await?;
        let items = value_items(&all).ok_or_else(|| unexpected("SMEMBERS", &all))?;
        return Ok((
            serde_json::Value::Array(items.iter().map(element_to_json).collect()),
            Some(card),
            false,
        ));
    }
    let (items, _more) = scan_collect(s, db, "SSCAN", key, max, 1).await?;
    Ok((
        serde_json::Value::Array(items.iter().map(element_to_json).collect()),
        Some(card),
        true,
    ))
}

/// `ZRANGE ... WITHSCORES` is `[[member, score], ...]` under RESP3 and `[member, score, ...]`
/// under RESP2; accept both.
fn zset_members(reply: &Value) -> Result<Vec<serde_json::Value>> {
    let items = value_items(reply).ok_or_else(|| unexpected("ZRANGE", reply))?;
    let entry = |member: &Value, score: &Value| {
        json!({
            "member": element_to_json(member),
            "score": value_f64(score).map(double_to_json).unwrap_or(serde_json::Value::Null),
        })
    };
    if items
        .iter()
        .all(|i| matches!(i, Value::Array(p) if p.len() == 2))
        && !items.is_empty()
    {
        Ok(items
            .iter()
            .filter_map(value_items)
            .map(|p| entry(&p[0], &p[1]))
            .collect())
    } else {
        Ok(items
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| entry(&c[0], &c[1]))
            .collect())
    }
}

async fn read_zset(s: &Session, db: u8, key: &str, max: usize) -> Result<Read> {
    let mut p = pipe();
    p.cmd("ZCARD")
        .arg(key)
        .cmd("ZRANGE")
        .arg(key)
        .arg(0)
        .arg(max - 1)
        .arg("WITHSCORES");
    let r = s.pipeline(db, &p, None).await?;
    let card = int_at(&r, 0, "ZCARD")?.max(0) as u64;
    let members = zset_members(
        r.get(1)
            .ok_or_else(|| CoreError::Valkey("missing ZRANGE reply".into()))?,
    )?;
    Ok((
        serde_json::Value::Array(members),
        Some(card),
        card as usize > max,
    ))
}

async fn read_stream(s: &Session, db: u8, key: &str, max: usize) -> Result<Read> {
    let mut p = pipe();
    p.cmd("XLEN")
        .arg(key)
        .cmd("XRANGE")
        .arg(key)
        .arg("-")
        .arg("+")
        .arg("COUNT")
        .arg(max);
    let r = s.pipeline(db, &p, None).await?;
    let len = int_at(&r, 0, "XLEN")?.max(0) as u64;
    let entries = r
        .get(1)
        .and_then(value_items)
        .ok_or_else(|| CoreError::Valkey("unexpected reply to XRANGE".into()))?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let parts = value_items(e).ok_or_else(|| unexpected("XRANGE", e))?;
        let [id, fields] = parts else {
            return Err(unexpected("XRANGE", e));
        };
        out.push(json!({
            "id": value_text(id).unwrap_or_default(),
            "fields": pairs_to_object(pairs_from_value(fields)),
        }));
    }
    Ok((serde_json::Value::Array(out), Some(len), len as usize > max))
}

async fn read_json(s: &Session, db: u8, key: &str) -> Result<Read> {
    let reply = s
        .exec(db, Cmd::new().arg("JSON.GET").arg(key), None, true)
        .await?;
    let text = value_bytes(&reply).ok_or_else(|| unexpected("JSON.GET", &reply))?;
    let size = text.len() as u64;
    // A pathological document is cut as text rather than parsed (and sent over IPC) in full.
    if text.len() > MAX_STRING_BYTES * 4 {
        let cut = String::from_utf8_lossy(&text[..MAX_STRING_BYTES]).into_owned();
        return Ok((serde_json::Value::String(cut), Some(size), true));
    }
    match serde_json::from_slice(&text) {
        Ok(v) => Ok((v, Some(size), false)),
        Err(_) => Ok((bytes_to_json(&text), Some(size), false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    #[test]
    fn zset_resp3_nested_pairs() {
        let reply = Value::Array(vec![
            Value::Array(vec![bulk("a"), Value::Double(1.0)]),
            Value::Array(vec![bulk("b"), Value::Double(2.5)]),
        ]);
        assert_eq!(
            zset_members(&reply).unwrap(),
            vec![
                json!({"member": "a", "score": 1.0}),
                json!({"member": "b", "score": 2.5})
            ]
        );
    }

    #[test]
    fn zset_resp2_flat_pairs() {
        let reply = Value::Array(vec![bulk("a"), bulk("1"), bulk("b"), bulk("2.5")]);
        assert_eq!(
            zset_members(&reply).unwrap(),
            vec![
                json!({"member": "a", "score": 1.0}),
                json!({"member": "b", "score": 2.5})
            ]
        );
    }

    #[test]
    fn zset_empty_and_infinite_scores() {
        assert!(zset_members(&Value::Array(vec![])).unwrap().is_empty());
        let reply = Value::Array(vec![bulk("m"), bulk("inf")]);
        assert_eq!(
            zset_members(&reply).unwrap(),
            vec![json!({"member": "m", "score": "inf"})]
        );
    }

    #[test]
    fn binary_zset_member_is_wrapped() {
        let reply = Value::Array(vec![Value::Array(vec![
            Value::BulkString(vec![0xff, 0x00]),
            Value::Double(3.0),
        ])]);
        let m = zset_members(&reply).unwrap();
        assert_eq!(m[0]["member"]["$bytes"], "/wA=");
    }

    #[test]
    fn hash_object_with_binary_field_and_value() {
        let pairs = vec![
            (b"f".to_vec(), bulk("v")),
            (vec![0xfe, 0xff], Value::BulkString(vec![0x00, 0xff])),
        ];
        let obj = pairs_to_object(pairs);
        assert_eq!(obj["f"], "v");
        assert_eq!(obj["base64:/v8="]["len"], 2);
    }
}
