# VibeLens — Design (V1)

> Status: **authoritative**. Supersedes the original "Product & Technical Specification (V1)" where they differ.
> Section 1 lists every deliberate deviation and why.

---

## 1. What changed from the original spec, and why

| # | Original spec said | V1 does instead | Why |
|---|---|---|---|
| 1 | `claude-3-5-sonnet`, `gemini-2.5-pro` hard-wired | Model id is **config**, never code. Presets for Anthropic, xAI, Gemini (via its OpenAI-compatible endpoint), and **local** (Ollama/LM Studio) | Claude 3.5 Sonnet is retired; model names churn every few months. A local provider matters for regulated data. |
| 2 | Three bespoke provider integrations | Two adapters: **Anthropic Messages** + **OpenAI-compatible Chat Completions** | xAI, Gemini, Ollama, vLLM, LM Studio and most gateways all speak the OpenAI shape. Fewer adapters, fewer bugs. |
| 3 | `process_agent_turn` as one `invoke` returning `updatedHistory`; conversation in TanStack Query cache | Agent loop lives in **Rust**; events stream to the UI over a **Tauri `Channel`**; transcript in a Zustand store | One request/response can't stream tokens or pause mid-turn for approval. TanStack Query is for server *state*, not an append-only event log. |
| 4 | "No destructive tools run automatically" enforced by the UI catching requests | Enforced in **Rust** (`agent::policy`): SQL classified with `sqlparser`; "safe" SQL *still* runs in `BEGIN READ ONLY`; Valkey write detection uses the server's own `COMMAND INFO` flags; file writes always need approval | A UI-only guard is bypassable. Classification is a routing hint; the READ ONLY transaction is the enforcement. **The real boundary is a least-privilege DB role** (see §6). |
| 5 | — (unaddressed) | Per-profile **`aiDataAccess`**: `none` / `schemaOnly` (default) / `resultsWithApproval` / `results` | Sending production rows to a third-party LLM is a data-egress decision, not a UX detail. Default is schema-only. |
| 6 | IAM "silent background refresh interceptor" | Token minted **per new physical connection** inside the pool's `create()` | RDS checks the IAM token **only at connection establishment**; open connections are unaffected by expiry. No refresh loop is needed. ElastiCache IAM connections *are* dropped after 12h; the connection manager reconnects with a fresh token. |
| 7 | `aws-sdk-ssooidc` writes to `~/.aws/sso/cache` | VibeLens writes a CLI-compatible cache file itself, **and** treats existing `~/.aws/config` profiles as first-class | The SDK doesn't write the cache. Most enterprise users already run `aws sso login`; reuse their profiles instead of forcing a second login flow. |
| 8 | "Zero-persistence policy" | Honest version: Secrets Manager values and IAM tokens are **memory-only** (`secrecy::SecretString`, zeroized on drop) and never cross IPC. **SSO access tokens are on disk** in `~/.aws/sso/cache` (same as the AWS CLI). Typed passwords cross IPC exactly once (form → keychain). | A policy that's quietly violated is worse than one that names its exceptions. |
| 9 | `ssh2` (+ `vendored-openssl`) or "`citrus-ssh`" | **`russh`** (pure Rust, async). Host keys are pinned once the user has checked them; an unpinned host is shown its fingerprint and gets no authentication attempt. | `citrus-ssh` doesn't exist on crates.io. `ssh2` wraps blocking libssh2 and drags in OpenSSL. |
| 10 | `native-tls` / `postgres-native-tls` | **rustls** everywhere + optional **RDS CA bundle** | One TLS stack (the AWS SDK already uses rustls). RDS/Aurora certs chain to Amazon RDS CAs that are *not* in OS trust stores; "verify-full" fails without the bundle. |
| 11 | `react-window` + TanStack Table | **`@tanstack/react-virtual`** + TanStack Table | Same vendor and model as the table; handles dynamic column widths and sticky headers more cleanly. |
| 12 | `@monaco-editor/react` defaults | Monaco **bundled locally**, with a trimmed language set | The default loader fetches from a CDN, which the app CSP blocks (and so do most bank networks). `import "monaco-editor"` pulls in ~80 languages. |
| 13 | "Max 5 MB IPC chunks" | Results stream as **pages of rows** (`QueryEvent::Rows`, 500 rows/page or about 4 MiB of cells, whichever comes first, capped at `maxRows`) over a `Channel` with backpressure. A value over 1 MiB is not sent whole (see §3). | Small messages *and* first rows show before the query finishes. A page can still exceed 4 MiB by its last row, and the result as a whole has no byte limit. |
| 14 | `int8`/`numeric` → JSON | **`int8`, `numeric`, `money` → strings** | JS numbers lose precision past 2^53. Silently wrong money is unacceptable. |
| 15 | `db0`–`db15` always | Logical DBs from `CONFIG GET databases` (falls back to probing `INFO keyspace`); **cluster mode = db0 only** | ElastiCache disables `CONFIG`; cluster mode has no logical DBs. |
| 16 | `tauri.conf.json` "privilege lists"; `csp: null` | Tauri v2 **capabilities** (`core:default` only) + a strict **CSP** | v2 moved permissions to capability files. `csp: null` disables CSP entirely. |
| 17 | Single `src-tauri` crate | Cargo **workspace**: `crates/vibelens-core` (no Tauri) + `src-tauri` (thin IPC adapter) | The engines compile and test headless (`cargo test -p vibelens-core`) in CI on any OS. Ports & adapters. |
| 18 | `keyring = "2.5"` | **`keyring-core` 1.x** + `apple-native-keyring-store` / `windows-native-keyring-store` | keyring v4 split the platform stores into separate crates. Note that v3 silently fell back to a mock store if you forgot the platform feature. |
| 19 | `redis` 0.24 with `tokio-comp` **and** `async-std-comp` | `redis` 1.x: `tokio-comp`, `tokio-rustls-comp`, `cluster-async`, `connection-manager` | async-std is discontinued; mixing runtimes is a footgun. |
| 20 | SSO start URL `https://awsapps.com` | `https://d-xxxxxxxxxx.awsapps.com/start` (or your org alias) | The bare domain isn't a valid start URL. |
| 21 | *(post-V1 request)* "Iceberg with Glue IAM auth", read as REST + SigV4 | Glue and S3 Tables use their **native** iceberg-rust catalogs on the app's AWS identity | iceberg-rust's REST catalog has no SigV4 signer. The native catalogs reach the same metadata with the same IAM permissions. |
| 22 | *(implied)* one Iceberg engine | **Local (DataFusion)** *and* **Athena**, chosen per connection | iceberg-rust 0.10 can append but not write delete files, so the local engine has no UPDATE/DELETE/MERGE. Athena does those and enforces Lake Formation, but bills per TB scanned. |
| 23 | *(implied)* use `iceberg-datafusion`'s `IcebergCatalogProvider` | Own **lazy** catalog/schema providers | The stock provider loads every table of every top-level namespace at connect, ignores nested namespaces, and can't express `table@snapshot` time travel. |
| 24 | *(implied)* catalog advertises storage config | The profile's explicit S3 settings **override** catalog-vended props | Local REST catalogs advertise endpoints such as `http://minio:9000` that don't resolve on the host. The REST crate applies user props last; a test pins this. |

