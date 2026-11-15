//! End-to-end tests of the SSO device login, token refresh, portal API, role-credentials provider
//! and Secrets Manager against an in-process mock server (the real SDK clients, no AWS).

mod aws_common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aws_common::{Handler, MockServer, Req, Resp, serve};
use aws_credential_types::provider::ProvideCredentials;
use chrono::{DateTime, Utc};
use secrecy::ExposeSecret;
use vibelens_core::CoreError;
use vibelens_core::aws::tokens::{SsoCacheEntry, cache_file_name, read_entry, write_entry};
use vibelens_core::aws::{AppCredentialSource, AwsService, AwsServiceConfig};
use vibelens_core::model::*;
use vibelens_core::ports::CredentialSource;
use vibelens_core::vault::MemoryVault;

const START_URL: &str = "https://d-1234567890.awsapps.com/start";
const REGION: &str = "eu-west-1";

struct Env {
    _dir: tempfile::TempDir,
    cache: std::path::PathBuf,
    svc: Arc<AwsService>,
    server: MockServer,
}

async fn env(handler: Handler, creds_file: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let server = serve(handler).await;
    let config = dir.path().join("config");
    let credentials = dir.path().join("credentials");
    std::fs::write(&config, "[profile ci]\nregion = eu-west-1\n").unwrap();
    std::fs::write(&credentials, creds_file).unwrap();
    let cache = dir.path().join("sso/cache");
    let cfg = AwsServiceConfig::with_paths(config, credentials, &cache)
        .endpoint_url(&server.url)
        .poll_interval(Duration::from_millis(20));
    Env {
        svc: AwsService::with_config(cfg),
        cache,
        server,
        _dir: dir,
    }
}

fn register_ok() -> Resp {
    let far = (Utc::now() + chrono::Duration::days(80)).timestamp();
    Resp::ok(&format!(
        r#"{{"clientId":"cid-1","clientSecret":"csecret-1","clientIdIssuedAt":1,"clientSecretExpiresAt":{far}}}"#
    ))
}

fn device_ok(expires_in: i64) -> Resp {
    Resp::ok(&format!(
        r#"{{"deviceCode":"dev-code","userCode":"ABCD-EFGH","verificationUri":"https://device.sso.eu-west-1.amazonaws.com/","verificationUriComplete":"https://device.sso.eu-west-1.amazonaws.com/?user_code=ABCD-EFGH","expiresIn":{expires_in},"interval":1}}"#
    ))
}

