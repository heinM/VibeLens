//! JSON-file profile store (OWNER: connections agent).
//!
//! File: `<config_dir>/profiles.json` = `{ "version": 1, "profiles": [...] }`, written atomically
//! (temp file in the same directory, fsync, rename; `0600` on unix). It is meant to contain no
//! secrets. Passwords, tokens and keys are not fields of [`ConnectionProfile`], but a few
//! free-text fields could carry one (Iceberg REST headers, a catalog URI with a password in it),
//! so [`upsert`](ProfileStore::upsert) refuses those through [`crate::cleartext::check_profile`],
//! together with settings that would send a secret unencrypted.
//!
//! The file is loaded lazily on first use and cached in memory behind a mutex; every mutation is
//! persisted *before* the cache is updated, so a failed write leaves memory and disk in agreement.
//! A file that doesn't parse is moved aside to `profiles.json.corrupt-<timestamp>` and the store
//! starts empty (with a `tracing::error!`), so a damaged file never blocks the app and never
//! silently destroys the user's data.

mod atomic;

pub(crate) use atomic::atomic_write;

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::model::{ConnectionProfile, TunnelConfig, is_host_key_fingerprint};
use crate::ports::ProfileStore;

/// On-disk schema version written by this build.
const FILE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct ProfilesFile {
    version: u32,
    #[serde(default)]
    profiles: Vec<ConnectionProfile>,
}

/// Pins `fingerprint` as the SSH host key of the saved profile `profile_id`. This is the only
/// place a pin is written on the user's behalf, and it is called only from the UI's "trust this
/// key" action; connecting and testing never pin anything.
///
/// `host` and `port` are where the key was presented. The pin is written only when the saved
/// tunnel still points at exactly that host and port (the same comparison `save_profile` makes
/// when it carries a pin over) and has no pin yet. An existing, different pin is never
/// replaced: removing one is the user's explicit Forget.
pub fn trust_host_key(
    store: &dyn ProfileStore,
    profile_id: &str,
    host: &str,
    port: u16,
    fingerprint: &str,
) -> Result<ConnectionProfile> {
    if !is_host_key_fingerprint(fingerprint) {
        return Err(CoreError::Invalid(
            "a host key fingerprint looks like SHA256: followed by 43 characters".into(),
        ));
    }
    let mut profile = store.get(profile_id)?;
    let Some(TunnelConfig::Ssh {
        host: saved_host,
        port: saved_port,
        known_host_fingerprint,
        ..
    }) = profile.tunnel.as_mut()
    else {
        return Err(CoreError::Invalid(
            "this connection has no SSH tunnel to pin a host key for".into(),
        ));
    };
    if saved_host != host || *saved_port != port {
        return Err(CoreError::Invalid(format!(
            "that host key was presented by {host}:{port}, but this connection's SSH tunnel now \
             points at {saved_host}:{saved_port}; connect again to see the right key"
        )));
    }
    match known_host_fingerprint.as_deref().map(str::trim) {
        Some(pinned) if pinned == fingerprint => return Ok(profile),
        Some(pinned) if !pinned.is_empty() => {
            return Err(CoreError::Invalid(format!(
                "this connection already has a pinned host key ({pinned}); use Forget in its \
                 SSH settings before trusting another"
            )));
        }
        _ => {}
    }
    *known_host_fingerprint = Some(fingerprint.to_string());
    store.upsert(profile)
}

/// [`ProfileStore`] backed by `profiles.json` in the app config directory.
pub struct JsonProfileStore {
    path: PathBuf,
    /// `None` until first use.
    cache: Mutex<Option<Vec<ConnectionProfile>>>,
}

impl JsonProfileStore {
    /// Does not touch the disk: the file (and `config_dir`) are created on first write.
    pub fn new(config_dir: PathBuf) -> Result<Self> {
        Ok(Self {
            path: config_dir.join("profiles.json"),
            cache: Mutex::new(None),
        })
    }

