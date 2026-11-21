//! The engine secret (Trino / Livy password or token) is bound to the endpoint it was saved
//! for.
//!
//! The profile file can be edited by hand and `test_connection` can be called directly with a
//! profile id and any URL. Without a binding, either would send the stored secret to an address
//! the user never typed it for. So the vault holds, next to the secret, its origin
//! ([`engine_origin`]: engine kind, scheme, host, port, path prefix, CA file path and a SHA-256
//! of the CA file's contents), written whenever the secret is, and the secret is used only
//! while the profile's current origin is the same. A secret without an origin entry is
//! treated as unbound: the user re-enters it.

use secrecy::{ExposeSecret, SecretString};
use sha2::Digest as _;

use super::config::endpoint_origin;
use crate::error::{CoreError, Result};
use crate::model::{ConnectionProfile, IcebergQueryEngine, vault_keys};
use crate::ports::SecretVault;

fn engine_of(profile: &ConnectionProfile) -> Option<&IcebergQueryEngine> {
    profile.iceberg.as_ref().map(|o| &o.query_engine)
}

/// The origin an engine secret is bound to: [`endpoint_origin`] plus `|sha256:<hex>` of the CA
/// bundle's contents (empty without one), so replacing the file's contents unbinds the secret
/// too. A CA file that cannot be read is an error, as it is when the engine is built.
/// `Ok(None)` for engines without an HTTP endpoint.
pub fn engine_origin(engine: &IcebergQueryEngine) -> Result<Option<String>> {
    let Some(endpoint) = endpoint_origin(engine) else {
        return Ok(None);
    };
    let ca = match engine {
        IcebergQueryEngine::Trino { ca_cert_path, .. }
        | IcebergQueryEngine::Spark { ca_cert_path, .. } => ca_cert_path.as_deref(),
        _ => None,
    };
    let hash = match ca.map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => {
            let pem = std::fs::read(path)
                .map_err(|e| CoreError::Invalid(format!("cannot read the CA file {path}: {e}")))?;
            hex::encode(sha2::Sha256::digest(&pem))
        }
        None => String::new(),
    };
    Ok(Some(format!("{endpoint}|sha256:{hash}")))
}

/// Saves the engine secret of `profile` with `save_profile`'s semantics: `Some("")` deletes it,
/// `Some(s)` stores it bound to the profile's current origin, `None` keeps it, unless the
/// profile now points somewhere else than the secret was saved for, in which case the secret
/// is deleted (it is not to follow the profile to another endpoint).
pub async fn save_engine_secret(
    vault: &dyn SecretVault,
    profile: &ConnectionProfile,
    secret: Option<String>,
) -> Result<()> {
    let key = vault_keys::engine_secret(&profile.id);
    let origin_key = vault_keys::engine_secret_origin(&profile.id);
    if matches!(&secret, Some(s) if s.is_empty()) {
        vault.delete(&key).await?;
        return vault.delete(&origin_key).await;
    }
    let origin = match engine_of(profile) {
        Some(e) => engine_origin(e)?,
        None => None,
    };
    match (secret, origin) {
        (Some(s), Some(origin)) => {
            // Unbind first, then write the secret, then bind it. If a write fails half-way, no
            // secret is left bound to an endpoint it was not saved for: the old origin is gone,
            // or the new secret has none yet. Either fails closed.
            vault.delete(&origin_key).await?;
            vault.set(&key, &SecretString::from(s)).await?;
            vault.set(&origin_key, &SecretString::from(origin)).await
        }
        (Some(_), None) => {
            vault.delete(&key).await?;
            vault.delete(&origin_key).await
        }
        (None, origin) => {
            let stored = vault.get(&origin_key).await?;
            let same = matches!((&stored, &origin), (Some(s), Some(o)) if s.expose_secret() == o);
            if !same {
                vault.delete(&key).await?;
                vault.delete(&origin_key).await?;
            }
            Ok(())
        }
    }
}

