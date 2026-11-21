//! The HTTP client every Trino and Livy request goes through.
//!
//! What it guarantees, whatever the caller does:
//!
//! * **Only the configured endpoint gets the credential (R3).** A request whose URL does not
//!   have the scheme, host and port of the configured base URL is refused before it is built,
//!   and the `Authorization` header is added only after that check.
//! * **No redirects.** The client never follows one: the credential and the SQL would go
//!   along. A 3xx is an error naming where it pointed, without credentials, query or fragment.
//! * **TLS is verified (R4)** with the OS roots plus an optional PEM bundle. There is no switch
//!   to turn verification off.
//! * **Bounded (R6).** Response bodies past a limit (64 MiB) fail the request; error text is
//!   redacted and cut to a few KiB.
//! * **Secrets stay out of messages (R1).** Error text never contains the password, the token
//!   or the `Authorization` value, including `reqwest` errors (which can print the URL).

use std::time::Duration;

use base64::Engine as _;
use reqwest::header::{AUTHORIZATION, HeaderValue};
use secrecy::{ExposeSecret, SecretString};

use crate::error::{CoreError, Result};
use crate::model::HttpEngineAuth;

/// Largest response body read, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Server error text kept in a message, in bytes.
pub(crate) const MAX_ERROR_TEXT: usize = 4 * 1024;

/// Per-request and connection limits. The defaults are the production values; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct HttpLimits {
    pub connect_timeout: Duration,
    /// Whole request, body included.
    pub request_timeout: Duration,
    pub max_body_bytes: usize,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(60),
            max_body_bytes: MAX_BODY_BYTES,
        }
    }
}

