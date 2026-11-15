//! Catalog construction (`REST`, `Glue`, `S3 Tables`, `SQL`, `HMS`), FileIO property assembly and
//! [`ManagedCatalog`]: an `iceberg::Catalog` that rebuilds itself before AWS credentials expire and
//! re-authenticates once after an auth failure.
//!
//! # Property precedence
//!
//! Explicit storage settings on the profile (`s3.endpoint`, `s3.path-style-access`, keys, region)
//! must beat whatever the catalog server hands out. For REST this holds by construction: in
//! `iceberg-catalog-rest` 0.10.1 `load_table` builds the FileIO from
//! `server /v1/config props` extended with `table response config` extended with *the user's own
//! props*, so the props we pass to the builder are applied last. The integration test
//! `iceberg_rest.rs` pins that behaviour against a mock server that vends
//! `s3.endpoint=http://minio:9000`; if an upgrade changes the merge order the test fails. Glue,
//! S3 Tables and SQL take the props at construction and have no server-side overrides.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use aws_config::{BehaviorVersion, Region};
use chrono::{DateTime, Utc};
use iceberg::table::Table;
use iceberg::{
    Catalog, CatalogBuilder, Error as IceError, ErrorKind, Namespace, NamespaceIdent, TableCommit,
    TableCreation, TableIdent,
};
use iceberg_storage_opendal::OpenDalResolvingStorageFactory;
use secrecy::ExposeSecret;
use tokio::sync::{Mutex, RwLock};

use super::IcebergDeps;
use super::refresh::{Decision, Lease, earliest};
use crate::aws::Clock;
use crate::error::{CoreError, Result};
use crate::model::{
    AwsTarget, ConnectionProfile, Environment, IcebergCatalog, IcebergOptions, IcebergStorage,
    RestAuth, vault_keys,
};
use crate::ports::SecretVault;

/// Temporary or static AWS credentials resolved for one identity.
#[derive(Clone)]
pub(crate) struct AwsCreds {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub region: String,
}

impl std::fmt::Debug for AwsCreds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AwsCreds")
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

/// Everything a factory needs to (re)build a catalog.
#[derive(Clone)]
pub(crate) struct Env {
    pub aws: Arc<crate::aws::AwsService>,
    pub vault: Arc<dyn SecretVault>,
    pub profile_id: String,
}

/// A freshly built catalog plus when the credentials inside it stop working.
pub(crate) struct Built {
    pub catalog: Arc<dyn Catalog>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub(crate) trait CatalogFactory: Send + Sync {
    async fn build(&self) -> Result<Built>;
}

// ---------------------------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------------------------

/// FileIO (`s3.*`) properties from the profile's storage settings. `aws` are the temporary
/// credentials of the identity that applies (see [`storage_identity`]); `static_keys` are the vault keys.
pub(crate) fn s3_props(
    storage: &IcebergStorage,
    aws: Option<&AwsCreds>,
    static_keys: Option<(&str, &str)>,
) -> HashMap<String, String> {
    let mut p = HashMap::new();
    let put_creds = |p: &mut HashMap<String, String>, c: &AwsCreds| {
        p.insert("s3.access-key-id".into(), c.access_key_id.clone());
        p.insert("s3.secret-access-key".into(), c.secret_access_key.clone());
        if let Some(t) = &c.session_token {
            p.insert("s3.session-token".into(), t.clone());
        }
    };
    let common = |p: &mut HashMap<String, String>,
                  endpoint: &Option<String>,
                  region: &Option<String>,
                  path_style: bool,
                  fallback_region: Option<&str>| {
        if let Some(e) = endpoint.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
            p.insert("s3.endpoint".into(), e.to_string());
        }
        if let Some(r) = region
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .or(fallback_region)
        {
            p.insert("s3.region".into(), r.to_string());
        }
        p.insert("s3.path-style-access".into(), path_style.to_string());
    };
    match storage {
        IcebergStorage::Auto => {
            if let Some(c) = aws {
                put_creds(&mut p, c);
                p.insert("s3.region".into(), c.region.clone());
            }
        }
        IcebergStorage::S3 {
            endpoint,
            region,
            path_style,
            ..
        } => {
            if let Some(c) = aws {
                put_creds(&mut p, c);
            }
            common(
                &mut p,
                endpoint,
                region,
                *path_style,
                aws.map(|c| c.region.as_str()),
            );
        }
        IcebergStorage::S3Static {
            endpoint,
            region,
            path_style,
        } => {
            if let Some((k, s)) = static_keys {
                p.insert("s3.access-key-id".into(), k.to_string());
                p.insert("s3.secret-access-key".into(), s.to_string());
            }
            common(&mut p, endpoint, region, *path_style, None);
        }
        IcebergStorage::Local => return p,
    }
    // Never fall back to ambient AWS credentials (env vars, ~/.aws, instance metadata): VibeLens only
    // signs S3 requests with an identity the profile chose or the catalog vended. Otherwise a storage
    // misconfiguration sends requests signed with the user's own AWS keys to whatever bucket the path
    // names, e.g. a stranger's public `warehouse` bucket on AWS instead of the local MinIO.
    p.insert("s3.disable-config-load".into(), "true".into());
    p.insert("s3.disable-ec2-metadata".into(), "true".into());
    p
}

/// The AWS identity whose credentials feed the FileIO: the storage's own `aws`, else (for `Auto`,
/// and for `S3` without an identity) the catalog's, when the catalog is an AWS one.
pub(crate) fn storage_identity(opts: &IcebergOptions) -> Option<&AwsTarget> {
    let catalog_aws = match &opts.catalog {
        IcebergCatalog::Glue { aws, .. } | IcebergCatalog::S3Tables { aws, .. } => Some(aws),
        _ => None,
    };
    match &opts.storage {
        IcebergStorage::Auto => catalog_aws,
        IcebergStorage::S3 { aws, .. } => aws.as_ref().or(catalog_aws),
        IcebergStorage::S3Static { .. } | IcebergStorage::Local => None,
    }
}

/// Rewrites a `postgres://` URI so it carries `password` (percent-encoded by `url`). Other schemes
/// and URIs that already carry a password are returned unchanged.
pub(crate) fn inject_password(uri: &str, password: &str) -> Result<String> {
    let mut url = url::Url::parse(uri)
        .map_err(|e| CoreError::Invalid(format!("catalog URI is not valid: {e}")))?;
    if !matches!(url.scheme(), "postgres" | "postgresql") || url.password().is_some() {
        return Ok(uri.to_string());
    }
    if url.username().is_empty() {
        return Err(CoreError::Invalid(
            "a password was saved but the catalog URI has no user name".into(),
        ));
    }
    url.set_password(Some(password))
        .map_err(|()| CoreError::Invalid("catalog URI cannot carry a password".into()))?;
    Ok(url.to_string())
}

