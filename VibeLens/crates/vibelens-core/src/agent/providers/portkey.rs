//! Portkey AI Gateway.
//!
//! Portkey is not a wire format of its own: it accepts OpenAI Chat Completions (and Anthropic
//! Messages) and routes each request to the real provider. What differs from a plain
//! OpenAI-compatible endpoint is authentication and routing, which travel in headers:
//!
//! * `x-portkey-api-key`: the Portkey API key (the provider's keychain secret). `Authorization`
//!   is deliberately not sent: Portkey would treat it as a provider credential.
//! * one routing header: `x-portkey-provider` (Model Catalog slug), `x-portkey-virtual-key`
//!   (legacy), `x-portkey-config` (saved config id or inline JSON), or none at all when the model
//!   id itself carries the catalog slug (`@bedrock-prod/anthropic.claude-...`).
//! * `x-portkey-metadata` with `_user`, so requests are attributable in Portkey's logs.
//!
//! A self-hosted gateway (such as the one `capitec/pg-proxy` points `PORTKEY_BASE_URL` at) works
//! the same way: set the base URL, and turn "Requires an API key" off if it has no auth.

use serde_json::json;

use crate::error::{CoreError, Result};
use crate::model::{PortkeyApiFormat, PortkeyOptions, PortkeyRouting, ProviderConfig};

pub(crate) const DEFAULT_BASE_URL: &str = "https://api.portkey.ai/v1";

/// The Portkey headers for one request (auth, routing, metadata).
pub(super) fn headers(
    opts: Option<&PortkeyOptions>,
    api_key: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(k) = api_key {
        out.push(("x-portkey-api-key", k.to_string()));
    }
    let default = PortkeyOptions::default();
    let opts = opts.unwrap_or(&default);
    match &opts.routing {
        PortkeyRouting::ModelCatalog => {}
        PortkeyRouting::Provider { slug } => {
            let slug = slug.trim();
            // Portkey accepts both `@slug` (catalog) and bare provider names (`openai`).
            out.push(("x-portkey-provider", slug.to_string()));
        }
        PortkeyRouting::VirtualKey { virtual_key } => {
            out.push(("x-portkey-virtual-key", virtual_key.trim().to_string()));
        }
        PortkeyRouting::Config { config } => {
            // Inline JSON is sent compacted so it is a valid single-line header value.
            let c = config.trim();
            let value = serde_json::from_str::<serde_json::Value>(c)
                .ok()
                .filter(serde_json::Value::is_object)
                .map_or_else(|| c.to_string(), |v| v.to_string());
            out.push(("x-portkey-config", value));
        }
    }
    let user = opts
        .user
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .unwrap_or("vibelens");
    out.push((
        "x-portkey-metadata",
        json!({ "_user": user, "app": "VibeLens" }).to_string(),
    ));
    out
}

/// The wire format this config speaks to the gateway.
pub(super) fn api_format(cfg: &ProviderConfig) -> PortkeyApiFormat {
    cfg.portkey
        .as_ref()
        .map(|o| o.api_format)
        .unwrap_or_default()
}

/// URL of the Anthropic Messages route. Portkey's base URL conventionally ends in `/v1` (the
/// OpenAI SDK convention); the Messages route is `/v1/messages` under the gateway root.
pub(super) fn messages_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let root = base.strip_suffix("/v1").unwrap_or(base);
    format!("{root}/v1/messages")
}

/// URL of the Chat Completions route; tolerates a base URL given without `/v1`.
pub(super) fn chat_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/chat/completions")
    } else {
        format!("{base}/v1/chat/completions")
    }
}

