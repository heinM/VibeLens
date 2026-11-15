//! Secret vaults (OWNER: connections agent).
//!
//! - [`KeychainVault`]: `keyring-core` with the native store for the platform
//!   (`apple-native-keyring-store` on macOS, `windows-native-keyring-store` on Windows).
//!   On other platforms (Linux dev/CI) [`KeychainVault::new`] fails with
//!   `CoreError::Vault("no OS keychain backend on this platform")` and the app falls back to
//!   [`MemoryVault`] (and shows a warning banner).
//! - [`MemoryVault`]: for tests and as an explicit fallback. Nothing is persisted.
//!
//! Keychain calls are blocking (Security.framework / wincred), so every call runs in
//! `spawn_blocking`. The native stores document that concurrent access to one credential from
//! several threads is not reliably ordered, so all keychain operations of one vault are
//! serialised behind a mutex.
//!
//! Entries are addressed as `Entry(service = vault_keys::SERVICE, user = <key>)`.
//!
//! - [`BundledVault`]: what macOS uses. The login keychain authorises per item and per code
//!   signature, so one item per secret means one prompt per secret. All secrets therefore live
//!   as JSON in the single item [`vault_keys::BUNDLE`], read once per process. Windows stays on
//!   plain [`KeychainVault`]: Credential Manager never prompts and caps a blob at 2560 bytes.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use keyring_core::api::CredentialStore;
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroize;

use crate::error::{CoreError, Result};
use crate::model::vault_keys;
use crate::ports::SecretVault;

/// The OS vault for this platform. Errors on platforms without a native backend.
pub fn native_vault() -> Result<Arc<dyn SecretVault>> {
    let vault = KeychainVault::new()?;
    if cfg!(target_os = "macos") {
        Ok(Arc::new(BundledVault::new(vault)))
    } else {
        Ok(Arc::new(vault))
    }
}

/// OS keychain backed vault (macOS Keychain / Windows Credential Manager).
pub struct KeychainVault {
    store: Arc<CredentialStore>,
    /// Serialises access: the native stores don't sequence same-credential access across threads.
    gate: Arc<Mutex<()>>,
}

impl fmt::Debug for KeychainVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeychainVault").finish_non_exhaustive()
    }
}

impl KeychainVault {
    /// Initialises the platform credential store (login keychain on macOS, Credential Manager
    /// on Windows). Errors on platforms without a native backend.
    pub fn new() -> Result<Self> {
        Ok(Self::with_store(native_store()?))
    }

    /// Builds a vault over an explicit credential store. Used by [`KeychainVault::new`] and by
    /// tests (with `keyring_core::mock::Store`), so the vault logic is testable on any OS.
    pub fn with_store(store: Arc<CredentialStore>) -> Self {
        Self {
            store,
            gate: Arc::new(Mutex::new(())),
        }
    }

