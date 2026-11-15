//! JsonProfileStore: CRUD, uniqueness, timestamps, atomic writes, corrupt-file recovery.

mod conn_common;

use std::fs;

use conn_common::pg_profile;
use tempfile::tempdir;
use vibelens_core::error::CoreError;
use vibelens_core::model::*;
use vibelens_core::ports::ProfileStore;
use vibelens_core::profiles::JsonProfileStore;

fn store(dir: &std::path::Path) -> JsonProfileStore {
    JsonProfileStore::new(dir.to_path_buf()).unwrap()
}

#[test]
fn conn_empty_store_lists_nothing_and_creates_no_file() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    assert!(s.list().unwrap().is_empty());
    assert!(
        !dir.path().join("profiles.json").exists(),
        "reads must not create the file"
    );
}

#[test]
fn conn_upsert_assigns_id_and_timestamps() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    let saved = s.upsert(pg_profile("Prod")).unwrap();
    assert!(!saved.id.is_empty());
    assert!(uuid_like(&saved.id), "id should be a UUID v4: {}", saved.id);
    assert!(saved.created_at.is_some());
    assert_eq!(
        saved.created_at, saved.updated_at,
        "first save stamps both with the same instant"
    );
    assert_eq!(s.get(&saved.id).unwrap(), saved);
    assert_eq!(s.list().unwrap(), vec![saved]);
}

fn uuid_like(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

#[test]
fn conn_update_preserves_created_at_and_bumps_updated_at() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    let first = s.upsert(pg_profile("Prod")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut edited = first.clone();
    edited.host = "other.example".into();
    // A client that sends a bogus created_at must not be able to rewrite history.
    edited.created_at = Some(chrono::Utc::now() + chrono::Duration::days(100));
    let second = s.upsert(edited).unwrap();

    assert_eq!(second.id, first.id);
    assert_eq!(second.created_at, first.created_at);
    assert!(second.updated_at > first.updated_at);
    assert_eq!(second.host, "other.example");
    assert_eq!(s.list().unwrap().len(), 1, "update must not duplicate");
}

#[test]
fn conn_name_must_be_non_empty_and_unique() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());

    assert!(matches!(
        s.upsert(pg_profile("")),
        Err(CoreError::Invalid(_))
    ));
    assert!(matches!(
        s.upsert(pg_profile("   ")),
        Err(CoreError::Invalid(_))
    ));

    let a = s.upsert(pg_profile("Prod")).unwrap();
    assert!(
        matches!(s.upsert(pg_profile("Prod")), Err(CoreError::Invalid(_))),
        "exact duplicate"
    );
    assert!(
        matches!(s.upsert(pg_profile("  prod ")), Err(CoreError::Invalid(_))),
        "case/space-insensitive duplicate"
    );

    // Renaming a profile to its own name (or re-saving it) is fine.
    s.upsert(a.clone()).unwrap();
    // Renaming onto another profile's name is not.
    let b = s.upsert(pg_profile("Staging")).unwrap();
    let mut clash = b.clone();
    clash.name = "PROD".into();
    assert!(matches!(s.upsert(clash), Err(CoreError::Invalid(_))));
    assert_eq!(
        s.get(&b.id).unwrap().name,
        "Staging",
        "failed upsert must not change anything"
    );
    assert_eq!(s.list().unwrap().len(), 2);
}

#[test]
fn conn_name_is_trimmed() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    assert_eq!(s.upsert(pg_profile("  Padded  ")).unwrap().name, "Padded");
}

#[test]
fn conn_get_missing_is_not_found() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    assert!(matches!(s.get("nope"), Err(CoreError::NotFound(_))));
}

#[test]
fn conn_delete_removes_and_is_idempotent() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    let a = s.upsert(pg_profile("A")).unwrap();
    let b = s.upsert(pg_profile("B")).unwrap();
    s.delete(&a.id).unwrap();
    assert!(matches!(s.get(&a.id), Err(CoreError::NotFound(_))));
    assert_eq!(s.list().unwrap(), vec![b]);
    s.delete(&a.id).unwrap();
    s.delete("never-existed").unwrap();
    // The name is free again.
    s.upsert(pg_profile("A")).unwrap();
}

