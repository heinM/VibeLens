This is the upstream `sqlx-sqlite` 0.8.6 crate. Its Rust sources are unchanged.

The `libsqlite3-sys` dependency range is raised to `>=0.39.0, <0.40.0` because
RustSec-2026-0459 (CVE-2026-119375) affects earlier bundled SQLite versions.
Remove this local patch when SQLx publishes a compatible 0.8 release.
