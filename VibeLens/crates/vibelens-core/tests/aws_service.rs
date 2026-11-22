//! Offline tests for `AwsService` / `AppCredentialSource` against fixture files in a temp dir.
//! Nothing here touches the real `~/.aws` or the network.

mod aws_common;

use std::path::Path;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use secrecy::{ExposeSecret, SecretString};
use vibelens_core::CoreError;
use vibelens_core::aws::tokens::{SsoCacheEntry, cache_file_name, write_entry};
use vibelens_core::aws::{AppCredentialSource, AwsService, AwsServiceConfig, fixed_clock};
use vibelens_core::model::*;
use vibelens_core::ports::{CredentialSource, SecretVault};
use vibelens_core::vault::MemoryVault;

const CONFIG: &str = r#"
[default]
region = eu-west-1

[profile ci]
region = eu-west-1

[profile sso-direct]
sso_start_url = https://d-111.awsapps.com/start
sso_region = eu-west-1
sso_account_id = 111111111111
sso_role_name = ReadOnly
region = eu-central-1

[profile sso-via-session]
sso_session = corp
sso_account_id = 222222222222
sso_role_name = Admin

[sso-session corp]
sso_start_url = https://corp.awsapps.com/start
sso_region = eu-west-1

[profile assume]
role_arn = arn:aws:iam::333333333333:role/Deploy
source_profile = ci

[profile proc]
credential_process = /bin/true
"#;

const CREDENTIALS: &str = "[ci]\naws_access_key_id = AKIDCI\naws_secret_access_key = SECRETCI\n\n[legacy]\naws_access_key_id = AKIDL\naws_secret_access_key = S\n";

struct Fixture {
    _dir: tempfile::TempDir,
    config: std::path::PathBuf,
    credentials: std::path::PathBuf,
    cache: std::path::PathBuf,
}

fn fixture(config: &str, credentials: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let config_p = dir.path().join("config");
    let creds_p = dir.path().join("credentials");
    let cache = dir.path().join("sso/cache");
    std::fs::write(&config_p, config).unwrap();
    std::fs::write(&creds_p, credentials).unwrap();
    Fixture {
        _dir: dir,
        config: config_p,
        credentials: creds_p,
        cache,
    }
}

fn clock_2026() -> vibelens_core::aws::Clock {
    fixed_clock(Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap())
}

fn service(f: &Fixture) -> Arc<AwsService> {
    AwsService::with_config(
        AwsServiceConfig::with_paths(&f.config, &f.credentials, &f.cache).clock(clock_2026()),
    )
}

fn profile_target(name: &str) -> AwsTarget {
    AwsTarget {
        identity: AwsIdentity::Profile {
            profile_name: name.into(),
        },
        region: "eu-west-1".into(),
    }
}

fn conn(auth: AuthMethod) -> ConnectionProfile {
    ConnectionProfile {
        id: "p1".into(),
        name: "Test DB".into(),
        engine: EngineKind::Postgres,
        host: "mydb.cluster-abc.eu-west-1.rds.amazonaws.com".into(),
        port: 5432,
        database: Some("app".into()),
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
        redshift: None,
        mysql: Default::default(),
    }
}

#[tokio::test]
async fn list_profiles_reads_both_files_and_classifies() {
    let f = fixture(CONFIG, CREDENTIALS);
    let v = service(&f).list_profiles().await.unwrap();
    let kind = |n: &str| {
        v.iter()
            .find(|p| p.name == n)
            .unwrap_or_else(|| panic!("no {n}"))
            .kind
            .clone()
    };
    assert_eq!(kind("sso-direct"), "sso");
    assert_eq!(kind("sso-via-session"), "sso");
    assert_eq!(kind("assume"), "assumeRole");
    assert_eq!(kind("proc"), "process");
    assert_eq!(kind("ci"), "static");
    assert_eq!(kind("legacy"), "static", "credentials-file-only profile");
    assert_eq!(kind("default"), "other");
    let via = v.iter().find(|p| p.name == "sso-via-session").unwrap();
    assert_eq!(
        via.sso_start_url.as_deref(),
        Some("https://corp.awsapps.com/start")
    );
    assert_eq!(via.sso_account_id.as_deref(), Some("222222222222"));
    assert_eq!(via.sso_role_name.as_deref(), Some("Admin"));
    let names: Vec<_> = v.iter().map(|p| p.name.clone()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
}

#[tokio::test]
async fn list_profiles_with_missing_files_is_empty_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let svc = AwsService::with_config(AwsServiceConfig::with_paths(
        dir.path().join("a"),
        dir.path().join("b"),
        dir.path().join("c"),
    ));
    assert!(svc.list_profiles().await.unwrap().is_empty());
    assert!(svc.sso_sessions().await.unwrap().is_empty());
}

