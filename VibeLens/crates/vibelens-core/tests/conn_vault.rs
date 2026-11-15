//! MemoryVault behaviour through the `SecretVault` port, and KeychainVault construction.

use secrecy::{ExposeSecret, SecretString};
use vibelens_core::model::vault_keys;
use vibelens_core::ports::SecretVault;
use vibelens_core::vault::{KeychainVault, MemoryVault};

fn secret(s: &str) -> SecretString {
    SecretString::from(s.to_string())
}

#[tokio::test]
async fn conn_memory_vault_crud_via_trait_object() {
    let vault: std::sync::Arc<dyn SecretVault> = std::sync::Arc::new(MemoryVault::new());
    let key = vault_keys::password("abc");
    assert!(vault.get(&key).await.unwrap().is_none());
    vault.set(&key, &secret("s3cret")).await.unwrap();
    assert_eq!(
        vault.get(&key).await.unwrap().unwrap().expose_secret(),
        "s3cret"
    );
    vault.set(&key, &secret("rotated")).await.unwrap();
    assert_eq!(
        vault.get(&key).await.unwrap().unwrap().expose_secret(),
        "rotated"
    );
    vault.delete(&key).await.unwrap();
    assert!(vault.get(&key).await.unwrap().is_none());
    vault.delete(&key).await.unwrap();
}

#[tokio::test]
async fn conn_memory_vault_keys_are_independent() {
    let vault = MemoryVault::new();
    vault
        .set(&vault_keys::ssh_password("p"), &secret("one"))
        .await
        .unwrap();
    vault
        .set(&vault_keys::ssh_passphrase("p"), &secret("two"))
        .await
        .unwrap();
    vault
        .set(&vault_keys::llm_api_key("anthropic"), &secret("three"))
        .await
        .unwrap();
    assert_eq!(
        vault
            .get(&vault_keys::ssh_password("p"))
            .await
            .unwrap()
            .unwrap()
            .expose_secret(),
        "one"
    );
    assert_eq!(
        vault
            .get(&vault_keys::ssh_passphrase("p"))
            .await
            .unwrap()
            .unwrap()
            .expose_secret(),
        "two"
    );
    assert!(
        vault
            .get(&vault_keys::password("p"))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn conn_memory_vault_is_shared_across_tasks() {
    let vault = std::sync::Arc::new(MemoryVault::new());
    let mut tasks = Vec::new();
    for i in 0..16 {
        let v = vault.clone();
        tasks.push(tokio::spawn(async move {
            v.set(&format!("k{i}"), &SecretString::from(format!("v{i}")))
                .await
                .unwrap()
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    for i in 0..16 {
        assert_eq!(
            vault
                .get(&format!("k{i}"))
                .await
                .unwrap()
                .unwrap()
                .expose_secret(),
            &format!("v{i}")
        );
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[test]
fn conn_keychain_vault_reports_missing_backend_on_other_platforms() {
    let err = KeychainVault::new().expect_err("no native keychain here");
    assert_eq!(err.code(), "vault");
    assert!(
        err.to_string()
            .contains("no OS keychain backend on this platform"),
        "{err}"
    );
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[test]
fn conn_keychain_vault_constructs_on_supported_platforms() {
    KeychainVault::new().unwrap();
}
