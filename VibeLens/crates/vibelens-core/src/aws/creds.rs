//! Credentials plumbing: injectable clock, a caching `ProvideCredentials` wrapper, error mapping,
//! RDS IAM token generation and ElastiCache IAM token presigning.
//!
//! Note: unlike the SDK clients, which cache identities per client, we call providers directly
//! (to sign DB tokens), so we need our own cache; otherwise every connect would re-hit SSO/STS.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use aws_config::{Region, SdkConfig};
use aws_credential_types::Credentials;
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider, future};
use aws_sigv4::http_request::{
    self, SignableBody, SignableRequest, SignatureLocation, SigningSettings,
};
use aws_sigv4::sign::v4;
use aws_smithy_async::time::TimeSource;
use aws_smithy_runtime_api::client::identity::Identity;
use chrono::{DateTime, Utc};
use secrecy::SecretString;

use crate::error::{CoreError, Result};

/// Source of "now". Injected so tests can pin signatures and expiry maths.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// The wall clock.
pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

/// A [`Clock`] frozen at `t` (tests).
pub fn fixed_clock(t: DateTime<Utc>) -> Clock {
    Arc::new(move || t)
}

/// Adapts a [`Clock`] to the SDK's [`TimeSource`].
#[derive(Clone)]
pub struct ClockTimeSource(pub Clock);

impl fmt::Debug for ClockTimeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ClockTimeSource")
    }
}

impl TimeSource for ClockTimeSource {
    fn now(&self) -> SystemTime {
        SystemTime::from((self.0)())
    }
}

/// Lifetime of RDS / ElastiCache IAM tokens (both services cap at 900 s).
pub const TOKEN_TTL_SECS: u64 = 900;

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

/// Maps an SDK operation error to `CoreError::Aws("<op> failed: <Code>: <message>")`.
/// Falls back to the full error chain for transport errors that carry no service code.
/// Never includes request payloads or credentials.
pub fn sdk_error<E, R>(op: &str, err: &aws_sdk_sso::error::SdkError<E, R>) -> CoreError
where
    E: std::error::Error + aws_sdk_sso::error::ProvideErrorMetadata + 'static,
    R: fmt::Debug,
{
    use aws_sdk_sso::error::ProvideErrorMetadata;
    match (err.code(), err.message()) {
        (Some(code), Some(msg)) => CoreError::Aws(format!("{op} failed: {code}: {msg}")),
        (Some(code), None) => CoreError::Aws(format!("{op} failed: {code}")),
        _ => CoreError::Aws(format!(
            "{op} failed: {}",
            aws_sdk_sso::error::DisplayErrorContext(err)
        )),
    }
}

/// Converts a credentials-provider failure into a `CoreError`. If one of our own providers
/// (SSO) produced it, its `CoreError` (for example `Auth("SSO session expired …")`) is preserved.
pub fn credentials_error(err: CredentialsError) -> CoreError {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(&err);
    while let Some(e) = cur {
        if let Some(core) = e.downcast_ref::<CoreError>() {
            return match core {
                CoreError::Auth(m) => CoreError::Auth(m.clone()),
                other => CoreError::Aws(other.to_string()),
            };
        }
        cur = e.source();
    }
    CoreError::Aws(format!(
        "could not load AWS credentials: {}",
        aws_sdk_sso::error::DisplayErrorContext(&err)
    ))
}

/// Wraps a `CoreError` for returning from a `ProvideCredentials` implementation.
pub fn to_credentials_error(err: CoreError) -> CredentialsError {
    CredentialsError::provider_error(err)
}

// ---------------------------------------------------------------------------------------------
// Caching provider
// ---------------------------------------------------------------------------------------------

/// Caches the credentials of an inner provider until shortly before they expire.
/// Concurrent callers share one in-flight load. Credentials without an expiry are re-loaded
/// after `default_ttl` so rotated static keys are eventually picked up.
pub struct CachingProvider {
    inner: SharedCredentialsProvider,
    state: tokio::sync::Mutex<Option<Cached>>,
    clock: Clock,
    buffer: chrono::Duration,
    default_ttl: chrono::Duration,
    load_timeout: Duration,
}

