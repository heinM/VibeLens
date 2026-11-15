use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValkeyServerInfo {
    pub version: Option<String>,
    /// "standalone" | "cluster" | "sentinel"
    pub mode: Option<String>,
    pub role: Option<String>,
    pub used_memory_human: Option<String>,
    pub extra: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValkeyDbInfo {
    pub index: u8,
    pub keys: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum KeyType {
    String,
    Hash,
    List,
    Set,
    Zset,
    Stream,
    Json,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyEntry {
    pub key: String,
    pub key_type: Option<KeyType>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScanPage {
    pub keys: Vec<KeyEntry>,
    /// Opaque; pass back unchanged. In cluster mode encodes node index + node cursor.
    pub cursor: String,
    pub done: bool,
}

/// Value encoding: UTF-8 strings as JSON strings, binary as `{"$bytes": "<base64>", "len": n}`.
/// Hash → object, List/Set → array, Zset → array of `{member, score}`, Stream → array of
/// `{id, fields}`, JSON module → parsed JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ValkeyValue {
    pub key: String,
    pub key_type: KeyType,
    /// -1 = no expiry.
    pub ttl_ms: i64,
    /// Element count (or byte length for strings).
    pub size: Option<u64>,
    pub value: serde_json::Value,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CommandOutcome {
    Ok {
        result: serde_json::Value,
        elapsed_ms: u64,
    },
    /// The command mutates data and `allow_write` was false. UI should confirm and resend.
    RequiresConfirmation { reason: String },
}