#[tokio::test]
async fn sso_sessions_lists_cache_dir_including_expired() {
    let f = fixture("", "");
    let mk = |url: &str, exp| SsoCacheEntry {
        start_url: url.into(),
        region: "eu-west-1".into(),
        access_token: "t".into(),
        expires_at: Some(exp),
        client_id: None,
        client_secret: None,
        registration_expires_at: None,
        refresh_token: None,
        extra: Default::default(),
    };
    let live = mk(
        "https://live.awsapps.com/start",
        Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).unwrap(),
    );
    let dead = mk(
        "https://dead.awsapps.com/start",
        Utc.with_ymd_and_hms(2001, 1, 1, 0, 0, 0).unwrap(),
    );
    write_entry(&f.cache.join(cache_file_name(&live.start_url)), &live).unwrap();
    write_entry(&f.cache.join(cache_file_name(&dead.start_url)), &dead).unwrap();
    std::fs::write(
        f.cache.join("botocore-client-id-eu-west-1.json"),
        r#"{"clientId":"x"}"#,
    )
    .unwrap();

    let s = service(&f).sso_sessions().await.unwrap();
    assert_eq!(
        s.iter().map(|x| x.start_url.as_str()).collect::<Vec<_>>(),
        [
            "https://dead.awsapps.com/start",
            "https://live.awsapps.com/start"
        ]
    );
    assert_eq!(s[1].expires_at, Some(live.expires_at.unwrap()));
}

#[tokio::test]
async fn app_credential_source_none_and_password() {
    let f = fixture(CONFIG, CREDENTIALS);
    let vault = Arc::new(MemoryVault::new());
    let src = AppCredentialSource::new(service(&f), vault.clone());

    let anon = src.resolve(&conn(AuthMethod::None)).await.unwrap();
    assert!(anon.username.is_none() && anon.password.is_none() && anon.expires_at.is_none());

    let profile = conn(AuthMethod::Password {
        username: "alice".into(),
    });
    let err = src.resolve(&profile).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("no password saved")),
        "{err:?}"
    );

    vault
        .set(&vault_keys::password("p1"), &SecretString::from("s3cret"))
        .await
        .unwrap();
    let ok = src.resolve(&profile).await.unwrap();
    assert_eq!(ok.username.as_deref(), Some("alice"));
    assert_eq!(ok.password.as_ref().unwrap().expose_secret(), "s3cret");
    assert!(ok.expires_at.is_none() && ok.host_override.is_none());
    // Debug output must not contain the password.
    assert!(!format!("{ok:?}").contains("s3cret"));
}

/// A MySQL password is used only for the endpoint it was saved for: the credential source
/// refuses it once the profile points at another host (hand-edited file, direct IPC call).
#[tokio::test]
async fn app_credential_source_binds_a_mysql_password_to_its_endpoint() {
    let f = fixture(CONFIG, CREDENTIALS);
    let vault = Arc::new(MemoryVault::new());
    let src = AppCredentialSource::new(service(&f), vault.clone());
    let mut profile = conn(AuthMethod::Password {
        username: "alice".into(),
    });
    profile.engine = EngineKind::Mysql;
    profile.port = 3306;
    vibelens_core::mysql::secret::save_password(vault.as_ref(), &profile, Some("s3cret".into()))
        .await
        .unwrap();
    let ok = src.resolve(&profile).await.unwrap();
    assert_eq!(ok.password.as_ref().unwrap().expose_secret(), "s3cret");

    let mut moved = profile.clone();
    moved.host = "attacker.example".into();
    let err = src.resolve(&moved).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("different host") && !m.contains("s3cret")),
        "{err:?}"
    );
    // A password written without its binding (e.g. by an older version) is not used either.
    vault
        .set(&vault_keys::password("p1"), &SecretString::from("other"))
        .await
        .unwrap();
    vault
        .delete(&vault_keys::password_origin("p1"))
        .await
        .unwrap();
    assert!(src.resolve(&profile).await.is_err());
    // Postgres passwords keep their old, unbound behaviour.
    let mut pg = profile.clone();
    pg.engine = EngineKind::Postgres;
    assert!(src.resolve(&pg).await.is_ok());
}

/// RDS IAM for MySQL: the token is signed for the profile's real host and MySQL port.
#[tokio::test]
async fn rds_iam_token_for_mysql_names_the_mysql_endpoint() {
    let f = fixture(CONFIG, CREDENTIALS);
    let src = AppCredentialSource::new(service(&f), Arc::new(MemoryVault::new()));
    let mut p = conn(AuthMethod::RdsIam {
        username: "iam_user".into(),
        aws: profile_target("ci"),
    });
    p.engine = EngineKind::Mysql;
    p.host = "mydb.cluster-xyz.eu-west-1.rds.amazonaws.com".into();
    p.port = 3306;
    let c = src.resolve(&p).await.unwrap();
    let tok = c.password.unwrap();
    assert!(
        tok.expose_secret().starts_with(
            "mydb.cluster-xyz.eu-west-1.rds.amazonaws.com:3306/?Action=connect&DBUser=iam_user&"
        ),
        "{}",
        tok.expose_secret()
    );
}