/// A response that passed the redirect and size checks.
#[derive(Debug)]
pub(crate) struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Response {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Whether `to` is the endpoint `base` names: same scheme, host and port (default ports
/// resolved). Unlike the Iceberg REST client, an upgrade from http to https is not accepted:
/// a URL handed back by the server is either the configured endpoint or someone else.
pub(crate) fn same_endpoint(base: &url::Url, to: &url::Url) -> bool {
    base.scheme() == to.scheme()
        && base.host_str().map(str::to_ascii_lowercase)
            == to.host_str().map(str::to_ascii_lowercase)
        && base.port_or_known_default() == to.port_or_known_default()
}

/// `url` as `scheme://host[:port]/path`, for messages: no credentials, query or fragment.
pub(crate) fn shown(url: &url::Url) -> String {
    let mut u = url.clone();
    let _ = u.set_username("");
    let _ = u.set_password(None);
    u.set_query(None);
    u.set_fragment(None);
    u.to_string()
}

/// `text` cut to at most `max` bytes at a character boundary, with an ellipsis when cut.
pub(crate) fn bounded(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// `64 MiB`, or `1024 bytes` below a MiB.
fn size_text(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// `reqwest::Error` with its source chain flattened and the URL removed.
fn describe_reqwest_error(e: reqwest::Error) -> String {
    use std::error::Error as _;
    let timeout = e.is_timeout();
    let e = e.without_url();
    let mut msg = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        let part = s.to_string();
        if !msg.contains(&part) {
            msg.push_str(": ");
            msg.push_str(&part);
        }
        src = s.source();
    }
    if timeout && !msg.contains("timed out") {
        msg.push_str(" (timed out)");
    }
    msg
}

/// Body limit for answers that carry no rows (status, submission, cancel, delete).
pub(crate) const SMALL_BODY: usize = 8 * 1024 * 1024;

/// Cheap to clone: the connection pool is shared.
#[derive(Clone)]
pub(crate) struct HttpClient {
    http: reqwest::Client,
    base: url::Url,
    /// `Authorization` value, marked sensitive so it is never printed by the HTTP stack.
    auth_header: Option<HeaderValue>,
    /// Strings that must never appear in a message: the secret and the header built from it.
    redact: Vec<String>,
    /// "Trino", "Livy".
    label: &'static str,
    limits: HttpLimits,
}

impl HttpClient {
    /// Builds the client for `base_url`. Runs the URL and cleartext rules again (the profile may
    /// have been edited by hand since it was saved) and refuses a missing secret.
    pub(crate) fn new(
        label: &'static str,
        base_url: &str,
        auth: &HttpEngineAuth,
        secret: Option<SecretString>,
        ca_cert_path: Option<&str>,
        limits: HttpLimits,
    ) -> Result<Self> {
        let what = format!("{label} URL");
        let base = super::config::parse_engine_url(base_url, &what)?;
        let secret = secret.filter(|s| !s.expose_secret().is_empty());
        let (auth_header, redact) = match auth {
            HttpEngineAuth::None => (None, Vec::new()),
            HttpEngineAuth::Basic { .. } | HttpEngineAuth::Bearer
                if crate::cleartext::is_cleartext_remote(&base) =>
            {
                return Err(CoreError::Invalid(format!(
                    "the {label} credential would be sent unencrypted to {}: use an https:// URL",
                    base.host_str().unwrap_or_default()
                )));
            }
            HttpEngineAuth::Basic { username } => {
                let Some(secret) = secret else {
                    return Err(CoreError::Auth(format!(
                        "no {label} password is stored for this connection: edit the connection \
                         and enter it"
                    )));
                };
                let pair = base64::engine::general_purpose::STANDARD
                    .encode(format!("{username}:{}", secret.expose_secret()));
                let value = format!("Basic {pair}");
                (
                    Some(sensitive(&value, label)?),
                    vec![secret.expose_secret().to_string(), pair, value],
                )
            }
            HttpEngineAuth::Bearer => {
                let Some(secret) = secret else {
                    return Err(CoreError::Auth(format!(
                        "no {label} token is stored for this connection: edit the connection and \
                         enter it"
                    )));
                };
                let value = format!("Bearer {}", secret.expose_secret().trim());
                (
                    Some(sensitive(&value, label)?),
                    vec![
                        secret.expose_secret().to_string(),
                        secret.expose_secret().trim().to_string(),
                        value,
                    ],
                )
            }
        };
        // Idempotent: an error only means a provider is already installed.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut builder = reqwest::Client::builder()
            .connect_timeout(limits.connect_timeout)
            .timeout(limits.request_timeout)
            .user_agent(concat!("VibeLens/", env!("CARGO_PKG_VERSION")))
            // Never follow a redirect: the Authorization header and the SQL would go along.
            // (No cookie store either: reqwest keeps none unless asked, so nothing a server sets
            // is carried into the next request.)
            .redirect(reqwest::redirect::Policy::none())
            .http1_only();
        // A system proxy must never carry the credential for a server on this machine (the
        // only place a credential may travel over plain http).
        if base
            .host_str()
            .is_some_and(crate::cleartext::is_loopback_host)
        {
            builder = builder.no_proxy();
        }
        if let Some(path) = ca_cert_path.map(str::trim).filter(|p| !p.is_empty()) {
            let pem = std::fs::read(path)
                .map_err(|e| CoreError::Invalid(format!("cannot read the CA file {path}: {e}")))?;
            let certs = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|e| CoreError::Invalid(format!("bad PEM in {path}: {e}")))?;
            if certs.is_empty() {
                return Err(CoreError::Invalid(format!(
                    "the CA file {path} holds no certificate"
                )));
            }
            builder = builder.tls_certs_merge(certs);
        }
        let http = builder
            .build()
            .map_err(|e| CoreError::Other(format!("could not set up the {label} client: {e}")))?;
        Ok(Self {
            http,
            base,
            auth_header,
            redact,
            label,
            limits,
        })
    }

    pub(crate) fn base(&self) -> &url::Url {
        &self.base
    }

    pub(crate) fn label(&self) -> &'static str {
        self.label
    }

    /// `path` (starting with `/`) under the base URL, keeping any path prefix the base has.
    pub(crate) fn url(&self, path: &str) -> Result<url::Url> {
        let joined = format!("{}{path}", self.base.as_str().trim_end_matches('/'));
        url::Url::parse(&joined)
            .map_err(|e| CoreError::Other(format!("{} URL for {path}: {e}", self.label)))
    }

    /// Removes the secret, in every form it was sent in, from `text`, and cuts it to
    /// [`MAX_ERROR_TEXT`].
    pub(crate) fn redact(&self, text: &str) -> String {
        let mut s = text.to_string();
        for r in &self.redact {
            if !r.is_empty() {
                s = s.replace(r.as_str(), "[redacted]");
            }
        }
        bounded(&s, MAX_ERROR_TEXT)
    }

    /// Sends one request. `url` must be on the configured endpoint ([`same_endpoint`]); the
    /// caller checks server-supplied URLs first to give a better message, this is the backstop.
    pub(crate) async fn send(
        &self,
        method: reqwest::Method,
        url: &url::Url,
        headers: &[(&str, String)],
        body: Option<Body>,
    ) -> Result<Response> {
        self.send_capped(method, url, headers, body, self.limits.max_body_bytes)
            .await
    }

    /// [`send`](Self::send) with a smaller body limit (`max_body`, never above the client's).
    pub(crate) async fn send_capped(
        &self,
        method: reqwest::Method,
        url: &url::Url,
        headers: &[(&str, String)],
        body: Option<Body>,
        max_body: usize,
    ) -> Result<Response> {
        let max_body = max_body.min(self.limits.max_body_bytes);
        if !same_endpoint(&self.base, url) {
            return Err(CoreError::Other(format!(
                "refused to send a {} request to {}, which is not the configured endpoint {}",
                self.label,
                shown(url),
                shown(&self.base)
            )));
        }
        let mut rb = self
            .http
            .request(method, url.as_str())
            .header("x-requested-by", "vibelens");
        for (k, v) in headers {
            rb = rb.header(*k, v.as_str());
        }
        rb = match body {
            Some(Body::Text(t)) => rb
                .header("content-type", "text/plain; charset=utf-8")
                .body(t),
            Some(Body::Json(v)) => rb
                .header("content-type", "application/json")
                .body(serde_json::to_vec(&v)?),
            None => rb,
        };
        // Last, after the endpoint check above.
        if let Some(h) = &self.auth_header {
            rb = rb.header(AUTHORIZATION, h.clone());
        }
        let mut resp = rb.send().await.map_err(|e| {
            CoreError::Other(format!(
                "could not reach {} at {}: {}",
                self.label,
                shown(&self.base),
                self.redact(&describe_reqwest_error(e))
            ))
        })?;
        let status = resp.status();
        if status.is_redirection() {
            let target = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|loc| url.join(loc.trim()).ok())
                .map(|t| format!(" to {}", self.redact(&shown(&t))))
                .unwrap_or_default();
            return Err(CoreError::Other(format!(
                "{} answered with a redirect (HTTP {}){target}. VibeLens does not follow \
                 redirects, because the credential and the SQL would be sent to that address. If \
                 it is the right endpoint, use it as the {} URL",
                self.label,
                status.as_u16(),
                self.label
            )));
        }
        let mut body = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > max_body {
                        return Err(CoreError::Other(format!(
                            "{} sent a response larger than {}; stopped reading it",
                            self.label,
                            size_text(max_body)
                        )));
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(CoreError::Other(format!(
                        "reading the {} response failed: {}",
                        self.label,
                        self.redact(&describe_reqwest_error(e))
                    )));
                }
            }
        }
        Ok(Response {
            status: status.as_u16(),
            body,
        })
    }

    /// The error for a response that is not a success and not one the caller handles itself:
    /// `Auth` for 401 and 403, otherwise `Other` with the server's text, redacted and bounded.
    pub(crate) fn status_error(&self, what: &str, resp: &Response) -> CoreError {
        let text = String::from_utf8_lossy(&resp.body);
        let text = self.redact(text.trim());
        match resp.status {
            401 | 403 => CoreError::Auth(format!(
                "{} refused the credentials (HTTP {}) for {what}{}",
                self.label,
                resp.status,
                if text.is_empty() {
                    String::new()
                } else {
                    format!(": {text}")
                }
            )),
            s => CoreError::Other(format!(
                "{} answered HTTP {s} to {what}{}",
                self.label,
                if text.is_empty() {
                    String::new()
                } else {
                    format!(": {text}")
                }
            )),
        }
    }
}