struct Cached {
    creds: Credentials,
    valid_until: DateTime<Utc>,
}

impl fmt::Debug for CachingProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachingProvider").finish_non_exhaustive()
    }
}

impl CachingProvider {
    /// Refreshes 5 minutes before expiry, 15 minutes default lifetime, 30 s load timeout.
    pub fn new(inner: SharedCredentialsProvider, clock: Clock) -> Self {
        Self {
            inner,
            state: tokio::sync::Mutex::new(None),
            clock,
            buffer: chrono::Duration::minutes(5),
            default_ttl: chrono::Duration::minutes(15),
            load_timeout: Duration::from_secs(30),
        }
    }

    async fn load(&self) -> std::result::Result<Credentials, CredentialsError> {
        let mut guard = self.state.lock().await;
        let now = (self.clock)();
        if let Some(c) = guard.as_ref()
            && now + self.buffer < c.valid_until
        {
            return Ok(c.creds.clone());
        }
        let creds =
            match tokio::time::timeout(self.load_timeout, self.inner.provide_credentials()).await {
                Ok(r) => r?,
                Err(_) => {
                    return Err(CredentialsError::provider_error(CoreError::Timeout(
                        "loading AWS credentials".into(),
                    )));
                }
            };
        let valid_until = creds
            .expiry()
            .map(DateTime::<Utc>::from)
            .unwrap_or(now + self.default_ttl);
        *guard = Some(Cached {
            creds: creds.clone(),
            valid_until,
        });
        Ok(creds)
    }
}

impl ProvideCredentials for CachingProvider {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        future::ProvideCredentials::new(self.load())
    }
}

// ---------------------------------------------------------------------------------------------
// RDS
// ---------------------------------------------------------------------------------------------

/// Generates an RDS/Aurora IAM auth token (`host:port/?Action=connect&DBUser=…&X-Amz-…`), valid
/// 900 s. `host` must be the real RDS endpoint: the host is part of the signature.
pub async fn rds_auth_token(
    creds: Credentials,
    region: &str,
    host: &str,
    port: u16,
    username: &str,
    clock: Clock,
) -> Result<SecretString> {
    use aws_sdk_rds::auth_token::{AuthTokenGenerator, Config};
    if host.trim().is_empty() {
        return Err(CoreError::Invalid("RDS host is empty".into()));
    }
    if username.trim().is_empty() {
        return Err(CoreError::Invalid("RDS IAM username is empty".into()));
    }
    let config = Config::builder()
        .hostname(host)
        .port(u64::from(port))
        .region(Region::new(region.to_string()))
        .username(username)
        .credentials(creds)
        .expires_in(TOKEN_TTL_SECS)
        .build()
        .map_err(|e| CoreError::Invalid(format!("RDS token config: {e}")))?;
    let sdk_config = SdkConfig::builder()
        .time_source(ClockTimeSource(clock))
        .build();
    let token = AuthTokenGenerator::new(config)
        .auth_token(&sdk_config)
        .await
        .map_err(|e| CoreError::Aws(format!("could not sign RDS auth token: {e}")))?;
    Ok(SecretString::from(token.as_str().to_string()))
}

// ---------------------------------------------------------------------------------------------
// ElastiCache
// ---------------------------------------------------------------------------------------------