---

## 2. Architecture

```
┌──────────────────────────── Tauri v2 window (WKWebView / WebView2) ─────────────────────────────┐
│ React 19 · Vite · Tailwind v4 · Monaco (local) · TanStack Query/Table/Virtual · Zustand          │
│   src/ipc/commands.ts  ← the ONLY place that calls invoke()                                       │
└───────────────┬───────────────────────────────────────────────────────────────▲──────────────────┘
                │ invoke (JSON)                                                   │ Channel<QueryEvent | AgentEvent>
┌───────────────▼──────────────── src-tauri (IPC adapter, ~400 LOC) ─────────────┴──────────────────┐
│ commands/*.rs: validate → call core → map CoreError → ErrorPayload. Prod write-guard lives here.  │
└───────────────┬──────────────────────────────────────────────────────────────────────────────────┘
                │ Rust API
┌───────────────▼──────────────────────── crates/vibelens-core ────────────────────────────────────┐
│  services:  ConnectionManager ──────────────┐            AgentService (loop · policy · providers)│
│               │  lazily builds handles       │  ToolBackend port ◄───────────┘                   │
│               ▼                              ▼                                                    │
│  adapters:  pg::PgHandle  valkey::ValkeyHandle  workspace::WorkspaceFs                            │
│               ▲ CredentialFn (per connection)  ▲ TunnelProvider                                    │
│             aws::AppCredentialSource ──► AwsService (SSO-OIDC · SSO · RDS signer · ElastiCache    │
│               │                                   SigV4 presign · Secrets Manager)                │
│             vault::KeychainVault (keyring-core)   tunnel::SshTunnelProvider (russh)               │
│             profiles::JsonProfileStore            tls (rustls + RDS bundle)   sql (sqlparser)     │
└──────────────────────────────────────────────────────────────────────────────────────────────────┘
```