/// Bucket name from `arn:aws:s3tables:<region>:<account>:bucket/<name>`.
pub(crate) fn table_bucket_name(arn: &str) -> Option<&str> {
    arn.rsplit_once(":bucket/")
        .map(|(_, n)| n.trim_end_matches('/'))
        .filter(|n| !n.is_empty())
}

/// `uri` of the REST catalog without a trailing slash (the crate joins paths with `/`).
fn trim_uri(uri: &str) -> String {
    uri.trim().trim_end_matches('/').to_string()
}

/// REST catalog properties (without storage props).
pub(crate) fn rest_props(
    uri: &str,
    warehouse: Option<&str>,
    auth: &RestAuth,
    secret: Option<&str>,
    headers: &std::collections::BTreeMap<String, String>,
) -> Result<HashMap<String, String>> {
    let mut p = HashMap::new();
    p.insert("uri".to_string(), trim_uri(uri));
    if let Some(w) = warehouse.map(str::trim).filter(|w| !w.is_empty()) {
        p.insert("warehouse".into(), w.to_string());
    }
    match auth {
        RestAuth::None => {}
        RestAuth::Bearer => {
            let t = secret
                .filter(|s| !s.is_empty())
                .ok_or_else(|| CoreError::Auth("no token saved".into()))?;
            p.insert("token".into(), t.to_string());
        }
        RestAuth::OAuth2 {
            client_id,
            scope,
            server_uri,
        } => {
            let s = secret
                .filter(|s| !s.is_empty())
                .ok_or_else(|| CoreError::Auth("no client secret saved".into()))?;
            p.insert("credential".into(), format!("{client_id}:{s}"));
            if let Some(sc) = scope.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                p.insert("scope".into(), sc.to_string());
            }
            if let Some(u) = server_uri
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                p.insert("oauth2-server-uri".into(), u.to_string());
            }
        }
    }
    for (k, v) in headers {
        p.insert(format!("header.{k}"), v.clone());
    }
    Ok(p)
}

/// Maps an iceberg error to the right [`CoreError`] variant.
pub(crate) fn to_core(e: IceError) -> CoreError {
    // A CoreError we wrapped ourselves (credential refresh) keeps its class.
    let mut src: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&e);
    while let Some(s) = src {
        if let Some(core) = s.downcast_ref::<CoreError>() {
            return match core {
                CoreError::Auth(m) => CoreError::Auth(m.clone()),
                CoreError::Aws(m) => CoreError::Aws(m.clone()),
                other => CoreError::Other(other.to_string()),
            };
        }
        src = s.source();
    }
    let msg = describe_error(&e);
    match e.kind() {
        ErrorKind::TableNotFound | ErrorKind::NamespaceNotFound => CoreError::NotFound(msg),
        ErrorKind::DataInvalid => CoreError::Invalid(msg),
        _ if is_auth_failure(&e) => CoreError::Auth(msg),
        _ => CoreError::Database(msg),
    }
}

/// `message` plus its context key/values (the REST crate puts the HTTP status in there), without secrets.
fn describe_error(e: &IceError) -> String {
    let full = format!("{e:#}");
    // `{:#}` of an iceberg Error is multi-line ("Unexpected => msg, context: { … }, source: …"); keep it on one line.
    let one: String = full.split_whitespace().collect::<Vec<_>>().join(" ");
    let one = redact(&one);
    match storage_hint(&one) {
        Some(hint) => format!("{hint} Details: {one}"),
        None => one,
    }
}

/// A plain-language explanation for storage errors whose raw text hides the cause.
fn storage_hint(msg: &str) -> Option<String> {
    if msg.contains("PermanentRedirect") && msg.contains("amazonaws.com") {
        let region = header_value(msg, "x-amz-bucket-region");
        let bucket = msg
            .split("https://")
            .nth(1)
            .and_then(|u| u.split(".s3.").next())
            .filter(|b| !b.contains('/') && !b.is_empty());
        let on_aws = match (bucket, region.as_deref()) {
            (Some(b), Some(r)) => format!("on AWS S3, where bucket \"{b}\" is in {r}"),
            (Some(b), None) => format!("on AWS S3, where bucket \"{b}\" is in another region"),
            _ => "on AWS S3, in another region".to_string(),
        };
        let fix_region = region
            .map(|r| format!(" set Bucket region to {r}"))
            .unwrap_or_else(|| " set its Bucket region".into());
        return Some(format!(
            "The table's data files were requested {on_aws}. If your data lives in MinIO or another S3-compatible store, \
             edit the connection: Storage → S3-compatible with access keys with its endpoint and path-style addressing. \
             If the data really is on AWS,{fix_region} in Storage."
        ));
    }
    if msg.contains("region is missing") {
        return Some(
            "The S3 client has no region. Set Bucket region in the connection's Storage settings."
                .into(),
        );
    }
    None
}

/// Value of `"name": "value"` in an error's debug-printed header map.
fn header_value(msg: &str, name: &str) -> Option<String> {
    let at = msg.find(&format!("\"{name}\""))?;
    let rest = &msg[at + name.len() + 2..];
    let start = rest.find('"')? + 1;
    let len = rest[start..].find('"')?;
    Some(rest[start..start + len].to_string())
}

/// Removes values that look like bearer tokens from error text.
fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.to_ascii_lowercase().find("bearer ") {
        out.push_str(&rest[..i + 7]);
        out.push_str("[redacted]");
        let tail = &rest[i + 7..];
        let end = tail
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | ')'))
            .unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// True if the failure looks like an expired or rejected credential (HTTP 401/403, expired AWS token).
pub(crate) fn is_auth_failure(e: &IceError) -> bool {
    let text = format!("{e:#}").to_ascii_lowercase();
    has_http_code(&text, "401")
        || text.contains("unauthorized")
        || text.contains("expiredtoken")
        || text.contains("token has expired")
        || text.contains("token expired")
        || text.contains("invalidtoken")
        || text.contains("security token")
        || has_http_code(&text, "403")
        || text.contains("forbidden")
        || text.contains("accessdenied")
}

/// `code` as a standalone number (so "snapshot 14012" is not a 401).
fn has_http_code(text: &str, code: &str) -> bool {
    text.match_indices(code).any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + code.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit()) && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

fn ice_err(msg: impl Into<String>) -> IceError {
    IceError::new(ErrorKind::Unexpected, msg)
}

// ---------------------------------------------------------------------------------------------
// The factory
// ---------------------------------------------------------------------------------------------

/// Builds the catalog described by a profile.
pub(crate) struct ProfileFactory {
    pub opts: IcebergOptions,
    pub env: Env,
    /// Whether VibeLens may add the missing `iceberg_type` column to a SQL catalog database
    /// itself. Only on `dev` connections: a test/staging/prod catalog is usually shared with
    /// Spark/Trino jobs, and a client app must not change its schema without the owner.
    pub may_migrate_catalog: bool,
}