    /// Runs `f` with the entry for `key` on the blocking pool.
    async fn with_entry<T, F>(&self, service: &'static str, key: &str, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&keyring_core::Entry) -> keyring_core::Result<T> + Send + 'static,
    {
        let store = self.store.clone();
        let gate = self.gate.clone();
        let key = key.to_string();
        tokio::task::spawn_blocking(move || {
            let _guard = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = store.build(service, &key, None)?;
            f(&entry)
        })
        .await
        .map_err(|e| CoreError::Vault(format!("keychain task failed: {e}")))?
        .map_err(map_keyring_error)
    }
}

#[cfg(target_os = "macos")]
fn native_store() -> Result<Arc<CredentialStore>> {
    // The login keychain. (`protected` data-protection keychain needs entitlements that an
    // unsandboxed Tauri app does not have.)
    let store: Arc<CredentialStore> =
        apple_native_keyring_store::keychain::Store::new().map_err(map_keyring_error)?;
    Ok(store)
}

#[cfg(target_os = "windows")]
fn native_store() -> Result<Arc<CredentialStore>> {
    let store: Arc<CredentialStore> =
        windows_native_keyring_store::Store::new().map_err(map_keyring_error)?;
    Ok(store)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn native_store() -> Result<Arc<CredentialStore>> {
    Err(CoreError::Vault(
        "no OS keychain backend on this platform".into(),
    ))
}

/// Maps a keyring error to a [`CoreError::Vault`]. Never includes secret material: the
/// `BadEncoding` / `BadDataFormat` variants carry the raw secret bytes, so they are described
/// by hand rather than formatted.
fn map_keyring_error(e: keyring_core::Error) -> CoreError {
    use keyring_core::Error as E;
    let msg = match e {
        E::BadEncoding(_) => "stored secret is not valid UTF-8".to_string(),
        E::BadDataFormat(_, inner) => format!("stored secret has an unexpected format: {inner}"),
        E::NoStorageAccess(inner) => {
            format!("keychain is not accessible (locked or denied): {inner}")
        }
        E::NoDefaultStore => "no credential store is installed".to_string(),
        E::Ambiguous(_) => "more than one matching keychain entry".to_string(),
        other => other.to_string(),
    };
    CoreError::Vault(msg)
}

#[async_trait]
impl SecretVault for KeychainVault {
    async fn get(&self, key: &str) -> Result<Option<SecretString>> {
        self.with_entry(vault_keys::SERVICE, key, |entry| {
            match entry.get_password() {
                Ok(p) => Ok(Some(SecretString::from(p))),
                Err(keyring_core::Error::NoEntry) => Ok(None),
                Err(e) => Err(e),
            }
        })
        .await
    }

    async fn set(&self, key: &str, value: &SecretString) -> Result<()> {
        // Copy into a String that is moved into the blocking task; it is dropped (not zeroized
        // by us) right after the call. The keychain APIs need `&str`.
        let secret = zeroize::Zeroizing::new(value.expose_secret().to_string());
        self.with_entry(vault_keys::SERVICE, key, move |entry| {
            entry.set_password(&secret)
        })
        .await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.with_entry(vault_keys::SERVICE, key, |entry| {
            match entry.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
                Err(e) => Err(e),
            }
        })
        .await?;
        Ok(())
    }
}

/// The decrypted bundle. Values are wiped on drop.
#[derive(Default)]
struct Bundle(HashMap<String, String>);

impl Drop for Bundle {
    fn drop(&mut self) {
        self.0.values_mut().for_each(|v| v.zeroize());
    }
}

/// Keeps every secret in one keychain item (see the module docs). The bundle is cached for the
/// life of the process, so a launch reads the keychain once however many connections open.
///
/// Entries still stored as individual items move into the bundle the first time they are read.
pub struct BundledVault {
    items: KeychainVault,
    /// Async so that concurrent first reads wait for one keychain read, not one prompt each.
    cache: tokio::sync::Mutex<Option<Bundle>>,
}

impl fmt::Debug for BundledVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BundledVault").finish_non_exhaustive()
    }
}

impl BundledVault {
    pub fn new(items: KeychainVault) -> Self {
        Self {
            items,
            cache: tokio::sync::Mutex::new(None),
        }
    }

    /// Reads the bundle from the keychain. A missing item is an empty bundle; an unreadable one
    /// is an error, so that a write can never replace it with an empty bundle.
    async fn read(&self) -> Result<Bundle> {
        let Some(json) = self.items.get(vault_keys::BUNDLE).await? else {
            return Ok(Bundle::default());
        };
        // Not formatted: serde errors can quote the input, which is the secrets.
        serde_json::from_str(json.expose_secret())
            .map(Bundle)
            .map_err(|_| CoreError::Vault("stored secrets have an unexpected format".into()))
    }

    async fn write(&self, bundle: &Bundle) -> Result<()> {
        let json = serde_json::to_string(&bundle.0)
            .map_err(|_| CoreError::Vault("could not encode secrets".into()))?;
        self.items
            .set(vault_keys::BUNDLE, &SecretString::from(json))
            .await
    }
}

#[async_trait]
impl SecretVault for BundledVault {
    async fn get(&self, key: &str) -> Result<Option<SecretString>> {
        let mut cache = self.cache.lock().await;
        if cache.is_none() {
            *cache = Some(self.read().await?);
        }
        if let Some(value) = cache.as_ref().and_then(|bundle| bundle.0.get(key)) {
            return Ok(Some(SecretString::from(value.clone())));
        }

        let Some(value) = self.items.get(key).await? else {
            return Ok(None);
        };
        let mut bundle = self.read().await?;
        bundle
            .0
            .insert(key.to_string(), value.expose_secret().to_string());
        self.write(&bundle).await?;
        *cache = Some(bundle);
        // The secret is safe in the bundle by now, and the bundle wins from here on.
        if let Err(e) = self.items.delete(key).await {
            tracing::warn!("could not remove the migrated keychain entry: {e}");
        }
        Ok(Some(value))
    }

