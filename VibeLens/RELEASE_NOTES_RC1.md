# VibeLens 0.1.0-1 (Release Candidate 1)

This is a prerelease for hands-on evaluation. The version uses a numeric prerelease identifier so the Windows MSI bundle accepts it.

## What's new

- Import saved database connections from supported local client configuration files, including DataGrip and DBeaver. Imported profiles are previewed before saving; secrets are not imported from those files.
- Authenticate PostgreSQL connections with a pasted bearer token or a command that obtains a fresh token for new database sessions. Includes shortcuts for common Azure, kubelogin, and Google Cloud commands.
- Configure Portkey AI Gateway providers, including model catalog, provider, virtual key, and config routing.
- Require Node.js 24 for development and CI; refresh frontend dependencies and retain the DOMPurify security override.
- Harden provider configuration file permissions, reject credential-bearing extra headers, remove obsolete keychain entries after authentication changes, and default release logs to less detailed output.

## Verification

- Frontend: typecheck, 694 tests, and production Vite build pass on this machine.
- Rust: formatting and compilation pass; all 382 core unit tests and the complete integration suite pass.
- Dependency advisories: npm registry access is unavailable in this environment, and `cargo-audit` is not installed. CI has RustSec and npm audit jobs.

## Known follow-up

- Query history remains in WebView localStorage as documented in the security review; history-on-disk or clear-on-quit policy remains future work.
- Run the manual platform smoke plan, especially Windows installation/keychain behavior and the real provider/token workflows, before sharing broadly.
- GitHub secret scanning and push protection are repository settings and were not verifiable from this checkout.
