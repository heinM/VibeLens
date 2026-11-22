# VibeLens 0.2.0-1 (Release Candidate 1)

This is a prerelease for hands-on evaluation. The version uses a numeric prerelease identifier so the Windows MSI bundle accepts it.

This build was made on a Mac, not by CI: it contains the macOS universal app (Apple Silicon and Intel) and, where attached, the Linux x64 packages. There are no Windows installers in this release. The macOS app is not signed or notarised: open it once through System Settings → Privacy & Security → Open Anyway.

## What's new

- **MySQL** as a connection type: MySQL 8.x, Aurora MySQL and RDS MySQL, with password or RDS IAM authentication over verified TLS (also through an SSH tunnel), a navigator, editor runs with server-side cancel, and the agent tools under the same access rules as Postgres.
- **Amazon Redshift** through the Data API with IAM only: no database password on the machine and no network path to the cluster.
- **Trino and Spark (Apache Livy)** as Iceberg query engines, next to Local and Athena.
- **Iceberg 0.11 and DataFusion 54**; the local engine gains the `"table$history"` metadata table.
- **Agent**: pin the target connection and database; a `list_databases` tool; "Open in tab" and "Run in tab" on the agent's queries so the full result appears in the grid; an optional context window per provider for local models such as Ollama.
- **Postgres**: partitions nest under their parent with bounds, row estimates and sizes, plus the pg_partman settings when the extension is installed.
- **Workspace**: open tabs and unsaved SQL are restored after a reload or restart; existing files can be imported; the folder can be revealed.
- **AWS**: "Sign in again" in the navigator when the IAM Identity Center session has expired.
- Error banners can be dismissed.
- The code-review fixes from the security review of 0.1.0-2.

## Security

- The new engines follow the rules in `docs/REMOTE_ENGINES_AND_REDSHIFT.md`: secrets only in the OS vault and bound to their endpoint, verified TLS, no redirects, read-only checked before a statement is sent, bounded results, server-side cancel, validation in Rust.
- The MySQL driver (`mysql_async` 0.37.1) is vendored with a patch (`vendor/mysql_async/PATCH.md`): no LOCAL INFILE, no multi-statements, no password without TLS, and no panic reachable from server bytes. A seeded fuzz test corrupts every stage of the server conversation.
- The new code went through several independent review rounds; their findings are fixed in this candidate, each with a test.

## Verification

- Rust: formatting and clippy clean; 1,050 tests pass against Postgres 17 (TLS), Valkey 8 and MySQL 8.4.
- Frontend: typecheck clean; 982 tests pass.
- CI did not run for this candidate.

## Known follow-up

- Trino, Spark and Redshift are tested against mock servers only, not real endpoints.
- MySQL was tested against MySQL 8.4 in a container: RDS IAM against real RDS or Aurora, Aurora MySQL itself and MariaDB were not exercised. A read-only run can still call a stored or loadable function with side effects outside the database; give the agent's database user no `EXECUTE`.
- MySQL has no inline cell editing, ERD or EXPLAIN plan view yet.
- Unsaved SQL and query history are kept in the WebView's local storage in plain text.
- The README screenshots show the real UI with the bundled demo data.