#[test]
fn conn_persists_across_instances_in_the_documented_format() {
    let dir = tempdir().unwrap();
    let saved = store(dir.path()).upsert(pg_profile("Prod")).unwrap();

    let raw = fs::read_to_string(dir.path().join("profiles.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(json["profiles"][0]["name"], "Prod");
    assert!(
        !raw.to_lowercase().contains("password\":"),
        "no secrets in the file"
    );

    let reopened = store(dir.path());
    assert_eq!(reopened.get(&saved.id).unwrap(), saved);
}

#[test]
fn conn_creates_missing_config_dir_on_first_write() {
    let dir = tempdir().unwrap();
    let nested = dir.path().join("a").join("b");
    let s = store(&nested);
    s.upsert(pg_profile("X")).unwrap();
    assert!(nested.join("profiles.json").is_file());
}

#[test]
fn conn_writes_are_atomic_and_leave_no_temp_files() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    for i in 0..20 {
        s.upsert(pg_profile(&format!("P{i}"))).unwrap();
    }
    let names: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        vec!["profiles.json".to_string()],
        "only the real file remains: {names:?}"
    );
    // And it is always complete JSON.
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("profiles.json")).unwrap()).unwrap();
    assert_eq!(json["profiles"].as_array().unwrap().len(), 20);
}

#[cfg(unix)]
#[test]
fn conn_file_is_private_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    s.upsert(pg_profile("X")).unwrap();
    s.upsert(pg_profile("Y")).unwrap(); // a rewrite must keep the mode too
    let mode = fs::metadata(dir.path().join("profiles.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn conn_corrupt_file_is_backed_up_and_store_starts_empty() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("profiles.json");
    fs::write(&path, b"{ this is not json").unwrap();

    let s = store(dir.path());
    assert!(s.list().unwrap().is_empty());

    let backups: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("profiles.json.corrupt-"))
        .collect();
    assert_eq!(backups.len(), 1, "exactly one backup: {backups:?}");
    assert_eq!(
        fs::read(dir.path().join(&backups[0])).unwrap(),
        b"{ this is not json"
    );
    assert!(!path.exists(), "the corrupt file was moved, not copied");

    // The store is usable afterwards.
    let saved = s.upsert(pg_profile("Fresh")).unwrap();
    assert_eq!(store(dir.path()).list().unwrap(), vec![saved]);
}

#[test]
fn conn_structurally_wrong_json_counts_as_corrupt() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("profiles.json"),
        br#"{"version":1,"profiles":[{"id":"x"}]}"#,
    )
    .unwrap();
    let s = store(dir.path());
    assert!(s.list().unwrap().is_empty());
    assert!(fs::read_dir(dir.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".corrupt-")
    }));
}

#[test]
fn conn_newer_file_version_is_refused_not_clobbered() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("profiles.json");
    fs::write(&path, br#"{"version":99,"profiles":[]}"#).unwrap();
    let s = store(dir.path());
    assert!(s.list().is_err());
    assert!(s.upsert(pg_profile("X")).is_err());
    assert_eq!(
        fs::read(&path).unwrap(),
        br#"{"version":99,"profiles":[]}"#,
        "file untouched"
    );
}

#[test]
fn conn_blank_file_is_just_empty() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("profiles.json"), b"  \n").unwrap();
    let s = store(dir.path());
    assert!(s.list().unwrap().is_empty());
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no spurious backup"
    );
}

#[test]
fn conn_tunnel_and_auth_variants_round_trip() {
    let dir = tempdir().unwrap();
    let mut p = pg_profile("Tunneled");
    p.auth = AuthMethod::RdsIam {
        username: "iam_user".into(),
        aws: AwsTarget {
            identity: AwsIdentity::Profile {
                profile_name: "dev".into(),
            },
            region: "eu-west-1".into(),
        },
    };
    p.tunnel = Some(TunnelConfig::Ssh {
        host: "bastion.example".into(),
        port: 22,
        username: "ec2-user".into(),
        auth: SshAuth::PrivateKey {
            path: "~/.ssh/id_ed25519".into(),
        },
        known_host_fingerprint: Some("SHA256:abc".into()),
    });
    let saved = store(dir.path()).upsert(p).unwrap();
    assert_eq!(store(dir.path()).get(&saved.id).unwrap(), saved);
}

#[test]
fn conn_concurrent_upserts_keep_every_profile() {
    let dir = tempdir().unwrap();
    let s = std::sync::Arc::new(store(dir.path()));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let s = s.clone();
            std::thread::spawn(move || s.upsert(pg_profile(&format!("T{i}"))).unwrap())
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(s.list().unwrap().len(), 8);
    assert_eq!(store(dir.path()).list().unwrap().len(), 8);
}