fn token_ok(access: &str, refresh: Option<&str>) -> Resp {
    let r = refresh
        .map(|r| format!(r#","refreshToken":"{r}""#))
        .unwrap_or_default();
    Resp::ok(&format!(
        r#"{{"accessToken":"{access}","tokenType":"Bearer","expiresIn":3600{r}}}"#
    ))
}

fn expired_entry(refresh: bool) -> SsoCacheEntry {
    SsoCacheEntry {
        start_url: START_URL.into(),
        region: REGION.into(),
        access_token: "old-access".into(),
        expires_at: Some(Utc::now() - chrono::Duration::hours(1)),
        client_id: Some("cid-old".into()),
        client_secret: Some("csecret-old".into()),
        registration_expires_at: Some(Utc::now() + chrono::Duration::days(10)),
        refresh_token: refresh.then(|| "refresh-old".into()),
        extra: Default::default(),
    }
}

fn portal_handler(token_calls: Arc<Mutex<u32>>) -> Handler {
    Arc::new(
        move |req: &Req| match (req.method.as_str(), req.path_only()) {
            ("POST", "/client/register") => register_ok(),
            ("POST", "/device_authorization") => device_ok(600),
            ("POST", "/token") => {
                let mut n = token_calls.lock().unwrap();
                *n += 1;
                if req.body.contains("refresh_token") {
                    return token_ok("refreshed-access", Some("refresh-new"));
                }
                match *n {
                    1 | 2 => Resp::error(400, "AuthorizationPendingException", "pending"),
                    3 => Resp::error(400, "SlowDownException", "slow down"),
                    _ => token_ok("fresh-access", Some("refresh-1")),
                }
            }
            ("GET", "/assignment/accounts") => match req.query("next_token") {
                None => Resp::ok(
                    r#"{"accountList":[{"accountId":"222222222222","accountName":"Zeta","emailAddress":"z@example.com"}],"nextToken":"page2"}"#,
                ),
                Some(_) => Resp::ok(
                    r#"{"accountList":[{"accountId":"111111111111","accountName":"Alpha","emailAddress":"a@example.com"}]}"#,
                ),
            },
            ("GET", "/assignment/roles") => Resp::ok(
                r#"{"roleList":[{"roleName":"Admin","accountId":"111111111111"},{"roleName":"ReadOnly","accountId":"111111111111"}]}"#,
            ),
            ("GET", "/federation/credentials") => Resp::ok(
                r#"{"roleCredentials":{"accessKeyId":"ASIASSO","secretAccessKey":"ssosecret","sessionToken":"ssotoken","expiration":4102444800000}}"#,
            ),
            _ => Resp::error(404, "NotFound", "no such route"),
        },
    )
}

fn sso_target() -> AwsTarget {
    AwsTarget {
        identity: AwsIdentity::Sso {
            start_url: START_URL.into(),
            sso_region: REGION.into(),
            account_id: "111111111111".into(),
            role_name: "ReadOnly".into(),
        },
        region: REGION.into(),
    }
}

#[tokio::test]
async fn device_login_polls_through_pending_and_slow_down_and_writes_cli_cache() {
    let calls = Arc::new(Mutex::new(0));
    let e = env(portal_handler(calls.clone()), "").await;

    let start = e.svc.sso_start(START_URL, REGION).await.unwrap();
    assert_eq!(start.user_code, "ABCD-EFGH");
    assert_eq!(
        start.verification_uri,
        "https://device.sso.eu-west-1.amazonaws.com/"
    );
    assert!(
        start
            .verification_uri_complete
            .as_deref()
            .unwrap()
            .contains("user_code=ABCD-EFGH")
    );
    assert_eq!(start.interval_secs, 1);
    assert!(start.expires_at > Utc::now() + chrono::Duration::minutes(9));
    assert!(!start.login_id.is_empty());

    // RegisterClient request carries our client identity.
    let reg = e
        .server
        .all()
        .into_iter()
        .find(|r| r.path_only() == "/client/register")
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&reg.body).unwrap();
    assert_eq!(body["clientName"], "VibeLens");
    assert_eq!(body["clientType"], "public");
    assert_eq!(body["scopes"], serde_json::json!(["sso:account:access"]));

    let info = e.svc.sso_complete(&start.login_id).await.unwrap();
    assert_eq!(info.start_url, START_URL);
    assert_eq!(info.region, REGION);
    assert!(info.expires_at.unwrap() > Utc::now() + chrono::Duration::minutes(55));
    assert_eq!(
        *calls.lock().unwrap(),
        4,
        "2x pending, 1x slow_down, then success"
    );

    // Device grant parameters.
    let tok_req = e
        .server
        .all()
        .into_iter()
        .find(|r| r.path_only() == "/token")
        .unwrap();
    let tb: serde_json::Value = serde_json::from_str(&tok_req.body).unwrap();
    assert_eq!(
        tb["grantType"],
        "urn:ietf:params:oauth:grant-type:device_code"
    );
    assert_eq!(tb["deviceCode"], "dev-code");
    assert_eq!(tb["clientId"], "cid-1");

    // CLI-compatible cache file.
    let path = e.cache.join(cache_file_name(START_URL));
    let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(raw["startUrl"], START_URL);
    assert_eq!(raw["region"], REGION);
    assert_eq!(raw["accessToken"], "fresh-access");
    assert_eq!(raw["refreshToken"], "refresh-1");
    assert_eq!(raw["clientId"], "cid-1");
    assert_eq!(raw["clientSecret"], "csecret-1");
    let exp = raw["expiresAt"].as_str().unwrap();
    assert!(
        exp.ends_with('Z') && DateTime::parse_from_rfc3339(exp).is_ok(),
        "{exp}"
    );
    assert!(
        raw["registrationExpiresAt"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    // Pending entry is gone; session is listed.
    assert!(matches!(
        e.svc.sso_complete(&start.login_id).await,
        Err(CoreError::NotFound(_))
    ));
    let sessions = e.svc.sso_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].start_url, START_URL);

    // The client registration is cached: a second login does not re-register.
    e.svc.sso_start(START_URL, REGION).await.unwrap();
    assert_eq!(e.server.count("/client/register"), 1);
}

#[tokio::test]
async fn accounts_roles_and_credentials_via_cached_token() {
    let e = env(portal_handler(Arc::default()), "").await;
    let start = e.svc.sso_start(START_URL, REGION).await.unwrap();
    e.svc.sso_complete(&start.login_id).await.unwrap();

    // Pagination across two pages, sorted by name.
    let accounts = e.svc.sso_list_accounts(START_URL).await.unwrap();
    assert_eq!(
        accounts
            .iter()
            .map(|a| a.account_id.as_str())
            .collect::<Vec<_>>(),
        ["111111111111", "222222222222"]
    );
    assert_eq!(accounts[0].account_name.as_deref(), Some("Alpha"));
    assert_eq!(accounts[0].email.as_deref(), Some("a@example.com"));
    let list_reqs: Vec<_> = e
        .server
        .all()
        .into_iter()
        .filter(|r| r.path_only() == "/assignment/accounts")
        .collect();
    assert_eq!(list_reqs.len(), 2);
    assert!(list_reqs.iter().all(
        |r| r.headers.get("x-amz-sso_bearer_token").map(String::as_str) == Some("fresh-access")
    ));

    let roles = e
        .svc
        .sso_list_roles(START_URL, "111111111111")
        .await
        .unwrap();
    assert_eq!(
        roles,
        vec![
            SsoRole {
                account_id: "111111111111".into(),
                role_name: "Admin".into()
            },
            SsoRole {
                account_id: "111111111111".into(),
                role_name: "ReadOnly".into()
            },
        ]
    );

    // Credentials provider: GetRoleCredentials once, then cached for repeated connects.
    let provider = e.svc.credentials_provider(&sso_target()).await.unwrap();
    let c = provider.provide_credentials().await.unwrap();
    assert_eq!(c.access_key_id(), "ASIASSO");
    assert_eq!(c.session_token(), Some("ssotoken"));
    assert!(c.expiry().is_some());
    let tok = e
        .svc
        .rds_auth_token(&sso_target(), "db.rds.amazonaws.com", 5432, "u")
        .await
        .unwrap();
    assert!(
        tok.expose_secret()
            .contains("X-Amz-Security-Token=ssotoken")
    );
    let tok2 = e
        .svc
        .elasticache_auth_token(&sso_target(), "cache", "u", false)
        .await
        .unwrap();
    assert!(
        tok2.expose_secret()
            .contains("X-Amz-Security-Token=ssotoken")
    );
    assert_eq!(
        e.server.count("/federation/credentials"),
        1,
        "credentials are cached across tokens"
    );
    let gr = e
        .server
        .all()
        .into_iter()
        .find(|r| r.path_only() == "/federation/credentials")
        .unwrap();
    assert_eq!(gr.query("role_name").as_deref(), Some("ReadOnly"));
    assert_eq!(gr.query("account_id").as_deref(), Some("111111111111"));
}

#[tokio::test]
async fn expired_token_is_refreshed_and_cache_rewritten_in_place() {
    let e = env(portal_handler(Arc::default()), "").await;
    // A CLI sso-session style file name (not the sha1 name): the refresh must rewrite *that* file.
    let path = e.cache.join("corp-session.json");
    let mut entry = expired_entry(true);
    entry
        .extra
        .insert("futureField".into(), serde_json::json!(true));
    write_entry(&path, &entry).unwrap();

    let accounts = e.svc.sso_list_accounts(START_URL).await.unwrap();
    assert_eq!(accounts.len(), 2);

    let refresh = e
        .server
        .all()
        .into_iter()
        .find(|r| r.path_only() == "/token")
        .unwrap();
    let b: serde_json::Value = serde_json::from_str(&refresh.body).unwrap();
    assert_eq!(b["grantType"], "refresh_token");
    assert_eq!(b["refreshToken"], "refresh-old");
    assert_eq!(b["clientId"], "cid-old");
    assert_eq!(b["clientSecret"], "csecret-old");

    let after = read_entry(&path).unwrap();
    assert_eq!(after.access_token, "refreshed-access");
    assert_eq!(after.refresh_token.as_deref(), Some("refresh-new"));
    assert_eq!(after.client_id.as_deref(), Some("cid-old"));
    assert!(after.expires_at.unwrap() > Utc::now() + chrono::Duration::minutes(55));
    assert_eq!(
        after.extra.get("futureField"),
        Some(&serde_json::json!(true)),
        "unknown fields survive a refresh"
    );
    assert!(
        !e.cache.join(cache_file_name(START_URL)).exists(),
        "no duplicate file created"
    );
    assert!(
        e.server
            .all()
            .iter()
            .any(|r| r.path_only() == "/assignment/accounts"
                && r.headers.get("x-amz-sso_bearer_token").map(String::as_str)
                    == Some("refreshed-access"))
    );
}

#[tokio::test]
async fn expired_without_refresh_token_asks_to_sign_in_again() {
    let e = env(portal_handler(Arc::default()), "").await;
    write_entry(
        &e.cache.join(cache_file_name(START_URL)),
        &expired_entry(false),
    )
    .unwrap();
    let err = e.svc.sso_list_accounts(START_URL).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m == "SSO session expired — sign in again"),
        "{err:?}"
    );
    assert_eq!(e.server.count("/token"), 0);
    let err = e.svc.sso_list_roles(START_URL, "1").await.unwrap_err();
    assert!(matches!(err, CoreError::Auth(_)));
    // And through the credentials path, the Auth error is preserved (not flattened into Aws).
    let err = e
        .svc
        .rds_auth_token(&sso_target(), "db", 5432, "u")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("sign in again")),
        "{err:?}"
    );
}

