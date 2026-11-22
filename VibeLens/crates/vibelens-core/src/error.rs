//! Error type shared by every core module. Serialized to the UI as [`ErrorPayload`].

use serde::Serialize;

use crate::model::PresentedHostKey;

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("not connected: {0}")]
    NotConnected(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("AWS: {0}")]
    Aws(String),
    #[error("database: {0}")]
    Database(String),
    #[error("valkey: {0}")]
    Valkey(String),
    #[error("tunnel: {0}")]
    Tunnel(String),
    /// The SSH host has no pinned key, so the connection stopped after the key exchange and
    /// before any authentication. Only a user action in the UI may pin the key.
    #[error(
        "the SSH host key of {}:{} has not been trusted yet. The server presented {}. \
         Check that against the server's real key, then trust it to connect",
        .0.host, .0.port, .0.fingerprint
    )]
    UnknownHostKey(PresentedHostKey),
    #[error("vault: {0}")]
    Vault(String),
    #[error("LLM provider: {0}")]
    Llm(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("blocked by policy: {0}")]
    PolicyDenied(String),
    #[error("{0}")]
    Other(String),
}

impl CoreError {
    /// Stable machine-readable code for the UI (`snake_case`).
    pub fn code(&self) -> &'static str {
        match self {
            CoreError::NotImplemented(_) => "not_implemented",
            CoreError::NotFound(_) => "not_found",
            CoreError::Invalid(_) => "invalid",
            CoreError::NotConnected(_) => "not_connected",
            CoreError::Auth(_) => "auth",
            CoreError::Aws(_) => "aws",
            CoreError::Database(_) => "database",
            CoreError::Valkey(_) => "valkey",
            CoreError::Tunnel(_) => "tunnel",
            CoreError::UnknownHostKey(_) => "unknown_host_key",
            CoreError::Vault(_) => "vault",
            CoreError::Llm(_) => "llm",
            CoreError::Io(_) => "io",
            CoreError::Serde(_) => "serde",
            CoreError::Cancelled => "cancelled",
            CoreError::Timeout(_) => "timeout",
            CoreError::PolicyDenied(_) => "policy_denied",
            CoreError::Other(_) => "other",
        }
    }
}

/// What crosses the IPC boundary. Never put secrets in `message`.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
    /// With code `unknown_host_key`: the key the server presented, for the trust dialog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_key: Option<PresentedHostKey>,
}

impl ErrorPayload {
    /// A payload that is only a code and a message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        ErrorPayload {
            code: code.into(),
            message: message.into(),
            host_key: None,
        }
    }
}

impl From<CoreError> for ErrorPayload {
    fn from(e: CoreError) -> Self {
        ErrorPayload::from(&e)
    }
}

impl From<&CoreError> for ErrorPayload {
    fn from(e: &CoreError) -> Self {
        ErrorPayload {
            code: e.code().to_string(),
            message: e.to_string(),
            host_key: match e {
                CoreError::UnknownHostKey(key) => Some(key.clone()),
                _ => None,
            },
        }
    }
}