    /// Location of the backing file.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Locks the cache, loading from disk on first use.
    fn loaded(&self) -> Result<MutexGuard<'_, Option<Vec<ConnectionProfile>>>> {
        // A panic while holding the lock can't leave the cache half-updated (we only assign
        // after a successful write), so recovering from poisoning is safe.
        let mut guard = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.is_none() {
            *guard = Some(self.load_from_disk()?);
        }
        Ok(guard)
    }

    fn load_from_disk(&self) -> Result<Vec<ConnectionProfile>> {
        let raw = match std::fs::read(&self.path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        if raw.iter().all(u8::is_ascii_whitespace) {
            return Ok(Vec::new());
        }
        match serde_json::from_slice::<ProfilesFile>(&raw) {
            Ok(file) if file.version > FILE_VERSION => Err(CoreError::Other(format!(
                "{} was written by a newer VibeLens (file version {}, this build understands {FILE_VERSION}); \
                 refusing to overwrite it",
                self.path.display(),
                file.version
            ))),
            Ok(file) => Ok(file.profiles),
            Err(parse_err) => {
                let backup = self.back_up_corrupt_file()?;
                tracing::error!(
                    path = %self.path.display(),
                    backup = %backup.display(),
                    error = %parse_err,
                    "profiles.json is corrupt; moved it aside and starting with no profiles"
                );
                Ok(Vec::new())
            }
        }
    }

    /// Moves the unreadable file to `profiles.json.corrupt-<UTC timestamp>`.
    fn back_up_corrupt_file(&self) -> Result<PathBuf> {
        let stamp = Utc::now().format("%Y%m%dT%H%M%S%3fZ");
        let mut name = self
            .path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_else(|| "profiles.json".into());
        name.push(format!(".corrupt-{stamp}"));
        let backup = self.path.with_file_name(name);
        // If this fails we must not continue: the next write would destroy the only copy.
        std::fs::rename(&self.path, &backup)?;
        Ok(backup)
    }

    fn persist(&self, profiles: &[ConnectionProfile]) -> Result<()> {
        // Serialise through a borrowed view so we don't clone the whole list again.
        #[derive(Serialize)]
        struct View<'a> {
            version: u32,
            profiles: &'a [ConnectionProfile],
        }
        let mut bytes = serde_json::to_vec_pretty(&View {
            version: FILE_VERSION,
            profiles,
        })?;
        bytes.push(b'\n');
        atomic_write(&self.path, &bytes, true)
    }
}

impl ProfileStore for JsonProfileStore {
    fn list(&self) -> Result<Vec<ConnectionProfile>> {
        Ok(self.loaded()?.clone().unwrap_or_default())
    }

    fn get(&self, id: &str) -> Result<ConnectionProfile> {
        let guard = self.loaded()?;
        guard
            .as_ref()
            .and_then(|list| list.iter().find(|p| p.id == id))
            .cloned()
            .ok_or_else(|| CoreError::NotFound(format!("profile {id}")))
    }

    /// Names are trimmed and must be unique, compared case-insensitively (two profiles that
    /// differ only by case would be indistinguishable to the agent and in the sidebar).
    /// An empty `id` gets a fresh UUID v4; `created_at` is kept from the stored profile on update.
    fn upsert(&self, mut profile: ConnectionProfile) -> Result<ConnectionProfile> {
        profile.name = profile.name.trim().to_string();
        if profile.name.is_empty() {
            return Err(CoreError::Invalid("profile name must not be empty".into()));
        }
        crate::cleartext::check_profile(&profile)?;
        profile.id = profile.id.trim().to_string();

        let mut guard = self.loaded()?;
        let list = guard.get_or_insert_with(Vec::new);

        let now = Utc::now();
        if profile.id.is_empty() {
            profile.id = uuid::Uuid::new_v4().to_string();
        }
        let existing = list.iter().position(|p| p.id == profile.id);

        let wanted = profile.name.to_lowercase();
        if let Some(clash) = list
            .iter()
            .find(|p| p.id != profile.id && p.name.to_lowercase() == wanted)
        {
            return Err(CoreError::Invalid(format!(
                "a profile named \"{}\" already exists",
                clash.name
            )));
        }

        profile.created_at = match existing {
            Some(i) => list[i].created_at.or(Some(now)),
            None => profile.created_at.or(Some(now)),
        };
        profile.updated_at = Some(now);

        let mut next = list.clone();
        match existing {
            Some(i) => next[i] = profile.clone(),
            None => next.push(profile.clone()),
        }
        self.persist(&next)?;
        *list = next;
        Ok(profile)
    }

    /// Deleting a profile that doesn't exist is a no-op (idempotent).
    fn delete(&self, id: &str) -> Result<()> {
        let mut guard = self.loaded()?;
        let list = guard.get_or_insert_with(Vec::new);
        if !list.iter().any(|p| p.id == id) {
            return Ok(());
        }
        let next: Vec<ConnectionProfile> = list.iter().filter(|p| p.id != id).cloned().collect();
        self.persist(&next)?;
        *list = next;
        Ok(())
    }
}