impl ProfileFactory {
    pub fn new(profile: &ConnectionProfile, deps: &IcebergDeps) -> Result<Self> {
        let opts = profile
            .iceberg
            .clone()
            .ok_or_else(|| CoreError::Invalid("this connection has no Iceberg options".into()))?;
        Ok(Self {
            opts,
            env: Env {
                aws: deps.aws.clone(),
                vault: deps.vault.clone(),
                profile_id: profile.id.clone(),
            },
            may_migrate_catalog: profile.environment == Environment::Dev,
        })
    }

    async fn secret(&self) -> Result<Option<String>> {
        Ok(self
            .env
            .vault
            .get(&vault_keys::iceberg_secret(&self.env.profile_id))
            .await?
            .map(|s| s.expose_secret().to_string()))
    }

    pub async fn aws_creds(&self, target: &AwsTarget) -> Result<AwsCreds> {
        let c = self.env.aws.credentials(target).await?;
        Ok(AwsCreds {
            access_key_id: c.access_key_id().to_string(),
            secret_access_key: c.secret_access_key().to_string(),
            session_token: c.session_token().map(str::to_string),
            expires_at: c.expiry().map(DateTime::<Utc>::from),
            region: target.region.trim().to_string(),
        })
    }

    /// FileIO props + the earliest expiry among the credentials they carry.
    async fn storage_props(&self) -> Result<(HashMap<String, String>, Option<DateTime<Utc>>)> {
        let creds = match storage_identity(&self.opts) {
            Some(t) => Some(self.aws_creds(t).await?),
            None => None,
        };
        let static_keys = if matches!(self.opts.storage, IcebergStorage::S3Static { .. }) {
            let id = self
                .env
                .vault
                .get(&vault_keys::s3_access_key_id(&self.env.profile_id))
                .await?;
            let sk = self
                .env
                .vault
                .get(&vault_keys::s3_secret_access_key(&self.env.profile_id))
                .await?;
            match (id, sk) {
                (Some(i), Some(s)) => {
                    Some((i.expose_secret().to_string(), s.expose_secret().to_string()))
                }
                _ => return Err(CoreError::Auth("no S3 access keys saved".into())),
            }
        } else {
            None
        };
        let props = s3_props(
            &self.opts.storage,
            creds.as_ref(),
            static_keys.as_ref().map(|(a, b)| (a.as_str(), b.as_str())),
        );
        Ok((props, creds.and_then(|c| c.expires_at)))
    }
}

fn storage_factory() -> Arc<dyn iceberg::io::StorageFactory> {
    Arc::new(RegionDefaulting(OpenDalResolvingStorageFactory::new()))
}

/// Region the S3 client uses when **nothing** supplied one: not the profile, not the catalog's
/// vended config. S3-compatible stores (MinIO, Ceph, R2, SeaweedFS) ignore it or expect exactly
/// this; AWS buckets elsewhere answer with an error naming their real region.
const DEFAULT_S3_REGION: &str = "us-east-1";

/// The props to build storage with: unchanged when a region is set, else with
/// [`DEFAULT_S3_REGION`] added. `None` means "use the config as is".
fn with_default_region(props: &HashMap<String, String>) -> Option<HashMap<String, String>> {
    let has = |k: &str| props.get(k).is_some_and(|v| !v.trim().is_empty());
    if has("s3.region") || has("client.region") {
        return None;
    }
    let mut p = props.clone();
    p.insert("s3.region".into(), DEFAULT_S3_REGION.into());
    Some(p)
}

/// Sends S3 requests unsigned when the merged props carry no keys. `s3.disable-*` only removes the
/// env, profile and IMDS sources: OpenDAL's default chain would still sign with an SSO, web-identity,
/// `credential_process` or ECS identity, whatever the storage kind. This runs for every FileIO, so
/// a path scheme that picks S3 regardless of the profile's storage setting is covered too.
fn without_ambient_credentials(mut props: HashMap<String, String>) -> HashMap<String, String> {
    if !props
        .get("s3.access-key-id")
        .is_some_and(|v| !v.trim().is_empty())
    {
        props.insert("s3.allow-anonymous".into(), "true".into());
    }
    props
}

/// Wraps the OpenDAL storage factory so an S3 FileIO built without `s3.region` / `client.region`
/// gets [`DEFAULT_S3_REGION`] instead of failing every read with "region is missing". It runs
/// after all props are merged (profile, catalog defaults/overrides, per-table config), so it never
/// overrides a region any of them set.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RegionDefaulting(OpenDalResolvingStorageFactory);

#[typetag::serde(name = "VibeLensRegionDefaulting")]
impl iceberg::io::StorageFactory for RegionDefaulting {
    fn build(
        &self,
        config: &iceberg::io::StorageConfig,
    ) -> iceberg::Result<Arc<dyn iceberg::io::Storage>> {
        let props = with_default_region(config.props()).unwrap_or_else(|| config.props().clone());
        let props = without_ambient_credentials(props);
        self.0.build(&iceberg::io::StorageConfig::from_props(props))
    }
}

/// `sqlx::any::install_default_drivers()` must run once before the first `AnyPool`.
fn install_sql_drivers() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(sqlx::any::install_default_drivers);
}