### Ports (`crates/vibelens-core/src/ports.rs`)
`SecretVault`, `ProfileStore`, `CredentialSource`, `CredentialFn`, `TunnelProvider`, `ToolBackend`. Services depend only on these traits, so every service is unit-testable with in-memory fakes.

### Connect sequence (Postgres, RDS IAM, behind a bastion)
1. `ConnectionManager::pg(profile, db)` → cache miss.
2. `TunnelProvider::open(...)` → `127.0.0.1:54123` → bastion → `mydb.cluster-xyz.eu-west-1.rds.amazonaws.com:5432`.
3. `PgHandle::connect(params{host: 127.0.0.1, port: 54123, tls_server_name: mydb.cluster-…}, creds_fn)`.
4. For each physical connection, the pool calls `creds_fn()` → `AppCredentialSource::resolve` → `AwsService::rds_auth_token(target, real_host, real_port, user)` → a fresh 15-minute token.
5. TLS verifies against `tls_server_name` (the real host), even though we dial localhost.
6. The idle reaper drops the pool and tunnel after `idleDisconnectSecs`, so Aurora Serverless v2 (min 0 ACU) can pause.

---

## 3. IPC contract

* Every command is in `src-tauri/src/lib.rs::generate_handler!` and mirrored in `src/ipc/commands.ts`. Types are mirrored in `src/ipc/types.ts` ↔ `crates/vibelens-core/src/model/*.rs`. **Change both sides together.**
* Errors: `{ code, message }`. Codes come from `CoreError::code()`, plus `confirm_required` (prod write-guard).
* Streaming: `pg_execute` and `agent_start_turn` take a `Channel`. They return an id immediately; the stream's last event is always terminal (`finished`/`failed`, `turnFinished`/`failed`).
* Cell encoding: see `model/query.rs::Cell`. Binary is `{ "$bytes": base64, "len": n }`.
* Oversized values: a Postgres or Iceberg value over 1 MiB (`MAX_CELL_BYTES`) is not decoded in full. Its cell is `{ "$truncated": { "kind", "bytes", "preview" } }`: the size of the whole value and its start. The marker is not the value. The UI shows the start with a label; the table editor refuses to edit it, to write it in `UPDATE`/`INSERT`, to use it in a `WHERE` (row lookup, quick filter, paging position) and to duplicate its row; copies and CSV/JSON/Markdown exports carry a note in its place and say so; SQL-INSERT exports stop with an error. This lowers peak memory by removing the copies made after the driver has read the row; it does not bound it, because the driver still receives the whole row, a result may hold many values just under the limit, and the UI keeps every page. Not covered: fetching the full value on demand, Athena results, and the elements of Valkey collections (Valkey strings are already cut at 1 MiB on the server).
* Secrets: UI → Rust only, and straight into the keychain. **No command ever returns a secret.**

