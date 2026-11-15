use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::error::ErrorPayload;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    /// Native Anthropic Messages API (tool use + SSE).
    Anthropic,
    /// Any OpenAI Chat Completions–compatible endpoint: xAI Grok, Gemini's OpenAI-compatible
    /// endpoint, Ollama / LM Studio / vLLM (local), Azure-style gateways.
    OpenAiCompatible,
    /// Portkey AI Gateway (hosted `https://api.portkey.ai` or self-hosted): one Portkey API key,
    /// routing by Model Catalog slug / config / virtual key. Options in [`ProviderConfig::portkey`].
    Portkey,
}

/// Which wire format a Portkey provider speaks through the gateway.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub enum PortkeyApiFormat {
    /// `/v1/chat/completions`: works for every provider behind the gateway.
    #[default]
    ChatCompletions,
    /// `/v1/messages` (Anthropic Messages format): Claude on Anthropic, Bedrock or Vertex keeps
    /// native tool use and prompt caching end to end.
    AnthropicMessages,
}

/// How the gateway decides which provider serves a request. Exactly one applies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PortkeyRouting {
    /// The model id names the Model Catalog provider: `@bedrock-prod/anthropic.claude-…`.
    #[default]
    ModelCatalog,
    /// `x-portkey-provider`: a catalog slug (`@openai-prod`) or provider name.
    Provider { slug: String },
    /// `x-portkey-virtual-key` (older Portkey workspaces; what capitec/pg-proxy uses).
    VirtualKey { virtual_key: String },
    /// `x-portkey-config`: a saved config id (`pc-…`) or inline JSON (fallbacks, retries, caching).
    Config { config: String },
}

/// Portkey settings. The Portkey API key lives in the keychain like any API key and is sent as
/// `x-portkey-api-key` (never as `Authorization`, which Portkey would treat as a provider key).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PortkeyOptions {
    pub api_format: PortkeyApiFormat,
    pub routing: PortkeyRouting,
    /// `_user` in `x-portkey-metadata`, for per-user cost and audit in Portkey. Empty = `vibelens`.
    pub user: Option<String>,
}

/// Non-secret provider config, persisted as JSON. The API key lives in the keychain under
/// `vault_keys::llm_api_key(id)`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub id: String,
    pub label: String,
    pub kind: ProviderKind,
    pub base_url: String,
    /// Model id string exactly as the provider expects it. Never hard-coded in logic.
    pub model: String,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    #[serde(default)]
    pub extra_headers: BTreeMap<String, String>,
    /// Local endpoints (Ollama etc.) don't need a key.
    #[serde(default)]
    pub requires_api_key: bool,
    /// Routing options when `kind == Portkey`; ignored otherwise.
    #[serde(default)]
    pub portkey: Option<PortkeyOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub config: ProviderConfig,
    pub has_api_key: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OpenFile {
    pub path: String,
    pub contents: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentContext {
    pub profile_id: Option<String>,
    pub database: Option<String>,
    pub open_file: Option<OpenFile>,
    pub selected_text: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ToolRisk {
    /// Read-only and within the profile's AiDataAccess policy → runs without asking.
    Safe,
    /// Writes, DDL, unparseable SQL, file writes, or data egress under `ResultsWithApproval`.
    NeedsApproval,
    /// Never allowed (policy `None`, path escapes workspace, …). Agent gets an error result.
    Denied,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "decision",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ApprovalDecision {
    Approve,
    Reject { reason: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AgentEvent {
    TurnStarted {
        turn_id: String,
        provider_id: String,
        model: String,
    },
    TextDelta {
        text: String,
    },
    ToolCallProposed {
        call_id: String,
        tool: String,
        arguments: serde_json::Value,
        risk: ToolRisk,
        reason: Option<String>,
    },
    /// UI must show the approval overlay and call `agent_resolve_approval`.
    ApprovalRequired {
        call_id: String,
        tool: String,
        arguments: serde_json::Value,
        reason: String,
        /// Human-readable preview: the SQL, or a unified diff for file writes.
        preview: Option<String>,
    },
    ToolStarted {
        call_id: String,
    },
    ToolFinished {
        call_id: String,
        ok: bool,
        summary: String,
        elapsed_ms: u64,
    },
    /// The agent wrote a workspace file; UI should open/refresh that tab.
    FileWritten {
        path: String,
    },
    TurnFinished {
        stop_reason: String,
        usage: Option<Usage>,
    },
    Failed {
        error: ErrorPayload,
    },
}

/// Provider-neutral transcript used internally by the agent loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: Vec<ContentBlock>,
}