#[tokio::test]
async fn rds_iam_and_elasticache_iam_through_a_static_profile() {
    let f = fixture(CONFIG, CREDENTIALS);
    let svc = service(&f);
    let src = AppCredentialSource::new(svc.clone(), Arc::new(MemoryVault::new()));

    let rds = src
        .resolve(&conn(AuthMethod::RdsIam {
            username: "iam_user".into(),
            aws: profile_target("ci"),
        }))
        .await
        .unwrap();
    assert_eq!(rds.username.as_deref(), Some("iam_user"));
    let tok = rds.password.unwrap();
    let tok = tok.expose_secret();
    assert!(tok.starts_with("mydb.cluster-abc.eu-west-1.rds.amazonaws.com:5432/?Action=connect&DBUser=iam_user&X-Amz-Algorithm=AWS4-HMAC-SHA256&"), "{tok}");
    assert!(
        tok.contains("X-Amz-Credential=AKIDCI%2F20260601%2Feu-west-1%2Frds-db%2Faws4_request"),
        "{tok}"
    );
    assert!(
        tok.contains("X-Amz-Date=20260601T120000Z")
            && tok.contains("X-Amz-Expires=900")
            && tok.contains("X-Amz-SignedHeaders=host")
    );
    assert_eq!(
        rds.expires_at,
        Some(Utc.with_ymd_and_hms(2026, 6, 1, 12, 14, 0).unwrap()),
        "now + 14 min"
    );

    let ec = src
        .resolve(&conn(AuthMethod::ElastiCacheIam {
            username: "cache_user".into(),
            cache_name: "my-valkey".into(),
            serverless: true,
            aws: profile_target("ci"),
        }))
        .await
        .unwrap();
    assert_eq!(ec.username.as_deref(), Some("cache_user"));
    let tok = ec.password.unwrap();
    let tok = tok.expose_secret();
    assert!(tok.starts_with("my-valkey/?Action=connect&User=cache_user&ResourceType=ServerlessCache&X-Amz-Algorithm=AWS4-HMAC-SHA256&"), "{tok}");
    assert!(tok.contains("%2Feu-west-1%2Felasticache%2Faws4_request"));
    assert!(!tok.contains("X-Amz-Security-Token"));
    assert_eq!(
        ec.expires_at,
        Some(Utc.with_ymd_and_hms(2026, 6, 1, 12, 14, 0).unwrap())
    );
}

#[tokio::test]
async fn unknown_profile_is_an_aws_error_without_secrets() {
    let f = fixture(CONFIG, CREDENTIALS);
    let err = service(&f)
        .rds_auth_token(
            &profile_target("does-not-exist"),
            "h.rds.amazonaws.com",
            5432,
            "u",
        )
        .await
        .unwrap_err();
    assert!(matches!(err, CoreError::Aws(_)), "{err:?}");
    let msg = err.to_string();
    assert!(msg.contains("does-not-exist"), "{msg}");
    assert!(!msg.contains("SECRETCI"));
}

