//! JsonProfileStore: CRUD, uniqueness, timestamps, atomic writes, corrupt-file recovery.

mod conn_common;

use std::fs;

use conn_common::pg_profile;
use tempfile::tempdir;
use vibelens_core::error::CoreError;
use vibelens_core::model::*;
use vibelens_core::ports::ProfileStore;
use vibelens_core::profiles::{JsonProfileStore, trust_host_key};

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

// ---- Pinning an SSH host key (review finding M2) -------------------------------------------

const FP_A: &str = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const FP_B: &str = "SHA256:n0Tar3alK3yButTheRightShape0123456789+/abcd";

fn tunneled(name: &str, host: &str, port: u16, pin: Option<&str>) -> ConnectionProfile {
    ConnectionProfile {
        tunnel: Some(TunnelConfig::Ssh {
            host: host.into(),
            port,
            username: "ops".into(),
            auth: SshAuth::Agent,
            known_host_fingerprint: pin.map(String::from),
        }),
        ..pg_profile(name)
    }
}

fn pin_of(s: &JsonProfileStore, id: &str) -> Option<String> {
    match s.get(id).unwrap().tunnel {
        Some(TunnelConfig::Ssh {
            known_host_fingerprint,
            ..
        }) => known_host_fingerprint,
        None => None,
    }
}

#[test]
fn conn_trusting_a_host_key_pins_it_on_the_matching_unpinned_tunnel() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    let p = s
        .upsert(tunneled("a", "bastion.example", 22, None))
        .unwrap();
    let saved = trust_host_key(&s, &p.id, "bastion.example", 22, FP_A).unwrap();
    assert_eq!(pin_of(&s, &p.id).as_deref(), Some(FP_A));
    assert_eq!(saved, s.get(&p.id).unwrap());
    // Everything else about the profile is untouched.
    assert_eq!(
        ConnectionProfile {
            tunnel: p.tunnel.clone(),
            updated_at: p.updated_at,
            ..saved
        },
        p
    );
    // Trusting the same key again is harmless.
    trust_host_key(&s, &p.id, "bastion.example", 22, FP_A).unwrap();
    assert_eq!(pin_of(&s, &p.id).as_deref(), Some(FP_A));
    // A blank stored pin counts as no pin.
    let q = s
        .upsert(tunneled("b", "bastion.example", 22, Some("  ")))
        .unwrap();
    trust_host_key(&s, &q.id, "bastion.example", 22, FP_B).unwrap();
    assert_eq!(pin_of(&s, &q.id).as_deref(), Some(FP_B));
}

/// The key was presented by one host and port. If the saved profile points somewhere else by
/// now (the user edited the form, or the answer arrives late), it is not that host's key.
#[test]
fn conn_a_host_key_is_not_pinned_on_a_tunnel_with_another_host_or_port() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    let p = s
        .upsert(tunneled("a", "bastion.example", 22, None))
        .unwrap();
    for (host, port) in [
        ("other.example", 22),
        ("bastion.example", 2222),
        ("BASTION.example", 22),
        ("", 22),
    ] {
        let e = trust_host_key(&s, &p.id, host, port, FP_A)
            .expect_err("a key from another host or port must not be pinned");
        assert!(matches!(e, CoreError::Invalid(_)), "{host}:{port}: {e:?}");
        assert_eq!(pin_of(&s, &p.id), None, "{host}:{port}");
    }
    assert_eq!(s.get(&p.id).unwrap(), p, "nothing was written");
}

/// An existing pin is only ever removed by the user's Forget. Trusting never replaces one.
#[test]
fn conn_a_host_key_never_replaces_a_different_pin() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    let p = s
        .upsert(tunneled("a", "bastion.example", 22, Some(FP_A)))
        .unwrap();
    let e = trust_host_key(&s, &p.id, "bastion.example", 22, FP_B)
        .expect_err("a pinned profile keeps its pin");
    assert!(matches!(e, CoreError::Invalid(_)), "{e:?}");
    assert_eq!(pin_of(&s, &p.id).as_deref(), Some(FP_A));
    assert_eq!(s.get(&p.id).unwrap(), p, "nothing was written");
}

