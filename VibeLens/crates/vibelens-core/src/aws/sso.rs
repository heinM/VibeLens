//! IAM Identity Center (SSO): device-code login, token cache refresh, account/role listing and a
//! credentials provider backed by `sso:GetRoleCredentials`.
//!
//! The SDK never writes the CLI-compatible token cache, so this module does (see [`super::tokens`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use aws_config::{BehaviorVersion, Region};
use aws_credential_types::Credentials;
use aws_credential_types::provider::error::CredentialsError;
use aws_credential_types::provider::{ProvideCredentials, future};
use chrono::{DateTime, Utc};

use super::creds::{Clock, sdk_error, to_credentials_error};
use super::tokens::{self, SsoCacheEntry};
use crate::error::{CoreError, Result};
use crate::model::{DeviceLoginStart, SsoAccount, SsoRole, SsoSessionInfo};

const CLIENT_NAME: &str = "VibeLens";
const SCOPE: &str = "sso:account:access";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const SESSION_EXPIRED: &str = "SSO session expired — sign in again";
/// Tokens with less than this much life left are refreshed (when a refresh token exists).
const REFRESH_WINDOW_SECS: i64 = 300;
/// Re-register the OIDC client this long before its secret expires.
const REGISTRATION_MARGIN_SECS: i64 = 300;
/// Give up polling after this many consecutive transport failures.
const MAX_POLL_TRANSPORT_FAILURES: u32 = 5;

#[derive(Clone)]
struct Registration {
    client_id: String,
    client_secret: String,
    expires_at: DateTime<Utc>,
}

/// A device authorization awaiting user approval. Deliberately not `Debug` (holds secrets).
#[derive(Clone)]
struct Pending {
    start_url: String,
    region: String,
    registration: Registration,
    device_code: String,
    interval: Duration,
    deadline: DateTime<Utc>,
}