---

## 4. Agent runtime

**Tools**

| Tool | Default risk | Notes |
|---|---|---|
| `list_connections` | safe | Names, engines, environments. No hosts or usernames. |
| `get_schema` | safe (if access ≠ none) | `SchemaMetadata`, capped at 200 tables. |
| `describe_table` | safe | |
| `run_sql` | readOnly → safe*, else **approval** | *Under `resultsWithApproval`, reads need approval too. Reads always run `read_only=true`. |
| `valkey_scan` | safe | Key names only. |
| `valkey_get` | follows `aiDataAccess` | |
| `valkey_command` | read → follows policy, write → **approval** | Write detection comes from `COMMAND INFO`. |
| `list_files` / `read_file` | safe | Inside the workspace jail. |
| `write_file` | **approval** (always) | Preview is a unified diff against the file on disk. The write carries that version (`FileVersion`, SHA-256) and is refused if the file has changed, appeared or gone by the time it is approved. If what is at the path cannot be read, the call is denied: there is no diff to show. A diff over 600 lines is cut, and the reason says how many lines are not shown. |

**Loop**: at most 16 tool steps per turn, then stop. Tool results go back to the model as *data* (tagged, truncated to about 24 KB). Under `schemaOnly`, row data never leaves the machine. Prompt injection in DB content can make the agent *request* any tool call. Writes and side effects hit the approval gate **only if they're recognised**. `READ ONLY` does not stop `dblink`, `pg_read_file`, `lo_*` or advisory locks, so those functions are classified as needing approval. Classification fails closed (§6).

**What the approval shows**: the reason and the preview (a statement, a command or a `write_file` diff) are made in Rust. Control characters, invisible characters and direction controls (the right-to-left override and its relatives) are written as their code point, `<U+202E>`, and the reason says how many there are, so the text on the card cannot read differently from what runs or is written. Execution always gets the call's own text. This covers the card only: tool output, chat text and the editor render characters as they are.

**Approval**: the loop emits `approvalRequired`, then parks on a `oneshot` keyed by `(session_id, call_id)`. `agent_resolve_approval` wakes it. Cancelling the turn rejects all pending approvals. The event carries a `target` (connection name, environment, and the Postgres database or Valkey db number), which the card shows in a fixed header apart from the reason. The model chooses the database, so Rust resolves it once with the rule the connection manager uses (`effective_database`), shows that name and hands the same name to execution. The model's `purpose` is flattened to one line and capped at 200 characters.

---

## 5. Data & storage

| What | Where | Persisted? |
|---|---|---|
| Profiles (no secrets) | `<app_config_dir>/profiles.json` (0600) | yes |
| Provider configs (no keys) | `<app_config_dir>/providers.json` | yes |
| Passwords, SSH secrets, LLM API keys | macOS Keychain (one item, `vault-bundle`) / Windows Credential Manager (one entry per secret), service `com.heinvandermerwe.vibelens` | yes (OS-encrypted); cached in process memory after the first read on macOS |
| SSO access/refresh token | `~/.aws/sso/cache/*.json` | **yes, plaintext JSON, like the AWS CLI** |
| IAM DB tokens, Secrets Manager values | process memory (`SecretString`) | no |
| Worksheets | `~/Documents/VibeLens/` (override: `VIBELENS_WORKSPACE`) | yes |

On Linux (dev/CI) there is no keychain backend. The app falls back to an in-memory vault and shows a warning banner.

---

## 6. Security model: what's enforced vs advisory