    // Writes start from a fresh read: nothing stops two instances running at once, and a stale
    // cache would drop whatever the other one saved.
    async fn set(&self, key: &str, value: &SecretString) -> Result<()> {
        let mut cache = self.cache.lock().await;
        let mut bundle = self.read().await?;
        bundle
            .0
            .insert(key.to_string(), value.expose_secret().to_string());
        self.write(&bundle).await?;
        *cache = Some(bundle);
        self.items.delete(key).await
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let mut cache = self.cache.lock().await;
        let mut bundle = self.read().await?;
        if bundle.0.remove(key).is_some() {
            self.write(&bundle).await?;
        }
        *cache = Some(bundle);
        self.items.delete(key).await
    }
}

/// In-memory vault for tests and as an explicit fallback when no OS keychain exists.
#[derive(Default)]
pub struct MemoryVault {
    inner: Mutex<HashMap<String, String>>,
}

impl MemoryVault {
    pub fn new() -> Self {
        Self::default()
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl fmt::Debug for MemoryVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryVault").finish_non_exhaustive()
    }
}

#[async_trait]
impl SecretVault for MemoryVault {
    async fn get(&self, key: &str) -> Result<Option<SecretString>> {
        Ok(self.map().get(key).map(|s| SecretString::from(s.clone())))
    }
    async fn set(&self, key: &str, value: &SecretString) -> Result<()> {
        self.map()
            .insert(key.to_string(), value.expose_secret().to_string());
        Ok(())
    }
    async fn delete(&self, key: &str) -> Result<()> {
        self.map().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use keyring_core::api::CredentialStoreApi;

    fn secret(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    #[tokio::test]
    async fn conn_memory_vault_round_trip() {
        let v = MemoryVault::new();
        assert!(v.get("k").await.unwrap().is_none());
        v.set("k", &secret("one")).await.unwrap();
        assert_eq!(v.get("k").await.unwrap().unwrap().expose_secret(), "one");
        v.set("k", &secret("two")).await.unwrap();
        assert_eq!(v.get("k").await.unwrap().unwrap().expose_secret(), "two");
        v.delete("k").await.unwrap();
        assert!(v.get("k").await.unwrap().is_none());
        // Deleting a missing key is fine.
        v.delete("k").await.unwrap();
    }

    #[tokio::test]
    async fn conn_keychain_vault_logic_over_mock_store() {
        let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let v = KeychainVault::with_store(store);
        let key = vault_keys::password("p1");
        assert!(
            v.get(&key).await.unwrap().is_none(),
            "missing entry is Ok(None)"
        );
        v.set(&key, &secret("hunter2")).await.unwrap();
        assert_eq!(
            v.get(&key).await.unwrap().unwrap().expose_secret(),
            "hunter2"
        );
        // Different key is independent.
        assert!(
            v.get(&vault_keys::ssh_password("p1"))
                .await
                .unwrap()
                .is_none()
        );
        v.set(&key, &secret("changed")).await.unwrap();
        assert_eq!(
            v.get(&key).await.unwrap().unwrap().expose_secret(),
            "changed"
        );
        v.delete(&key).await.unwrap();
        assert!(v.get(&key).await.unwrap().is_none());
        v.delete(&key).await.unwrap(); // deleting a missing entry is Ok
    }

    #[tokio::test]
    async fn conn_keychain_vault_uses_the_documented_service_name() {
        let store = keyring_core::mock::Store::new().unwrap();
        let v = KeychainVault::with_store(store.clone());
        v.set("some-key", &secret("x")).await.unwrap();
        let entry = store.build(vault_keys::SERVICE, "some-key", None).unwrap();
        assert_eq!(entry.get_password().unwrap(), "x");
        assert_eq!(
            entry.get_specifiers(),
            Some((vault_keys::SERVICE.to_string(), "some-key".to_string()))
        );
    }

    fn bundled_over(store: &Arc<CredentialStore>) -> BundledVault {
        BundledVault::new(KeychainVault::with_store(store.clone()))
    }

    fn raw(store: &Arc<CredentialStore>, key: &str) -> keyring_core::Entry {
        store.build(vault_keys::SERVICE, key, None).unwrap()
    }

    #[tokio::test]
    async fn conn_bundled_vault_keeps_every_secret_in_one_item() {
        let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let v = bundled_over(&store);
        assert!(v.get("a").await.unwrap().is_none());
        v.set("a", &secret("one")).await.unwrap();
        v.set("b", &secret("two")).await.unwrap();
        assert_eq!(v.get("a").await.unwrap().unwrap().expose_secret(), "one");
        assert_eq!(v.get("b").await.unwrap().unwrap().expose_secret(), "two");

        assert!(
            raw(&store, "a").get_password().is_err(),
            "no per-secret item"
        );
        assert!(raw(&store, vault_keys::BUNDLE).get_password().is_ok());

        v.delete("a").await.unwrap();
        v.delete("a").await.unwrap(); // deleting a missing entry is Ok
        assert!(v.get("a").await.unwrap().is_none());
        assert_eq!(v.get("b").await.unwrap().unwrap().expose_secret(), "two");
    }

    #[tokio::test]
    async fn conn_bundled_vault_reads_the_keychain_once() {
        let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
        bundled_over(&store).set("a", &secret("one")).await.unwrap();

        let v = bundled_over(&store);
        assert!(v.get("a").await.unwrap().is_some());
        raw(&store, vault_keys::BUNDLE).delete_credential().unwrap();
        assert_eq!(
            v.get("a").await.unwrap().unwrap().expose_secret(),
            "one",
            "served from the cache, without touching the keychain"
        );
    }

    #[tokio::test]
    async fn conn_bundled_vault_migrates_individual_items_on_first_read() {
        let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let legacy = KeychainVault::with_store(store.clone());
        legacy.set("old", &secret("hunter2")).await.unwrap();

        let v = bundled_over(&store);
        assert_eq!(
            v.get("old").await.unwrap().unwrap().expose_secret(),
            "hunter2"
        );
        assert!(legacy.get("old").await.unwrap().is_none(), "item removed");
        assert_eq!(
            bundled_over(&store)
                .get("old")
                .await
                .unwrap()
                .unwrap()
                .expose_secret(),
            "hunter2",
            "a new process finds it in the bundle"
        );
    }

    #[tokio::test]
    async fn conn_bundled_vault_set_and_delete_clear_individual_items() {
        let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let legacy = KeychainVault::with_store(store.clone());
        legacy.set("set-me", &secret("old")).await.unwrap();
        legacy.set("delete-me", &secret("old")).await.unwrap();

        let v = bundled_over(&store);
        v.set("set-me", &secret("new")).await.unwrap();
        v.delete("delete-me").await.unwrap();

        assert!(legacy.get("set-me").await.unwrap().is_none());
        assert!(legacy.get("delete-me").await.unwrap().is_none());
        assert_eq!(
            v.get("set-me").await.unwrap().unwrap().expose_secret(),
            "new"
        );
        assert!(v.get("delete-me").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn conn_bundled_vault_writes_keep_another_instances_secrets() {
        let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let first = bundled_over(&store);
        let second = bundled_over(&store);
        assert!(first.get("loaded").await.unwrap().is_none());

        second.set("theirs", &secret("kept")).await.unwrap();
        first.set("mine", &secret("x")).await.unwrap();

        assert_eq!(
            bundled_over(&store)
                .get("theirs")
                .await
                .unwrap()
                .unwrap()
                .expose_secret(),
            "kept"
        );
    }

    #[tokio::test]
    async fn conn_bundled_vault_never_overwrites_an_unreadable_bundle() {
        let store: Arc<CredentialStore> = keyring_core::mock::Store::new().unwrap();
        let junk = "not json hunter2";
        raw(&store, vault_keys::BUNDLE).set_password(junk).unwrap();

        let v = bundled_over(&store);
        let err = v.set("a", &secret("x")).await.unwrap_err();
        assert!(!err.to_string().contains("hunter2"), "{err}");
        assert!(v.get("a").await.is_err());
        assert_eq!(
            raw(&store, vault_keys::BUNDLE).get_password().unwrap(),
            junk
        );
    }

    #[test]
    fn conn_debug_output_has_no_secrets() {
        let v = MemoryVault::new();
        assert!(!format!("{v:?}").contains("secret"));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn conn_new_errors_without_native_backend() {
        match KeychainVault::new() {
            Err(CoreError::Vault(m)) => assert!(m.contains("no OS keychain backend")),
            other => panic!("expected Vault error, got {other:?}"),
        }
    }
}
