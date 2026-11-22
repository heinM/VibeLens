//! The outside services the live tests need, and what happens when one is missing.
//!
//! A test that needs a service which is not there gives up through [`skip`] and returns: it
//! prints why and counts as passed. That is right on a laptop. It is wrong where the service was
//! meant to be there, because a mistake in the setup then turns dozens of tests into passes
//! that tested nothing.
//!
//! `VG_TEST_REQUIRE` lists the services that must be present, by the names in [`SERVICES`],
//! separated by commas: `VG_TEST_REQUIRE=pg,valkey`. A skip for a listed service is a failure.
//! So is a name that is not in [`SERVICES`], so that a typing mistake cannot switch the rule off.
//!
//! Every skip goes through here; `tests/live_requirements.rs` holds the tests of the rule and
//! fails if a test prints a skip message of its own. The unit tests in `src/` reach this file
//! as `crate::test_services`.
#![allow(dead_code)]

pub const REQUIRE_VAR: &str = "VG_TEST_REQUIRE";

/// Every service a test can be skipped for: its name in `VG_TEST_REQUIRE`, and what provides it.
pub const SERVICES: &[(&str, &str)] = &[
    (
        "pg",
        "a Postgres server, VG_TEST_PG=\"host=… port=… user=… password=… dbname=…\"",
    ),
    (
        "valkey",
        "a Valkey or Redis server, VG_TEST_VALKEY=redis://host:port",
    ),
    (
        "valkey-tls",
        "`redis-server` and `openssl` on PATH, for the TLS server these tests start themselves",
    ),
    (
        "valkey-cluster",
        "a Valkey cluster, VG_TEST_VALKEY_CLUSTER=host:port",
    ),
    (
        "valkey-cluster-tls",
        "a Valkey cluster with TLS, VG_TEST_VALKEY_CLUSTER_TLS, _CA and _SNI",
    ),
    (
        "ssh",
        "the throwaway sshd of tests/conn_tunnel_setup.sh, VG_TEST_SSH=1",
    ),
    (
        "ssh-password",
        "an sshd user with a password, which the setup script can only create as root",
    ),
    (
        "iceberg",
        "the pyiceberg fixture, VG_TEST_ICEBERG_SQL_DB and VG_TEST_ICEBERG_WAREHOUSE",
    ),
    (
        "iceberg-seed",
        "python3 with pyiceberg, to seed a private copy of the fixture",
    ),
    ("s3", "an S3-compatible store, VG_TEST_S3_ENDPOINT"),
    (
        "non-root",
        "a user that file permissions apply to (not root)",
    ),
    (
        "mysql",
        "a MySQL 8 server, VG_TEST_MYSQL=\"host=… port=… user=… password=… dbname=…\"",
    ),
    (
        "mysql-tls",
        "a MySQL server whose certificate is valid for 127.0.0.1, signed by the CA in VG_TEST_MYSQL_CA",
    ),
];

/// Whether a test may skip for lack of `service` when `VG_TEST_REQUIRE` holds `require`.
/// `Err` is the message the test must fail with.
pub fn check(require: &str, service: &str, why: &str) -> Result<(), String> {
    let known = |name: &str| SERVICES.iter().find(|(n, _)| *n == name);
    let names = || {
        let all: Vec<&str> = SERVICES.iter().map(|(n, _)| *n).collect();
        all.join(", ")
    };
    let Some((_, provided_by)) = known(service) else {
        return Err(format!(
            "a test asked to skip for {service:?}, which is not a service. Add it to SERVICES \
             in tests/services/mod.rs, or use one of: {}",
            names()
        ));
    };
    let mut required = false;
    for item in require.split(',').map(str::trim).filter(|i| !i.is_empty()) {
        let item = item.to_ascii_lowercase();
        if known(&item).is_none() {
            return Err(format!(
                "{REQUIRE_VAR} names {item:?}, which is not a service, so nothing would be \
                 required for it. The services are: {}",
                names()
            ));
        }
        required |= item == service;
    }
    if required {
        return Err(format!(
            "{service} is required ({REQUIRE_VAR}={require}) and this test cannot run: {why}. \
             It needs {provided_by}."
        ));
    }
    Ok(())
}

/// Call this where a test cannot run because `service` is missing, then return from the test.
/// Panics, and so fails the test, when the service is required.
pub fn skip(service: &str, why: impl std::fmt::Display) {
    let why = why.to_string();
    let require = std::env::var(REQUIRE_VAR).unwrap_or_default();
    if let Err(problem) = check(&require, service, &why) {
        panic!("{problem}");
    }
    eprintln!("skipping ({service}): {why}");
}