#[tokio::test]
async fn invalid_targets_are_rejected_early() {
    let f = fixture(CONFIG, CREDENTIALS);
    let svc = service(&f);
    let mut t = profile_target("ci");
    t.region = " ".into();
    assert!(matches!(
        svc.credentials_provider(&t).await,
        Err(CoreError::Invalid(_))
    ));
    let t = AwsTarget {
        identity: AwsIdentity::Profile {
            profile_name: "".into(),
        },
        region: "eu-west-1".into(),
    };
    assert!(matches!(
        svc.credentials_provider(&t).await,
        Err(CoreError::Invalid(_))
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn credential_process_profile_works_and_provider_is_cached_per_target() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let counter = dir.path().join("count");
    let script = dir.path().join("gen.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho x >> {}\necho '{{\"Version\":1,\"AccessKeyId\":\"AKIDPROC\",\"SecretAccessKey\":\"SECPROC\",\"SessionToken\":\"SESSIONPROC\",\"Expiration\":\"2099-01-01T00:00:00Z\"}}'\n",
            counter.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let f = fixture(
        &format!(
            "[profile viaproc]\nregion = eu-west-1\ncredential_process = {}\n",
            script.display()
        ),
        "",
    );
    let svc = service(&f);
    let target = profile_target("viaproc");

    let a = svc
        .rds_auth_token(&target, "h.rds.amazonaws.com", 5432, "u")
        .await
        .unwrap();
    let b = svc
        .elasticache_auth_token(&target, "c", "u", false)
        .await
        .unwrap();
    assert!(
        a.expose_secret()
            .contains("X-Amz-Security-Token=SESSIONPROC")
    );
    assert!(
        b.expose_secret()
            .contains("X-Amz-Security-Token=SESSIONPROC")
    );
    assert!(a.expose_secret().contains("AKIDPROC%2F"));
    let runs = std::fs::read_to_string(&counter).unwrap().lines().count();
    assert_eq!(
        runs, 1,
        "credential_process must run once; credentials are cached"
    );

    svc.invalidate_credentials();
    svc.rds_auth_token(&target, "h.rds.amazonaws.com", 5432, "u")
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&counter).unwrap().lines().count(),
        2
    );
}

#[tokio::test]
async fn sso_target_without_login_is_an_auth_error() {
    let f = fixture("", "");
    let svc = service(&f);
    let target = AwsTarget {
        identity: AwsIdentity::Sso {
            start_url: "https://nobody.awsapps.com/start".into(),
            sso_region: "eu-west-1".into(),
            account_id: "1".into(),
            role_name: "R".into(),
        },
        region: "eu-west-1".into(),
    };
    let err = svc
        .rds_auth_token(&target, "h", 5432, "u")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("sign in")),
        "{err:?}"
    );
    assert!(matches!(
        svc.sso_list_accounts("https://nobody.awsapps.com/start")
            .await,
        Err(CoreError::Auth(_))
    ));
}

#[test]
fn config_defaults_do_not_panic_without_home() {
    let _ = AwsServiceConfig::from_env();
    let _ = Path::new("/");
}

fn jwt_with_exp(exp: i64) -> String {
    use base64::Engine;
    let e = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "{}.{}.{}",
        e.encode(r#"{"alg":"RS256","typ":"JWT"}"#),
        e.encode(format!(r#"{{"exp":{exp},"email":"dev@example.com"}}"#)),
        e.encode("signature")
    )
}

#[tokio::test]
async fn access_token_from_the_keychain_honours_jwt_expiry() {
    let f = fixture(CONFIG, CREDENTIALS);
    let vault = Arc::new(MemoryVault::new());
    let src = AppCredentialSource::new(service(&f), vault.clone());
    let profile = conn(AuthMethod::AccessToken {
        username: String::new(),
        command: None,
    });
    let err = src.resolve(&profile).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("no access token saved")),
        "{err:?}"
    );

    let expired = jwt_with_exp(1_000_000_000);
    vault
        .set(
            &vault_keys::password("p1"),
            &SecretString::from(expired.clone()),
        )
        .await
        .unwrap();
    let err = src.resolve(&profile).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("expired") && !m.contains(&expired)),
        "{err:?}"
    );

    let fresh = jwt_with_exp(4_102_444_800);
    vault
        .set(
            &vault_keys::password("p1"),
            &SecretString::from(fresh.clone()),
        )
        .await
        .unwrap();
    let ok = src.resolve(&profile).await.unwrap();
    assert_eq!(
        ok.username.as_deref(),
        Some("vibelens"),
        "default startup user"
    );
    assert_eq!(ok.password.as_ref().unwrap().expose_secret(), fresh);
    assert_eq!(ok.expires_at.map(|t| t.timestamp()), Some(4_102_444_800));
    // A pasted token is stored like a password, so its keychain entry is not "unused".
    assert!(
        !profile
            .unused_vault_keys()
            .contains(&vault_keys::password("p1"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn access_token_from_a_command() {
    let f = fixture(CONFIG, CREDENTIALS);
    let src = AppCredentialSource::new(service(&f), Arc::new(MemoryVault::new()));
    let token = jwt_with_exp(4_102_444_800);
    let profile = conn(AuthMethod::AccessToken {
        username: "dev@example.com".into(),
        command: Some(format!(r#"printf '{{"accessToken":"%s"}}' '{token}'"#)),
    });
    let ok = src.resolve(&profile).await.unwrap();
    assert_eq!(ok.username.as_deref(), Some("dev@example.com"));
    assert_eq!(ok.password.as_ref().unwrap().expose_secret(), token);
    // Token produced by a command: nothing is kept in the keychain for it.
    assert!(
        profile
            .unused_vault_keys()
            .contains(&vault_keys::password("p1"))
    );

    let failing = conn(AuthMethod::AccessToken {
        username: String::new(),
        command: Some("echo 'please run az login' >&2; exit 1".into()),
    });
    let err = src.resolve(&failing).await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Auth(m) if m.contains("az login")),
        "{err:?}"
    );
}
