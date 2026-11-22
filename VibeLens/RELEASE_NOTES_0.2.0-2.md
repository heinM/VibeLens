# VibeLens 0.2.0-2 (Release Candidate 2)

This is a prerelease for hands-on evaluation. It has the same features as 0.2.0-1, rebuilt with every dependency brought up to date, and it is the first candidate built by CI for all three platforms.

Installers: macOS universal (Apple Silicon and Intel), Windows x64 (`.exe` and `.msi`) and Linux x64 (`.AppImage` and `.deb`). None of them are signed. On macOS, open the app once through System Settings → Privacy & Security → Open Anyway. On Windows, SmartScreen shows "Windows protected your PC": choose More info → Run anyway.

## What changed since 0.2.0-1

- **Security fix:** `url` 2.5.9, which fixes a stack overflow on nested `blob:` URLs (RUSTSEC-2026-0592).
- **Rust dependencies:** 150 crates moved to their latest compatible versions, including Tauri 2.12.7, russh 0.65.4 (which now uses the fixed `rsa` 0.10) and redis 1.8.2.
- **sqlparser 0.64:** a bare `select` is now parsed as the valid PostgreSQL statement it is and classified read-only. `select into` and `select ... for update` are still classified as writes.
- **Frontend dependencies:** all at their latest versions.
- **Dependency audit:** the list of accepted RustSec advisories is down from six to one (`rsa` 0.9.10, present in the lockfile only for a database driver that is never built).

DataFusion stays at 54 and sqlx at 0.8: Iceberg 0.11, the newest release, requires both.

## Verification

- CI ran for this candidate: formatting and clippy clean, the Rust tests pass on Linux against Postgres 17 (TLS) and Valkey, the core compiles on macOS and Windows, the frontend typecheck, 982 tests and build pass, and `cargo audit` and `npm audit` are clean.
- Locally on macOS: 1,051 Rust tests and 982 frontend tests pass.

## Known follow-up

- **Windows and Linux installers have not been launched by anyone yet.** Treat them as untested; reports are welcome.
- Everything listed under "Known follow-up" for 0.2.0-1 still applies (`RELEASE_NOTES_0.2.0-1.md`): Trino, Spark and Redshift are tested against mock servers only; MySQL was tested against MySQL 8.4 in a container; unsaved SQL and query history are kept in the WebView's local storage in plain text.
