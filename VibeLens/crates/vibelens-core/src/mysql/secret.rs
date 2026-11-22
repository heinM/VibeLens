//! A MySQL password is bound to the endpoint it was saved for (R1).
//!
//! The profile file can be edited by hand and `test_connection` can be called directly. Without
//! a binding, either could point a saved profile at another server, and `caching_sha2_password`
//! lets that server ask for the password itself. So the vault holds, next to the password, its
//! origin ([`password_origin`]: host, port, user, TLS mode, CA file and a SHA-256 of its
//! contents, the RDS bundle switch and the SSH tunnel endpoint), written whenever the password
//! is, and the password is used only while the profile's current origin is the same. This is
//! the scheme of `remote::secret` (Trino and Livy secrets).

use secrecy::{ExposeSecret, SecretString};
use sha2::Digest as _;

use crate::error::{CoreError, Result};
use crate::model::{ConnectionProfile, TlsMode, TunnelConfig, vault_keys};
use crate::ports::SecretVault;

/// Where a password saved for `p` may be sent. A CA file that cannot be read is an error.
pub fn password_origin(p: &ConnectionProfile) -> Result<String> {
    let user = match &p.auth {
        crate::model::AuthMethod::Password { username } => username.as_str(),
        _ => "",
    };
    let tls = match p.tls.mode {
        TlsMode::Disable => "disable",
        TlsMode::Require => "require",
        TlsMode::VerifyFull => "verify-full",
    };
    let ca = match p
        .tls
        .ca_cert_path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(path) => {
            let pem = std::fs::read(path)
                .map_err(|e| CoreError::Invalid(format!("cannot read the CA file {path}: {e}")))?;
            format!("{path}#sha256:{}", hex::encode(sha2::Sha256::digest(&pem)))
        }
        None => String::new(),
    };
    let tunnel = match &p.tunnel {
        Some(TunnelConfig::Ssh {
            host,
            port,
            username,
            ..
        }) => format!(
            "ssh://{username}@{}:{port}",
            host.trim().to_ascii_lowercase()
        ),
        None => String::new(),
    };
    Ok(format!(
        "mysql|{}|{}|{user}|{tls}|ca={ca}|rds={}|tunnel={tunnel}",
        p.host.trim().to_ascii_lowercase(),
        p.port,
        p.tls.use_rds_bundle
    ))
}

/// Saves the password of a MySQL profile with `save_profile`'s semantics: `Some("")` deletes
/// it, `Some(s)` stores it bound to the profile's current origin, `None` keeps it unless the
/// profile now points somewhere else than it was saved for, in which case it is deleted.
pub async fn save_password(
    vault: &dyn SecretVault,
    p: &ConnectionProfile,
    password: Option<String>,
) -> Result<()> {
    let key = vault_keys::password(&p.id);
    let origin_key = vault_keys::password_origin(&p.id);
    match password {
        Some(s) if s.is_empty() || !p.auth.uses_stored_password() => {
            vault.delete(&key).await?;
            vault.delete(&origin_key).await
        }
        Some(s) => {
            let origin = password_origin(p)?;
            // Unbind, write, bind: a failure half-way leaves nothing bound to the wrong place.
            vault.delete(&origin_key).await?;
            vault.set(&key, &SecretString::from(s)).await?;
            vault.set(&origin_key, &SecretString::from(origin)).await
        }
        None => {
            let current = password_origin(p).ok();
            let stored = vault.get(&origin_key).await?;
            let same = matches!((&stored, &current), (Some(s), Some(c)) if s.expose_secret() == c);
            if !same {
                vault.delete(&key).await?;
                vault.delete(&origin_key).await?;
            }
            Ok(())
        }
    }
}