#[test]
fn conn_trusting_a_host_key_needs_a_saved_tunnel_and_a_real_fingerprint() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());
    assert!(matches!(
        trust_host_key(&s, "missing", "bastion.example", 22, FP_A),
        Err(CoreError::NotFound(_))
    ));
    let direct = s.upsert(pg_profile("no tunnel")).unwrap();
    assert!(matches!(
        trust_host_key(&s, &direct.id, "bastion.example", 22, FP_A),
        Err(CoreError::Invalid(_))
    ));
    assert_eq!(s.get(&direct.id).unwrap(), direct);

    let p = s
        .upsert(tunneled("a", "bastion.example", 22, None))
        .unwrap();
    for bad in [
        "",
        "garbage",
        "MD5:aa:bb:cc",
        "SHA256:tooshort",
        "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
        " SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        assert!(
            matches!(
                trust_host_key(&s, &p.id, "bastion.example", 22, bad),
                Err(CoreError::Invalid(_))
            ),
            "{bad:?}"
        );
        assert_eq!(pin_of(&s, &p.id), None, "{bad:?}");
    }
    assert!(is_host_key_fingerprint(FP_A) && is_host_key_fingerprint(FP_B));
}

// ---- secrets in the wrong place (review finding M7) ---------------------------------------

fn iceberg_profile(name: &str, catalog: IcebergCatalog) -> ConnectionProfile {
    let mut p = pg_profile(name);
    p.engine = EngineKind::Iceberg;
    p.host = String::new();
    p.port = 0;
    p.database = None;
    p.auth = AuthMethod::None;
    p.iceberg = Some(IcebergOptions {
        catalog,
        storage: IcebergStorage::Auto,
        query_engine: IcebergQueryEngine::Local,
        default_namespace: None,
    });
    p
}

fn rest_catalog(uri: &str, auth: RestAuth, headers: &[(&str, &str)]) -> IcebergCatalog {
    IcebergCatalog::Rest {
        uri: uri.into(),
        warehouse: None,
        auth,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

fn sql_catalog(uri: &str) -> IcebergCatalog {
    IcebergCatalog::Sql {
        uri: uri.into(),
        warehouse: "s3://lake/warehouse".into(),
        catalog_name: "main".into(),
    }
}

#[test]
fn conn_a_profile_that_would_leak_a_secret_is_not_saved() {
    let dir = tempdir().unwrap();
    let s = store(dir.path());

    let mut token_without_tls = pg_profile("token, no TLS");
    token_without_tls.auth = AuthMethod::AccessToken {
        username: "me".into(),
        command: None,
    };
    token_without_tls.tls.mode = TlsMode::Disable;

    for p in [
        iceberg_profile(
            "token to a cleartext catalog",
            rest_catalog("http://catalog.internal:8181", RestAuth::Bearer, &[]),
        ),
        iceberg_profile(
            "credential in a header",
            rest_catalog(
                "https://catalog.example",
                RestAuth::None,
                &[("Authorization", "Basic c2VjcmV0")],
            ),
        ),
        iceberg_profile(
            "password in the catalog URI",
            sql_catalog("postgres://iceberg:hunter2@db.internal/catalog"),
        ),
        token_without_tls.clone(),
    ] {
        let name = p.name.clone();
        let e = s.upsert(p).unwrap_err();
        assert!(matches!(e, CoreError::Invalid(_)), "{name}: {e:?}");
    }
    assert!(s.list().unwrap().is_empty());
    let on_disk = fs::read_to_string(dir.path().join("profiles.json")).unwrap_or_default();
    assert!(
        !on_disk.contains("hunter2") && !on_disk.contains("c2VjcmV0"),
        "{on_disk}"
    );

    // What the form offers by default, and the corrected versions of the above, are saved.
    token_without_tls.tls.mode = TlsMode::VerifyFull;
    for p in [
        iceberg_profile(
            "local catalog",
            rest_catalog("http://localhost:8181", RestAuth::None, &[]),
        ),
        iceberg_profile(
            "remote catalog",
            rest_catalog(
                "https://catalog.example",
                RestAuth::Bearer,
                &[("X-Tenant", "acme")],
            ),
        ),
        iceberg_profile(
            "sql catalog",
            sql_catalog("postgres://iceberg@db.internal/catalog"),
        ),
        token_without_tls,
    ] {
        let name = p.name.clone();
        s.upsert(p).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}