/// Shared SSO state. Held in an `Arc` by `AwsService` and by every [`SsoRoleProvider`].
pub struct SsoCore {
    cache_dir: Option<PathBuf>,
    endpoint_url: Option<String>,
    clock: Clock,
    poll_interval: Option<Duration>,
    oidc_clients: Mutex<HashMap<String, aws_sdk_ssooidc::Client>>,
    sso_clients: Mutex<HashMap<String, aws_sdk_sso::Client>>,
    registrations: Mutex<HashMap<String, Registration>>,
    pending: Mutex<HashMap<String, Pending>>,
    /// Refresh tokens can be single-use, so token refreshes are serialised.
    refresh_lock: tokio::sync::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl SsoCore {
    pub fn new(
        cache_dir: Option<PathBuf>,
        endpoint_url: Option<String>,
        clock: Clock,
        poll_interval: Option<Duration>,
    ) -> Self {
        Self {
            cache_dir,
            endpoint_url,
            clock,
            poll_interval,
            oidc_clients: Mutex::default(),
            sso_clients: Mutex::default(),
            registrations: Mutex::default(),
            pending: Mutex::default(),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    fn cache_dir(&self) -> Result<&PathBuf> {
        self.cache_dir.as_ref().ok_or_else(|| {
            CoreError::Other("cannot locate the AWS SSO cache: no home directory".into())
        })
    }

    fn oidc_client(&self, region: &str) -> aws_sdk_ssooidc::Client {
        let mut map = lock(&self.oidc_clients);
        map.entry(region.to_string())
            .or_insert_with(|| {
                let mut b = aws_sdk_ssooidc::Config::builder()
                    .behavior_version(BehaviorVersion::latest())
                    .region(Region::new(region.to_string()));
                if let Some(url) = &self.endpoint_url {
                    b = b.endpoint_url(url);
                }
                aws_sdk_ssooidc::Client::from_conf(b.build())
            })
            .clone()
    }

    fn sso_client(&self, region: &str) -> aws_sdk_sso::Client {
        let mut map = lock(&self.sso_clients);
        map.entry(region.to_string())
            .or_insert_with(|| {
                let mut b = aws_sdk_sso::Config::builder()
                    .behavior_version(BehaviorVersion::latest())
                    .region(Region::new(region.to_string()));
                if let Some(url) = &self.endpoint_url {
                    b = b.endpoint_url(url);
                }
                aws_sdk_sso::Client::from_conf(b.build())
            })
            .clone()
    }

    // -----------------------------------------------------------------------------------------
    // Device login
    // -----------------------------------------------------------------------------------------

    /// RegisterClient, cached per region until shortly before the client secret expires.
    async fn registration(&self, region: &str) -> Result<Registration> {
        let now = self.now();
        if let Some(r) = lock(&self.registrations).get(region)
            && r.expires_at - chrono::Duration::seconds(REGISTRATION_MARGIN_SECS) > now
        {
            return Ok(r.clone());
        }
        let out = self
            .oidc_client(region)
            .register_client()
            .client_name(CLIENT_NAME)
            .client_type("public")
            .scopes(SCOPE)
            .send()
            .await
            .map_err(|e| sdk_error("SSO RegisterClient", &e))?;
        let (Some(id), Some(secret)) = (out.client_id(), out.client_secret()) else {
            return Err(CoreError::Aws(
                "SSO RegisterClient returned no client credentials".into(),
            ));
        };
        let expires_at = DateTime::from_timestamp(out.client_secret_expires_at(), 0)
            .unwrap_or(now + chrono::Duration::days(80));
        let reg = Registration {
            client_id: id.to_string(),
            client_secret: secret.to_string(),
            expires_at,
        };
        lock(&self.registrations).insert(region.to_string(), reg.clone());
        Ok(reg)
    }

    /// Starts device authorization for `start_url`.
    pub async fn start(&self, start_url: &str, region: &str) -> Result<DeviceLoginStart> {
        let start_url = start_url.trim();
        let region = region.trim();
        if !start_url.starts_with("https://") || start_url.len() <= "https://".len() {
            return Err(CoreError::Invalid(
                "SSO start URL must look like https://d-xxxxxxxxxx.awsapps.com/start".into(),
            ));
        }
        if region.is_empty() {
            return Err(CoreError::Invalid("SSO region is required".into()));
        }
        let now = self.now();
        lock(&self.pending).retain(|_, p| p.deadline > now);

        let reg = self.registration(region).await?;
        let out = self
            .oidc_client(region)
            .start_device_authorization()
            .client_id(&reg.client_id)
            .client_secret(&reg.client_secret)
            .start_url(start_url)
            .send()
            .await
            .map_err(|e| {
                // A stale/invalid registration should not stick around.
                lock(&self.registrations).remove(region);
                sdk_error("SSO StartDeviceAuthorization", &e)
            })?;
        let (Some(device_code), Some(user_code), Some(verification_uri)) =
            (out.device_code(), out.user_code(), out.verification_uri())
        else {
            return Err(CoreError::Aws(
                "SSO StartDeviceAuthorization returned an incomplete response".into(),
            ));
        };
        let expires_in = i64::from(out.expires_in()).max(1);
        let interval_secs = u64::try_from(out.interval())
            .ok()
            .filter(|i| *i > 0)
            .unwrap_or(5);
        let expires_at = now + chrono::Duration::seconds(expires_in);

        let login_id = uuid::Uuid::new_v4().to_string();
        lock(&self.pending).insert(
            login_id.clone(),
            Pending {
                start_url: start_url.to_string(),
                region: region.to_string(),
                registration: reg,
                device_code: device_code.to_string(),
                interval: Duration::from_secs(interval_secs),
                deadline: expires_at,
            },
        );
        Ok(DeviceLoginStart {
            login_id,
            user_code: user_code.to_string(),
            verification_uri: verification_uri.to_string(),
            verification_uri_complete: out.verification_uri_complete().map(str::to_string),
            expires_at,
            interval_secs,
        })
    }

    /// Polls CreateToken until the user approves, then writes the CLI-compatible cache file.
    pub async fn complete(&self, login_id: &str) -> Result<SsoSessionInfo> {
        let pending = lock(&self.pending).get(login_id).cloned().ok_or_else(|| {
            CoreError::NotFound(
                "unknown or already completed SSO login; start sign-in again".into(),
            )
        })?;
        let cache_dir = self.cache_dir()?.clone();
        let client = self.oidc_client(&pending.region);

        let mut interval = self.poll_interval.unwrap_or(pending.interval);
        let mut transport_failures = 0u32;
        let token = loop {
            let now = self.now();
            if now >= pending.deadline {
                lock(&self.pending).remove(login_id);
                return Err(CoreError::Auth(
                    "SSO sign-in timed out before it was approved — start sign-in again".into(),
                ));
            }
            let result = client
                .create_token()
                .client_id(&pending.registration.client_id)
                .client_secret(&pending.registration.client_secret)
                .grant_type(DEVICE_GRANT)
                .device_code(&pending.device_code)
                .send()
                .await;
            match result {
                Ok(out) => break out,
                Err(e) => {
                    use aws_sdk_sso::error::ProvideErrorMetadata;
                    match e.code() {
                        Some("AuthorizationPendingException") => transport_failures = 0,
                        Some("SlowDownException") => {
                            transport_failures = 0;
                            // RFC 8628 §3.5: back off by 5 s (scaled when a test overrides the interval).
                            interval += self.poll_interval.unwrap_or(Duration::from_secs(5));
                        }
                        Some("ExpiredTokenException") | Some("InvalidGrantException") => {
                            lock(&self.pending).remove(login_id);
                            return Err(CoreError::Auth(
                                "SSO device code expired — start sign-in again".into(),
                            ));
                        }
                        Some("AccessDeniedException") => {
                            lock(&self.pending).remove(login_id);
                            return Err(CoreError::Auth("SSO sign-in was denied".into()));
                        }
                        _ if e.as_service_error().is_none() => {
                            transport_failures += 1;
                            if transport_failures >= MAX_POLL_TRANSPORT_FAILURES {
                                return Err(sdk_error("SSO CreateToken", &e));
                            }
                        }
                        _ => {
                            lock(&self.pending).remove(login_id);
                            return Err(sdk_error("SSO CreateToken", &e));
                        }
                    }
                }
            }
            let remaining = (pending.deadline - self.now()).to_std().unwrap_or_default();
            tokio::time::sleep(interval.min(remaining)).await;
        };

        let (Some(access_token), expires_in) = (token.access_token(), token.expires_in()) else {
            return Err(CoreError::Aws(
                "SSO CreateToken returned no access token".into(),
            ));
        };
        let now = self.now();
        let entry = SsoCacheEntry {
            start_url: pending.start_url.clone(),
            region: pending.region.clone(),
            access_token: access_token.to_string(),
            expires_at: Some(now + chrono::Duration::seconds(i64::from(expires_in).max(0))),
            client_id: Some(pending.registration.client_id.clone()),
            client_secret: Some(pending.registration.client_secret.clone()),
            registration_expires_at: Some(pending.registration.expires_at),
            refresh_token: token.refresh_token().map(str::to_string),
            extra: Default::default(),
        };
        let path = cache_dir.join(tokens::cache_file_name(&pending.start_url));
        tokens::write_entry(&path, &entry)?;
        lock(&self.pending).remove(login_id);
        Ok(entry.info())
    }

    // -----------------------------------------------------------------------------------------
    // Token cache
    // -----------------------------------------------------------------------------------------

    /// Returns a usable access token for `start_url` plus the region it was issued in, refreshing
    /// it first when it is close to expiry and a refresh token is available.
    pub async fn access_token(&self, start_url: &str) -> Result<(String, String)> {
        let dir = self.cache_dir()?.clone();
        let _guard = self.refresh_lock.lock().await;
        let Some(loaded) = tokens::find_for_start_url(&dir, start_url) else {
            return Err(CoreError::Auth(
                "not signed in to this SSO start URL — sign in first".into(),
            ));
        };
        let now = self.now();
        let entry = loaded.entry;
        let remaining = entry.expires_at.map(|t| t - now);
        let fresh = remaining.is_some_and(|r| r > chrono::Duration::seconds(REFRESH_WINDOW_SECS));
        if fresh {
            return Ok((entry.access_token, entry.region));
        }
        let still_valid = remaining.is_some_and(|r| r > chrono::Duration::zero());

        if entry.can_refresh(now) {
            match self.refresh(&entry).await {
                Ok(refreshed) => {
                    tokens::write_entry(&loaded.path, &refreshed)?;
                    return Ok((refreshed.access_token, refreshed.region));
                }
                Err(e) => {
                    tracing::debug!(error = %e, "SSO token refresh failed");
                    if !still_valid {
                        return Err(CoreError::Auth(SESSION_EXPIRED.into()));
                    }
                }
            }
        }
        if still_valid {
            Ok((entry.access_token, entry.region))
        } else {
            Err(CoreError::Auth(SESSION_EXPIRED.into()))
        }
    }

    /// CreateToken with the `refresh_token` grant. Keeps the old refresh token if none is returned.
    async fn refresh(&self, entry: &SsoCacheEntry) -> Result<SsoCacheEntry> {
        let (Some(client_id), Some(client_secret), Some(refresh_token)) =
            (&entry.client_id, &entry.client_secret, &entry.refresh_token)
        else {
            return Err(CoreError::Auth(SESSION_EXPIRED.into()));
        };
        let out = self
            .oidc_client(&entry.region)
            .create_token()
            .client_id(client_id)
            .client_secret(client_secret)
            .grant_type("refresh_token")
            .refresh_token(refresh_token)
            .send()
            .await
            .map_err(|e| sdk_error("SSO CreateToken (refresh)", &e))?;
        let Some(access_token) = out.access_token() else {
            return Err(CoreError::Aws(
                "SSO token refresh returned no access token".into(),
            ));
        };
        let mut next = entry.clone();
        next.access_token = access_token.to_string();
        next.expires_at =
            Some(self.now() + chrono::Duration::seconds(i64::from(out.expires_in()).max(0)));
        if let Some(rt) = out.refresh_token() {
            next.refresh_token = Some(rt.to_string());
        }
        Ok(next)
    }

    // -----------------------------------------------------------------------------------------
    // Portal API
    // -----------------------------------------------------------------------------------------

    /// ListAccounts (all pages), sorted by name then id.
    pub async fn list_accounts(&self, start_url: &str) -> Result<Vec<SsoAccount>> {
        let (token, region) = self.access_token(start_url).await?;
        let mut items = self
            .sso_client(&region)
            .list_accounts()
            .access_token(token)
            .into_paginator()
            .items()
            .send();
        let mut out = Vec::new();
        while let Some(item) = items.next().await {
            let a = item.map_err(|e| portal_error("SSO ListAccounts", &e))?;
            let Some(id) = a.account_id() else { continue };
            out.push(SsoAccount {
                account_id: id.to_string(),
                account_name: a.account_name().map(str::to_string),
                email: a.email_address().map(str::to_string),
            });
        }
        out.sort_by(|a, b| (&a.account_name, &a.account_id).cmp(&(&b.account_name, &b.account_id)));
        Ok(out)
    }

    /// ListAccountRoles (all pages), sorted by role name.
    pub async fn list_roles(&self, start_url: &str, account_id: &str) -> Result<Vec<SsoRole>> {
        let (token, region) = self.access_token(start_url).await?;
        let mut items = self
            .sso_client(&region)
            .list_account_roles()
            .access_token(token)
            .account_id(account_id)
            .into_paginator()
            .items()
            .send();
        let mut out = Vec::new();
        while let Some(item) = items.next().await {
            let r = item.map_err(|e| portal_error("SSO ListAccountRoles", &e))?;
            let Some(name) = r.role_name() else { continue };
            out.push(SsoRole {
                account_id: r.account_id().unwrap_or(account_id).to_string(),
                role_name: name.to_string(),
            });
        }
        out.sort_by(|a, b| a.role_name.cmp(&b.role_name));
        Ok(out)
    }

    /// GetRoleCredentials for one account/role using the cached (and possibly refreshed) token.
    pub async fn role_credentials(
        &self,
        start_url: &str,
        account_id: &str,
        role_name: &str,
    ) -> Result<Credentials> {
        let (token, region) = self.access_token(start_url).await?;
        let out = self
            .sso_client(&region)
            .get_role_credentials()
            .access_token(token)
            .account_id(account_id)
            .role_name(role_name)
            .send()
            .await
            .map_err(|e| portal_error("SSO GetRoleCredentials", &e))?;
        let Some(rc) = out.role_credentials() else {
            return Err(CoreError::Aws(
                "SSO GetRoleCredentials returned no credentials".into(),
            ));
        };
        let (Some(akid), Some(secret)) = (rc.access_key_id(), rc.secret_access_key()) else {
            return Err(CoreError::Aws(
                "SSO GetRoleCredentials returned incomplete credentials".into(),
            ));
        };
        let expiry = u64::try_from(rc.expiration())
            .ok()
            .map(|ms| UNIX_EPOCH + Duration::from_millis(ms));
        Ok(Credentials::new(
            akid,
            secret,
            rc.session_token().map(str::to_string),
            expiry,
            "VibeLensSso",
        ))
    }
}

/// Like [`sdk_error`], but an `UnauthorizedException` (revoked or invalid token) becomes
/// `CoreError::Auth` so the UI can prompt for sign-in.
fn portal_error<E, R>(op: &str, err: &aws_sdk_sso::error::SdkError<E, R>) -> CoreError
where
    E: std::error::Error + aws_sdk_sso::error::ProvideErrorMetadata + 'static,
    R: std::fmt::Debug,
{
    use aws_sdk_sso::error::ProvideErrorMetadata;
    if err.code() == Some("UnauthorizedException") {
        return CoreError::Auth(SESSION_EXPIRED.into());
    }
    sdk_error(op, err)
}

/// `ProvideCredentials` for a VibeLens-managed Identity Center login (`AwsIdentity::Sso`).
#[derive(Clone)]
pub struct SsoRoleProvider {
    core: Arc<SsoCore>,
    start_url: String,
    account_id: String,
    role_name: String,
}

impl std::fmt::Debug for SsoRoleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SsoRoleProvider")
            .field("account_id", &self.account_id)
            .field("role_name", &self.role_name)
            .finish_non_exhaustive()
    }
}

impl SsoRoleProvider {
    pub fn new(
        core: Arc<SsoCore>,
        start_url: String,
        account_id: String,
        role_name: String,
    ) -> Self {
        Self {
            core,
            start_url,
            account_id,
            role_name,
        }
    }
}

impl ProvideCredentials for SsoRoleProvider {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        future::ProvideCredentials::new(async move {
            self.core
                .role_credentials(&self.start_url, &self.account_id, &self.role_name)
                .await
                .map_err(|e: CoreError| -> CredentialsError { to_credentials_error(e) })
        })
    }
}
