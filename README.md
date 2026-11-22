<div align="center">

<img src="VibeLens/public/vibelens.svg" width="88" alt="VibeLens logo" />

# VibeLens

**An AI-first desktop client for PostgreSQL, MySQL, Valkey, Apache Iceberg and Amazon Redshift, built for teams whose data lives inside private AWS VPCs.**

Native Rust core · Tauri v2 shell · an agent that can read your schema and write your migrations, and that can't run a write unless you approve it. Or switch the AI off entirely: **Classic** mode is a fast, normal database client.

[![Licence: source-available](https://img.shields.io/badge/licence-source--available-blue)](LICENCE.md)
![Tauri 2](https://img.shields.io/badge/Tauri-2-24C8DB?logo=tauri&logoColor=white)
![Rust 2024](https://img.shields.io/badge/Rust-2024-000?logo=rust)
![React 19](https://img.shields.io/badge/React-19-61DAFB?logo=react&logoColor=black)
![Platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Windows%20(beta)%20%7C%20Linux%20(beta)-lightgrey)
[![CI](https://github.com/heinM/VibeLens/actions/workflows/ci.yml/badge.svg)](.github/workflows/ci.yml)

<br/>

<img src="docs/images/overview.png" alt="VibeLens main window: seven connections (PostgreSQL, Aurora PostgreSQL, MySQL, Valkey, two Iceberg lakehouses and Redshift) in the sidebar, the Postgres schema tree, a SQL tab with 25 result rows, and the agent panel below" width="100%" />

<sub>The main window: every connection type in the sidebar, the schema tree, a query and its results, and the agent panel.</sub>

<sub>All screenshots show the bundled demo data (the seeded <code>shop</code> schema) with fictional connection names and hosts. They are rendered from the real UI by <a href="VibeLens/scripts/screenshots/capture.mjs"><code>VibeLens/scripts/screenshots</code></a>.</sub>

</div>

---

## Why VibeLens?

Most database GUIs were built for a world of static passwords and public endpoints. Today's production databases are **Aurora Serverless behind a bastion, authenticated with 15-minute IAM tokens, reached through an SSO session**. The usual workaround is a pile of shell scripts to mint tokens and open tunnels.

VibeLens makes that path native, and adds an agent that does real work (inspecting the schema, writing a migration into a `.sql` file, explaining a slow query) under rules the agent can't override.

|                      | Typical DB GUI | VibeLens |
|----------------------|:---:|:---:|
| AWS IAM Identity Center (SSO) sign-in, account and role picker | – | ✅ |
| RDS / Aurora IAM auth, minted per connection | plugin or script | ✅ built in |
| Secrets Manager ARN as the credential | – | ✅ in-memory only |
| ElastiCache (Valkey) IAM auth | – | ✅ |
| MySQL 8, Aurora MySQL and RDS MySQL, with RDS IAM auth | ✅ / plugin | ✅ |
| Apache Iceberg: REST, Glue, S3 Tables, SQL catalogs; local engine, Athena, Trino or Spark | – / separate tool | ✅ |
| Amazon Redshift (provisioned or Serverless) with IAM, through the Data API | JDBC + IAM plugin | ✅ no password, no tunnel |
| Expired SSO session: sign in again from where the error is shown | – | ✅ |
| Works with the AI switched off (Classic mode) | ✅ | ✅ |
| SSH bastion tunnel with host-key pinning | ✅ | ✅ |
| Idle disconnect so Serverless can scale to zero | – | ✅ |
| AI agent with tool use and an approval gate | – / chat box | ✅ agentic loop |
| Per-connection control over what the AI may *see* | – | ✅ |
| Binary size and memory | JVM / Electron | Tauri (system webview) |

---

## Highlights

### 🐘 PostgreSQL
- Lazy schema navigator: databases → schemas → tables, views, matviews, functions, sequences → columns, with PK, FK and index detail.
- **Partitioned tables:** partitions nest under their parent with their bounds, row estimates and sizes (sub-partitions nest too), and a summary row with the partition key and, when the extension manages the table, the **pg_partman** settings (interval, premake, retention, maintenance).
- Monaco editor with `pgsql` highlighting. **⌘/Ctrl+Enter** runs the statement under the cursor (or the selection); **⌘/Ctrl+⇧+Enter** runs the whole script.
- Results **stream** in 500-row pages into a grid virtualized in both directions, so 100k × 50 stays smooth. Queries are cancellable.
- **Exact numbers:** `int8`, `numeric` and `money` arrive as strings, never rounded through a JS double.
- A **production write-guard**: on a `prod` connection, writes and DDL need you to type the connection name.
- Tuned for scale-to-zero: 30s connect timeout to cover Aurora Serverless v2 or Neon resuming, and pools dropped after 5 minutes idle.

<img src="docs/images/postgres-partitions.png" alt="Postgres navigator with the partitioned table shop.order_events expanded: its columns, a summary row reading RANGE (created_at), 11 partitions, pg_partman, and monthly partitions with bounds, row estimates and sizes; a query grouping orders by status in the editor" width="100%" />

<sub>A partitioned table expanded: columns, the key and pg_partman summary, then each partition with its bound, row estimate and size.</sub>

### 🐬 MySQL
- **MySQL 8.x, Aurora MySQL and RDS MySQL**, with a password or **RDS IAM** auth (a fresh token per physical connection), TLS verified against the real host, also through an SSH tunnel.
- Navigator: databases → tables, views, functions, procedures → columns, with keys, indexes and foreign keys. Editor runs, scripts and server-side cancel (`KILL QUERY`), with an optional per-statement timeout.
- **Exact values:** `BIGINT` and `DECIMAL` arrive as strings, `BIT` as binary digits, `JSON` parsed, dates exactly as the server sent them (`0000-00-00` included), binary data as bytes.
- **Read-only runs** are checked by a MySQL-specific lexer before sending, run inside `START TRANSACTION READ ONLY`, and the server's status flags are checked after each statement. `LOCAL INFILE` and multi-statements are removed from the driver (a vendored, patched `mysql_async`, rustls only).
- A stored password is bound to the endpoint and engine it was saved for; to a host other than this machine, or through a tunnel, it needs **Verify full**.

<img src="docs/images/mysql.png" alt="MySQL navigator with the customers table expanded to its columns (bigint unsigned auto_increment, varchar, char, datetime, date, bit(4), json), and a query result showing a zero date, BIT digits, NULLs and JSON values" width="100%" />

<sub>MySQL values as the server sends them: the zero date, <code>BIT(4)</code> as digits, <code>JSON</code> parsed.</sub>

### ⚡ Valkey / Redis OSS
- Key browser built on `SCAN` (never `KEYS`), with a namespace tree (`user:883:auth` → `user / 883 / auth`), lazily paged.
- Typed inspectors for strings (JSON auto-detected), hashes, lists, sets, sorted sets, streams and RedisJSON.
- A console with history and quoting-aware parsing. **Write detection asks the server** (`COMMAND INFO` flags) instead of trusting a hard-coded list, so new commands are classified correctly.
- RESP3 throughout. One connection per logical DB (no shared `SELECT` state). Cluster mode scans every primary.
- TLS is done by VibeLens's own rustls config, so the client can dial a tunnel's local port and still verify the real ElastiCache hostname.

<img src="docs/images/valkey-inspector.png" alt="Valkey key browser with the namespace tree (cart, config, counter, events, feature, leaderboard, user) and the inspector showing the string key config:app parsed as a JSON tree, next to an empty Valkey console" width="100%" />

<sub>The key tree built from <code>SCAN</code>, and a string value detected as JSON.</sub>

### 🧊 Apache Iceberg
- **Catalogs:** Iceberg **REST** (Polaris, Lakekeeper, Nessie, Gravitino, …) with no auth, a bearer token or OAuth2 client credentials; **AWS Glue**; **Amazon S3 Tables**; the **SQL/JDBC** catalog (SQLite or Postgres); Hive Metastore in builds with the `iceberg-hms` feature.
- **AWS identity everywhere:** Glue and S3 Tables use the same SSO / profile / IAM role as the rest of the app. Temporary credentials are refreshed before they expire.
- **Four query engines**, chosen per connection (metadata browsing always goes to the catalog):
  - **Local:** embedded Apache DataFusion that reads the Parquet files directly. Nothing is billed. It supports SELECT, joins and aggregates, `INSERT` (append), `CREATE TABLE`/CTAS, **time travel** (`shop."orders@<snapshot-id|branch|tag|timestamp>"`) and metadata tables (`"orders$snapshots"`, `"orders$manifests"`, `"orders$history"`). Built on iceberg-rust 0.11 and DataFusion 54.
  - **Athena** (Glue / S3 Tables): full `UPDATE`/`DELETE`/`MERGE` and Lake Formation enforcement. Every result shows data scanned and an **≈ cost estimate** at list price.
  - **Trino**, over its client REST protocol, with any catalog kind: name the Trino catalog that points at the same Iceberg catalog. Auth none, basic or bearer token; results show data processed and Trino's query id.
  - **Spark**, through **Apache Livy** (for example EMR on EKS's interactive endpoint): one session per connection, reused, with an allow-list of Spark settings.
- **Table explorer:** schema with field IDs and nested types, partition spec and sort order, snapshot timeline with "query as of this snapshot", data preview, and **expire snapshots** behind a confirmation.
- **S3-compatible storage** (MinIO, R2, Ceph) with your own endpoint. It overrides whatever endpoint the catalog advertises, so a REST catalog that says `http://minio:9000` still works from your laptop.
- A Trino or Livy password or token lives only in the OS vault, bound to its endpoint and CA file. TLS is always verified, no redirect is followed, and a Trino `nextUri` must stay on the same origin.

<img src="docs/images/iceberg-trino.png" alt="Iceberg catalog tree (REST catalog, namespaces history and shop, the partitioned orders table expanded to its fields with field IDs) and a query on the Trino engine: six rows of revenue by country, with a Trino badge, 60.4 KB scanned and the Trino query id" width="100%" />

<sub>An Iceberg REST catalog queried through Trino. The status line names the engine and what it reported.</sub>

<img src="docs/images/iceberg-engines.png" alt="Edit connection dialog, Query engine section: Local (DataFusion, free), Amazon Athena (about $5 per TB scanned, Glue and S3 Tables only), Trino (selected) and Spark (Livy), each with what it can and cannot do" width="100%" />

<sub>The engine is a per-connection choice, with what each one can do spelled out.</sub>

### 🟪 Amazon Redshift
- **Provisioned clusters and Serverless workgroups**, through the **Redshift Data API** over HTTPS, signed with your AWS identity: no database password on the machine and no network path to the cluster.
- Sign in as your IAM identity (default), as a named database user (provisioned), or with a Secrets Manager secret.
- Navigator: databases → schemas → tables and views → columns. Results show Redshift's statement id; the approval card shows cluster or workgroup, database, region and database user.
- Read-only is checked before a statement is submitted. Each statement commits on its own (the Data API has no multi-statement transactions), so `BEGIN`/`COMMIT`/`ROLLBACK` are refused.

<img src="docs/images/redshift.png" alt="Redshift navigator (schemas public and shop, the orders table expanded to int8, varchar, numeric and timestamptz columns) and a query result of 25 rows with a Redshift badge and the statement id" width="100%" />

<sub>A Serverless workgroup through the Data API. The status bar shows the target: <code>Redshift · analytics · dev · eu-west-1 · as the IAM identity</code>.</sub>

### ☁️ AWS, natively
- **Your existing `~/.aws/config` profiles just work**: SSO, `sso-session`, assume-role chains, `credential_process`.
- Or **sign in with IAM Identity Center** inside the app: device code, browser opens, then pick an account and role. The token is cached in the same CLI-compatible format, so the `aws` CLI sees it too.
- **RDS IAM tokens** are signed for the real endpoint, per new physical connection. RDS checks the token only at connect time, so there's no refresh loop to babysit.
- **Secrets Manager**: paste an ARN; the secret is fetched at connect time, held as a zeroizing `SecretString`, and **never crosses into the UI**.
- **ElastiCache IAM** for Valkey/Redis OSS 7+ (replication groups and Serverless), with transparent re-auth after the service's 12-hour connection limit.
- **Session expired?** When the IAM Identity Center session has run out, the connect screen and every engine's tree offer **Sign in again** in place: the device login runs, then the connection reconnects and the tree reloads.

<img src="docs/images/sso-sign-in-again.png" alt="Navigator for an Athena lakehouse connection that is not connected: the error reads authentication failed: SSO session expired, with a Sign in again button and a Connect button below it" width="100%" />

<sub>An expired Identity Center session, with the sign-in offered where the error is.</sub>

### 🧭 Use it your way: Classic, Assist or Agent-first
On first run you pick a mode. Change it any time in Settings or the command palette.
- **Classic:** no AI anywhere: no agent panel, no inline AI, no AI buttons, and no traffic to any model provider. A fast, keyboard-driven database client.
- **Assist** (default): the agent panel is a ⌘J away; ⌘K inline AI, "Fix with AI" on errors and "Explain with AI" on plans.
- **Agent-first:** the agent sits beside the editor and drives; you review and approve.

### 🛠️ Editor and data tools
- **Explain visualiser:** plan tree, cost hot spots, row misestimates and other insights. "Explain with AI" redacts literal values unless the connection allows results.
- **Table tab** (Postgres): server-side filter and sort; edit cells, insert and delete rows; review the generated SQL; apply in one transaction with row-count checks.
- **ER diagram** of a schema (⌘⇧E) with SVG export. **Query history** with secrets redacted; prod connections aren't recorded.
- **Export** CSV / TSV / JSON / SQL / Markdown, with spreadsheet formula injection defused. Files are written owner-only.
- **Command palette:** ⌘P go to anything, ⌘⇧P commands. Schema-aware autocomplete and hover.
- **Unsaved work is safe:** ⌘W closes a tab, and closing the window or ⌘Q with unsaved tabs asks first. That check runs natively, so it holds even if the UI has crashed. Open tabs and their unsaved SQL come back after a reload or restart (kept in local storage; results are not).
- **Workspace files:** import existing files through a native picker, or reveal the folder. Path, extension and size rules are enforced in Rust.
- Query error banners and the "Fix with AI" strip can be dismissed.

### 🤖 The agent
A real tool-use loop in Rust, not a text-to-SQL box. The agent can list connections and databases, read schema metadata, describe tables, run SQL, scan and read Valkey keys, and read and write files in your workspace.

```text
you › Add a soft-delete column to orders and backfill it safely. Save it as a migration.

agent › calling get_schema(main_db, public)                         ✓ 38 tables · 41 ms
agent › calling run_sql  SELECT count(*) FROM orders WHERE …        ✓ 1 row · 12 ms
agent › wants to write_file migrations/2026_10_31_orders_soft_delete.sql
        ┌ approval required ─────────────────────────────────────────┐
        │ + ALTER TABLE orders ADD COLUMN deleted_at timestamptz;      │
        │ + CREATE INDEX CONCURRENTLY … WHERE deleted_at IS NULL;     │
        └───────────────────────────── [Approve ⌘↵]  [Reject Esc] ───┘
agent › Written. Review it in the new tab; it batches the backfill in 10k-row chunks…
```

- **Pin the target:** the agent follows the active tab unless you pin it to a connection and database; pinned, a follow-up cannot land on another database when you switch tabs.
- **Open in tab / Run in tab:** every `run_sql` card can open its SQL in a new tab on the connection and database it actually ran on, and a read-only one can be re-run there (read-only again), so the full result lands in the grid. The agent path sends no rows to the UI.
- `list_databases` (Postgres, MySQL, Redshift) returns names only and is refused at AI access `none`. The transcript follows the stream until you scroll up, and errors in it can be dismissed.
- **Providers:** Anthropic (native Messages API), the Portkey AI gateway, or any OpenAI-compatible endpoint: xAI Grok, Gemini's OpenAI-compatible endpoint, or a **local model through Ollama / LM Studio / vLLM**, so nothing leaves your machine. Model IDs are configuration, not code.
- **Context window per provider:** for small local models, the history sent each turn is cut to fit. The Ollama preset starts at 16,384 tokens with 4,096 for output.
- API keys live in the **OS keychain**.

<img src="docs/images/agent-run-in-tab.png" alt="Agent-first layout: the agent pinned to Shop / vibelens_test answered a question about top customers after get_schema, describe_table and run_sql tool cards; the run_sql card has Open in tab and Run in tab buttons, and the query re-run in a tab on the left shows 25 rows" width="100%" />

<sub>Agent-first mode: tool cards, the pinned target, and a read-only query re-run in a tab with <b>Run in tab</b>.</sub>

<img src="docs/images/settings-providers.png" alt="Settings, AI providers: the Ollama (local) provider expanded with model qwen3:14b, base URL http://localhost:11434/v1, max output tokens 4096 and context window 16384, with a hint about OLLAMA_CONTEXT_LENGTH" width="100%" />

<sub>A local Ollama model, with the context window the agent sizes its history to.</sub>

---

## 🔒 Security model

> **Production-honest summary:** the approval gate and the read-only transaction are enforced in Rust. SQL classification is a routing hint. **Your real boundary is a least-privilege database role.**

**What the agent may *see***, set per connection:

| AI data access | What can leave your machine |
|---|---|
| `none` | Nothing. The agent's tools refuse this connection. |
| `schemaOnly` *(default)* | Catalog metadata: table and column names, types, indexes, key names. **No rows, no values.** |
| `resultsWithApproval` | Rows and values, but every call that reads data asks you first. |
| `results` | Rows and values from read-only calls, without asking. |

**What the agent may *do***:

| Action | Behaviour |
|---|---|
| `SELECT`, `SHOW`, `EXPLAIN` | Postgres: runs inside `BEGIN READ ONLY` with a snapshot already taken, so the server rejects any write, and `SET TRANSACTION READ WRITE` is refused. `COMMIT`/`ROLLBACK` are blocked inside the read-only run. MySQL: checked by its own lexer, run in `START TRANSACTION READ ONLY`, status flags checked after each statement. Iceberg engines and Redshift: checked before the statement is submitted; anything the check cannot classify is refused. |
| `INSERT`/`UPDATE`/`DELETE`/DDL/unparseable SQL | **Approval required**, with the SQL shown. Prod connections are flagged in red. |
| Valkey writes (per the server's `COMMAND INFO`), `EVAL`/`FCALL` | **Approval required.** |
| Valkey admin commands (`CONFIG`, `ACL`, `KEYS`, `CLIENT`, `MONITOR`, …) | **Always denied** to the agent. `CONFIG GET *` can return `requirepass`. |
| Writing a workspace file | **Always** needs approval, with a unified diff preview. |
| Where an approved call runs | The card shows the real target, resolved by Rust from the stored profile: connection, environment, database; for Redshift, Trino and Spark also cluster or workgroup, endpoint, catalog and acting user. |
| Files outside the workspace, `..`, symlink escapes | Impossible: path jail, extension allowlist, 5 MiB cap. |

<img src="docs/images/agent-approval.png" alt="The agent counted 1,040 stale pending orders with a read-only run_sql, then proposed an UPDATE; the approval card shows Runs on Shop DEV database vibelens_test, the reason, the exact SQL, a reject reason field, and Reject and Approve buttons" width="100%" />

<sub>A write waits for approval. The card names where it runs and shows exactly the SQL that will execute.</sub>

<img src="docs/images/connection-ai-access.png" alt="New connection dialog, AI access section: None, Schema only (default), Results with approval, and Results, each with what reaches the model provider" width="100%" />

<sub>What the agent may see is set per connection.</sub>

**Where secrets live:**

| Secret | Stored |
|---|---|
| Passwords, SSH keys' passphrases, LLM API keys, Trino / Livy passwords and tokens | macOS Keychain / Windows Credential Manager. MySQL passwords and engine secrets are bound to the endpoint they were saved for: editing the profile file does not redirect them. |
| Secrets Manager values, IAM DB tokens | Process memory only (zeroized on drop), never sent to the UI |
| SSO access token | `~/.aws/sso/cache/*.json`, **plaintext, exactly like the AWS CLI** |
| Connection profiles | `profiles.json` in the app config dir. No secrets: passwords, tokens and keys are not part of a profile, and a save is refused if a header or a catalog URI carries one. |

Also: a strict CSP, Tauri capabilities limited to `core:default` (plus opening `https://` links), TLS verify-full by default (with the RDS CA bundle), and pinned SSH host keys: a key is never trusted silently, and no credential is sent to a host whose key you have not checked.

**Prompt injection, honestly.** Tool output is fed back to the model framed as untrusted data, and a write needs your approval. But `READ ONLY` is not a sandbox: functions such as `dblink`, `pg_read_file`, `lo_export`, `pg_terminate_backend` and session-level advisory locks still work inside a read-only transaction. VibeLens routes any SELECT that calls a known side-effecting function to approval, and classification **fails closed**: anything it can't positively prove read-only goes to approval. That list is still a denylist. A least-privilege role is what actually contains a determined injection (see below).

<details>
<summary><b>Recommended: a read-only role for agent use</b></summary>

```sql
CREATE ROLE vibelens_agent LOGIN;
GRANT rds_iam TO vibelens_agent;                       -- RDS IAM auth (or set a password)
GRANT CONNECT ON DATABASE main_db TO vibelens_agent;
GRANT USAGE ON SCHEMA public TO vibelens_agent;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO vibelens_agent;
ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO vibelens_agent;
ALTER ROLE vibelens_agent SET default_transaction_read_only = on;
ALTER ROLE vibelens_agent SET statement_timeout = '30s';
```
Then make a second profile for that role and point the agent at it.
</details>

---

## Architecture

```mermaid
flowchart TB
  subgraph UI["React 19 · Monaco · TanStack Query/Table/Virtual · Zustand"]
    IPC["src/ipc/commands.ts<br/>(only place that calls invoke)"]
  end
  subgraph Shell["src-tauri · thin IPC adapter"]
    CMD["commands/*.rs<br/>prod write-guard · error mapping"]
  end
  subgraph Core["crates/vibelens-core · no Tauri dependency"]
    CM["ConnectionManager<br/>lazy pools · idle reaper"]
    AG["AgentService<br/>loop · policy · providers"]
    PG["pg (tokio-postgres + deadpool)"]
    MY["mysql (mysql_async, vendored + patched)"]
    VK["valkey (redis-rs RESP3)"]
    ICE["iceberg (iceberg-rust + DataFusion)"]
    REM["athena · remote (Trino, Livy)"]
    RS["redshift (Data API)"]
    AWS["aws (SSO-OIDC · SSO · RDS signer · SigV4 · Secrets Manager)"]
    TUN["tunnel (russh)"]
    VAULT["vault (keyring-core)"]
    WS["workspace (path jail)"]
  end
  IPC -- "invoke / Channel&lt;events&gt;" --> CMD
  CMD --> CM & AG
  AG -- "ToolBackend port" --> CM & WS
  CM --> PG & MY & VK & ICE & RS & TUN
  ICE --> REM
  PG & MY & VK -- "CredentialFn per connection" --> AWS
  ICE & RS & REM --> AWS
  AWS --> VAULT
```

The core is a **ports-and-adapters** crate. Every service depends on traits (`SecretVault`, `ProfileStore`, `CredentialSource`, `TunnelProvider`, `ToolBackend`), so the engines are tested headless against real Postgres, Valkey and MySQL in CI, Athena, Trino, Livy and the Redshift Data API against in-process mock servers, and the agent loop against a mock LLM server. Full rationale, including every deviation from the original spec, is in **[docs/DESIGN.md](docs/DESIGN.md)**.

---

## Getting started

### Prerequisites
| | macOS | Windows |
|---|---|---|
| Rust (stable, via [rustup](https://rustup.rs)) | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` | `winget install Rustlang.Rustup` |
| Build tools | `xcode-select --install` | Visual Studio Build Tools ("Desktop development with C++") |
| Webview | built in (WKWebView) | WebView2 (preinstalled on Windows 10/11) |
| Node.js | 24 | 24 |

### Run it: macOS, one command
```bash
git clone https://github.com/heinM/VibeLens.git && cd VibeLens/VibeLens
./scripts/dev-mac.sh            # installs Rust if needed, Node 24 via nvm, starts seeded Postgres + Valkey, launches the app
```
It starts Postgres on `localhost:55432` (`postgres` / `vibelens`, with a demo `shop` schema of 25k orders) and Valkey on `localhost:56379` (about 1,800 sample keys). Pass `--no-db` to skip Docker, or `--iceberg` to also start MinIO and an Iceberg REST catalog with demo tables (needs `python3`). On an empty sidebar, **Add local demo connections** creates the Postgres and Valkey connections for you.

MySQL is a separate service in the same compose file:
```bash
docker compose -f docker-compose.dev.yml up -d mysql
```
It listens on `127.0.0.1:53306` with a demo `shop` database. Connect as `vibelens` / `vibelens` (or `vibelens_ro` / `vibelens`, SELECT only, for agent use), database `shop`, TLS **Require**: the image has a self-signed certificate, and MySQL's default authentication sends a password only over TLS.

### Run it: manually
```bash
git clone https://github.com/heinM/VibeLens.git && cd VibeLens/VibeLens
npm install
./scripts/fetch-rds-ca.sh        # Windows: scripts\fetch-rds-ca.ps1 — needed for TLS to RDS/Aurora
npm run tauri dev
```

### Build installers
```bash
npm run tauri build              # → src-tauri/../target/release/bundle/{dmg,macos,msi,nsis}
```
Or build a release on a Mac without GitHub Actions: `scripts/release-local.sh` builds the universal macOS app (signed and notarised when the Apple variables of [docs/RELEASING.md](docs/RELEASING.md) are set), with `--linux` also the `.deb` and `.AppImage` in a container, and uploads them to a draft GitHub release (`--no-upload` builds only). Windows installers need a Windows machine or runner.
Unsigned by default. Signing, notarisation, Windows certificates (and what they cost from South Africa), auto-update and the tag-triggered release workflow (`.github/workflows/release.yml`) are covered in **[docs/RELEASING.md](docs/RELEASING.md)**.

---

## Connecting to AWS

**New connection** (in the title bar, or **+** in the sidebar) asks for the engine first; the sections that follow depend on it. Existing connections can be imported from DataGrip, DBeaver, Azure Data Studio, pgAdmin and `pg_service.conf`; no secrets are imported.

<img src="docs/images/new-connection.png" alt="New connection dialog, General section: engine choices PostgreSQL, Valkey / Redis, Apache Iceberg, MySQL and Amazon Redshift, then name, host, port and database fields" width="100%" />

<sub>The five connection types.</sub>

<details>
<summary><b>RDS / Aurora with IAM authentication</b></summary>

1. Enable IAM DB authentication on the instance or cluster.
2. In the database: `CREATE USER app_ro; GRANT rds_iam TO app_ro;`
3. Allow your role `rds-db:connect` on `arn:aws:rds-db:<region>:<account>:dbuser:<DbiResourceId or cluster-ResourceId>/app_ro`.
4. In VibeLens: **Auth → RDS IAM**, username `app_ro`, pick your AWS profile or Identity Center role, and set the region. Leave TLS on **Verify full** and tick **Trust Amazon RDS CA bundle**.

**Aurora MySQL / RDS MySQL** work the same way: `CREATE USER app_ro IDENTIFIED WITH AWSAuthenticationPlugin AS 'RDS';`, the same `rds-db:connect` permission, then **Auth → RDS IAM** on a MySQL connection with **Verify full**.
</details>

<details>
<summary><b>Secrets Manager</b></summary>

**Auth → Secrets Manager**, then paste the secret ARN. RDS-managed secret JSON (`username`, `password`, `host`, `port`, `dbname`) is understood; host, port and database from the secret fill in anything the profile leaves blank. Your role needs `secretsmanager:GetSecretValue`, plus `kms:Decrypt` if the secret uses a customer-managed key.
</details>

<details>
<summary><b>ElastiCache for Valkey with IAM</b></summary>

Create an IAM-enabled user (`--authentication-mode Type=iam`), add it to the cache's user group, and allow `elasticache:Connect` on both the cache and the user. In-transit encryption must be on. In VibeLens: **Auth → ElastiCache IAM**, with the user id, the replication-group or serverless-cache name, and the region.
</details>

<details>
<summary><b>Apache Iceberg on Glue, S3 Tables, Athena, Trino and Spark</b></summary>

New connection → **Apache Iceberg**. Pick a catalog, an AWS identity (SSO profile or Identity Center role) and an engine.

| Engine | Read needs | Writes also need |
|---|---|---|
| Local, Glue catalog | `glue:GetDatabase(s)`, `glue:GetTable(s)`; `s3:GetObject`, `s3:ListBucket` on the table locations | `glue:UpdateTable`, `s3:PutObject` |
| Local, S3 Tables | `s3tables:ListNamespaces`, `ListTables`, `GetTable`, `GetTableMetadataLocation`, `GetTableData` | `s3tables:PutTableData`, `UpdateTableMetadataLocation` |
| Athena | the above for its catalog, plus `athena:StartQueryExecution`, `GetQueryExecution`, `GetQueryResults`, `StopQueryExecution`, `GetWorkGroup`, and S3 write on the results location | Lake Formation grants, if LF governs the tables |

The local engine reads the files with **your** credentials, so Lake Formation column and row filters don't apply there; Athena enforces them. Give the Athena workgroup a **per-query data-scanned limit**: the cost shown in VibeLens is an estimate (US$5/TB list price, 10 MB minimum), not a cap.

**Trino** and **Spark (Livy)** work with any catalog kind and need no AWS permissions of their own: give the coordinator or Livy URL, the Trino or Spark catalog name that points at the same Iceberg catalog, and the user (Trino) or optional proxy user (Livy). Each statement starts fresh on Trino, so `USE`, `SET SESSION` and transactions are refused; on Livy the first query waits for the session to start, and Livy may cap results (1000 rows by default). Design and security rules: [docs/REMOTE_ENGINES_AND_REDSHIFT.md](docs/REMOTE_ENGINES_AND_REDSHIFT.md).
</details>

<details>
<summary><b>Amazon Redshift with IAM</b></summary>

New connection → **Amazon Redshift**. Pick a provisioned cluster or a Serverless workgroup, the
database, an AWS identity and how the database user is chosen. Every call goes through the Redshift
Data API over HTTPS, signed with that identity: no database password is stored, and no tunnel or VPN
to the cluster is needed.

| Auth | IAM permissions (besides the Data API calls below) |
|---|---|
| IAM identity (default): your IAM user or role is the database user (`IAM:user` / `IAMR:role`) | provisioned: `redshift:GetClusterCredentialsWithIAM`; Serverless: `redshift-serverless:GetCredentials` |
| Database user (provisioned only) | `redshift:GetClusterCredentials` on that `dbuser` (and `dbname`) |
| Secrets Manager secret | `secretsmanager:GetSecretValue` on the secret |

Data API calls: `redshift-data:ExecuteStatement`, `DescribeStatement`, `GetStatementResult`,
`CancelStatement`, `ListDatabases`, `ListSchemas`, `ListTables`, `DescribeTable`. Each statement commits on
its own (the Data API has no multi-statement transactions), so `BEGIN`/`COMMIT`/`ROLLBACK` are refused;
results are limited to 100 MB per statement.
</details>

<details>
<summary><b>Private VPC via SSH bastion</b></summary>

**Network → SSH tunnel**: bastion host, user, and ssh-agent, key file or password. The first time you connect or test, VibeLens shows the host key fingerprint and sends nothing to the server (no user name, key or password) until you have checked it and chosen to trust it. You can also paste the fingerprint into the connection up front. A later mismatch is refused. VibeLens forwards a random local port and still verifies TLS against the real database hostname.
</details>

---

## Where things live

| What | macOS | Windows |
|---|---|---|
| Profiles and provider config | `~/Library/Application Support/com.heinvandermerwe.vibelens/` | `%APPDATA%\com.heinvandermerwe.vibelens\` |
| Worksheets (agent-writable) | `~/Documents/VibeLens/` | `Documents\VibeLens\` |
| Secrets | Keychain, service `com.heinvandermerwe.vibelens` | Credential Manager, same name |

Override the workspace with `VIBELENS_WORKSPACE=/path`.

## Keyboard shortcuts
| | |
|---|---|
| Run statement / selection | ⌘/Ctrl + Enter |
| Run script | ⌘/Ctrl + ⇧ + Enter |
| Save worksheet | ⌘/Ctrl + S |
| New SQL tab | ⌘/Ctrl + N |
| Toggle sidebar / agent panel | ⌘/Ctrl + B / ⌘/Ctrl + J |
| Approve / reject agent action | ⌘/Ctrl + Enter / Esc |

---

## Development

```text
.
├── .github/workflows/          ← CI and multi-platform releases
├── docs/                       ← architecture and release documentation
└── VibeLens/                   ← desktop application
    ├── Cargo.toml              ← Rust workspace
    ├── crates/vibelens-core/   ← engine logic (no Tauri dependency)
    ├── src-tauri/              ← IPC commands, app state, config
    └── src/                    ← React app (ipc/, store/, components/, hooks/)
```

```bash
docker compose -f docker-compose.dev.yml up -d
export VG_TEST_PG="host=127.0.0.1 port=55432 user=postgres password=vibelens dbname=vibelens_test"
export VG_TEST_VALKEY=redis://127.0.0.1:56379
export VG_TEST_MYSQL="host=127.0.0.1 port=53306 user=root password=vibelens dbname=shop"
export VG_TEST_REQUIRE=pg,valkey,mysql   # optional: a test that would skip for lack of one of these fails
cargo test -p vibelens-core     # engines, AWS signing, agent loop vs mock LLM
npm run typecheck && npm test   # frontend
```

Tests that need a service which is not configured skip: they print `skipping (<service>): <why>` (shown with `-- --nocapture`) and pass. `VG_TEST_REQUIRE` lists the services that must be there, and a skip for one of them is a failure; CI sets it to `pg,valkey,mysql`. CI runs only on a pull request into `main`; the branch model is in [docs/RELEASING.md](docs/RELEASING.md) §0. The service names are in `crates/vibelens-core/tests/services/mod.rs`.

README screenshots are rendered from the real UI with a demo stand-in for the backend: `node scripts/screenshots/capture.mjs` (see the comment at its top; `refresh-data.mjs` copies the rows from the seeded containers).

**Contract rule:** `crates/vibelens-core/src/model/*.rs` ⇄ `src/ipc/types.ts`, and `src-tauri/src/lib.rs` ⇄ `src/ipc/commands.ts`. Change both sides in the same commit.

---

## Known limitations
- **Valkey cluster mode doesn't work through an SSH tunnel** (`MOVED` redirects point at node IPs). Use a VPN-routable network, or a non-cluster endpoint.
- **No SSM Session Manager port forwarding yet**; it's the top V1.1 item.
- Identity Center sign-in uses the **device-code** grant. Authorization code + PKCE (the AWS CLI's current default) is planned.
- Results are capped at `maxRows` (100k by default) rather than using server-side cursors.
- A single value over 1 MiB (Postgres, local Iceberg engine) is shown cut, with its real size. It cannot be edited, and copies and exports carry a note instead of it. To read more of it, select a part with SQL (`substr`, `->>`). There is no "load full value" yet.
- **Linux** builds (`.deb`, `.AppImage`) run but have no keychain backend; secrets are kept in memory only, and the app shows a warning.
- **Windows** builds compile in CI but haven't had a manual test pass yet. Treat the first Windows release as a beta.
- **Iceberg, local engine:**
  - No row-level `UPDATE`/`DELETE`/`MERGE`; use Athena, Trino or Spark for DML.
  - No views listed.
  - `uuid` columns display as bytes.
  - No bytes-scanned statistic.
  - It reads S3 directly, so **Lake Formation permissions are not applied**; use Athena for LF-governed tables.
- **Iceberg SQL catalogs** created by pyiceberg or Java's default JDBC schema lack a nullable `iceberg_type` column that the Rust catalog needs. VibeLens adds it automatically **only on connections marked dev**. Elsewhere it tells you the one `ALTER TABLE` for the catalog's owner to run.
- **Athena** results and cost come from real AWS APIs, but VibeLens's Athena integration has so far been tested against a mock endpoint, not a live account. Set a per-query data-scanned limit on the workgroup you use.
- **Redshift** has so far been tested against a mock of the Data API, not a live warehouse. No multi-statement transactions (each statement commits on its own); results up to 100 MB per statement.
- **Trino and Spark (Livy)** have so far been tested against mock servers, not a live cluster.
- **MySQL** is tested against MySQL 8.4 and a hostile fake server; Aurora MySQL, RDS IAM end to end and the SSH tunnel path for MySQL have not had a live test yet. MariaDB may work but is not a goal. The EXPLAIN visualiser and the table editor are Postgres only; `EXPLAIN` on MySQL and Redshift runs as an ordinary query.
- A read-only run on MySQL or Redshift still executes functions that a view, a policy or a UDF calls; keep `EXECUTE` and external functions away from the role the agent uses.

## Roadmap
**Done since V1:** interface modes · query history · exports · ERD · explain visualiser · table editing · Apache Iceberg + Athena · Trino and Spark engines · Amazon Redshift · MySQL / Aurora MySQL · Postgres partitions and pg_partman · restored tabs · agent target pinning
**Next:** SSM port forwarding · PKCE SSO · auto-update · Parquet export · Iceberg row-level deletes on the local engine (once iceberg-rust supports them)
**Later:** DynamoDB (read-only) · audit log · shareable profile packs (no secrets)

---

## Licence

VibeLens is source-available under the **[VibeLens Source-Available Licence 1.0](LICENCE.md)**:

| Use | Allowed? |
|---|---|
| **Personal use** by an individual (learning, hobby projects, personal evaluation) | ✅ Free |
| **Any use by or for a company, business or other organisation**, including against its systems or data, or in providing services | 💼 Needs a paid commercial licence **or** explicit written approval from the author |
| **Using, or forking, VibeLens to work with Microsoft SQL Server or Oracle Database** | 🍷 Prohibited, unless you deliver a case of **Domaine de la Romanée-Conti, Romanée-Conti 2015**, unspoiled, and the author confirms receipt in writing |

Each version converts to **Apache 2.0** on its Change Date (31 October 2036, or earlier if announced), and the restrictions above then lapse for that version. For commercial licensing, open an issue or contact the author via GitHub.

<sub>Built with Tauri, Rust and React. PostgreSQL, MySQL, Valkey, Redis, Apache Iceberg, Trino, Apache Spark, AWS and the AI providers named here are trademarks of their respective owners; VibeLens is not affiliated with or endorsed by them.</sub>
