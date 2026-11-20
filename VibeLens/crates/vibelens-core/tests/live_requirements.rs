//! `VG_TEST_REQUIRE`: the rule every skip in the live tests goes through (`tests/services`).
//!
//! Before it, a test whose service was missing printed a line and passed. With Postgres and
//! Valkey both unconfigured, `pg_engine` and `valkey_engine` reported 71 passed tests.

mod services;

use std::path::{Path, PathBuf};

use services::{REQUIRE_VAR, SERVICES, check};

const WHY: &str = "VG_TEST_PG is not set";

#[test]
fn a_skip_is_allowed_when_nothing_is_required() {
    for (service, _) in SERVICES {
        assert_eq!(check("", service, WHY), Ok(()), "{service}");
        assert_eq!(check("  ", service, WHY), Ok(()), "{service}");
    }
}

#[test]
fn a_skip_for_a_required_service_is_a_failure_that_says_what_is_missing() {
    let problem = check("pg,valkey", "pg", WHY).expect_err("pg is required");
    assert!(problem.contains(WHY), "the reason for the skip: {problem}");
    assert!(
        problem.contains("VG_TEST_REQUIRE"),
        "why it is a failure: {problem}"
    );
    assert!(
        problem.contains("a Postgres server"),
        "what provides the service: {problem}"
    );
    assert!(check("pg,valkey", "valkey", "VG_TEST_VALKEY is not set").is_err());
}

#[test]
fn only_the_listed_services_are_required() {
    assert_eq!(check("pg,valkey", "ssh", "VG_TEST_SSH is not 1"), Ok(()));
    assert_eq!(check("pg,valkey", "iceberg", "no fixture"), Ok(()));
    // A longer name that starts like a listed one is another service.
    assert_eq!(
        check("valkey", "valkey-tls", "redis-server is not installed"),
        Ok(())
    );
    assert_eq!(check("valkey", "valkey-cluster", "not set"), Ok(()));
    assert!(check("valkey-cluster", "valkey-cluster", "not set").is_err());
    assert_eq!(check("valkey-cluster", "valkey", "not set"), Ok(()));
}

#[test]
fn the_list_tolerates_spaces_capitals_and_empty_items() {
    for list in ["pg", " pg ", "valkey, pg", "PG", "valkey,,pg,", "pg,pg"] {
        assert!(check(list, "pg", WHY).is_err(), "{list:?} requires pg");
    }
}

/// A name that is not a service must not be passed over: `VG_TEST_REQUIRE=postgres` would
/// otherwise require nothing, and look as if it did.
#[test]
fn a_name_that_is_not_a_service_is_refused_wherever_a_test_would_skip() {
    for list in ["postgres", "pg,redis", "pg valkey", "pg;valkey"] {
        let problem = check(list, "ssh", "VG_TEST_SSH is not 1").expect_err(list);
        assert!(problem.contains("VG_TEST_REQUIRE"), "{problem}");
        assert!(
            problem.contains("pg, valkey, "),
            "the message lists the names that exist: {problem}"
        );
    }
    let problem = check("postgres", "ssh", "x").unwrap_err();
    assert!(problem.contains("\"postgres\""), "{problem}");
}

/// The other half: a test that asks to skip for a service nobody can require.
#[test]
fn a_test_cannot_skip_for_a_service_that_is_not_in_the_list() {
    let problem = check("", "postgres", WHY).expect_err("not a service");
    assert!(problem.contains("\"postgres\""), "{problem}");
    assert!(problem.contains("tests/services/mod.rs"), "{problem}");
}

#[test]
fn service_names_are_unique_and_safe_to_put_in_a_list() {
    let mut seen = std::collections::BTreeSet::new();
    for (name, provided_by) in SERVICES {
        assert!(seen.insert(*name), "{name} is listed twice");
        assert!(
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
            "{name:?}"
        );
        assert!(!provided_by.is_empty(), "{name} says what provides it");
    }
}

/// Checked on every run, also when no test skips: a mistake in the list fails here.
#[test]
fn the_services_required_in_this_run_all_exist() {
    let require = std::env::var(REQUIRE_VAR).unwrap_or_default();
    for (service, _) in SERVICES {
        if let Err(problem) = check(&require, service, "") {
            assert!(
                !problem.contains("is not a service"),
                "{REQUIRE_VAR}={require:?}: {problem}"
            );
        }
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every skip goes through `services::skip`, or the rule does not hold for it. A test that
/// prints its own "skipping" line and returns is exactly what this replaced.
#[test]
fn no_test_prints_a_skip_message_of_its_own() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("tests"), &mut files);
    rust_files(&root.join("src"), &mut files);
    let exempt = [
        root.join("tests/services/mod.rs"),
        root.join("tests/live_requirements.rs"),
    ];
    let mut found = Vec::new();
    for file in files.iter().filter(|f| !exempt.contains(f)) {
        let text = std::fs::read_to_string(file).expect("read source");
        for (i, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            let in_a_string = code
                .find('"')
                .is_some_and(|quote| code[quote..].to_ascii_lowercase().contains("skipping"));
            if in_a_string {
                let shown = file.strip_prefix(root).unwrap_or(file).display();
                found.push(format!("{shown}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(
        found.is_empty(),
        "{} place(s) print a skip message of their own; use services::skip so that {REQUIRE_VAR} applies:\n{}",
        found.len(),
        found.join("\n")
    );
}