pub(crate) enum Body {
    Text(String),
    Json(serde_json::Value),
}

fn sensitive(value: &str, label: &str) -> Result<HeaderValue> {
    let mut h = HeaderValue::from_str(value).map_err(|_| {
        CoreError::Invalid(format!(
            "the {label} credential contains characters that cannot be sent in an HTTP header"
        ))
    })?;
    h.set_sensitive(true);
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn same_endpoint_is_scheme_host_and_port() {
        let base = u("https://trino.example:8443/");
        assert!(same_endpoint(
            &base,
            &u("https://trino.example:8443/v1/statement/x/1")
        ));
        assert!(same_endpoint(&base, &u("https://TRINO.example:8443/x")));
        assert!(!same_endpoint(&base, &u("https://trino.example:8444/x")));
        assert!(!same_endpoint(&base, &u("http://trino.example:8443/x")));
        assert!(!same_endpoint(&base, &u("https://evil.example:8443/x")));
        assert!(!same_endpoint(
            &base,
            &u("https://trino.example.evil:8443/x")
        ));
        let base = u("https://trino.example");
        assert!(same_endpoint(&base, &u("https://trino.example:443/x")));
        assert!(!same_endpoint(&base, &u("http://trino.example/x")));
    }

    #[test]
    fn shown_urls_carry_no_credentials_or_query() {
        assert_eq!(
            shown(&u("https://a:b@h.example:1/p?token=x#f")),
            "https://h.example:1/p"
        );
    }

    #[test]
    fn bounded_cuts_at_a_character_boundary() {
        assert_eq!(bounded("abc", 5), "abc");
        assert_eq!(bounded("ééé", 3), "é…");
    }

    #[test]
    fn secrets_are_redacted_in_every_form() {
        let c = HttpClient::new(
            "Trino",
            "https://t.example",
            &HttpEngineAuth::Basic {
                username: "ana".into(),
            },
            Some(SecretString::from("pw-1".to_string())),
            None,
            HttpLimits::default(),
        )
        .unwrap();
        let pair = base64::engine::general_purpose::STANDARD.encode("ana:pw-1");
        let m = c.redact(&format!("x pw-1 y Basic {pair} z"));
        assert!(!m.contains("pw-1") && !m.contains(&pair), "{m}");
    }

    #[test]
    fn a_missing_secret_or_a_cleartext_credential_is_refused() {
        let basic = HttpEngineAuth::Basic {
            username: "ana".into(),
        };
        let r = HttpClient::new(
            "Trino",
            "https://t.example",
            &basic,
            None,
            None,
            HttpLimits::default(),
        );
        assert!(matches!(r, Err(CoreError::Auth(_))));
        let r = HttpClient::new(
            "Trino",
            "http://t.example",
            &HttpEngineAuth::Bearer,
            Some(SecretString::from("tok".to_string())),
            None,
            HttpLimits::default(),
        );
        assert!(matches!(r, Err(CoreError::Invalid(_))));
        let r = HttpClient::new(
            "Trino",
            "https://t.example",
            &HttpEngineAuth::None,
            None,
            Some("/nonexistent/ca.pem"),
            HttpLimits::default(),
        );
        assert!(matches!(r, Err(CoreError::Invalid(_))));
    }
}