#[async_trait]
impl CatalogFactory for ProfileFactory {
    async fn build(&self) -> Result<Built> {
        let (storage, storage_expiry) = self.storage_props().await?;
        let name = if self.env.profile_id.is_empty() {
            "vibelens"
        } else {
            self.env.profile_id.as_str()
        };
        match &self.opts.catalog {
            IcebergCatalog::Rest {
                uri,
                warehouse,
                auth,
                headers,
            } => {
                if uri.trim().is_empty() {
                    return Err(CoreError::Invalid("REST catalog URI is empty".into()));
                }
                let secret = self.secret().await?;
                let mut props =
                    rest_props(uri, warehouse.as_deref(), auth, secret.as_deref(), headers)?;
                // Explicit storage settings go in last: the REST crate applies the user's props after
                // the server's, so these win (pinned by tests/iceberg_rest.rs).
                props.extend(storage);
                let client = reqwest012::Client::builder()
                    .connect_timeout(Duration::from_secs(15))
                    .timeout(Duration::from_secs(60))
                    .build()
                    .map_err(|e| CoreError::Other(format!("HTTP client: {e}")))?;
                let cat = iceberg_catalog_rest::RestCatalogBuilder::default()
                    .with_client(client)
                    .with_storage_factory(storage_factory())
                    .load(name, props)
                    .await
                    .map_err(to_core)?;
                Ok(Built {
                    catalog: Arc::new(cat),
                    expires_at: None,
                })
            }
            IcebergCatalog::Glue {
                aws,
                catalog_id,
                warehouse,
            } => {
                let creds = self.aws_creds(aws).await?;
                let mut props = storage;
                props.insert("aws_access_key_id".into(), creds.access_key_id.clone());
                props.insert(
                    "aws_secret_access_key".into(),
                    creds.secret_access_key.clone(),
                );
                if let Some(t) = &creds.session_token {
                    props.insert("aws_session_token".into(), t.clone());
                }
                props.insert("region_name".into(), creds.region.clone());
                if let Some(id) = catalog_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    props.insert("catalog_id".into(), id.to_string());
                }
                // The Glue crate insists on a warehouse even for read-only use; it is only used
                // as the default location when creating tables.
                let wh = warehouse
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("s3://vibelens-unused-warehouse/");
                props.insert("warehouse".into(), wh.to_string());
                let cat = iceberg_catalog_glue::GlueCatalogBuilder::default()
                    .with_storage_factory(storage_factory())
                    .load(name, props)
                    .await
                    .map_err(to_core)?;
                Ok(Built {
                    catalog: Arc::new(cat),
                    expires_at: earliest(creds.expires_at, storage_expiry),
                })
            }
            IcebergCatalog::S3Tables {
                aws,
                table_bucket_arn,
            } => {
                let provider = self.env.aws.credentials_provider(aws).await?;
                // Resolve once so a dead SSO session surfaces as `Auth`, not as an opaque dispatch error.
                let creds = self.aws_creds(aws).await?;
                let conf = aws_sdk_s3tables::Config::builder()
                    .behavior_version(BehaviorVersion::latest())
                    .region(Region::new(aws.region.trim().to_string()))
                    .credentials_provider(provider)
                    .build();
                let client = aws_sdk_s3tables::Client::from_conf(conf);
                let cat = iceberg_catalog_s3tables::S3TablesCatalogBuilder::default()
                    .with_client(client)
                    .with_table_bucket_arn(table_bucket_arn.trim())
                    .with_storage_factory(storage_factory())
                    .load(name, storage)
                    .await
                    .map_err(to_core)?;
                // The SDK client refreshes by itself; only the FileIO props carry a fixed expiry.
                Ok(Built {
                    catalog: Arc::new(cat),
                    expires_at: earliest(creds.expires_at, storage_expiry),
                })
            }
            IcebergCatalog::Sql {
                uri,
                warehouse,
                catalog_name,
            } => {
                install_sql_drivers();
                let secret = self.secret().await?;
                let uri = match secret.as_deref().filter(|s| !s.is_empty()) {
                    Some(pw) => inject_password(uri.trim(), pw)?,
                    None => uri.trim().to_string(),
                };
                migrate_sql_catalog(&uri, self.may_migrate_catalog).await?;
                let bind = if uri.starts_with("postgres") {
                    "DollarNumeric"
                } else {
                    "QMark"
                };
                let mut props = storage;
                props.insert("uri".into(), uri);
                props.insert("warehouse".into(), warehouse.trim().to_string());
                props.insert("sql_bind_style".into(), bind.into());
                let cat = iceberg_catalog_sql::SqlCatalogBuilder::default()
                    .with_storage_factory(storage_factory())
                    .load(catalog_name.trim(), props)
                    .await
                    .map_err(|e| match to_core(e) {
                        // Never echo the connection URI back (it may carry the password).
                        CoreError::Database(m) => CoreError::Database(redact_uri(&m)),
                        other => other,
                    })?;
                Ok(Built {
                    catalog: Arc::new(cat),
                    expires_at: storage_expiry,
                })
            }
            IcebergCatalog::Hms { address, warehouse } => {
                build_hms(name, address, warehouse, storage).await
            }
        }
    }
}

/// Catalogs written by Java's JDBC catalog (default schema) and by pyiceberg 0.10 lack the
/// `iceberg_type` column that `iceberg-catalog-sql` 0.10.1 puts in every query. Java's own "V1"
/// schema migration is exactly `ALTER TABLE ... ADD COLUMN iceberg_type`, and a NULL there means
/// "table" to every implementation, so adding the nullable column is harmless to other clients.
/// Without it the connection could not list or load a single table.
///
/// Only done automatically on `dev` connections (`allowed`); otherwise the connection fails with
/// the exact statement for the catalog's owner to run.
async fn migrate_sql_catalog(uri: &str, allowed: bool) -> Result<()> {
    use sqlx::any::AnyPoolOptions;
    let pool = AnyPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(15))
        .connect(uri)
        .await
        .map_err(|e| {
            CoreError::Database(redact_uri(&format!(
                "cannot connect to the catalog database: {e}"
            )))
        })?;
    let has_column = sqlx::query("SELECT iceberg_type FROM iceberg_tables LIMIT 0")
        .execute(&pool)
        .await
        .is_ok();
    if has_column {
        return Ok(());
    }
    let has_table = sqlx::query("SELECT 1 FROM iceberg_tables LIMIT 0")
        .execute(&pool)
        .await
        .is_ok();
    if !has_table {
        return Ok(()); // empty database: the catalog creates its own schema
    }
    if !allowed {
        return Err(CoreError::Invalid(
            "this SQL catalog database lacks the nullable `iceberg_type` column that VibeLens's Iceberg SQL catalog needs \
             (catalogs created by pyiceberg or Java's default JDBC schema don't have it). VibeLens only adds it by itself on \
             connections marked dev. Ask the catalog's owner to run: ALTER TABLE iceberg_tables ADD COLUMN iceberg_type VARCHAR(5) \
             (the same change as Java's JDBC catalog V1 migration; NULL means \"table\" to every client)."
                .into(),
        ));
    }
    tracing::warn!(
        "iceberg: adding the missing nullable column iceberg_tables.iceberg_type to the catalog database"
    );
    sqlx::query("ALTER TABLE iceberg_tables ADD COLUMN iceberg_type VARCHAR(5)")
        .execute(&pool)
        .await
        .map_err(|e| {
            CoreError::Database(redact_uri(&format!(
                "the catalog database predates the `iceberg_type` column that the Rust Iceberg SQL catalog needs, and VibeLens could not add it ({e}). Run: ALTER TABLE iceberg_tables ADD COLUMN iceberg_type VARCHAR(5)"
            )))
        })?;
    Ok(())
}

/// Strips `user:password@` from anything URI-shaped in an error message.
pub(crate) fn redact_uri(msg: &str) -> String {
    let mut out = String::new();
    for (i, word) in msg.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match (word.find("://"), word.find('@')) {
            (Some(s), Some(at)) if at > s => {
                out.push_str(&word[..s + 3]);
                out.push_str("[redacted]");
                out.push_str(&word[at..]);
            }
            _ => out.push_str(word),
        }
    }
    out
}

#[cfg(feature = "iceberg-hms")]
async fn build_hms(
    name: &str,
    address: &str,
    warehouse: &str,
    storage: HashMap<String, String>,
) -> Result<Built> {
    let mut props = storage;
    props.insert("uri".into(), address.trim().to_string());
    props.insert("warehouse".into(), warehouse.trim().to_string());
    let cat = iceberg_catalog_hms::HmsCatalogBuilder::default()
        .with_storage_factory(storage_factory())
        .load(name, props)
        .await
        .map_err(to_core)?;
    Ok(Built {
        catalog: Arc::new(cat),
        expires_at: None,
    })
}