/// The stored password of `p`, if it was saved for `p`'s current origin. `Ok(None)` when none
/// is stored; `Err(Auth)` when one is stored for another endpoint (or without an origin).
pub async fn load_password(
    vault: &dyn SecretVault,
    p: &ConnectionProfile,
) -> Result<Option<SecretString>> {
    let Some(secret) = vault.get(&vault_keys::password(&p.id)).await? else {
        return Ok(None);
    };
    let stored = vault.get(&vault_keys::password_origin(&p.id)).await?;
    let current = password_origin(p)?;
    match stored {
        Some(s) if s.expose_secret() == current => Ok(Some(secret)),
        _ => Err(CoreError::Auth(format!(
            "the password saved for \"{}\" was saved for a different host, port, user, TLS \
             setting or tunnel. Enter it again in the connection settings so it is used for \
             this one",
            p.name
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::vault::MemoryVault;

    fn profile(host: &str) -> ConnectionProfile {
        let mut p: ConnectionProfile = serde_json::from_value(serde_json::json!({
            "id": "p", "name": "shop", "engine": "mysql", "host": host, "port": 3306,
            "database": "shop", "auth": {"kind": "password", "username": "app"},
            "tls": {"mode": "verifyFull", "caCertPath": null}, "tunnel": null, "color": null,
            "createdAt": null, "updatedAt": null,
        }))
        .unwrap();
        p.tls.use_rds_bundle = true;
        p
    }

    /// Without the binding, editing the host in the profile file (or calling test_connection
    /// with another host) sends the stored password there.
    #[tokio::test]
    async fn a_password_is_used_only_for_the_endpoint_it_was_saved_for() {
        let vault = MemoryVault::new();
        let good = profile("db.example");
        save_password(&vault, &good, Some("pw".into()))
            .await
            .unwrap();
        let s = load_password(&vault, &good).await.unwrap().unwrap();
        assert_eq!(s.expose_secret(), "pw");

        let edits: [fn(&mut ConnectionProfile); 6] = [
            |p| p.host = "attacker.example".into(),
            |p| p.port = 3307,
            |p| {
                p.auth = AuthMethod::Password {
                    username: "root".into(),
                }
            },
            |p| p.tls.mode = TlsMode::Require,
            |p| p.tls.use_rds_bundle = false,
            |p| {
                p.tunnel = Some(TunnelConfig::Ssh {
                    host: "evil-bastion".into(),
                    port: 22,
                    username: "u".into(),
                    auth: SshAuth::Agent,
                    known_host_fingerprint: None,
                })
            },
        ];
        for edit in &edits {
            let mut evil = good.clone();
            edit(&mut evil);
            let e = load_password(&vault, &evil).await.unwrap_err();
            assert!(matches!(e, CoreError::Auth(_)), "{e:?}");
            assert!(!e.to_string().contains("pw\""));
        }
        // Saving the profile pointed elsewhere without a new password deletes it.
        save_password(&vault, &profile("attacker.example"), None)
            .await
            .unwrap();
        assert!(load_password(&vault, &good).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn keep_delete_and_unbound_passwords() {
        let vault = MemoryVault::new();
        let p = profile("db.example");
        save_password(&vault, &p, Some("pw".into())).await.unwrap();
        save_password(&vault, &p, None).await.unwrap();
        assert!(load_password(&vault, &p).await.unwrap().is_some());
        save_password(&vault, &p, Some(String::new()))
            .await
            .unwrap();
        assert!(load_password(&vault, &p).await.unwrap().is_none());
        assert!(
            vault
                .get("profile:p:password-origin")
                .await
                .unwrap()
                .is_none()
        );
        // A password without an origin (written by something else) is not used.
        vault
            .set("profile:p:password", &SecretString::from("x".to_string()))
            .await
            .unwrap();
        assert!(matches!(
            load_password(&vault, &p).await,
            Err(CoreError::Auth(_))
        ));
    }

    #[tokio::test]
    async fn changing_the_ca_file_contents_unbinds_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(&ca, "A").unwrap();
        let mut p = profile("db.example");
        p.tls.ca_cert_path = Some(ca.display().to_string());
        let vault = MemoryVault::new();
        save_password(&vault, &p, Some("pw".into())).await.unwrap();
        assert!(load_password(&vault, &p).await.unwrap().is_some());
        std::fs::write(&ca, "B").unwrap();
        assert!(matches!(
            load_password(&vault, &p).await,
            Err(CoreError::Auth(_))
        ));
    }
}