**Enforced (Rust):** the read-only transaction for agent reads; the approval gate; the workspace path jail (no `..`, canonicalised under root, extension allowlist, 5 MiB cap); `aiDataAccess`; the prod write-guard (`confirm_required`); CSP; minimal capabilities; TLS verify-full by default; SSH host-key pinning; no redirects for requests that carry a secret (the LLM client follows none, the Iceberg REST client only within the same host and port); no secret over an unencrypted connection to another machine, checked on save and again on connect (`cleartext.rs`): LLM API keys and Iceberg REST tokens or client secrets need `https://` unless the address is this machine, access-token and IAM-token database logins need TLS unless the host is this machine or reached through the SSH tunnel, and credentials are refused in Iceberg headers, provider headers and catalog URIs because those are stored as plain text. Password logins without TLS are allowed, as in libpq; the TLS section warns about them.

**Advisory:** SQL classification. It fails closed: `ReadOnly` is returned only when sqlparser yields a Query, Explain (non-analyze), Show or Values AST with no known side-effecting function (`dblink*`, `pg_read_file`, `lo_*`, `pg_terminate_backend`, advisory locks, `set_config`, `nextval`, …). Anything unparseable goes to approval. But a user-defined `SECURITY DEFINER` function, or an extension we don't know about, can still do work inside READ ONLY. **Use a least-privilege role for anything the agent touches.** The README says so plainly.

**Defence in depth:** after every statement in a read-only run, the executor checks that the session is still inside its READ ONLY transaction. If not, it aborts and discards the connection. The one statement that is not checked is a read-only statement cut off at the row cap: the check is a query on the same connection and would wait for the whole result, so that connection is cancelled and closed instead and nothing else runs on it. Pooled connections get a `DISCARD ALL`-equivalent reset when a run ends and on recycle (`ROLLBACK`, `RESET ALL`, advisory locks released, temp objects dropped). It omits `DEALLOCATE ALL`, which would break the driver's cached statements, and a connection whose run was dropped mid-flight is never returned to the pool.

**Independent review (2026-10-31):** a separate reviewer probed the build and found a CR-terminated-comment bypass (critical), connection reuse after a cancelled run (high), `dblink` under READ ONLY (high), quoted `EXPLAIN ("analyze")` (medium), `EVAL_RO` (medium), and value leakage via error text under schemaOnly (medium), plus minor items. All were fixed with regression tests; see the git history for this date.

**Not in V1:** audit log export, SSO/IdP-backed licence enforcement, MDM-managed config, SSM Session Manager tunnels (see §8).

---

## 7. Module ownership (parallel build)