#[cfg(not(feature = "iceberg-hms"))]
async fn build_hms(
    _name: &str,
    _address: &str,
    _warehouse: &str,
    _storage: HashMap<String, String>,
) -> Result<Built> {
    Err(CoreError::Invalid(
        "this build has no Hive Metastore support (feature iceberg-hms)".into(),
    ))
}

// ---------------------------------------------------------------------------------------------
// ManagedCatalog
// ---------------------------------------------------------------------------------------------

struct Current {
    catalog: Arc<dyn Catalog>,
    lease: Lease,
    built_at: DateTime<Utc>,
}

/// An `iceberg::Catalog` that swaps its inner catalog for a fresh one before credentials expire
/// ([`Lease`]), and once after an authentication failure (OAuth2 tokens are not refreshed by the
/// REST crate). Read-only calls are retried once after such a rebuild; mutations never are.
pub(crate) struct ManagedCatalog {
    factory: Arc<dyn CatalogFactory>,
    cur: RwLock<Current>,
    rebuild: Mutex<()>,
    clock: Clock,
}

impl std::fmt::Debug for ManagedCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ManagedCatalog")
    }
}

/// Don't rebuild for an auth failure more often than this.
const AUTH_REBUILD_MIN_GAP_SECS: i64 = 3;

impl ManagedCatalog {
    pub async fn new(factory: Arc<dyn CatalogFactory>, clock: Clock) -> Result<Arc<Self>> {
        let built = factory.build().await?;
        let now = clock();
        Ok(Arc::new(Self {
            factory,
            cur: RwLock::new(Current {
                catalog: built.catalog,
                lease: Lease::new(built.expires_at),
                built_at: now,
            }),
            rebuild: Mutex::new(()),
            clock,
        }))
    }

    /// The catalog to use right now, rebuilding first if the credentials are inside the margin.
    pub async fn current(&self) -> Result<Arc<dyn Catalog>> {
        let now = (self.clock)();
        let decision = self.cur.read().await.lease.decide(now);
        match decision {
            Decision::Use | Decision::UseStale => Ok(self.cur.read().await.catalog.clone()),
            Decision::Expired => Err(CoreError::Auth(
                "AWS credentials expired and could not be refreshed".into(),
            )),
            Decision::Rebuild => {
                let _g = self.rebuild.lock().await;
                // Someone else may have rebuilt while we waited for the lock.
                let now = (self.clock)();
                if self.cur.read().await.lease.decide(now) != Decision::Rebuild {
                    return Ok(self.cur.read().await.catalog.clone());
                }
                match self.factory.build().await {
                    Ok(b) => {
                        let mut w = self.cur.write().await;
                        *w = Current {
                            catalog: b.catalog,
                            lease: Lease::new(b.expires_at),
                            built_at: now,
                        };
                        Ok(w.catalog.clone())
                    }
                    Err(e) => {
                        let mut w = self.cur.write().await;
                        w.lease.record_failure(now);
                        if now < w.lease.expires_at.unwrap_or(now) {
                            tracing::warn!(
                                "iceberg: credential refresh failed, using the current ones: {e}"
                            );
                            Ok(w.catalog.clone())
                        } else {
                            Err(e)
                        }
                    }
                }
            }
        }
    }

    /// Rebuilds after an auth failure unless we just did. Returns true if a new catalog is in place.
    async fn rebuild_after_auth_failure(&self) -> bool {
        let _g = self.rebuild.lock().await;
        let now = (self.clock)();
        if (now - self.cur.read().await.built_at).num_seconds() < AUTH_REBUILD_MIN_GAP_SECS {
            return false;
        }
        match self.factory.build().await {
            Ok(b) => {
                *self.cur.write().await = Current {
                    catalog: b.catalog,
                    lease: Lease::new(b.expires_at),
                    built_at: now,
                };
                true
            }
            Err(e) => {
                tracing::warn!("iceberg: re-authentication failed: {e}");
                false
            }
        }
    }

    async fn inner(&self) -> iceberg::Result<Arc<dyn Catalog>> {
        self.current()
            .await
            .map_err(|e| ice_err(e.to_string()).with_source(e))
    }
}

/// Runs a read-only catalog call; after an auth failure rebuilds the catalog and retries once.
macro_rules! read_call {
    ($self:ident, |$c:ident| $body:expr) => {{
        let $c = $self.inner().await?;
        match $body.await {
            Err(e) if is_auth_failure(&e) && $self.rebuild_after_auth_failure().await => {
                let $c = $self.inner().await?;
                $body.await
            }
            r => r,
        }
    }};
}

#[async_trait]
impl Catalog for ManagedCatalog {
    async fn list_namespaces(
        &self,
        parent: Option<&NamespaceIdent>,
    ) -> iceberg::Result<Vec<NamespaceIdent>> {
        read_call!(self, |c| c.list_namespaces(parent))
    }

    async fn create_namespace(
        &self,
        namespace: &NamespaceIdent,
        properties: HashMap<String, String>,
    ) -> iceberg::Result<Namespace> {
        self.inner()
            .await?
            .create_namespace(namespace, properties)
            .await
    }

    async fn get_namespace(&self, namespace: &NamespaceIdent) -> iceberg::Result<Namespace> {
        read_call!(self, |c| c.get_namespace(namespace))
    }

    async fn namespace_exists(&self, namespace: &NamespaceIdent) -> iceberg::Result<bool> {
        read_call!(self, |c| c.namespace_exists(namespace))
    }

    async fn update_namespace(
        &self,
        namespace: &NamespaceIdent,
        properties: HashMap<String, String>,
    ) -> iceberg::Result<()> {
        self.inner()
            .await?
            .update_namespace(namespace, properties)
            .await
    }

    async fn drop_namespace(&self, namespace: &NamespaceIdent) -> iceberg::Result<()> {
        self.inner().await?.drop_namespace(namespace).await
    }

    async fn list_tables(&self, namespace: &NamespaceIdent) -> iceberg::Result<Vec<TableIdent>> {
        read_call!(self, |c| c.list_tables(namespace))
    }

    async fn create_table(
        &self,
        namespace: &NamespaceIdent,
        creation: TableCreation,
    ) -> iceberg::Result<Table> {
        self.inner().await?.create_table(namespace, creation).await
    }

    async fn load_table(&self, table: &TableIdent) -> iceberg::Result<Table> {
        read_call!(self, |c| c.load_table(table))
    }

    async fn drop_table(&self, table: &TableIdent) -> iceberg::Result<()> {
        self.inner().await?.drop_table(table).await
    }

    async fn purge_table(&self, table: &TableIdent) -> iceberg::Result<()> {
        self.inner().await?.purge_table(table).await
    }

    async fn table_exists(&self, table: &TableIdent) -> iceberg::Result<bool> {
        read_call!(self, |c| c.table_exists(table))
    }

    async fn rename_table(&self, src: &TableIdent, dest: &TableIdent) -> iceberg::Result<()> {
        self.inner().await?.rename_table(src, dest).await
    }