#[tokio::test]
async fn failed_refresh_on_an_expired_token_is_an_auth_error() {
    let handler: Handler = Arc::new(|req: &Req| match req.path_only() {
        "/token" => Resp::error(400, "InvalidGrantException", "refresh token revoked"),
        _ => Resp::error(404, "NotFound", "x"),
    });
    let e = env(handler, "").await;
    write_entry(
        &e.cache.join(cache_file_name(START_URL)),
        &expired_entry(true),
    )
    .unwrap();
    let err = e.svc.sso_list_accounts(START_URL).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("sign in again")),
        "{err:?}"
    );
}

#[tokio::test]
async fn revoked_portal_token_maps_to_auth() {
    let handler: Handler = Arc::new(|req: &Req| match req.path_only() {
        "/assignment/accounts" => Resp::error(
            401,
            "UnauthorizedException",
            "Session token not found or invalid",
        ),
        _ => Resp::error(404, "NotFound", "x"),
    });
    let e = env(handler, "").await;
    let mut entry = expired_entry(false);
    entry.expires_at = Some(Utc::now() + chrono::Duration::hours(1));
    write_entry(&e.cache.join(cache_file_name(START_URL)), &entry).unwrap();
    let err = e.svc.sso_list_accounts(START_URL).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("sign in again")),
        "{err:?}"
    );
}

