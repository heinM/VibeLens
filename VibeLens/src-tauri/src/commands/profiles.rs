use secrecy::SecretString;
use tauri::State;
use vibelens_core::model::{ConnectionProfile, SshAuth, TestResult, TunnelConfig, vault_keys};

use super::{CmdResult, err};
use crate::state::AppState;

#[tauri::command]
pub async fn list_profiles(state: State<'_, AppState>) -> CmdResult<Vec<ConnectionProfile>> {
    state.profiles.list().map_err(err)
}

/// `password` → keychain `profile:<id>:password` (Password auth, or a pasted access token).
/// `ssh_secret` → keychain ssh-password or ssh-passphrase depending on the tunnel auth.
/// `None` leaves an existing secret untouched; `Some("")` deletes it.
#[tauri::command]
pub async fn save_profile(
    state: State<'_, AppState>,
    profile: ConnectionProfile,
    password: Option<String>,
    ssh_secret: Option<String>,
    iceberg_secret: Option<String>,
    s3_access_key_id: Option<String>,
    s3_secret_access_key: Option<String>,
    forget_host_key: Option<bool>,
) -> CmdResult<ConnectionProfile> {
    let mut profile = profile;
    // The pin is written by the tunnel behind the UI's back, so a form opened before the first
    // connect still says "not pinned". Only an explicit Forget may clear it.
    if !forget_host_key.unwrap_or(false)
        && let Some(TunnelConfig::Ssh {
            host,
            port,
            known_host_fingerprint: pin @ None,
            ..
        }) = profile.tunnel.as_mut()
        && let Ok(stored) = state.profiles.get(&profile.id)
        && let Some(TunnelConfig::Ssh {
            host: stored_host,
            port: stored_port,
            known_host_fingerprint: Some(fingerprint),
            ..
        }) = stored.tunnel
        && *host == stored_host
        && *port == stored_port
    {
        *pin = Some(fingerprint);
    }
    let saved = state.profiles.upsert(profile).map_err(err)?;
    // Iceberg secrets: same semantics as `password` (None = keep, "" = delete).
    for (value, key) in [
        (iceberg_secret, vault_keys::iceberg_secret(&saved.id)),
        (s3_access_key_id, vault_keys::s3_access_key_id(&saved.id)),
        (
            s3_secret_access_key,
            vault_keys::s3_secret_access_key(&saved.id),
        ),
    ] {
        if let Some(v) = value {
            if v.is_empty() {
                state.vault.delete(&key).await.map_err(err)?;
            } else {
                state
                    .vault
                    .set(&key, &SecretString::from(v))
                    .await
                    .map_err(err)?;
            }
        }
    }
    if let Some(pw) = password {
        let key = vault_keys::password(&saved.id);
        if pw.is_empty() {
            state.vault.delete(&key).await.map_err(err)?;
        } else if saved.auth.uses_stored_password() {
            state
                .vault
                .set(&key, &SecretString::from(pw))
                .await
                .map_err(err)?;
        }
    }
    if let (Some(secret), Some(TunnelConfig::Ssh { auth, .. })) =
        (ssh_secret, saved.tunnel.as_ref())
    {
        let key = match auth {
            SshAuth::Password => Some(vault_keys::ssh_password(&saved.id)),
            SshAuth::PrivateKey { .. } => Some(vault_keys::ssh_passphrase(&saved.id)),
            SshAuth::Agent => None,
        };
        if let Some(key) = key {
            if secret.is_empty() {
                state.vault.delete(&key).await.map_err(err)?;
            } else {
                state
                    .vault
                    .set(&key, &SecretString::from(secret))
                    .await
                    .map_err(err)?;
            }
        }
    }
    // A changed auth method or tunnel must not leave the old secret behind in the keychain.
    for key in saved.unused_vault_keys() {
        state.vault.delete(&key).await.map_err(err)?;
    }
    Ok(saved)
}

#[tauri::command]
pub async fn delete_profile(state: State<'_, AppState>, id: String) -> CmdResult<()> {
    let _ = state.connections.disconnect(&id).await;
    for key in [
        vault_keys::password(&id),
        vault_keys::ssh_password(&id),
        vault_keys::ssh_passphrase(&id),
        vault_keys::iceberg_secret(&id),
        vault_keys::s3_access_key_id(&id),
        vault_keys::s3_secret_access_key(&id),
    ] {
        let _ = state.vault.delete(&key).await;
    }
    state.profiles.delete(&id).map_err(err)
}

#[tauri::command]
pub async fn test_connection(
    state: State<'_, AppState>,
    profile: ConnectionProfile,
    password: Option<String>,
) -> CmdResult<TestResult> {
    let pw = password.filter(|p| !p.is_empty()).map(SecretString::from);
    state.connections.test(&profile, pw).await.map_err(err)
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretStatus {
    pub has_password: bool,
    pub has_ssh_password: bool,
    pub has_ssh_passphrase: bool,
    pub has_iceberg_secret: bool,
    pub has_s3_keys: bool,
}

/// Which keychain entries exist for a profile. Never returns the secrets themselves.
#[tauri::command]
pub async fn profile_secret_status(
    state: State<'_, AppState>,
    id: String,
) -> CmdResult<SecretStatus> {
    let has = |key: String| {
        let vault = state.vault.clone();
        async move { vault.get(&key).await.map(|v| v.is_some()) }
    };
    Ok(SecretStatus {
        has_password: has(vault_keys::password(&id)).await.map_err(err)?,
        has_ssh_password: has(vault_keys::ssh_password(&id)).await.map_err(err)?,
        has_ssh_passphrase: has(vault_keys::ssh_passphrase(&id)).await.map_err(err)?,
        has_iceberg_secret: has(vault_keys::iceberg_secret(&id)).await.map_err(err)?,
        has_s3_keys: has(vault_keys::s3_secret_access_key(&id))
            .await
            .map_err(err)?,
    })
}