/// Rejects Portkey configs that cannot route anywhere.
pub(crate) fn validate(cfg: &ProviderConfig) -> Result<()> {
    let bad = |m: &str| Err(CoreError::Invalid(m.to_string()));
    let default = PortkeyOptions::default();
    let opts = cfg.portkey.as_ref().unwrap_or(&default);
    match &opts.routing {
        PortkeyRouting::ModelCatalog => {
            let m = cfg.model.trim();
            if !m.is_empty() && !m.starts_with('@') {
                return bad(
                    "with Model Catalog routing the model must name its provider: @provider-slug/model-id \
                     (or pick Provider, Virtual key or Config routing)",
                );
            }
        }
        PortkeyRouting::Provider { slug } => {
            if slug.trim().is_empty() {
                return bad("enter the Portkey provider slug, e.g. @bedrock-prod");
            }
        }
        PortkeyRouting::VirtualKey { virtual_key } => {
            if virtual_key.trim().is_empty() {
                return bad("enter the Portkey virtual key");
            }
        }
        PortkeyRouting::Config { config } => {
            let c = config.trim();
            if c.is_empty() {
                return bad("enter a Portkey config id (pc-...) or inline config JSON");
            }
            if c.starts_with('{') && serde_json::from_str::<serde_json::Value>(c).is_err() {
                return bad("the inline Portkey config is not valid JSON");
            }
            // Inline configs can carry provider keys ("api_key"); providers.json is not the vault.
            if c.starts_with('{') && c.contains("\"api_key\"") {
                return bad(
                    "the inline Portkey config contains an api_key: save it as a config in Portkey and use its id instead",
                );
            }
        }
    }
    for (name, value) in headers(Some(opts), None) {
        if reqwest::header::HeaderValue::from_str(&value).is_err() {
            return Err(CoreError::Invalid(format!(
                "the Portkey setting for {name} contains characters that cannot be sent in a header"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ProviderKind;

    fn cfg(routing: PortkeyRouting, model: &str) -> ProviderConfig {
        ProviderConfig {
            id: "portkey".into(),
            label: "Portkey".into(),
            kind: ProviderKind::Portkey,
            base_url: DEFAULT_BASE_URL.into(),
            model: model.into(),
            max_tokens: 1024,
            temperature: None,
            extra_headers: Default::default(),
            requires_api_key: true,
            portkey: Some(PortkeyOptions {
                routing,
                ..Default::default()
            }),
        }
    }

    fn header<'a>(h: &'a [(&'static str, String)], name: &str) -> Option<&'a str> {
        h.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn api_key_goes_in_the_portkey_header_never_authorization() {
        let h = headers(None, Some("pk-secret"));
        assert_eq!(header(&h, "x-portkey-api-key"), Some("pk-secret"));
        assert!(header(&h, "authorization").is_none());
        assert!(header(&h, "x-portkey-provider").is_none());
        let meta: serde_json::Value =
            serde_json::from_str(header(&h, "x-portkey-metadata").expect("metadata"))
                .expect("json");
        assert_eq!(meta["_user"], "vibelens");
    }

    #[test]
    fn routing_headers() {
        let o = |routing| PortkeyOptions {
            routing,
            ..Default::default()
        };
        let h = headers(
            Some(&o(PortkeyRouting::Provider {
                slug: " @bedrock-prod ".into(),
            })),
            None,
        );
        assert_eq!(header(&h, "x-portkey-provider"), Some("@bedrock-prod"));
        assert!(header(&h, "x-portkey-api-key").is_none());

        let h = headers(
            Some(&o(PortkeyRouting::VirtualKey {
                virtual_key: "vk-1".into(),
            })),
            None,
        );
        assert_eq!(header(&h, "x-portkey-virtual-key"), Some("vk-1"));

        let h = headers(
            Some(&o(PortkeyRouting::Config {
                config: "{\n  \"retry\": {\"attempts\": 3}\n}".into(),
            })),
            None,
        );
        assert_eq!(
            header(&h, "x-portkey-config"),
            Some(r#"{"retry":{"attempts":3}}"#)
        );
        let h = headers(
            Some(&o(PortkeyRouting::Config {
                config: "pc-abc".into(),
            })),
            None,
        );
        assert_eq!(header(&h, "x-portkey-config"), Some("pc-abc"));
    }

    #[test]
    fn urls() {
        assert_eq!(
            chat_url("https://api.portkey.ai/v1/"),
            "https://api.portkey.ai/v1/chat/completions"
        );
        assert_eq!(
            chat_url("https://gw.internal:8787"),
            "https://gw.internal:8787/v1/chat/completions"
        );
        assert_eq!(
            messages_url("https://api.portkey.ai/v1"),
            "https://api.portkey.ai/v1/messages"
        );
        assert_eq!(
            messages_url("https://api.portkey.ai"),
            "https://api.portkey.ai/v1/messages"
        );
    }

    #[test]
    fn validation() {
        assert!(
            validate(&cfg(
                PortkeyRouting::ModelCatalog,
                "@bedrock/anthropic.claude"
            ))
            .is_ok()
        );
        assert!(validate(&cfg(PortkeyRouting::ModelCatalog, "")).is_ok());
        assert!(validate(&cfg(PortkeyRouting::ModelCatalog, "gpt-5")).is_err());
        assert!(validate(&cfg(PortkeyRouting::Provider { slug: " ".into() }, "m")).is_err());
        assert!(
            validate(&cfg(
                PortkeyRouting::Config {
                    config: "{not json".into()
                },
                "m"
            ))
            .is_err()
        );
        assert!(
            validate(&cfg(
                PortkeyRouting::Config {
                    config: r#"{"provider":"openai","api_key":"sk-x"}"#.into()
                },
                "m"
            ))
            .is_err()
        );
        assert!(
            validate(&cfg(
                PortkeyRouting::Config {
                    config: "pc-123".into()
                },
                "m"
            ))
            .is_ok()
        );
    }
}