#[tokio::test]
async fn access_denied_and_expired_device_code_are_auth_errors_and_clear_pending() {
    for (code, needle) in [
        ("AccessDeniedException", "denied"),
        ("ExpiredTokenException", "expired"),
    ] {
        let handler: Handler = Arc::new(move |req: &Req| match req.path_only() {
            "/client/register" => register_ok(),
            "/device_authorization" => device_ok(600),
            "/token" => Resp::error(400, code, "nope"),
            _ => Resp::error(404, "NotFound", "x"),
        });
        let e = env(handler, "").await;
        let start = e.svc.sso_start(START_URL, REGION).await.unwrap();
        let err = e.svc.sso_complete(&start.login_id).await.unwrap_err();
        assert!(
            matches!(&err, CoreError::Auth(m) if m.contains(needle)),
            "{code}: {err:?}"
        );
        assert!(
            matches!(
                e.svc.sso_complete(&start.login_id).await,
                Err(CoreError::NotFound(_))
            ),
            "pending cleared"
        );
        assert!(!e.cache.join(cache_file_name(START_URL)).exists());
    }
}

#[tokio::test]
async fn polling_stops_at_the_deadline() {
    let handler: Handler = Arc::new(|req: &Req| match req.path_only() {
        "/client/register" => register_ok(),
        "/device_authorization" => device_ok(1),
        "/token" => Resp::error(400, "AuthorizationPendingException", "pending"),
        _ => Resp::error(404, "NotFound", "x"),
    });
    let e = env(handler, "").await;
    let start = e.svc.sso_start(START_URL, REGION).await.unwrap();
    let started = std::time::Instant::now();
    let err = e.svc.sso_complete(&start.login_id).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("timed out")),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        e.server.count("/token") >= 2,
        "it did poll before giving up"
    );
}