/// The stored engine secret for `profile_id`, if it was saved for `engine`'s current origin.
/// `Ok(None)` when none is stored; `Err(Auth)` when one is stored for another endpoint (or
/// without an origin), telling the user to enter it again.
pub async fn load_engine_secret(
    vault: &dyn SecretVault,
    profile_id: &str,
    engine: &IcebergQueryEngine,
) -> Result<Option<SecretString>> {
    let Some(secret) = vault.get(&vault_keys::engine_secret(profile_id)).await? else {
        return Ok(None);
    };
    let stored = vault
        .get(&vault_keys::engine_secret_origin(profile_id))
        .await?;
    let current = engine_origin(engine)?;
    match (stored, current) {
        (Some(s), Some(c)) if s.expose_secret() == c => Ok(Some(secret)),
        _ => Err(CoreError::Auth(
            "the stored query engine password or token was saved for a different URL, CA file \
             or engine. Enter it again in the connection settings so it is used for this one"
                .into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::MemoryVault;

    fn profile(url: &str) -> ConnectionProfile {
        serde_json::from_value(serde_json::json!({
            "id": "p", "name": "p", "engine": "iceberg", "host": "", "port": 0,
            "database": null, "auth": {"kind": "none"}, "tunnel": null, "color": null,
            "createdAt": null, "updatedAt": null,
            "iceberg": {"catalog": {"kind": "hms", "address": "h:9083", "warehouse": "w"},
                        "queryEngine": {"kind": "trino", "url": url, "catalog": "c", "user": "u",
                                        "auth": {"kind": "bearer"}},
                        "defaultNamespace": null},
        }))
        .unwrap()
    }

    fn engine(p: &ConnectionProfile) -> &IcebergQueryEngine {
        engine_of(p).unwrap()
    }

    /// Audit 2026-11-21, item 5. Without the binding, editing the URL in the profile file (or
    /// calling test_connection with another URL) sends the stored token to that URL.
    #[tokio::test]
    async fn a_stored_secret_is_used_only_for_the_endpoint_it_was_saved_for() {
        let vault = MemoryVault::new();
        let good = profile("https://trino.example/");
        save_engine_secret(&vault, &good, Some("tok".into()))
            .await
            .unwrap();
        let s = load_engine_secret(&vault, "p", engine(&good))
            .await
            .unwrap();
        assert_eq!(s.unwrap().expose_secret(), "tok");

        for evil in [
            "https://attacker.example/",
            "https://trino.example:8443/",
            "https://trino.example/other/",
        ] {
            let e = load_engine_secret(&vault, "p", engine(&profile(evil)))
                .await
                .unwrap_err();
            assert!(matches!(e, CoreError::Auth(_)), "{evil}");
        }

        // Saving the profile with another URL and no new secret deletes the old secret.
        save_engine_secret(&vault, &profile("https://attacker.example/"), None)
            .await
            .unwrap();
        assert!(
            vault
                .get("profile:p:engine-secret")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            load_engine_secret(&vault, "p", engine(&good))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// A vault whose writes of one key fail.
    struct FailingVault {
        inner: MemoryVault,
        fail_key: String,
    }

    #[async_trait::async_trait]
    impl SecretVault for FailingVault {
        async fn get(&self, key: &str) -> Result<Option<SecretString>> {
            self.inner.get(key).await
        }
        async fn set(&self, key: &str, value: &SecretString) -> Result<()> {
            if key == self.fail_key {
                return Err(CoreError::Vault("keychain write failed".into()));
            }
            self.inner.set(key, value).await
        }
        async fn delete(&self, key: &str) -> Result<()> {
            self.inner.delete(key).await
        }
    }

    /// Re-audit item 7: when the origin write fails half-way, no secret may end up bound to an
    /// endpoint it was not saved for.
    #[tokio::test]
    async fn a_failed_origin_write_leaves_nothing_bound() {
        let vault = FailingVault {
            inner: MemoryVault::new(),
            fail_key: String::new(),
        };
        let old = profile("https://trino.example/");
        save_engine_secret(&vault, &old, Some("old-token".into()))
            .await
            .unwrap();
        let vault = FailingVault {
            inner: vault.inner,
            fail_key: "profile:p:engine-secret-origin".into(),
        };
        let new = profile("https://other.example/");
        assert!(
            save_engine_secret(&vault, &new, Some("new-token".into()))
                .await
                .is_err()
        );
        // Neither the old nor the new endpoint gets a secret.
        assert!(load_engine_secret(&vault, "p", engine(&new)).await.is_err());
        assert!(load_engine_secret(&vault, "p", engine(&old)).await.is_err());
    }

    /// Re-audit item 7: the origin covers the CA bundle's contents, not only its path.
    #[tokio::test]
    async fn changing_the_ca_file_contents_unbinds_the_secret() {
        let dir = tempfile::tempdir().unwrap();
        let ca = dir.path().join("ca.pem");
        std::fs::write(
            &ca,
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        let mut p = profile("https://trino.example/");
        if let Some(IcebergQueryEngine::Trino { ca_cert_path, .. }) =
            p.iceberg.as_mut().map(|o| &mut o.query_engine)
        {
            *ca_cert_path = Some(ca.display().to_string());
        }
        let vault = MemoryVault::new();
        save_engine_secret(&vault, &p, Some("tok".into()))
            .await
            .unwrap();
        assert!(
            load_engine_secret(&vault, "p", engine(&p))
                .await
                .unwrap()
                .is_some()
        );
        std::fs::write(
            &ca,
            "-----BEGIN CERTIFICATE-----\nBBBB\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        assert!(matches!(
            load_engine_secret(&vault, "p", engine(&p)).await,
            Err(CoreError::Auth(_))
        ));
        std::fs::remove_file(&ca).unwrap();
        assert!(matches!(
            load_engine_secret(&vault, "p", engine(&p)).await,
            Err(CoreError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn saving_keeps_deletes_and_rebinds() {
        let vault = MemoryVault::new();
        let p = profile("https://trino.example");
        save_engine_secret(&vault, &p, Some("tok".into()))
            .await
            .unwrap();
        // Unchanged origin, no new secret: kept.
        save_engine_secret(&vault, &p, None).await.unwrap();
        assert!(
            load_engine_secret(&vault, "p", engine(&p))
                .await
                .unwrap()
                .is_some()
        );
        // "" deletes.
        save_engine_secret(&vault, &p, Some(String::new()))
            .await
            .unwrap();
        assert!(
            load_engine_secret(&vault, "p", engine(&p))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            vault
                .get("profile:p:engine-secret-origin")
                .await
                .unwrap()
                .is_none()
        );
        // A secret stored without an origin (before this binding existed) is not used.
        vault
            .set(
                "profile:p:engine-secret",
                &SecretString::from("old".to_string()),
            )
            .await
            .unwrap();
        assert!(load_engine_secret(&vault, "p", engine(&p)).await.is_err());
    }
}
