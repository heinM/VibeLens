//! AWS identity (OWNER: aws agent).
//!
//! - `~/.aws/config` profiles are first-class: most enterprise users already run
//!   `aws sso login`; we reuse that via `aws-config` profile providers.
//! - Built-in Identity Center login: `aws-sdk-ssooidc` RegisterClient +
//!   StartDeviceAuthorization + CreateToken polling. Token is written to
//!   `~/.aws/sso/cache/<sha1(start_url)>.json` in AWS CLI–compatible format (this IS on disk —
//!   same as the CLI; see DESIGN.md "Zero-persistence, honestly").
//! - RDS IAM tokens via `aws_sdk_rds::auth_token::AuthTokenGenerator` (≤900s).
//! - ElastiCache IAM tokens: no SDK helper → presign `https://<cache>/?Action=connect&User=<u>`
//!   (+ `&ResourceType=ServerlessCache` when serverless) with `aws-sigv4`, service
//!   `elasticache`, 900s, then strip the scheme.
//! - Secrets Manager: GetSecretValue → parse RDS-style JSON; held as `SecretString`.
//!
//! Layout: [`profiles`] (INI parsing), [`tokens`] (CLI-compatible SSO cache), `sso` (device login,
//! portal API), [`creds`] (clock, caching provider, RDS/ElastiCache tokens), [`secrets`].
//!
//! Everything is testable offline: paths, clock, SDK endpoint and poll interval are injectable via
//! [`AwsServiceConfig`].

pub mod creds;
pub mod profiles;
pub mod secrets;
mod sso;
pub mod tokens;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use aws_config::default_provider::credentials::DefaultCredentialsChain;
use aws_config::profile::ProfileFileCredentialsProvider;
// `aws_runtime` is not a direct dependency; these aliases are the supported path to the same types.
#[allow(deprecated)]
use aws_config::profile::profile_file::{ProfileFileKind, ProfileFiles};
use aws_config::provider_config::ProviderConfig;
use aws_config::{BehaviorVersion, Region};
use aws_credential_types::Credentials;
use aws_credential_types::provider::{ProvideCredentials, SharedCredentialsProvider};
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};

use crate::error::{CoreError, Result};
use crate::model::*;
use crate::ports::{CredentialSource, SecretVault};

use creds::{CachingProvider, credentials_error, sdk_error};
pub use creds::{Clock, fixed_clock, system_clock};
pub use secrets::parse_db_secret;
use sso::{SsoCore, SsoRoleProvider};

/// Parsed Secrets Manager database secret (RDS-managed secret JSON shape).
#[derive(Debug, Clone)]
pub struct DbSecret {
    pub username: String,
    pub password: SecretString,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub dbname: Option<String>,
    pub engine: Option<String>,
}

/// How long IAM-token credentials are reported valid for new connections (tokens last 15 min;
/// one minute of slack so a token is never handed out at the edge of its life).
const TOKEN_VALID_FOR_NEW_CONNECTIONS: chrono::Duration = chrono::Duration::minutes(14);

/// Where [`AwsService`] reads and writes, plus test seams. [`AwsServiceConfig::from_env`] is what
/// the app uses; tests build one with explicit temp paths.
#[derive(Clone)]
pub struct AwsServiceConfig {
    /// Config file to list profiles from (`$AWS_CONFIG_FILE` or `~/.aws/config`).
    pub config_file: Option<PathBuf>,
    /// Credentials file to list static profiles from (`$AWS_SHARED_CREDENTIALS_FILE` or `~/.aws/credentials`).
    pub credentials_file: Option<PathBuf>,
    /// SSO token cache directory (`~/.aws/sso/cache`).
    pub sso_cache_dir: Option<PathBuf>,
    /// When true, SDK profile credential providers are pointed at `config_file`/`credentials_file`
    /// instead of the SDK's own discovery. `false` for the real app (SDK honours the env vars itself).
    pub pin_sdk_profile_files: bool,
    /// Overrides the endpoint of the SSO / SSO-OIDC / Secrets Manager clients (mock servers in tests).
    pub endpoint_url: Option<String>,
    /// Source of "now".
    pub clock: Clock,
    /// Overrides the device-login polling interval (and slow-down step). Tests only.
    pub poll_interval: Option<Duration>,
}

fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(p)
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| expand_tilde(v.trim()))
}

impl AwsServiceConfig {
    /// Real-world defaults: `$AWS_CONFIG_FILE`/`~/.aws/config`, `$AWS_SHARED_CREDENTIALS_FILE`/`~/.aws/credentials`,
    /// `~/.aws/sso/cache`, wall clock.
    pub fn from_env() -> Self {
        let home_aws = dirs::home_dir().map(|h| h.join(".aws"));
        Self {
            config_file: env_path("AWS_CONFIG_FILE")
                .or_else(|| home_aws.as_ref().map(|d| d.join("config"))),
            credentials_file: env_path("AWS_SHARED_CREDENTIALS_FILE")
                .or_else(|| home_aws.as_ref().map(|d| d.join("credentials"))),
            sso_cache_dir: home_aws.map(|d| d.join("sso").join("cache")),
            pin_sdk_profile_files: false,
            endpoint_url: None,
            clock: system_clock(),
            poll_interval: None,
        }
    }

    /// Fully explicit paths (never touches the real `~/.aws`); the SDK's profile providers are pinned to them too.
    pub fn with_paths(
        config_file: impl Into<PathBuf>,
        credentials_file: impl Into<PathBuf>,
        sso_cache_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            config_file: Some(config_file.into()),
            credentials_file: Some(credentials_file.into()),
            sso_cache_dir: Some(sso_cache_dir.into()),
            pin_sdk_profile_files: true,
            endpoint_url: None,
            clock: system_clock(),
            poll_interval: None,
        }
    }

    /// Replaces the clock.
    pub fn clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// Points the SSO / SSO-OIDC / Secrets Manager clients at `url`.
    pub fn endpoint_url(mut self, url: impl Into<String>) -> Self {
        self.endpoint_url = Some(url.into());
        self
    }

    /// Overrides the device-login poll interval.
    pub fn poll_interval(mut self, d: Duration) -> Self {
        self.poll_interval = Some(d);
        self
    }

    #[allow(deprecated)]
    fn sdk_profile_files(&self) -> Option<ProfileFiles> {
        if !self.pin_sdk_profile_files {
            return None;
        }
        let mut b = ProfileFiles::builder();
        let mut any = false;
        if let Some(p) = &self.config_file {
            b = b.with_file(ProfileFileKind::Config, p);
            any = true;
        }
        if let Some(p) = &self.credentials_file {
            b = b.with_file(ProfileFileKind::Credentials, p);
            any = true;
        }
        any.then(|| b.build())
    }
}

impl Default for AwsServiceConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

/// AWS identity service: profiles, Identity Center login, credentials and IAM/Secrets tokens.
pub struct AwsService {
    cfg: AwsServiceConfig,
    sso: Arc<SsoCore>,
    providers: Mutex<HashMap<String, SharedCredentialsProvider>>,
}

impl AwsService {
    /// The app's service: real `~/.aws` locations and wall clock.
    pub fn new() -> Arc<Self> {
        Self::with_config(AwsServiceConfig::from_env())
    }

    /// A service with explicit paths/clock/endpoint (tests, embedding).
    pub fn with_config(cfg: AwsServiceConfig) -> Arc<Self> {
        let sso = Arc::new(SsoCore::new(
            cfg.sso_cache_dir.clone(),
            cfg.endpoint_url.clone(),
            cfg.clock.clone(),
            cfg.poll_interval,
        ));
        Arc::new(Self {
            cfg,
            sso,
            providers: Mutex::new(HashMap::new()),
        })
    }

