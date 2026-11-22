//! What a MySQL profile must satisfy, checked in Rust on save and again before every connect
//! or test (`cleartext::check_profile`), because the profile file can be edited by hand and IPC
//! can be called directly.
//!
//! Credentials and channels:
//! - Authentication is `password` (vault) or `rdsIam` (a 15-minute token per connection).
//! - An RDS IAM token is a bearer credential sent with the cleartext plugin, so it needs TLS
//!   with verification (`verifyFull`) in every case, tunnel or not, this machine or not.
//! - A password goes to another machine only over verified TLS (`verifyFull`), tunnel or not:
//!   `mysql_native_password` sends a hash that can be replayed against the server's stored
//!   hash and attacked offline, and `caching_sha2_password` full authentication sends the
//!   password itself. Without verification an attacker in the path can ask for either. On this
//!   machine (`localhost`, `127.0.0.0/8`, `::1`, no tunnel) any TLS mode is accepted.

use crate::cleartext::is_loopback_host;
use crate::error::{CoreError, Result};
use crate::model::{
    AuthMethod, ConnectionProfile, MYSQL_MAX_CONNECT_TIMEOUT_SECS, MYSQL_MAX_POOL_SIZE, TlsMode,
};

fn invalid<T>(m: impl Into<String>) -> Result<T> {
    Err(CoreError::Invalid(m.into()))
}

/// MySQL identifiers (database, user) are at most 64 characters (users 32) and may not hold
/// NUL or other control characters.
fn check_name(what: &str, value: &str, max_chars: usize) -> Result<()> {
    if value.trim().is_empty() {
        return invalid(format!("the {what} is required"));
    }
    if value.chars().count() > max_chars {
        return invalid(format!("the {what} is longer than {max_chars} characters"));
    }
    if value.chars().any(char::is_control) {
        return invalid(format!("the {what} contains a control character"));
    }
    if value != value.trim() {
        return invalid(format!("the {what} starts or ends with a space; remove it"));
    }
    Ok(())
}

/// The database name a MySQL call may use (a profile's default, or one chosen in the UI or by
/// the agent): it is sent in the handshake, so it is validated like the profile's.
pub fn check_database_name(name: &str) -> Result<()> {
    check_name("database name", name, 64)
}