/// RFC 3986 unreserved characters stay; everything else becomes `%XX` (upper-case hex).
fn uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Builds an ElastiCache IAM auth token: a SigV4-presigned `connect` request (service
/// `elasticache`, 900 s, query-parameter signing, empty-body payload hash) with the `https://`
/// prefix stripped. Use it as the password for `AUTH <user> <token>`.
///
/// Pure function of its inputs (credentials and time are injected), so it is unit-testable.
pub fn elasticache_auth_token(
    creds: &Credentials,
    region: &str,
    cache_name: &str,
    username: &str,
    serverless: bool,
    now: DateTime<Utc>,
) -> Result<SecretString> {
    let cache = cache_name.trim();
    if cache.is_empty() {
        return Err(CoreError::Invalid("ElastiCache cache name is empty".into()));
    }
    if cache.contains("://")
        || cache.contains('/')
        || cache.contains('?')
        || cache.chars().any(char::is_whitespace)
    {
        return Err(CoreError::Invalid(
            "ElastiCache cache name must be the bare replication group id or serverless cache name"
                .into(),
        ));
    }
    if username.is_empty() {
        return Err(CoreError::Invalid(
            "ElastiCache IAM username is empty".into(),
        ));
    }

    let mut query = format!("Action=connect&User={}", uri_encode(username));
    if serverless {
        query.push_str("&ResourceType=ServerlessCache");
    }
    let url = format!("https://{cache}/?{query}");

    let mut settings = SigningSettings::default();
    settings.expires_in = Some(Duration::from_secs(TOKEN_TTL_SECS));
    settings.signature_location = SignatureLocation::QueryParams;

    let identity: Identity = creds.clone().into();
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name("elasticache")
        .time(SystemTime::from(now))
        .settings(settings)
        .build()
        .map_err(|e| {
            CoreError::Aws(format!(
                "could not build ElastiCache signing parameters: {e}"
            ))
        })?;

    let signable = SignableRequest::new("GET", &url, std::iter::empty(), SignableBody::empty())
        .map_err(|e| {
            CoreError::Invalid(format!("ElastiCache token request is not signable: {e}"))
        })?;
    let (instructions, _signature) = http_request::sign(signable, &params.into())
        .map_err(|e| CoreError::Aws(format!("could not sign ElastiCache auth token: {e}")))?
        .into_parts();

    let mut token = format!("{cache}/?{query}");
    for (name, value) in instructions.params() {
        token.push('&');
        token.push_str(&uri_encode(name));
        token.push('=');
        token.push_str(&uri_encode(value));
    }
    Ok(SecretString::from(token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use secrecy::ExposeSecret;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap()
    }

    #[test]
    fn uri_encode_matches_sigv4_rules() {
        assert_eq!(uri_encode("AZaz09-_.~"), "AZaz09-_.~");
        assert_eq!(uri_encode("a b/c+d=e@f"), "a%20b%2Fc%2Bd%3De%40f");
        assert_eq!(uri_encode("é"), "%C3%A9");
    }

    #[test]
    fn credentials_error_preserves_core_auth_errors() {
        let e = to_credentials_error(CoreError::Auth(
            "SSO session expired — sign in again".into(),
        ));
        match credentials_error(e) {
            CoreError::Auth(m) => assert!(m.contains("sign in again")),
            other => panic!("expected Auth, got {other:?}"),
        }
        let e = CredentialsError::not_loaded_no_source();
        assert!(matches!(credentials_error(e), CoreError::Aws(_)));
    }

    #[derive(Debug)]
    struct Counting(Arc<AtomicUsize>, Option<SystemTime>);
    impl ProvideCredentials for Counting {
        fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
        where
            Self: 'a,
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            let n = self.0.load(Ordering::SeqCst);
            future::ProvideCredentials::ready(Ok(Credentials::new(
                format!("AKID{n}"),
                "secret",
                None,
                self.1,
                "test",
            )))
        }
    }

    #[tokio::test]
    async fn caching_provider_reuses_until_buffer_before_expiry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let now = Arc::new(std::sync::Mutex::new(t0()));
        let clock: Clock = {
            let now = now.clone();
            Arc::new(move || *now.lock().unwrap())
        };
        let expiry = SystemTime::from(t0() + chrono::Duration::minutes(60));
        let p = CachingProvider::new(
            SharedCredentialsProvider::new(Counting(calls.clone(), Some(expiry))),
            clock,
        );

        assert_eq!(
            p.provide_credentials().await.unwrap().access_key_id(),
            "AKID1"
        );
        assert_eq!(
            p.provide_credentials().await.unwrap().access_key_id(),
            "AKID1"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        *now.lock().unwrap() = t0() + chrono::Duration::minutes(54);
        assert_eq!(
            p.provide_credentials().await.unwrap().access_key_id(),
            "AKID1",
            "still 6 min of validity"
        );
        *now.lock().unwrap() = t0() + chrono::Duration::minutes(56);
        assert_eq!(
            p.provide_credentials().await.unwrap().access_key_id(),
            "AKID2",
            "inside the 5 min buffer"
        );
    }

    #[tokio::test]
    async fn caching_provider_expires_credentials_without_expiry_after_default_ttl() {
        let calls = Arc::new(AtomicUsize::new(0));
        let now = Arc::new(std::sync::Mutex::new(t0()));
        let clock: Clock = {
            let now = now.clone();
            Arc::new(move || *now.lock().unwrap())
        };
        let p = CachingProvider::new(
            SharedCredentialsProvider::new(Counting(calls.clone(), None)),
            clock,
        );
        p.provide_credentials().await.unwrap();
        *now.lock().unwrap() = t0() + chrono::Duration::minutes(5);
        p.provide_credentials().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        *now.lock().unwrap() = t0() + chrono::Duration::minutes(12);
        p.provide_credentials().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    fn static_creds(token: Option<&str>) -> Credentials {
        Credentials::new(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            token.map(str::to_string),
            None,
            "test",
        )
    }

    fn params_of(token: &str) -> std::collections::BTreeMap<String, String> {
        let (_, q) = token.split_once("/?").expect("token has /?");
        q.split('&')
            .map(|kv| {
                let (k, v) = kv.split_once('=').expect("k=v");
                (k.to_string(), v.to_string())
            })
            .collect()
    }

    #[test]
    fn elasticache_token_structure() {
        let tok = elasticache_auth_token(
            &static_creds(None),
            "eu-west-1",
            "my-cache",
            "app-user",
            false,
            t0(),
        )
        .unwrap();
        let tok = tok.expose_secret();
        assert!(
            tok.starts_with("my-cache/?Action=connect&User=app-user&"),
            "{tok}"
        );
        assert!(!tok.contains("https://"));
        assert!(!tok.contains("ResourceType"));
        let p = params_of(tok);
        assert_eq!(p["Action"], "connect");
        assert_eq!(p["User"], "app-user");
        assert_eq!(p["X-Amz-Algorithm"], "AWS4-HMAC-SHA256");
        assert_eq!(
            p["X-Amz-Credential"],
            "AKIDEXAMPLE%2F20260304%2Feu-west-1%2Felasticache%2Faws4_request"
        );
        assert_eq!(p["X-Amz-Date"], "20260304T050607Z");
        assert_eq!(p["X-Amz-Expires"], "900");
        assert_eq!(p["X-Amz-SignedHeaders"], "host");
        assert_eq!(p["X-Amz-Signature"].len(), 64);
        assert!(
            p["X-Amz-Signature"]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert!(!p.contains_key("X-Amz-Security-Token"));
    }

    #[test]
    fn elasticache_token_is_deterministic_and_depends_on_inputs() {
        let a = elasticache_auth_token(&static_creds(None), "eu-west-1", "c", "u", false, t0())
            .unwrap();
        let b = elasticache_auth_token(&static_creds(None), "eu-west-1", "c", "u", false, t0())
            .unwrap();
        assert_eq!(a.expose_secret(), b.expose_secret());
        let later = elasticache_auth_token(
            &static_creds(None),
            "eu-west-1",
            "c",
            "u",
            false,
            t0() + chrono::Duration::seconds(1),
        )
        .unwrap();
        assert_ne!(a.expose_secret(), later.expose_secret());
        let other_region =
            elasticache_auth_token(&static_creds(None), "eu-central-1", "c", "u", false, t0())
                .unwrap();
        assert_ne!(
            params_of(a.expose_secret())["X-Amz-Signature"],
            params_of(other_region.expose_secret())["X-Amz-Signature"]
        );
    }

    #[test]
    fn elasticache_serverless_adds_resource_type_and_changes_signature() {
        let plain = elasticache_auth_token(
            &static_creds(None),
            "us-east-1",
            "sl-cache",
            "u",
            false,
            t0(),
        )
        .unwrap();
        let sl = elasticache_auth_token(
            &static_creds(None),
            "us-east-1",
            "sl-cache",
            "u",
            true,
            t0(),
        )
        .unwrap();
        let sl = sl.expose_secret();
        assert!(
            sl.starts_with("sl-cache/?Action=connect&User=u&ResourceType=ServerlessCache&X-Amz-"),
            "{sl}"
        );
        let p = params_of(sl);
        assert_eq!(p["ResourceType"], "ServerlessCache");
        assert_ne!(
            p["X-Amz-Signature"],
            params_of(plain.expose_secret())["X-Amz-Signature"],
            "ResourceType must be covered by the signature"
        );
    }

    #[test]
    fn elasticache_session_token_variant() {
        let tok = elasticache_auth_token(
            &static_creds(Some("FQoG/+session=token")),
            "eu-west-1",
            "c",
            "u",
            false,
            t0(),
        )
        .unwrap();
        let p = params_of(tok.expose_secret());
        assert_eq!(p["X-Amz-Security-Token"], "FQoG%2F%2Bsession%3Dtoken");
        assert_eq!(p["X-Amz-Signature"].len(), 64);
    }

    #[test]
    fn elasticache_encodes_username_and_rejects_bad_input() {
        let tok = elasticache_auth_token(
            &static_creds(None),
            "eu-west-1",
            "c",
            "my user@corp",
            false,
            t0(),
        )
        .unwrap();
        assert!(
            tok.expose_secret()
                .starts_with("c/?Action=connect&User=my%20user%40corp&X-Amz-")
        );
        assert!(
            elasticache_auth_token(&static_creds(None), "eu-west-1", "", "u", false, t0()).is_err()
        );
        assert!(
            elasticache_auth_token(
                &static_creds(None),
                "eu-west-1",
                "https://c",
                "u",
                false,
                t0()
            )
            .is_err()
        );
        assert!(
            elasticache_auth_token(&static_creds(None), "eu-west-1", "c", "", false, t0()).is_err()
        );
    }

    #[tokio::test]
    async fn rds_token_is_pure_signing_offline() {
        let clock = fixed_clock(t0());
        let tok = rds_auth_token(
            static_creds(None),
            "eu-west-1",
            "mydb.cluster-x.eu-west-1.rds.amazonaws.com",
            5432,
            "iam_user",
            clock.clone(),
        )
        .await
        .unwrap();
        let tok = tok.expose_secret();
        assert!(
            tok.starts_with(
                "mydb.cluster-x.eu-west-1.rds.amazonaws.com:5432/?Action=connect&DBUser=iam_user&"
            ),
            "{tok}"
        );
        let p = params_of(tok);
        assert_eq!(p["X-Amz-Algorithm"], "AWS4-HMAC-SHA256");
        assert!(
            p["X-Amz-Credential"].contains("%2Feu-west-1%2Frds-db%2Faws4_request"),
            "{}",
            p["X-Amz-Credential"]
        );
        assert_eq!(p["X-Amz-Date"], "20260304T050607Z");
        assert_eq!(p["X-Amz-Expires"], "900");
        assert_eq!(p["X-Amz-SignedHeaders"], "host");
        assert_eq!(p["X-Amz-Signature"].len(), 64);

        let again = rds_auth_token(
            static_creds(None),
            "eu-west-1",
            "mydb.cluster-x.eu-west-1.rds.amazonaws.com",
            5432,
            "iam_user",
            clock,
        )
        .await
        .unwrap();
        assert_eq!(
            tok,
            again.expose_secret(),
            "deterministic for a pinned clock"
        );

        let with_session = rds_auth_token(
            static_creds(Some("sess")),
            "eu-west-1",
            "h.rds.amazonaws.com",
            5432,
            "u",
            fixed_clock(t0()),
        )
        .await
        .unwrap();
        assert!(params_of(with_session.expose_secret()).contains_key("X-Amz-Security-Token"));
        assert!(
            rds_auth_token(
                static_creds(None),
                "eu-west-1",
                " ",
                5432,
                "u",
                fixed_clock(t0())
            )
            .await
            .is_err()
        );
    }
}