    /// The service's notion of "now".
    pub fn now(&self) -> DateTime<Utc> {
        (self.cfg.clock)()
    }

    /// Profiles from the AWS config and credentials files, sorted by name. Missing files give an empty list.
    pub async fn list_profiles(&self) -> Result<Vec<AwsProfileSummary>> {
        let config = self.cfg.config_file.clone();
        let credentials = self.cfg.credentials_file.clone();
        blocking(move || profiles::list_profiles_from(config.as_deref(), credentials.as_deref()))
            .await
    }

    /// Starts device authorization. Caller (UI shell) opens `verification_uri_complete`.
    pub async fn sso_start(&self, start_url: &str, region: &str) -> Result<DeviceLoginStart> {
        self.sso.start(start_url, region).await
    }

    /// Polls CreateToken until approved/expired; persists CLI-compatible cache file.
    pub async fn sso_complete(&self, login_id: &str) -> Result<SsoSessionInfo> {
        self.sso.complete(login_id).await
    }

    /// Sessions in the SSO token cache, including expired ones (the UI shows them as such).
    pub async fn sso_sessions(&self) -> Result<Vec<SsoSessionInfo>> {
        let dir = self.cfg.sso_cache_dir.clone();
        blocking(move || Ok(dir.as_deref().map(tokens::sessions).unwrap_or_default())).await
    }

    /// Accounts visible to the cached SSO session for `start_url`.
    pub async fn sso_list_accounts(&self, start_url: &str) -> Result<Vec<SsoAccount>> {
        self.sso.list_accounts(start_url).await
    }

    /// Roles available in `account_id` for the cached SSO session for `start_url`.
    pub async fn sso_list_roles(&self, start_url: &str, account_id: &str) -> Result<Vec<SsoRole>> {
        self.sso.list_roles(start_url, account_id).await
    }

    /// Cached, auto-refreshing credentials provider for a target. Providers are memoised per target,
    /// so repeated connects do not re-hit SSO/STS until the credentials near expiry.
    pub async fn credentials_provider(
        &self,
        target: &AwsTarget,
    ) -> Result<SharedCredentialsProvider> {
        let key = serde_json::to_string(target)?;
        if let Some(p) = lock(&self.providers).get(&key) {
            return Ok(p.clone());
        }
        let built = self.build_provider(target).await?;
        // A concurrent builder may have won the race; keep the first so everyone shares one cache.
        Ok(lock(&self.providers).entry(key).or_insert(built).clone())
    }

    /// Drops memoised credential providers (for example after the user edits their AWS config).
    pub fn invalidate_credentials(&self) {
        lock(&self.providers).clear();
    }

    async fn build_provider(&self, target: &AwsTarget) -> Result<SharedCredentialsProvider> {
        let region = target.region.trim();
        if region.is_empty() {
            return Err(CoreError::Invalid("AWS region is required".into()));
        }
        let inner = match &target.identity {
            AwsIdentity::Profile { profile_name } => {
                let name = profile_name.trim();
                if name.is_empty() {
                    return Err(CoreError::Invalid("AWS profile name is required".into()));
                }
                // The profile provider (not the default chain) is used on purpose: an explicitly
                // chosen profile must not be shadowed by AWS_ACCESS_KEY_ID in the environment.
                let conf =
                    ProviderConfig::default().with_region(Some(Region::new(region.to_string())));
                let mut b = ProfileFileCredentialsProvider::builder()
                    .configure(&conf)
                    .profile_name(name);
                if let Some(files) = self.cfg.sdk_profile_files() {
                    b = b.profile_files(files);
                }
                SharedCredentialsProvider::new(b.build())
            }
            AwsIdentity::Sso {
                start_url,
                account_id,
                role_name,
                ..
            } => SharedCredentialsProvider::new(SsoRoleProvider::new(
                self.sso.clone(),
                start_url.clone(),
                account_id.clone(),
                role_name.clone(),
            )),
            AwsIdentity::DefaultChain => {
                let chain = DefaultCredentialsChain::builder()
                    .region(Region::new(region.to_string()))
                    .build()
                    .await;
                SharedCredentialsProvider::new(chain)
            }
        };
        Ok(SharedCredentialsProvider::new(CachingProvider::new(
            inner,
            self.cfg.clock.clone(),
        )))
    }