| Module | Owner | Files |
|---|---|---|
| Contracts (model, ports, error, tls, src-tauri glue, ipc/*.ts, stores, ui primitives) | lead | as listed |
| Postgres engine | agent `pg` | `crates/vibelens-core/src/pg/**`, `tests/pg_*.rs` |
| Valkey engine | agent `valkey` | `crates/vibelens-core/src/valkey/**`, `tests/valkey_*.rs` |
| AWS identity | agent `aws` | `crates/vibelens-core/src/aws/**` |
| Connections, vault, profiles, tunnel, workspace | agent `connections` | `src/{connections,vault,profiles,tunnel,workspace}/**` |
| Agent runtime + SQL classifier | agent `agent` | `src/{agent,sql}/**` |
| UI: shell, sidebar, editor, grid, Valkey views | agent `fe-workspace` | `src/App.tsx`, `src/components/{layout,sidebar,editor,grid,valkey}/**`, `src/hooks/**` (except agent hooks) |
| UI: connection wizard, settings, agent terminal | agent `fe-agent` | `src/components/{connections,settings,agent}/**`, `src/store/useAgentStore.ts`, `src/hooks/useAgent*.ts` |

---

## 7a. Apache Iceberg

```text
ConnectionManager ──iceberg(profile)──► IcebergHandle (single-flight, cached by profile fingerprint, idle-reaped)
                                         ├── ManagedCatalog  REST | Glue | S3 Tables | SQL | HMS(feature)
                                         │     └─ rebuilds itself < 5 min before AWS creds expire; once after a 401
                                         ├── FileIO props    Auto (catalog identity / vended) | S3 | S3Static | Local
                                         └── engine          Local: DataFusion SessionContext, lazy providers
                                                             Athena: athena::AthenaEngine (AwsDataCatalog | s3tablescatalog/<bucket>)
                                                             Trino:  remote::TrinoEngine (client REST protocol; any catalog kind)
                                                             Spark:  remote::SparkEngine (Apache Livy, kind "sql"; any catalog kind)
```

- **Trino and Spark** (`remote/`, design in `REMOTE_ENGINES_AND_REDSHIFT.md` §1–2). Both go through one hardened HTTP client (`remote/http.rs`): no redirects, TLS always verified (OS roots + optional PEM), the credential (vault key `engine_secret`) attached only to requests for the configured scheme/host/port, bodies capped at 64 MiB, the secret redacted from every message. Trino's `nextUri` must be on the configured endpoint; Livy session and statement ids must be non-negative integers. The Livy session is created on first use, switched to `catalog.namespace` with `USE`, replaced once if it died, and deleted when the connection closes. `USE`/`SET SESSION`/transactions (Trino) and `USE`/`SET`/`RESET` (Spark) are refused in every mode, because each statement must run against the catalog the profile (and the approval card) names.

- **Read-only enforcement.**
  - *Local:* every statement is classified (`sql::classify`) before planning, and DataFusion is configured to refuse DDL/DML on read-only runs. That makes two independent layers.
  - *Athena:* there are no read-only transactions, so the pre-submit check is the only one. It's the classifier, plus a narrow text fallback (`athena/scan.rs`) for Trino syntax that sqlparser can't parse. The fallback admits only `SELECT`/`WITH`/`SHOW`/`DESCRIBE` with no comments, semicolons, unbalanced quotes or write keywords.
  - *Trino:* the Athena check (`athena::check_read_only_as`), same dialect.
  - *Spark:* a stricter scan (`remote/spark_scan.rs`): only `SELECT`/`WITH`/`SHOW`/`DESCRIBE`/`EXPLAIN`, and no `reflect`/`java_method`/`try_reflect`, no `format.\`path\`` file relations, no `TRANSFORM`/`USING 'script'`, `IDENTIFIER(…)`, `EXECUTE IMMEDIATE`, `LATERAL VIEW` over a non-built-in generator, and no write or session keyword. Literals are read with and without backslash escapes and the statement must pass both ways; text that cannot be tokenised is refused.
  - *Hardening after the 2026-11-21 audit:* read-only Spark runs accept no comments at all (Spark nests block comments, continues `--` comments over a backslash-newline and parses `/*+ hints */`); `MAP`/`REDUCE` without `(`, `USING` followed by any quoted text, `RECORDWRITER`/`RECORDREADER`, `libsvm`/`image`/`kafka` and class-name sources are refused. Any Spark statement containing `${` is refused in every mode, because Spark substitutes variables (`spark.sql.variable.substitute`, on by default) in the raw text before parsing, inside literals and comments: `'${env:AWS_SECRET_ACCESS_KEY}'` would read the driver's environment. Cluster admins can also set `spark.sql.variable.substitute=false`. Trino read-only runs refuse `TABLE(…)` table functions. For agent policy and the prod guard, SQL the PostgreSQL classifier calls `Unknown` on a Trino or Spark connection is judged by the engine's own check (`remote::classify_on`); a write never changes class.
  - *Engine secret binding:* the Trino/Livy secret is stored with the endpoint it was saved for (`engine_secret_origin`: kind, scheme, host, port, path prefix, CA file) and used only while the profile still points there; a changed endpoint without a new secret deletes it. `sessionConf` is an allow-list (`remote::config::SPARK_SESSION_CONF_ALLOWED`). Livy sessions are verified by their generated name before each script and before delete. Agent Stop and the tool timeout cancel the engine's token and wait for the server-side cancel (Trino DELETE, Livy cancel, StopQueryExecution, CancelStatement); agent SQL has a 150 s whole-script deadline.
  - *Re-audit (2026-11-21):* one Livy session serves approved writes and unapproved reads, so session-scoped state is refused in every mode on Spark: `CREATE [OR REPLACE] [GLOBAL] TEMPORARY|TEMP …`, `DROP TEMPORARY …`, `ADD`, `DECLARE`, `SET VAR`, `EXECUTE IMMEDIATE`, `CACHE`, `UNCACHE`, `CLEAR CACHE` (with `USE`/`SET`/`RESET`). Read-only Spark runs refuse two-part names starting with a file format (`text.passwd`; write `catalog.text.passwd`). Spark catalog names match `^[A-Za-z_][A-Za-z0-9_]*$`; the default namespace may not contain `${` or control characters, and every statement sent to Livy, including the generated `USE`, is checked for `${`. `sessionConf` no longer allows `spark.sql.catalog.<C>.type` or `.warehouse`. Trino `nextUri` paths are plain segments under `{base}/v1/statement/` with no `%`, `;`, `\`, query or fragment. For Trino/Spark a PostgreSQL "read" the engine check refuses is `Unknown` for the prod guard and denied (with the engine's reason) for the agent. The secret origin includes a SHA-256 of the CA bundle's contents and is written after the secret (origin deleted first). Submissions (`POST /v1/statement`, `POST /sessions`, `POST …/statements`) run in a task that deletes or cancels what was created when the caller has gone.
  - `EXPLAIN ANALYZE <write>` is a write on both engines.
- **Cost.** `athena::cost_estimate_usd` gives the list price: billed bytes rounded up to the next MB, 10 MB minimum, US$5/TB. It is shown as an estimate, never as a cap. A workgroup scan limit is the real control.
- **Agent.**
  - The same `aiDataAccess` matrix applies. Iceberg writes need approval, with "creates a new table snapshot" in the reason, and Athena adds "billed per TB scanned".
  - Under `schemaOnly`, engine errors collapse to a fixed sentence, because DataFusion cast errors echo values.
- **Catalog DB migration.** A SQL catalog lacking `iceberg_type` is migrated automatically only on `dev` profiles; otherwise the connection fails with the exact `ALTER TABLE`.

## 7b. Amazon Redshift (Data API, IAM)

```text
ConnectionManager ──redshift(profile)──► RedshiftEngine (single-flight, cached by profile fingerprint; no socket, no tunnel)
                                          └── aws-sdk-redshiftdata client, SigV4 with the profile's AwsTarget
                                              ExecuteStatement(ClientToken) → DescribeStatement (100 ms → 1 s) → GetStatementResult pages
                                              cancel / timeout → CancelStatement · browser: ListDatabases / ListSchemas / ListTables / DescribeTable
```

Design: `docs/REMOTE_ENGINES_AND_REDSHIFT.md` §3. No database password exists on the machine: auth is
`iamIdentity` (the IAM identity is the database user), `dbUser` (temporary credentials for one user,
provisioned only) or `secret` (a Secrets Manager ARN the Data API reads server-side).

- **Validation** (`redshift/validate.rs`, via `cleartext::check_profile` on save and on use): cluster
  identifier, workgroup, database, db user, region and secret ARN against AWS's documented rules, so
  nothing unexpected is signed and sent. No endpoint override in the profile.
- **Read-only.** No read-only session exists, so the pre-submit check is the enforcement: the
  classifier must parse and class every statement read-only; Redshift `SHOW …` forms are admitted by a
  narrow text check. Fails closed. Every function call in a read-only run must be on an allow-list
  of Redshift built-ins that only read (schema-qualified calls other than `pg_catalog.` are
  refused): a Lambda UDF or SageMaker function could send rows out, and no read-only session
  would stop it. Such a statement can still run with approval.
- **One meaning only, every mode.** Redshift reads `\'` in a literal as an escaped quote, the
  classifier does not; a statement with a backslash in a string literal (under either reading), or
  a split statement still holding a top-level `;`, is refused before anything is sent. The agent
  policy and `rs_execute`'s production guard apply the same check.
- **Deadlines.** A statement without a timeout gets a 1-hour ceiling; a closed result channel
  stops polling and cancels the statement; `rs_schema_metadata` is capped at 2000 tables and 60 s.
  Endpoint overrides exist only in test builds (`test-util` feature).
- **No transactions.** Each statement commits on its own, so `BEGIN`/`COMMIT`/`ROLLBACK`/`START
  TRANSACTION`/`END`/`ABORT` (and savepoints) are refused in every mode with that reason.
- **Bounds.** `max_rows` with `truncated`, `MAX_CELL_BYTES` markers, `PAGE_BYTE_BUDGET` page cuts,
  error text at 4 KiB, listings at a fixed page count.
- **Agent.** Postgres `aiDataAccess` semantics; the approval reason names
  `Redshift · {cluster or workgroup} · {database} · {region}`; the run_sql database is pinned once as
  for Postgres; agent timeout 120 s.

## 8. Known limitations (V1), stated plainly

* **Valkey cluster mode through an SSH tunnel doesn't work.** Cluster redirects (`MOVED`) point at node IPs the tunnel doesn't forward. ElastiCache Serverless behaves the same way. Workaround: run VibeLens where the VPC is routable (VPN/Direct Connect), or use a non-cluster replica.
* **No SSM Session Manager tunnels yet.** Many enterprises ban bastions in favour of SSM port forwarding. This needs the `session-manager-plugin` protocol and is the top V1.1 item.
* **SSO uses the device-code grant.** The AWS CLI now defaults to authorization-code + PKCE. Device code still works but is phishable; PKCE (loopback redirect) is V1.1.
* **Multi-statement scripts** run statement-by-statement in autocommit unless the script contains its own `BEGIN`/`COMMIT`.
* **Server-side cursors** aren't used. Results are capped at `maxRows` (100k default) and the rest are discarded with `truncated=true`. In a read-only run (every agent read, 200 rows) hitting the cap cancels the statement and closes its connection, so each truncated read costs a reconnect, and a multi-statement read-only script stops at the truncated statement with an error naming the statements that were not run.
* **Iceberg, local engine:**
  * no row-level DML;
  * no views (iceberg-rust 0.10 has no view API);
  * `uuid` columns are shown as bytes;
  * no scan statistics;
  * Lake Formation is not applied, because the engine reads S3 with the user's own credentials.
* **Trino and Spark (Livy)** are tested against in-process mocks only (`tests/remote_*`). Livy cuts SQL results at `livy.rsc.sql.num-rows` (1000) without saying so; a result of exactly 1000 rows is reported as truncated. Livy returns decimals as JSON numbers, so a decimal with more digits than a double holds loses precision before VibeLens sees it. A Spark connection is not probed by "Test connection" (that would start a session); its first query reports problems.
* **Athena** is tested against a mock endpoint only. The header-row and `UpdateCount` handling rest on the documented API shape until test plan ICE-60 to ICE-65 runs on a real account.
* **Redshift** is tested against a mock Data API only (RS-01 to RS-10 are manual). Results are limited to 100 MB per statement by the Data API, and there are no multi-statement transactions.
* **Windows** compiles in CI but hasn't had a manual test pass.

## 9. Roadmap
Done since V1: interface modes · query history · exports · ERD · explain visualiser · table editing · Apache Iceberg + Athena.
V1.1: SSM port forwarding · PKCE SSO · auto-update (docs/RELEASING.md §8) · Parquet export · Iceberg row-level DML on the local engine when iceberg-rust supports delete files.
V1.2: MySQL/Aurora MySQL · DynamoDB read-only · audit log · team-shared profile packs (no secrets).