#[tokio::test]
async fn start_validates_input() {
    let e = env(portal_handler(Arc::default()), "").await;
    assert!(matches!(
        e.svc.sso_start("awsapps.com", REGION).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        e.svc.sso_start("http://insecure/start", REGION).await,
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        e.svc.sso_start(START_URL, " ").await,
        Err(CoreError::Invalid(_))
    ));
    assert_eq!(e.server.all().len(), 0);
    assert!(matches!(
        e.svc.sso_complete("nope").await,
        Err(CoreError::NotFound(_))
    ));
}

#[tokio::test]
async fn service_errors_carry_code_and_message() {
    let handler: Handler = Arc::new(|req: &Req| match req.path_only() {
        "/client/register" => Resp::error(400, "InvalidRequestException", "bad region thing"),
        _ => Resp::error(404, "NotFound", "x"),
    });
    let e = env(handler, "").await;
    let err = e.svc.sso_start(START_URL, REGION).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Aws(m) if m.contains("RegisterClient") && m.contains("InvalidRequestException") && m.contains("bad region thing")),
        "{err:?}"
    );
}

// ---- Secrets Manager -------------------------------------------------------------------------

fn secrets_handler(secret_response: Resp) -> Handler {
    Arc::new(move |req: &Req| {
        if req.headers.get("x-amz-target").map(String::as_str)
            == Some("secretsmanager.GetSecretValue")
        {
            if req.body.contains("missing-secret") {
                return Resp::error(
                    400,
                    "ResourceNotFoundException",
                    "Secrets Manager can't find the specified secret.",
                );
            }
            return secret_response.clone();
        }
        Resp::error(404, "NotFound", "x")
    })
}

const CREDS: &str = "[ci]\naws_access_key_id = AKIDCI\naws_secret_access_key = SECRETCI\n";

fn ci_target() -> AwsTarget {
    AwsTarget {
        identity: AwsIdentity::Profile {
            profile_name: "ci".into(),
        },
        region: REGION.into(),
    }
}