    /// Resolves concrete credentials for a target, mapping failures to `CoreError`
    /// (`Auth` for an expired SSO session, `Aws` otherwise).
    pub async fn credentials(&self, target: &AwsTarget) -> Result<Credentials> {
        let provider = self.credentials_provider(target).await?;
        provider
            .provide_credentials()
            .await
            .map_err(credentials_error)
    }

    /// RDS/Aurora IAM auth token (15 min). `host` must be the real RDS endpoint (it is signed).
    pub async fn rds_auth_token(
        &self,
        target: &AwsTarget,
        host: &str,
        port: u16,
        username: &str,
    ) -> Result<SecretString> {
        let creds = self.credentials(target).await?;
        creds::rds_auth_token(
            creds,
            target.region.trim(),
            host,
            port,
            username,
            self.cfg.clock.clone(),
        )
        .await
    }

    /// ElastiCache IAM auth token (15 min), to be used as the password for `username`.
    pub async fn elasticache_auth_token(
        &self,
        target: &AwsTarget,
        cache_name: &str,
        username: &str,
        serverless: bool,
    ) -> Result<SecretString> {
        let creds = self.credentials(target).await?;
        creds::elasticache_auth_token(
            &creds,
            target.region.trim(),
            cache_name,
            username,
            serverless,
            self.now(),
        )
    }

    /// Fetches and parses an RDS-style database secret. The value never leaves this process
    /// boundary un-wrapped and is never logged.
    pub async fn get_db_secret(&self, target: &AwsTarget, secret_arn: &str) -> Result<DbSecret> {
        if secret_arn.trim().is_empty() {
            return Err(CoreError::Invalid("secret ARN is empty".into()));
        }
        let provider = self.credentials_provider(target).await?;
        // Resolve once up front (cached) so credential failures keep their precise error
        // (for example `Auth` for an expired SSO session) instead of a generic dispatch error.
        provider
            .provide_credentials()
            .await
            .map_err(credentials_error)?;
        let mut conf = aws_sdk_secretsmanager::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(target.region.trim().to_string()))
            .credentials_provider(provider);
        if let Some(url) = &self.cfg.endpoint_url {
            conf = conf.endpoint_url(url);
        }
        let client = aws_sdk_secretsmanager::Client::from_conf(conf.build());
        let out = client
            .get_secret_value()
            .secret_id(secret_arn.trim())
            .send()
            .await
            .map_err(|e| sdk_error("Secrets Manager GetSecretValue", &e))?;
        match out.secret_string() {
            Some(s) => parse_db_secret(s),
            None if out.secret_binary().is_some() => {
                Err(CoreError::Aws("secret is binary-only; VibeLens needs a JSON SecretString with username and password".into()))
            }
            None => Err(CoreError::Aws("secret has no value".into())),
        }
    }

    /// Path of the SSO cache directory in use, if any.
    pub fn sso_cache_dir(&self) -> Option<&Path> {
        self.cfg.sso_cache_dir.as_deref()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| CoreError::Other(format!("background task failed: {e}")))?
}

/// The app's [`CredentialSource`]: handles every [`AuthMethod`] (keychain passwords too).
pub struct AppCredentialSource {
    pub aws: Arc<AwsService>,
    pub vault: Arc<dyn SecretVault>,
    /// Output of access-token commands, reused until shortly before the token expires.
    pub tokens: crate::token::TokenCache,
}

impl AppCredentialSource {
    pub fn new(aws: Arc<AwsService>, vault: Arc<dyn SecretVault>) -> Arc<Self> {
        Arc::new(Self {
            aws,
            vault,
            tokens: crate::token::TokenCache::new(),
        })
    }
}