/// Every rule above for a MySQL profile.
pub fn check_profile(p: &ConnectionProfile) -> Result<()> {
    let host = p.host.trim();
    if host.is_empty() {
        return invalid("host is required");
    }
    if host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return invalid("the host contains spaces or control characters");
    }
    if p.port == 0 {
        return invalid("port must be between 1 and 65535");
    }
    check_database_name(p.database.as_deref().unwrap_or(""))
        .map_err(|_| CoreError::Invalid("a default database is required for MySQL connections (1 to 64 characters, no control characters)".into()))?;
    let o = &p.mysql;
    if !(1..=MYSQL_MAX_CONNECT_TIMEOUT_SECS).contains(&o.connect_timeout_secs) {
        return invalid(format!(
            "the connect timeout must be between 1 and {MYSQL_MAX_CONNECT_TIMEOUT_SECS} seconds"
        ));
    }
    if !(1..=MYSQL_MAX_POOL_SIZE).contains(&o.max_pool_size) {
        return invalid(format!(
            "the pool size must be between 1 and {MYSQL_MAX_POOL_SIZE}"
        ));
    }
    if o.statement_timeout_ms == Some(0) {
        return invalid("the statement timeout must be at least 1 ms, or empty for none");
    }
    let remote = p.tunnel.is_some() || !is_loopback_host(host);
    match &p.auth {
        AuthMethod::RdsIam { username, aws } => {
            check_name("user name", username, 32)?;
            if aws.region.trim().is_empty() {
                return invalid("the AWS region is required for RDS IAM authentication");
            }
            if p.tls.mode != TlsMode::VerifyFull {
                return invalid(
                    "RDS IAM authentication sends the token with the cleartext plugin, so it \
                     needs TLS with certificate verification (Verify full; turn on the Amazon \
                     RDS CA bundle for RDS and Aurora)",
                );
            }
        }
        AuthMethod::Password { username } => {
            check_name("user name", username, 32)?;
            if remote && p.tls.mode != TlsMode::VerifyFull {
                return invalid(format!(
                    "TLS with certificate verification (Verify full) is required to send a \
                     password to {host}: without it, whoever is in the path can obtain a \
                     reusable credential. Turn on Verify full (with the server's CA file, or the \
                     Amazon RDS CA bundle for RDS and Aurora). Connections to this machine \
                     (localhost, no tunnel) may use any TLS mode",
                ));
            }
        }
        _ => {
            return invalid("MySQL connections use password or RDS IAM authentication");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    pub(crate) fn profile(host: &str, auth: AuthMethod, tls: TlsMode) -> ConnectionProfile {
        ConnectionProfile {
            id: "m".into(),
            name: "m".into(),
            engine: EngineKind::Mysql,
            host: host.into(),
            port: 3306,
            database: Some("shop".into()),
            auth,
            tls: TlsConfig {
                mode: tls,
                ..Default::default()
            },
            tunnel: None,
            environment: Environment::Dev,
            ai_data_access: AiDataAccess::default(),
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

    fn pw() -> AuthMethod {
        AuthMethod::Password {
            username: "app".into(),
        }
    }

    fn iam() -> AuthMethod {
        AuthMethod::RdsIam {
            username: "app".into(),
            aws: AwsTarget {
                identity: AwsIdentity::DefaultChain,
                region: "eu-west-1".into(),
            },
        }
    }

    fn msg(r: Result<()>) -> String {
        match r {
            Err(CoreError::Invalid(m)) => m,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_password_to_another_machine_needs_verified_tls() {
        for tls in [TlsMode::Disable, TlsMode::Require] {
            let m = msg(check_profile(&profile("db.internal", pw(), tls)));
            assert!(
                m.contains("Verify full") && m.contains("db.internal"),
                "{m}"
            );
            // A tunnel does not change that: the hop from the bastion is not ours.
            let mut t = profile("db.internal", pw(), tls);
            t.tunnel = Some(TunnelConfig::Ssh {
                host: "bastion".into(),
                port: 22,
                username: "u".into(),
                auth: SshAuth::Agent,
                known_host_fingerprint: None,
            });
            msg(check_profile(&t));
            // Not even a tunnel to localhost.
            let mut t = profile("127.0.0.1", pw(), tls);
            t.tunnel = Some(TunnelConfig::Ssh {
                host: "bastion".into(),
                port: 22,
                username: "u".into(),
                auth: SshAuth::Agent,
                known_host_fingerprint: None,
            });
            msg(check_profile(&t));
            // This machine: any mode.
            check_profile(&profile("127.0.0.1", pw(), tls)).unwrap();
            check_profile(&profile("localhost", pw(), tls)).unwrap();
        }
        check_profile(&profile("db.internal", pw(), TlsMode::VerifyFull)).unwrap();
    }

    #[test]
    fn an_iam_token_always_needs_verified_tls() {
        for host in ["db.internal", "127.0.0.1"] {
            for tls in [TlsMode::Disable, TlsMode::Require] {
                let m = msg(check_profile(&profile(host, iam(), tls)));
                assert!(m.contains("cleartext plugin"), "{m}");
            }
            check_profile(&profile(host, iam(), TlsMode::VerifyFull)).unwrap();
        }
    }

    #[test]
    fn other_auth_methods_and_bad_fields_are_refused() {
        for auth in [
            AuthMethod::None,
            AuthMethod::AccessToken {
                username: "u".into(),
                command: None,
            },
            AuthMethod::SecretsManager {
                secret_arn: "arn".into(),
                aws: AwsTarget {
                    identity: AwsIdentity::DefaultChain,
                    region: "eu-west-1".into(),
                },
            },
        ] {
            msg(check_profile(&profile(
                "localhost",
                auth,
                TlsMode::VerifyFull,
            )));
        }
        let mut p = profile("localhost", pw(), TlsMode::Disable);
        p.database = None;
        assert!(msg(check_profile(&p)).contains("default database"));
        p.database = Some("bad\0name".into());
        msg(check_profile(&p));
        let mut p = profile("localhost", pw(), TlsMode::Disable);
        p.mysql.max_pool_size = 0;
        msg(check_profile(&p));
        p.mysql.max_pool_size = 4;
        p.mysql.connect_timeout_secs = 0;
        msg(check_profile(&p));
        let mut p = profile(
            "localhost",
            AuthMethod::Password {
                username: "x".repeat(33),
            },
            TlsMode::Disable,
        );
        msg(check_profile(&p));
        p.auth = pw();
        p.port = 0;
        msg(check_profile(&p));
        p.port = 3306;
        p.host = "db host".into();
        msg(check_profile(&p));
    }
}