fn profile_with(auth: AuthMethod) -> ConnectionProfile {
    ConnectionProfile {
        id: "p9".into(),
        name: "Secret DB".into(),
        engine: EngineKind::Postgres,
        host: "configured.example.com".into(),
        port: 5432,
        database: None,
        auth,
        tls: Default::default(),
        tunnel: None,
        environment: Default::default(),
        ai_data_access: Default::default(),
        color: None,
        pg: Default::default(),
        valkey: Default::default(),
        created_at: None,
        updated_at: None,
        iceberg: None,
    }
}

#[tokio::test]
async fn get_db_secret_and_app_credential_source_secrets_manager() {
    let secret = r#"{\"username\":\"dbuser\",\"password\":\"hunter2\",\"host\":\"real.rds.amazonaws.com\",\"port\":5433,\"dbname\":\"orders\",\"engine\":\"postgres\",\"extra\":1}"#;
    let e = env(
        secrets_handler(Resp::ok(&format!(
            r#"{{"ARN":"arn:x","Name":"n","SecretString":"{secret}"}}"#
        ))),
        CREDS,
    )
    .await;
    let arn = "arn:aws:secretsmanager:eu-west-1:111111111111:secret:db-AbCdEf";

    let s = e.svc.get_db_secret(&ci_target(), arn).await.unwrap();
    assert_eq!(s.username, "dbuser");
    assert_eq!(s.password.expose_secret(), "hunter2");
    assert_eq!(s.host.as_deref(), Some("real.rds.amazonaws.com"));
    assert_eq!(s.port, Some(5433));
    assert_eq!(s.dbname.as_deref(), Some("orders"));
    assert!(!format!("{s:?}").contains("hunter2"));

    // The request was signed with the profile's keys for secretsmanager.
    let req = e.server.all().into_iter().next().unwrap();
    let auth = req.headers.get("authorization").unwrap();
    assert!(
        auth.contains("Credential=AKIDCI/")
            && auth.contains("/eu-west-1/secretsmanager/aws4_request"),
        "{auth}"
    );
    assert!(req.body.contains(arn));

    let src = AppCredentialSource::new(e.svc.clone(), Arc::new(MemoryVault::new()));
    let r = src
        .resolve(&profile_with(AuthMethod::SecretsManager {
            secret_arn: arn.into(),
            aws: ci_target(),
        }))
        .await
        .unwrap();
    assert_eq!(r.username.as_deref(), Some("dbuser"));
    assert_eq!(r.password.as_ref().unwrap().expose_secret(), "hunter2");
    assert_eq!(r.host_override.as_deref(), Some("real.rds.amazonaws.com"));
    assert_eq!(r.port_override, Some(5433));
    assert_eq!(r.database_override.as_deref(), Some("orders"));
    assert!(r.expires_at.is_none());
}

#[tokio::test]
async fn secret_errors_are_clear() {
    // Not JSON.
    let e = env(
        secrets_handler(Resp::ok(r#"{"SecretString":"just-a-password"}"#)),
        CREDS,
    )
    .await;
    let err = e
        .svc
        .get_db_secret(&ci_target(), "arn:ok")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, CoreError::Aws(m) if m.contains("not JSON") && !m.contains("just-a-password")),
        "{err:?}"
    );

    // Binary only.
    let e = env(
        secrets_handler(Resp::ok(r#"{"SecretBinary":"AAEC"}"#)),
        CREDS,
    )
    .await;
    let err = e
        .svc
        .get_db_secret(&ci_target(), "arn:ok")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, CoreError::Aws(m) if m.contains("binary")),
        "{err:?}"
    );

    // Service error.
    let err = e
        .svc
        .get_db_secret(&ci_target(), "missing-secret")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, CoreError::Aws(m) if m.contains("ResourceNotFoundException") && m.contains("can't find")),
        "{err:?}"
    );

    assert!(matches!(
        e.svc.get_db_secret(&ci_target(), "  ").await,
        Err(CoreError::Invalid(_))
    ));
}