    async fn register_table(
        &self,
        table: &TableIdent,
        metadata_location: String,
    ) -> iceberg::Result<Table> {
        self.inner()
            .await?
            .register_table(table, metadata_location)
            .await
    }

    async fn update_table(&self, commit: TableCommit) -> iceberg::Result<Table> {
        self.inner().await?.update_table(commit).await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn explains_a_wrong_bucket_redirect() {
        let raw = r#"Unexpected => Failed to load manifest list in cache, source: Unexpected (persistent) at read, context: { uri: https://warehouse.s3.us-east-1.amazonaws.com/shop/orders/metadata/snap-1.avro, response: Parts { status: 301, version: HTTP/1.1, headers: {"x-amz-bucket-region": "eu-central-1", "server": "AmazonS3"} }, service: s3 } => S3Error { code: "PermanentRedirect", message: "The bucket you are attempting to access must be addressed using the specified endpoint." }"#;
        let hint = storage_hint(raw).unwrap();
        assert!(
            hint.contains(r#"bucket "warehouse" is in eu-central-1"#),
            "{hint}"
        );
        assert!(
            hint.contains("S3-compatible with access keys")
                && hint.contains("Bucket region to eu-central-1"),
            "{hint}"
        );
        assert!(storage_hint("Unexpected => something else").is_none());
    }

    #[test]
    fn region_defaults_only_when_nothing_set_one() {
        let m = |kv: &[(&str, &str)]| -> HashMap<String, String> {
            kv.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };
        let p = with_default_region(&m(&[("s3.endpoint", "http://localhost:9000")])).unwrap();
        assert_eq!(p["s3.region"], DEFAULT_S3_REGION);
        assert_eq!(p["s3.endpoint"], "http://localhost:9000");
        assert!(with_default_region(&m(&[("s3.region", "af-south-1")])).is_none());
        assert!(with_default_region(&m(&[("client.region", "eu-west-1")])).is_none());
        assert_eq!(
            with_default_region(&m(&[("s3.region", "  ")])).unwrap()["s3.region"],
            DEFAULT_S3_REGION
        );
    }

    /// The real wrapper builds S3 storage without a region instead of failing.
    #[test]
    fn region_defaulting_factory_builds_s3_storage_without_region() {
        use iceberg::io::{StorageConfig, StorageFactory};
        let f = RegionDefaulting(OpenDalResolvingStorageFactory::new());
        let cfg = StorageConfig::from_props(
            [
                ("s3.endpoint", "http://127.0.0.1:9"),
                ("s3.access-key-id", "a"),
                ("s3.secret-access-key", "b"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        assert!(f.build(&cfg).is_ok());
    }

    /// A non-dev connection must never change the user's catalog schema; a dev one may.
    #[tokio::test]
    async fn sql_catalog_migration_only_on_dev() {
        install_sql_drivers();
        let dir = tempfile::tempdir().unwrap();
        let uri = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("catalog.db").display()
        );
        {
            let pool = sqlx::any::AnyPoolOptions::new()
                .max_connections(1)
                .connect(&uri)
                .await
                .unwrap();
            sqlx::query("CREATE TABLE iceberg_tables (catalog_name VARCHAR(255), table_namespace VARCHAR(255), table_name VARCHAR(255), metadata_location VARCHAR(1000), previous_metadata_location VARCHAR(1000))")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
        }
        let has_col = |uri: String| async move {
            let pool = sqlx::any::AnyPoolOptions::new()
                .max_connections(1)
                .connect(&uri)
                .await
                .unwrap();
            let ok = sqlx::query("SELECT iceberg_type FROM iceberg_tables LIMIT 0")
                .execute(&pool)
                .await
                .is_ok();
            pool.close().await;
            ok
        };

        let err = migrate_sql_catalog(&uri, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("ALTER TABLE iceberg_tables ADD COLUMN iceberg_type VARCHAR(5)"),
            "{err}"
        );
        assert!(
            !has_col(uri.clone()).await,
            "non-dev connect changed the catalog schema"
        );

        migrate_sql_catalog(&uri, true).await.unwrap();
        assert!(has_col(uri.clone()).await);
        migrate_sql_catalog(&uri, false).await.unwrap(); // already migrated: fine on any connection
    }

    use std::collections::BTreeMap;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::Duration as CDuration;

    use super::*;
    use crate::model::{AwsIdentity, IcebergQueryEngine};

    fn creds(region: &str) -> AwsCreds {
        AwsCreds {
            access_key_id: "AKIA".into(),
            secret_access_key: "sek".into(),
            session_token: Some("tok".into()),
            expires_at: None,
            region: region.into(),
        }
    }

    fn target() -> AwsTarget {
        AwsTarget {
            identity: AwsIdentity::DefaultChain,
            region: "eu-west-1".into(),
        }
    }

    #[test]
    fn auto_storage_uses_catalog_credentials() {
        let p = s3_props(&IcebergStorage::Auto, Some(&creds("eu-west-1")), None);
        assert_eq!(p["s3.access-key-id"], "AKIA");
        assert_eq!(p["s3.secret-access-key"], "sek");
        assert_eq!(p["s3.session-token"], "tok");
        assert_eq!(p["s3.region"], "eu-west-1");
        assert!(!p.contains_key("s3.endpoint"));
        // REST vends its own credentials; we only switch off the ambient AWS chain.
        let rest = s3_props(&IcebergStorage::Auto, None, None);
        assert_eq!(rest.len(), 2, "{rest:?}");
        assert_eq!(rest["s3.disable-config-load"], "true");
        assert_eq!(rest["s3.disable-ec2-metadata"], "true");
    }

    #[test]
    fn s3_storage_sets_endpoint_region_and_path_style() {
        let st = IcebergStorage::S3 {
            aws: None,
            endpoint: Some("http://localhost:9000".into()),
            region: Some("us-east-1".into()),
            path_style: true,
        };
        let p = s3_props(&st, None, None);
        assert_eq!(p["s3.endpoint"], "http://localhost:9000");
        assert_eq!(p["s3.region"], "us-east-1");
        assert_eq!(p["s3.path-style-access"], "true");
        assert!(!p.contains_key("s3.access-key-id"));
        // With an identity: its credentials, and its region as the fallback.
        let st = IcebergStorage::S3 {
            aws: Some(target()),
            endpoint: None,
            region: None,
            path_style: false,
        };
        let p = s3_props(&st, Some(&creds("eu-west-1")), None);
        assert_eq!(p["s3.region"], "eu-west-1");
        assert_eq!(p["s3.path-style-access"], "false");
        assert_eq!(p["s3.session-token"], "tok");
    }

    #[test]
    fn static_storage_uses_vault_keys_and_blank_endpoint_is_ignored() {
        let st = IcebergStorage::S3Static {
            endpoint: Some("  ".into()),
            region: None,
            path_style: true,
        };
        let p = s3_props(&st, None, Some(("minio", "minio123")));
        assert_eq!(p["s3.access-key-id"], "minio");
        assert_eq!(p["s3.secret-access-key"], "minio123");
        assert!(!p.contains_key("s3.endpoint"));
        assert!(!p.contains_key("s3.session-token"));
        assert!(s3_props(&IcebergStorage::Local, None, None).is_empty());
    }

    #[test]
    fn storage_without_keys_is_never_signed_by_an_ambient_identity() {
        let p = without_ambient_credentials(HashMap::new());
        assert_eq!(p["s3.allow-anonymous"], "true");
        let keyed = HashMap::from([("s3.access-key-id".to_string(), "AKIA".to_string())]);
        assert!(!without_ambient_credentials(keyed).contains_key("s3.allow-anonymous"));
    }

    #[test]
    fn storage_identity_falls_back_to_the_aws_catalog() {
        let glue = IcebergOptions {
            catalog: IcebergCatalog::Glue {
                aws: target(),
                catalog_id: None,
                warehouse: None,
            },
            storage: IcebergStorage::Auto,
            query_engine: IcebergQueryEngine::Local,
            default_namespace: None,
        };
        assert_eq!(storage_identity(&glue), Some(&target()));
        let mut rest = glue.clone();
        rest.catalog = IcebergCatalog::Rest {
            uri: "http://x".into(),
            warehouse: None,
            auth: RestAuth::None,
            headers: BTreeMap::new(),
        };
        assert_eq!(storage_identity(&rest), None);
        let mut other = glue.clone();
        let t2 = AwsTarget {
            identity: AwsIdentity::Profile {
                profile_name: "p".into(),
            },
            region: "af-south-1".into(),
        };
        other.storage = IcebergStorage::S3 {
            aws: Some(t2.clone()),
            endpoint: None,
            region: None,
            path_style: false,
        };
        assert_eq!(
            storage_identity(&other),
            Some(&t2),
            "the storage identity wins over the catalog's"
        );
        other.storage = IcebergStorage::Local;
        assert_eq!(storage_identity(&other), None);
    }

    #[test]
    fn rest_props_for_each_auth_mode() {
        let mut h = BTreeMap::new();
        h.insert("X-Tenant".to_string(), "acme".to_string());
        let p = rest_props(
            "https://cat.example.com/api/",
            Some("wh"),
            &RestAuth::None,
            None,
            &h,
        )
        .unwrap();
        assert_eq!(p["uri"], "https://cat.example.com/api");
        assert_eq!(p["warehouse"], "wh");
        assert_eq!(p["header.X-Tenant"], "acme");
        assert!(!p.contains_key("token") && !p.contains_key("credential"));

        let p = rest_props(
            "http://x",
            None,
            &RestAuth::Bearer,
            Some("abc"),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(p["token"], "abc");

        let oauth = RestAuth::OAuth2 {
            client_id: "cid".into(),
            scope: Some("PRINCIPAL_ROLE:ALL".into()),
            server_uri: Some("https://idp/token".into()),
        };
        let p = rest_props("http://x", None, &oauth, Some("s3cr3t"), &BTreeMap::new()).unwrap();
        assert_eq!(p["credential"], "cid:s3cr3t");
        assert_eq!(p["scope"], "PRINCIPAL_ROLE:ALL");
        assert_eq!(p["oauth2-server-uri"], "https://idp/token");
    }

    #[test]
    fn missing_secret_is_an_auth_error() {
        let e =
            rest_props("http://x", None, &RestAuth::Bearer, None, &BTreeMap::new()).unwrap_err();
        assert!(
            matches!(&e, CoreError::Auth(m) if m == "no token saved"),
            "{e:?}"
        );
        let e = rest_props(
            "http://x",
            None,
            &RestAuth::Bearer,
            Some(""),
            &BTreeMap::new(),
        )
        .unwrap_err();
        assert!(matches!(e, CoreError::Auth(_)));
        let oauth = RestAuth::OAuth2 {
            client_id: "c".into(),
            scope: None,
            server_uri: None,
        };
        assert!(matches!(
            rest_props("http://x", None, &oauth, None, &BTreeMap::new()),
            Err(CoreError::Auth(_))
        ));
    }

    #[test]
    fn password_injection_is_percent_encoded_and_scheme_aware() {
        let u =
            inject_password("postgres://iceberg@db.internal:5432/catalog", "p@ss/w:rd#1").unwrap();
        let parsed = url::Url::parse(&u).unwrap();
        assert_eq!(parsed.username(), "iceberg");
        assert_eq!(parsed.host_str(), Some("db.internal"));
        assert_eq!(parsed.path(), "/catalog");
        assert_eq!(percent_decode(parsed.password().unwrap()), "p@ss/w:rd#1");
        assert!(!u.contains("p@ss"), "raw password must not appear: {u}");
        // sqlite is untouched; an existing password is respected.
        assert_eq!(
            inject_password("sqlite:///tmp/c.db", "x").unwrap(),
            "sqlite:///tmp/c.db"
        );
        assert_eq!(
            inject_password("postgres://u:old@h/db", "new").unwrap(),
            "postgres://u:old@h/db"
        );
        assert!(
            inject_password("postgres://h/db", "x").is_err(),
            "no user name"
        );
    }

    fn percent_decode(s: &str) -> String {
        url::form_urlencoded::parse(format!("k={s}").as_bytes())
            .next()
            .map(|(_, v)| v.to_string())
            .unwrap_or_default()
    }

    #[test]
    fn bucket_name_from_arn() {
        assert_eq!(
            table_bucket_name("arn:aws:s3tables:eu-west-1:123456789012:bucket/my-lake"),
            Some("my-lake")
        );
        assert_eq!(
            table_bucket_name("arn:aws:s3tables:eu-west-1:123456789012:bucket/"),
            None
        );
        assert_eq!(table_bucket_name("nonsense"), None);
    }

    #[test]
    fn uris_and_tokens_are_redacted_in_errors() {
        assert_eq!(
            redact_uri("error connecting to postgres://u:hunter2@h/db now"),
            "error connecting to postgres://[redacted]@h/db now"
        );
        assert_eq!(
            redact("Authorization: Bearer abc.def.ghi, next"),
            "Authorization: Bearer [redacted], next"
        );
    }

    #[test]
    fn error_classification() {
        let nf = IceError::new(ErrorKind::TableNotFound, "no such table");
        assert!(matches!(to_core(nf), CoreError::NotFound(_)));
        let unauth = IceError::new(
            ErrorKind::Unexpected,
            "Received response with unexpected status code",
        )
        .with_context("status", "401 Unauthorized");
        assert!(is_auth_failure(&unauth));
        assert!(matches!(to_core(unauth), CoreError::Auth(_)));
        let lookalike = IceError::new(ErrorKind::Unexpected, "snapshot 14012 is broken");
        assert!(!is_auth_failure(&lookalike));
        let other = IceError::new(ErrorKind::Unexpected, "boom");
        assert!(!is_auth_failure(&other));
        assert!(matches!(to_core(other), CoreError::Database(_)));
        // A CoreError carried as source keeps its class.
        let wrapped = ice_err("x").with_source(CoreError::Aws("sso expired".into()));
        assert!(matches!(to_core(wrapped), CoreError::Aws(m) if m == "sso expired"));
    }

    // ---- ManagedCatalog with a fake factory and clock ----------------------------------------

    #[derive(Debug)]
    struct NullCatalog(usize);

    #[async_trait]
    impl Catalog for NullCatalog {
        async fn list_namespaces(
            &self,
            _p: Option<&NamespaceIdent>,
        ) -> iceberg::Result<Vec<NamespaceIdent>> {
            Ok(vec![NamespaceIdent::new(format!("gen{}", self.0))])
        }
        async fn create_namespace(
            &self,
            _n: &NamespaceIdent,
            _p: HashMap<String, String>,
        ) -> iceberg::Result<Namespace> {
            Err(ice_err("unsupported"))
        }
        async fn get_namespace(&self, _n: &NamespaceIdent) -> iceberg::Result<Namespace> {
            Err(ice_err("unsupported"))
        }
        async fn namespace_exists(&self, _n: &NamespaceIdent) -> iceberg::Result<bool> {
            Ok(true)
        }
        async fn update_namespace(
            &self,
            _n: &NamespaceIdent,
            _p: HashMap<String, String>,
        ) -> iceberg::Result<()> {
            Err(ice_err("unsupported"))
        }
        async fn drop_namespace(&self, _n: &NamespaceIdent) -> iceberg::Result<()> {
            Err(ice_err("unsupported"))
        }
        async fn list_tables(&self, _n: &NamespaceIdent) -> iceberg::Result<Vec<TableIdent>> {
            if self.0 == 0 {
                Err(IceError::new(ErrorKind::Unexpected, "bad")
                    .with_context("status", "401 Unauthorized"))
            } else {
                Ok(vec![])
            }
        }
        async fn create_table(
            &self,
            _n: &NamespaceIdent,
            _c: TableCreation,
        ) -> iceberg::Result<Table> {
            Err(ice_err("unsupported"))
        }
        async fn load_table(&self, _t: &TableIdent) -> iceberg::Result<Table> {
            Err(ice_err("unsupported"))
        }
        async fn drop_table(&self, _t: &TableIdent) -> iceberg::Result<()> {
            Err(ice_err("unsupported"))
        }
        async fn purge_table(&self, _t: &TableIdent) -> iceberg::Result<()> {
            Err(ice_err("unsupported"))
        }
        async fn table_exists(&self, _t: &TableIdent) -> iceberg::Result<bool> {
            Ok(false)
        }
        async fn rename_table(&self, _a: &TableIdent, _b: &TableIdent) -> iceberg::Result<()> {
            Err(ice_err("unsupported"))
        }
        async fn register_table(&self, _t: &TableIdent, _m: String) -> iceberg::Result<Table> {
            Err(ice_err("unsupported"))
        }
        async fn update_table(&self, _c: TableCommit) -> iceberg::Result<Table> {
            Err(ice_err("unsupported"))
        }
    }

    struct FakeFactory {
        builds: AtomicUsize,
        /// Credential lifetime handed out by each build.
        ttl: CDuration,
        clock: Clock,
        fail_next: StdMutex<bool>,
        first_gen: usize,
    }

    #[async_trait]
    impl CatalogFactory for FakeFactory {
        async fn build(&self) -> Result<Built> {
            let n = self.builds.fetch_add(1, Ordering::SeqCst);
            if std::mem::take(&mut *self.fail_next.lock().unwrap()) {
                return Err(CoreError::Aws("sso session expired".into()));
            }
            Ok(Built {
                catalog: Arc::new(NullCatalog(self.first_gen + n)),
                expires_at: Some((self.clock)() + self.ttl),
            })
        }
    }

    struct Fixture {
        now: Arc<StdMutex<DateTime<Utc>>>,
        factory: Arc<FakeFactory>,
        cat: Arc<ManagedCatalog>,
    }

    async fn fixture(ttl_min: i64, first_gen: usize) -> Fixture {
        let now = Arc::new(StdMutex::new(
            DateTime::parse_from_rfc3339("2026-11-01T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ));
        let n2 = now.clone();
        let clock: Clock = Arc::new(move || *n2.lock().unwrap());
        let factory = Arc::new(FakeFactory {
            builds: AtomicUsize::new(0),
            ttl: CDuration::minutes(ttl_min),
            clock: clock.clone(),
            fail_next: StdMutex::new(false),
            first_gen,
        });
        let cat = ManagedCatalog::new(factory.clone(), clock).await.unwrap();
        Fixture { now, factory, cat }
    }

    impl Fixture {
        fn advance_min(&self, m: i64) {
            *self.now.lock().unwrap() += CDuration::minutes(m);
        }
        async fn generation(&self) -> String {
            self.cat.list_namespaces(None).await.unwrap()[0].to_url_string()
        }
    }

    #[tokio::test]
    async fn rebuilds_once_when_inside_the_five_minute_margin() {
        let f = fixture(60, 1).await;
        assert_eq!(f.generation().await, "gen1");
        f.advance_min(54);
        assert_eq!(f.generation().await, "gen1", "6 minutes left: keep");
        assert_eq!(f.factory.builds.load(Ordering::SeqCst), 1);
        f.advance_min(2); // 4 minutes left
        assert_eq!(f.generation().await, "gen2");
        assert_eq!(
            f.generation().await,
            "gen2",
            "fresh lease: no second rebuild"
        );
        assert_eq!(f.factory.builds.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn failed_refresh_keeps_the_old_catalog_until_it_really_expires() {
        let f = fixture(60, 1).await;
        f.advance_min(57);
        *f.factory.fail_next.lock().unwrap() = true;
        assert_eq!(
            f.generation().await,
            "gen1",
            "refresh failed but credentials still valid"
        );
        assert_eq!(f.factory.builds.load(Ordering::SeqCst), 2);
        assert_eq!(f.generation().await, "gen1");
        assert_eq!(
            f.factory.builds.load(Ordering::SeqCst),
            2,
            "backing off, no hammering"
        );
    }

    #[tokio::test]
    async fn expired_and_unrefreshable_is_an_error() {
        let f = fixture(60, 1).await;
        f.advance_min(61);
        *f.factory.fail_next.lock().unwrap() = true;
        let e = f.cat.current().await.map(|_| ()).unwrap_err();
        assert!(matches!(e, CoreError::Aws(_)), "{e:?}");
    }

    #[tokio::test]
    async fn auth_failure_triggers_one_rebuild_and_retry() {
        // First generation (0) answers list_tables with 401; the rebuilt one succeeds.
        let f = fixture(10_000, 0).await;
        f.advance_min(1); // past the min gap between rebuilds
        let ns = NamespaceIdent::new("a".into());
        let tables = f.cat.list_tables(&ns).await.unwrap();
        assert!(tables.is_empty());
        assert_eq!(f.factory.builds.load(Ordering::SeqCst), 2);
    }
}