/// Startup user name for access-token auth when the profile leaves it empty.
const DEFAULT_TOKEN_USER: &str = "vibelens";

#[async_trait]
impl CredentialSource for AppCredentialSource {
    async fn resolve(&self, profile: &ConnectionProfile) -> Result<ResolvedCredentials> {
        match &profile.auth {
            AuthMethod::None => Ok(ResolvedCredentials::anonymous()),
            AuthMethod::Password { username } => {
                // A MySQL password is used only for the endpoint it was saved for.
                // A password bound to a MySQL endpoint is released only to that endpoint, whatever
                // engine the profile now names.
                let stored =
                    crate::mysql::secret::load_any_password(self.vault.as_ref(), profile).await?;
                let password = stored
                    .filter(|p| !p.expose_secret().is_empty())
                    .ok_or_else(|| CoreError::Auth(format!("no password saved for connection \"{}\"; edit the connection to add one", profile.name)))?;
                Ok(ResolvedCredentials {
                    username: Some(username.clone()),
                    password: Some(password),
                    ..ResolvedCredentials::anonymous()
                })
            }
            AuthMethod::AccessToken { username, command } => {
                let command = command.as_deref().map(str::trim).filter(|c| !c.is_empty());
                let (token, expires_at) = match command {
                    Some(cmd) => self.tokens.get_or_run(&profile.id, cmd).await?,
                    None => {
                        let token = crate::mysql::secret::load_any_password(
                            self.vault.as_ref(),
                            profile,
                        )
                        .await?
                        .filter(|p| !p.expose_secret().trim().is_empty())
                            .ok_or_else(|| CoreError::Auth(format!("no access token saved for connection \"{}\"; edit the connection to paste one or set a token command", profile.name)))?;
                        let exp = crate::token::jwt_expiry(token.expose_secret());
                        if let Some(exp) = exp
                            && exp <= Utc::now()
                        {
                            return Err(CoreError::Auth(format!(
                                "the access token saved for \"{}\" expired at {}; paste a fresh one, or set a token command so VibeLens can fetch tokens itself",
                                profile.name,
                                exp.format("%Y-%m-%d %H:%M UTC")
                            )));
                        }
                        (token, exp)
                    }
                };
                let username = username.trim();
                Ok(ResolvedCredentials {
                    username: Some(if username.is_empty() {
                        DEFAULT_TOKEN_USER.to_string()
                    } else {
                        username.to_string()
                    }),
                    password: Some(token),
                    expires_at,
                    ..ResolvedCredentials::anonymous()
                })
            }
            AuthMethod::RdsIam { username, aws } => {
                let token = self
                    .aws
                    .rds_auth_token(aws, &profile.host, profile.port, username)
                    .await?;
                Ok(ResolvedCredentials {
                    username: Some(username.clone()),
                    password: Some(token),
                    expires_at: Some(self.aws.now() + TOKEN_VALID_FOR_NEW_CONNECTIONS),
                    ..ResolvedCredentials::anonymous()
                })
            }
            AuthMethod::SecretsManager { secret_arn, aws } => {
                let secret = self.aws.get_db_secret(aws, secret_arn).await?;
                Ok(ResolvedCredentials {
                    username: Some(secret.username),
                    password: Some(secret.password),
                    expires_at: None,
                    host_override: secret.host,
                    port_override: secret.port,
                    database_override: secret.dbname,
                })
            }
            AuthMethod::ElastiCacheIam {
                username,
                cache_name,
                serverless,
                aws,
            } => {
                let token = self
                    .aws
                    .elasticache_auth_token(aws, cache_name, username, *serverless)
                    .await?;
                Ok(ResolvedCredentials {
                    username: Some(username.clone()),
                    password: Some(token),
                    expires_at: Some(self.aws.now() + TOKEN_VALID_FOR_NEW_CONNECTIONS),
                    ..ResolvedCredentials::anonymous()
                })
            }
        }
    }
}
