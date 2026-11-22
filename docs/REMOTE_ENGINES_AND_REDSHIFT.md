# Design: Trino and Spark query engines, and Amazon Redshift with IAM

Status: design, 2026-11-21. Two features, built to the same rules as `docs/SECURITY_REVIEW.md`
and the 2026-11-20 review fixes (PR #9).

1. **Trino and Spark as remote query engines for Iceberg connections**, next to Athena.
2. **Amazon Redshift connections authenticated with IAM**, through the Redshift Data API.

Sections 1 to 3 are the design. Section 4 is the contract between Rust and the UI: the names in
it are fixed, so the three implementations (two Rust, one UI) can be written apart and still fit.

---

## 0. Security rules that apply to everything below

These are not new. They are the rules the code already follows, stated once so each new piece
can be checked against them.

- **R1. Secrets live in the vault only.** Never in `profiles.json`, never in a log, an error
  message or an event. Error text that could echo a credential goes through redaction.
- **R2. A secret is never sent in cleartext to another machine.** `http://` is accepted for
  loopback only when a credential is attached. Enforced in Rust, on save
  (`cleartext::check_profile`) and again on use. The UI check is a convenience.
- **R3. A secret is sent only to the endpoint the user configured.** No redirect is followed
  with a credential attached. A URL that comes back from a server (Trino's `nextUri`) is used
  only when it has the same scheme, host and port as the configured endpoint.
- **R4. TLS certificates are always verified.** There is no "skip verification" switch for the
  new HTTP engines. A private CA is trusted by naming its PEM file.
- **R5. Read-only is enforced before anything is submitted.** None of the new engines has a
  read-only transaction, so the check is the enforcement and it fails closed: a statement that
  is not provably a read is refused, including one the parser cannot read.
- **R6. Server-supplied data is bounded.** Response bodies, cells (`MAX_CELL_BYTES`), pages
  (`PAGE_BYTE_BUDGET`), row counts (`ExecOptions.max_rows`), error text and identifiers that
  are put into URLs are all limited or validated.
- **R7. The agent sees no more than `AiDataAccess` allows**, and the approval card names the
  real target (engine, endpoint or cluster, catalog or database), resolved once and handed to
  execution.
- **R8. Every run can be cancelled and has a timeout**, and a cancelled or timed-out run stops
  the work on the server (not only the wait on the client).
- **R9. Files on disk can be edited by hand and IPC can be called directly.** Validation is in
  Rust. Every enum added here is rejected, not defaulted, when it is malformed.

---

## 1. Trino query engine

### 1.1 What it is

`IcebergQueryEngine::Trino`: SQL of an Iceberg connection runs on a Trino coordinator (the
user's, for example on EKS) through Trino's client REST protocol. Metadata browsing (namespaces,
tables, snapshots) keeps going straight to the Iceberg catalog, as with Athena. Unlike Athena,
Trino works with every catalog kind: the user names the Trino catalog that points at the same
Iceberg catalog.

### 1.2 Configuration (in the profile, no secrets)

| Field | Meaning |
|---|---|
| `url` | Coordinator base URL, `https://trino.example:8443`. No credentials, query or fragment. |
| `catalog` | Trino catalog name (`iceberg`, `lake`, …). Sent as `X-Trino-Catalog`. |
| `user` | `X-Trino-User`. Required. |
| `auth` | `none`, `basic` (password in the vault) or `jwt` (bearer token in the vault). |
| `caCertPath` | Optional PEM bundle to trust in addition to the OS roots. |

The connection's `defaultNamespace` is sent as `X-Trino-Schema`. `X-Trino-Source` is `vibelens`.

### 1.3 Protocol

`POST {url}/v1/statement` with the SQL as the body, then `GET nextUri` until the answer has
none. Columns come from `columns`, rows from `data`, the end state and `processedBytes` from
`stats`, failures from `error`. One statement per request; a script runs its statements in
order and stops at the first failure. Cancel is `DELETE` on the last `nextUri`.

Event contract is the one Athena and Postgres use: `Started`, then per statement `Columns`,
`Rows`*, `Stats`, `StatementDone`, and exactly one `Finished` or `Failed`.
`Stats { engine: "trino", bytes_scanned: processedBytes, cost_usd_estimate: None, engine_query_id: id }`.

### 1.4 Security decisions

- **`nextUri` is checked (R3).** It must have the scheme, host and port of `url`. Anything else
  fails the statement before a request is made, with a message that says so and names
  `http-server.process-forwarded=true` (the usual cause behind a TLS-terminating load
  balancer). The same check applies to the URL that is sent the cancel.
- **No redirects** (`redirect::Policy::none()`); a 3xx is an error that names the target
  without its credentials or query, as the LLM client does.
- **Credentials need TLS (R2).** `basic` or `jwt` with an `http://` URL that is not loopback is
  refused on save and on use. `none` over `http://` to another machine is allowed (no secret
  travels) and the UI warns that queries and results are not encrypted.
- **TLS is verified (R4)**, OS roots plus `caCertPath`.
- **Read-only (R5)** uses Athena's check (`athena::engine::check_read_only`, Trino is the same
  dialect), moved to a shared place.
- **Session headers from the server are ignored.** `X-Trino-Set-Session`,
  `X-Trino-Set-Catalog`, `X-Trino-Started-Transaction-Id` and the like are not carried into the
  next request: each statement starts from the profile. Statements that only make sense with
  them (`USE`, `SET SESSION`, `START TRANSACTION`) are refused with that explanation.
- **Bounded (R6):** a response body larger than 64 MiB fails the statement; cells and pages use
  the existing caps; the query id is used only inside server-supplied URLs that passed the
  endpoint check.
- **Errors are redacted (R1):** the password or token never appears in an error, including
  `reqwest` errors that print the URL.
- **Timeout and cancel (R8):** `ExecOptions.timeout_ms` bounds queue plus run time per
  statement; on expiry or cancel the query is deleted on the coordinator.
- **Retries:** HTTP 502, 503 and 504 on `GET nextUri` are retried (Trino documents this) up to
  3 times with jittered backoff. `POST /v1/statement` is not retried: it is not idempotent.

---

## 2. Spark query engine (Apache Livy)

### 2.1 Why Livy

Spark has three remote interfaces: Spark Connect (gRPC, needs a protobuf stack and has no
standard authentication), the Thrift server (HiveServer2 binary protocol) and Apache Livy
(REST and JSON). Livy is the one that fits what is here: the same HTTP client and rules as
Trino, it runs on Kubernetes (Livy 0.8+), and it is what EMR on EKS exposes as its interactive
endpoint. Spark Connect can be added later behind the same engine enum.

### 2.2 Configuration

| Field | Meaning |
|---|---|
| `url` | Livy base URL, `https://livy.example:8998`. |
| `catalog` | Spark catalog name configured for this Iceberg catalog (`glue`, `lake`, …). |
| `auth` | `none`, `basic` or `bearer`; the secret is in the vault. |
| `caCertPath` | Optional PEM bundle. |
| `proxyUser` | Optional Livy `proxyUser`. |
| `sessionConf` | Spark configuration for the session (`spark.sql.catalog.lake=…`). Not secret. |
| `sessionStartTimeoutSecs` | How long to wait for a new session to become idle. Default 300. |

### 2.3 Protocol

- A session is created on the first query: `POST /sessions` with `kind: "sql"`, `proxyUser`,
  `conf` and a name, then `GET /sessions/{id}` until `idle`. It is reused for later queries
  and deleted (`DELETE /sessions/{id}`) when the connection closes. A session that has died or
  was garbage-collected by Livy is replaced once, transparently.
- A statement: `POST /sessions/{id}/statements` with `kind: "sql"`, then
  `GET /sessions/{id}/statements/{n}` until `available`. Rows and schema come from
  `output.data["application/json"]`. Cancel is `POST …/statements/{n}/cancel`.
- The first statement of a new session is `USE {catalog}.{defaultNamespace}` (identifiers
  quoted with backticks, a backtick in a name doubled).
- Every request carries `X-Requested-By: vibelens` (Livy's CSRF protection wants it on writes).
- `Stats { engine: "spark", bytes_scanned: None, cost_usd_estimate: None, engine_query_id: "{session}/{statement}" }`.

### 2.4 Security decisions

Everything in 1.4 about redirects, cleartext, TLS, redaction, bounds, timeout and cancel
applies. In addition:

- **Read-only on Spark is stricter than on Trino (R5).** A Spark `SELECT` can do things a Trino
  one cannot, so a read-only run (which is every agent run without approval) refuses, besides
  everything that is not `SELECT`, `WITH`, `SHOW`, `DESCRIBE` or `EXPLAIN`:
  - `reflect`, `java_method` and `try_reflect`, which call arbitrary static Java methods in the
    driver;
  - direct file relations, `` parquet.`path` ``, `` text.`path` ``, `` csv.`…` ``, `` json.`…` ``,
    `` orc.`…` ``, `` binaryFile.`…` ``, `` delta.`…` `` and any `` word.`…` `` whose quoted
    part contains `/` or `:`. They read any file the Spark driver can, which on Kubernetes
    includes its service-account token;
  - `ADD JAR`, `ADD FILE`, `SET`, `RESET`, `CACHE`, `REFRESH`, `LOAD`, `TRANSFORM` and
    `LATERAL VIEW` over a user-defined function are not reads and are refused like any write.
  The scan works on the statement with comments and string literals removed, and it fails
  closed: text it cannot tokenise is refused.
- **`sessionConf` cannot carry secrets (R1).** A key or value that looks like a credential
  (key contains `secret`, `password`, `token`, `credential` or ends in `.key`/`access.key`;
  value starts with `Bearer `) is refused on save, with a message that says to configure it on
  the cluster. `spark.jars`, `spark.jars.packages`, `spark.files`, `spark.submit.pyFiles`,
  `spark.driver.extraJavaOptions`, `spark.executor.extraJavaOptions`,
  `spark.driver.extraClassPath` and `spark.executor.extraClassPath` are refused too: they load
  code into the cluster from a profile file that can be edited by hand.
- **Identifiers from the server are validated (R6).** Session and statement ids must be
  non-negative integers before they are put into a URL.
- **Error text is bounded.** `evalue` is kept (redacted, at most 4 KiB); the Java `traceback` is
  dropped.
- **Result size.** Livy returns a SQL result in one response, capped by the server at
  `livy.rsc.sql.num-rows` (1000 by default) without saying it cut anything. The engine cannot
  see that, so a result with exactly as many rows as a known cap is flagged in the stats note
  and the limitation is documented. `max_rows` is still applied on the client.
- **The session is the user's.** It is created with the profile's `proxyUser` and never joined
  from a list: the engine does not adopt sessions it did not create.

---

## 3. Amazon Redshift with IAM

### 3.1 Approach: the Redshift Data API

Redshift speaks an old Postgres wire protocol, so the alternative was to reuse the Postgres
engine with temporary credentials from `GetClusterCredentials`. The Data API is the better fit:

- **IAM is the whole authentication.** Every call is SigV4-signed with the identity VibeLens
  already resolves (named profile, Identity Center session, default chain). No database
  password exists on the laptop at any point, not even a temporary one.
- **No network path to the cluster.** Warehouses in private subnets are reachable through the
  AWS API endpoint, with no bastion, tunnel or VPN. Access is governed by IAM policy and logged
  in CloudTrail.
- **It has the shape of the Athena engine** (submit, poll, page, cancel), so the event
  contract, retries and bounds are reused, and it can be tested offline against a mock.
- **No dialect guesswork in the driver.** The Postgres engine relies on catalog queries, `ctid`
  and binary result formats that Redshift does not have.

Costs of that choice, stated plainly: results are limited to 100 MB per statement, a statement
runs for at most 24 hours, there are no multi-statement transactions (each statement commits on
its own), and results stay retrievable by the same IAM identity for 24 hours.

### 3.2 Configuration

A new `EngineKind::Redshift` with `ConnectionProfile.redshift: Option<RedshiftOptions>`.
`host`, `port`, `auth`, `tls` and `tunnel` are unused, as for Iceberg.

| Field | Meaning |
|---|---|
| `target` | `provisioned { clusterIdentifier }` or `serverless { workgroupName }`. |
| `database` | Database to run in. Required. |
| `aws` | `AwsTarget`: identity and region. |
| `auth` | `iamIdentity` (default), `dbUser { dbUser }` (provisioned only) or `secret { secretArn }`. |

- `iamIdentity`: the caller's IAM identity is the database user (`IAM:user` or `IAMR:role`).
  Needs `redshift:GetClusterCredentialsWithIAM` or `redshift-serverless:GetCredentials`.
- `dbUser`: the Data API fetches temporary credentials for that database user
  (`redshift:GetClusterCredentials` scoped to it).
- `secret`: the Data API reads a Secrets Manager secret. VibeLens only passes the ARN.

### 3.3 Protocol

`ExecuteStatement` (with a `ClientToken`, so a retry cannot run it twice) → poll
`DescribeStatement` (100 ms doubling to 1 s) → page `GetStatementResult` → events. Cancel and
timeout call `CancelStatement`. The browser uses `ListDatabases`, `ListSchemas`, `ListTables`
and `DescribeTable`.

`Stats { engine: "redshift", bytes_scanned: None, cost_usd_estimate: None, engine_query_id: RedshiftQueryId }`
plus the statement's own duration and result size where the UI shows them.

### 3.4 Security decisions

- **No stored secret (R1).** A Redshift profile has no vault entry. `secretArn` is an
  identifier, not a secret, and is validated as an ARN of Secrets Manager.
- **TLS and endpoint (R2 to R4).** Calls go to the regional AWS endpoint over HTTPS with the
  SDK's verification. There is no endpoint override in the profile; tests inject one in code.
- **Read-only (R5).** The Data API has no session to set read-only, so the check is the
  enforcement: a statement is submitted only when the PostgreSQL-dialect classifier parses it
  and classes it read-only. Unparsed text is refused. `SHOW` and Redshift's `SHOW TABLES`,
  `SHOW COLUMNS` forms are admitted by a narrow text check, as for Athena.
- **No transaction control.** `BEGIN`, `COMMIT`, `ROLLBACK`, `START TRANSACTION`, `END` and
  `ABORT` are refused in every mode with the reason (each statement is its own transaction),
  so a script cannot look atomic when it is not.
- **Identifiers are validated before they reach AWS.** Cluster identifier, workgroup name,
  database and db user are checked against Redshift's documented character sets and lengths,
  so nothing unexpected is signed and sent.
- **Bounded (R6).** Cells, pages and rows use the existing caps; paging stops at `max_rows`
  and reports truncation. Error text from Redshift is redacted like Postgres errors (it can
  quote values) before it reaches the agent.
- **Agent (R7).** `AiDataAccess` applies as for Postgres. The approval card shows
  `Redshift · {cluster or workgroup} · {database} · {region}`. Catalog metadata is all that
  `SchemaOnly` gets.
- **Production guard.** The same write guard as Postgres and Iceberg (`prod_guard_reason`).
- **Timeout and cancel (R8).** `CancelStatement` on cancel and on timeout. The agent's default
  timeout is 120 s, as for Athena.
- **Retries.** Throttling and transient transport errors, up to 3 times with jittered backoff,
  as in the Athena engine.
- **What the text check cannot see.** A read-only `SELECT` from an existing view, or from a
  table under a row-level-security or masking policy, whose definition calls a Lambda or
  SageMaker UDF still invokes that function; no check of the statement's text can see it. The
  database role used with the agent should therefore not have `EXECUTE` on external functions.

---

## 4. Contract (fixed names)

### 4.1 Rust model, serialised camelCase

```rust
// model/iceberg.rs — IcebergQueryEngine gains two variants
Trino {
    url: String,
    catalog: String,
    user: String,
    auth: HttpEngineAuth,
    #[serde(default)] ca_cert_path: Option<String>,
},
Spark {
    url: String,
    catalog: String,
    auth: HttpEngineAuth,
    #[serde(default)] ca_cert_path: Option<String>,
    #[serde(default)] proxy_user: Option<String>,
    #[serde(default)] session_conf: BTreeMap<String, String>,
    #[serde(default)] session_start_timeout_secs: Option<u64>,
},

/// tag = "kind": "none" | "basic" | "bearer"
pub enum HttpEngineAuth {
    None,
    /// Password in the vault under `vault_keys::engine_secret`.
    Basic { username: String },
    /// Token (a JWT for Trino) in the vault under `vault_keys::engine_secret`.
    Bearer,
}

// model/profile.rs
EngineKind::Redshift                                   // "redshift"
ConnectionProfile.redshift: Option<RedshiftOptions>    // #[serde(default)]
vault_keys::engine_secret(profile_id) -> "profile:{id}:engine-secret"

// model/redshift.rs
pub struct RedshiftOptions { target: RedshiftTarget, database: String, aws: AwsTarget, auth: RedshiftAuth }
/// tag = "kind": "provisioned" | "serverless"
pub enum RedshiftTarget { Provisioned { cluster_identifier: String }, Serverless { workgroup_name: String } }
/// tag = "kind": "iamIdentity" | "dbUser" | "secret"
pub enum RedshiftAuth { IamIdentity, DbUser { db_user: String }, Secret { secret_arn: String } }

pub struct RedshiftTableRef { schema: String, name: String, kind: String /* TABLE, VIEW, … as Redshift reports it */ }
pub struct RedshiftColumn { name: String, type_name: String, nullable: bool, length: Option<i32>, precision: Option<i32>, scale: Option<i32>, default_value: Option<String> }
```

### 4.2 TypeScript (`src/ipc/types.ts`)

```ts
export type EngineKind = "postgres" | "valkey" | "iceberg" | "redshift";
export type HttpEngineAuth = { kind: "none" } | { kind: "basic"; username: string } | { kind: "bearer" };
export type IcebergQueryEngine =
  | { kind: "local" }
  | { kind: "athena"; workgroup: string; outputLocation: string | null; aws: AwsTarget | null }
  | { kind: "trino"; url: string; catalog: string; user: string; auth: HttpEngineAuth; caCertPath: string | null }
  | { kind: "spark"; url: string; catalog: string; auth: HttpEngineAuth; caCertPath: string | null; proxyUser: string | null; sessionConf: Record<string, string>; sessionStartTimeoutSecs: number | null };

export type RedshiftTarget = { kind: "provisioned"; clusterIdentifier: string } | { kind: "serverless"; workgroupName: string };
export type RedshiftAuth = { kind: "iamIdentity" } | { kind: "dbUser"; dbUser: string } | { kind: "secret"; secretArn: string };
export interface RedshiftOptions { target: RedshiftTarget; database: string; aws: AwsTarget; auth: RedshiftAuth }
export interface RedshiftTableRef { schema: string; name: string; kind: string }
export interface RedshiftColumn { name: string; typeName: string; nullable: boolean; length: number | null; precision: number | null; scale: number | null; defaultValue: string | null }
// ConnectionProfile gains:  redshift: RedshiftOptions | null;
```

### 4.3 Tauri commands

| Command | Arguments | Returns |
|---|---|---|
| `save_profile` (existing) | new optional `engine_secret: Option<String>` (JS `engineSecret`): `None` keeps, `""` deletes, as `iceberg_secret` | profile |
| `rs_list_databases` | `profile_id` | `Vec<String>` |
| `rs_list_schemas` | `profile_id`, `database: Option<String>` | `Vec<String>` |
| `rs_list_tables` | `profile_id`, `database: Option<String>`, `schema: String` | `Vec<RedshiftTableRef>` |
| `rs_describe_table` | `profile_id`, `database: Option<String>`, `schema`, `name` | `Vec<RedshiftColumn>` |
| `rs_schema_metadata` | `profile_id`, `database: Option<String>`, `max_tables: Option<usize>` | `SchemaMetadata` |
| `rs_execute` | `profile_id`, `sql`, `database: Option<String>`, `options`, `confirmed`, `on_event` | query id; same contract as `ice_execute`, cancelled with `pg_cancel` |

Trino and Spark need no new command: they run through `ice_execute`.

`test_connection` and `connect` work for all of them through the existing commands.
`unused_vault_keys` and `delete_profile` cover `engine_secret`.

### 4.4 Ownership

| Part | Owns | May touch, additively |
|---|---|---|
| A. Trino and Spark (Rust) | `crates/vibelens-core/src/remote/**`, `model/iceberg.rs`, `iceberg/handle.rs`, `athena/` (to share the read-only check), `tests/remote_*` | `cleartext.rs`, `model/profile.rs` (vault key), `agent/{policy,prompt,tools}.rs`, `connections/mod.rs`, `src-tauri/src/commands/profiles.rs`, docs |
| B. Redshift (Rust) | `crates/vibelens-core/src/redshift/**`, `model/redshift.rs`, `src-tauri/src/commands/redshift.rs`, `tests/redshift_*` | `model/{mod,profile}.rs`, `lib.rs`, `cleartext.rs`, `connections/mod.rs`, `agent/{policy,prompt,tools}.rs`, `src-tauri/src/{lib.rs,commands/mod.rs}`, `Cargo.toml`, docs |
| C. UI | `VibeLens/src/**` | nothing in Rust |

---

## 5. Testing

- Trino, Livy and the Redshift Data API are each tested against an in-process mock HTTP server
  (the pattern of `tests/athena_common`), covering the protocol and every rule in section 0
  that applies: a `nextUri` to another host is not requested, a redirect is not followed, a
  credential is not sent over cleartext, read-only refusals happen before any request,
  cancellation reaches the server, oversized bodies and cells are cut.
- Each fix-style test states what would go wrong without the rule.
- UI: validation, draft and dispatch tests in Vitest.
- Not covered by automation: a real Trino, Livy or Redshift. First use against the EKS cluster
  and a warehouse is a manual test (`docs/TEST_PLAN.md` gets the cases).

---

## 6. MySQL

Status: implemented 2026-11-22 on `feat/mysql`. MySQL 8.x, Aurora MySQL and RDS MySQL; MariaDB
may work but is not a goal. Code: `crates/vibelens-core/src/mysql/`, `src-tauri/src/commands/mysql.rs`,
`vendor/mysql_async/` (patched driver), UI under `src/` (`mysqlForm.ts`, `MysqlTree.tsx`, the
`mysql` dialect in `lib/sqlIntel`).

### 6.1 Driver: `mysql_async`, vendored and patched

Both candidates were read at their current versions (`mysql_async` 0.37.1 with `mysql_common`
0.37.3; `sqlx-mysql` 0.8.6, the version already in `Cargo.lock`) against the requirements:

| Requirement | `sqlx-mysql` 0.8.6 | `mysql_async` 0.37.1 |
|---|---|---|
| rustls only, no OpenSSL / native-tls | `tls-rustls` feature, but it is a feature of the whole `sqlx` crate, which the Iceberg SQL catalog also uses | `default-features = false` + `rustls-tls`, `aws-lc-rs`; `cargo tree -i openssl-sys` and `-i native-tls` are empty |
| Custom root store, hostname verification, TLS name ≠ dialed address (SSH tunnel) | CA file path only; verifies the name it dials, so a tunnel to `127.0.0.1` cannot be verified as the real host | Root certificates as buffers, built-in roots can be switched off, `tls_hostname_override` |
| Cleartext plugin only when wanted | Accepts `mysql_clear_password` when `enable_cleartext_plugin` | Same option |
| No RSA public-key retrieval from an unverified server | Always retrieves the key without TLS; also pulls `rsa` 0.9 (RUSTSEC-2023-0071) into the build | Retrieves it without TLS too (own RSA code, no `rsa` crate) |
| Multi-statements off | `CLIENT_MULTI_STATEMENTS` hard-coded | Hard-coded |
| No server-initiated file reads | Does not advertise `CLIENT_LOCAL_FILES` | Advertises it; reads a file only if a handler is registered |

Neither meets every point as published. `mysql_async` needs smaller changes, keeps TLS
configurable enough for tunnels and custom CAs, and adds no RustSec advisory, so it is used,
vendored in `vendor/mysql_async` the way `vendor/sqlx-sqlite` is (`[patch.crates-io]`). The
upstream copy is its own commit; the patch is the next one and is listed in
`vendor/mysql_async/PATCH.md`:

1. `CLIENT_LOCAL_FILES` and `CLIENT_MULTI_STATEMENTS` are never advertised.
2. A `LOCAL INFILE` request (0xFB) is never answered with a file: no handler is consulted, the
   connection is closed.
3. `caching_sha2_password` full authentication sends the password only over TLS; without TLS it
   fails (`InsecureFullAuth`) instead of asking the server for an RSA key. A Unix socket no longer
   counts as secure (VibeLens never uses one: `prefer_socket(false)`).
4. An auth switch to `mysql_clear_password` is answered only when enabled *and* over TLS.
5. (Audit 2026-11-22) A server cannot abort the process (release builds use `panic = "abort"`):
   an auth switch to MariaDB's `client_ed25519` or `parsec` is refused as an unknown plugin
   (both reached a `panic`/`unwrap`), a second switch is an error instead of `unreachable!`,
   and a result header announcing more than 4096 columns, or column definitions totalling more
   than 16 MiB, is refused before anything is allocated for it.
6. (Third review) The whole class was swept: a zero-length row packet no longer reaches
   `packet[0]`, a handshake length that overflows in `mysql_common` is refused before the
   parse, and every other `unwrap`/`expect`/index/`unreachable!` on the paths VibeLens uses is
   listed in `PATCH.md` with why server bytes cannot reach it. A seeded fuzz test
   (`mysql_hostile::fuzz`) corrupts the conversation at every stage and fails on any panic or
   hang.

### 6.2 Design

- **Profile.** `EngineKind::Mysql` uses `host`, `port`, `database` (required: the default
  database), `auth` (`password` or `rdsIam`), `tls` and `tunnel`, like Postgres.
  `ConnectionProfile.mysql: MysqlOptions` holds `connectTimeoutSecs` (1–600), `statementTimeoutMs`,
  `idleDisconnectSecs` and `maxPoolSize` (1–32). Rules in `mysql/validate.rs`, run on save and on
  every connect and test (`cleartext::check_profile`).
- **Connect.** One `deadpool` pool per (profile, database), at most 8 databases per connection
  at once (a name differing only in case opens another pool, so the number is capped; counting,
  a slot is reserved under the state lock and the dial runs outside any lock, so a slow server
  holds up nobody; at the cap the least recently used pool that nobody holds is closed); `create()` resolves credentials per
  physical connection (a fresh RDS IAM token each time, signed for the real host and port).
  The SSH tunnel and its pinned host key are the Postgres ones; the driver dials the tunnel's
  local port and verifies TLS against the profile's host. Driver options: `prefer_socket(false)`
  (otherwise the driver switches to the Unix socket the *server* names, which through a tunnel
  would be a socket of a server on this machine), `max_allowed_packet` fixed at 64 MiB (never
  taken from the server), `secure_auth`, statement cache off.
- **Pool hygiene.** A run that ends cleanly returns its connection after `COM_RESET_CONNECTION`
  (rolls back, drops temporary tables, releases table locks and `GET_LOCK`s, resets session and
  user variables, `sql_mode`) and `USE` of the pool's database, which the reset does not restore.
  A server without `COM_RESET_CONNECTION` gets its connections closed instead of reused. Every
  other ending closes the connection.
- **Execution.** Scripts are split on the client (`mysql/check.rs`, a lexer that follows
  `sql_lex.cc`: `#` and `-- ` comments ending at `\n` only, `--` without a following space is not a
  comment, block comments do not nest, `/*! */`, `/*M! */` (MariaDB), `/*T! */` (TiDB) and
  `/*+ */` are recognised, backslash escapes,
  backticks, `BEGIN … END` bodies of `CREATE PROCEDURE/FUNCTION/TRIGGER/EVENT` kept whole).
  `DELIMITER` is refused with advice. Each statement is one `COM_QUERY` (text protocol); rows
  stream into `QueryEvent::Rows` pages. A script that leaves a transaction open is rolled back
  with a message.
- **Catalog.** `information_schema` only; names are sent as `CONVERT(X'…' USING utf8mb4)` hex
  literals, which no `sql_mode` reads differently, so a database or table name cannot change the
  query. Listings are bounded (10 000 databases, 20 000 objects, 200 000 detail rows, each value
  cut at 64 KiB, 32 MiB of text per query, 60 s); a table cap from IPC is clamped to 1–20 000.
- **Values.** BIGINT and DECIMAL → strings; other integers and finite floats → numbers; BIT →
  binary digits; JSON → parsed (text when it does not parse or nests deeper than 128); dates as
  sent (`0000-00-00` included); binary strings, BLOB, GEOMETRY, VECTOR and text that is not
  UTF-8 → `{"$bytes"}`; over 1 MiB → `$truncated`.
- **Commands.** `my_list_databases`, `my_list_objects`, `my_describe_table`,
  `my_schema_metadata`, `my_execute` (cancel with `pg_cancel`); `sql_classify` takes an optional
  `engine`. Capabilities are unchanged.

### 6.3 The security rules

- **R1 (vault only, bound to its endpoint).** The password is in the vault (`profile:{id}:password`)
  and, for MySQL, stored with its origin (`profile:{id}:password-origin`: host, port, user, TLS
  mode, CA path and SHA-256 of its contents, RDS bundle switch, SSH tunnel endpoint, and the
  engine) in the order unbind, write, bind. Once a binding exists, the credential source releases
  that vault entry only through `mysql::secret::load_password`, for every engine and both the
  password and access-token branches (`load_any_password`): a profile whose engine was edited to
  Postgres or Valkey, or a `test_connection` with the same id and another engine, gets an
  authentication error. A hand-edited `profiles.json` or a direct `test_connection` cannot send it
  elsewhere; saving a profile that points elsewhere without a new password deletes it, and saving
  it as another engine deletes password and binding together. Writes go: delete password, delete
  binding, write binding, write password, so a failure leaves at most a lone binding, which
  fails closed;
  deleting a profile removes the password before the binding and stops if it cannot. An
  unreadable CA file is an error on save, not a reason to drop the password. RDS IAM tokens are minted per connection and never stored. Errors
  carry no password (`map_error` never formats options).
- **R2 (no secret in cleartext to another machine).** Stricter than Postgres: a password to any
  host other than `localhost`, `127.0.0.0/8` or `::1`, or through any SSH tunnel, needs
  `verifyFull`; an RDS IAM token needs `verifyFull` always. Checked in Rust on save and before
  the tunnel or any credential is touched. Below that, the patched driver sends no password and
  no cleartext-plugin answer without TLS, whatever the server asks.
- **R3 (only the configured endpoint, no redirects).** The protocol has no redirects; the
  driver never follows a server-named Unix socket (`prefer_socket(false)`).
- **R4 (TLS verified).** `verifyFull` uses OS roots, the bundled RDS CA bundle when switched on,
  and the profile's CA file, with the driver's own Mozilla roots off, and verifies the profile's
  host name even through a tunnel. `require` (encrypt without verification) remains for this
  machine only when a credential is sent, with the UI warning, as for Postgres.
- **R5 (read-only before submission, fail closed).** Every statement of a read-only run (agent
  reads, and editor runs in read-only mode) must pass `mysql::check_read_only` before it is sent:
  first keyword `SELECT`, `WITH`, `TABLE`, `VALUES`, `SHOW`, `DESCRIBE`/`DESC` or `EXPLAIN`; lexing
  must give the same tokens with and without backslash escapes (so `NO_BACKSLASH_ESCAPES` and
  `ANSI_QUOTES` cannot change the meaning); no executable comment, hint, `:=`, `;`, backslash or
  non-ASCII outside quotes, control character, or `\r` inside a line comment; no `INTO`,
  `OUTFILE`, `DUMPFILE`, `FOR UPDATE/SHARE/CONNECTION`, `LOCK`, `SET` (except `CHARACTER SET`),
  `HANDLER`, `DO`, `CALL`, `LOAD`, `KILL`, `USE` (except index hints) or any other write,
  session or administration keyword as a bare word; `SHOW CREATE USER` refused (password
  hashes); a `SHOW … WHERE` may hold no subquery (`SELECT`, `WITH`) and no locking clause; and
  every function call must be a MySQL built-in on the read-only allow-list. Schema-qualified
  calls and calls through a quoted name (backticks, or double quotes, which `ANSI_QUOTES` makes
  identifiers: `"SLEEP"(1)` is a call there) are stored or loadable functions and are refused.
  Then the run is inside `START TRANSACTION READ ONLY`; after every statement the server's status
  flags must still say "in a read-only transaction", otherwise the run stops and the session is
  closed.
- **R6 (bounded).** `max_rows` with `truncated` (a cut read-only result kills the statement and
  closes the connection; the rest of the script is not run), `MAX_CELL_BYTES`,
  `PAGE_BYTE_BUDGET`, one packet (row) at most 64 MiB, at most 4096 columns, server error text at
  4 KiB, catalog listings capped by rows, value size and total bytes.
- **R7 (agent sees what `AiDataAccess` allows; card shows the target).** The Postgres matrix
  with MySQL's classifier. The database is resolved once with `effective_database` and pinned
  for execution; the card's detail line is `MySQL · host:port · as user [· via SSH host:port]`
  from the profile. Refused at every access level, even with approval
  (`mysql::agent_refusal`): `INTO OUTFILE/DUMPFILE`, `LOAD DATA/XML`, `LOAD_FILE()`,
  `SET GLOBAL/PERSIST`, `KILL`, `SHUTDOWN`, `RESTART`, `CLONE`, `INSTALL/UNINSTALL`, anything
  with `SONAME`, `USE`, `DELIMITER`, `PREPARE` anywhere in a statement and
  `EXECUTE`/`DEALLOCATE` (dynamic SQL), inside a `CREATE PROCEDURE/FUNCTION/TRIGGER/EVENT` body
  also `SET GLOBAL`/`PERSIST`, `KILL`, `SHUTDOWN`, `RESTART`, `CLONE` and `INSTALL`/`UNINSTALL`
  (checked as if the body ran now), executable
  comments (the card would not read as what runs), `LOAD_FILE` however its name is quoted, and
  any statement that does not lex, or lexes differently with and without backslash escapes.
  MariaDB's `SET STATEMENT … FOR`, `EXECUTE IMMEDIATE` (refused wherever `IMMEDIATE` appears)
  and any `EXECUTE` inside a routine body are refused too.
  Statements that change the server or other sessions are never "session-only": `SET PASSWORD`,
  `SET DEFAULT ROLE`, `SET ROLE`, `SET RESOURCE GROUP` and every `START` other than
  `START TRANSACTION` are DDL; `LOCK` and `XA` are writes; `@@global`/`@@persist` count with or
  without spacing before the dot (`SET @@global . x = 1` is accepted by MySQL 8.4); a statement
  with an executable comment and MariaDB's `SET STATEMENT … FOR` are Unknown, as is a `SET`
  with a function call or subquery (`SET @a = f()`) and any statement that lexes differently
  with and without backslash escapes; so the production guard and the card treat them as
  changes. Database errors under a withholding policy collapse to `SQLSTATE` + a fixed sentence
  (the message carries `SQLSTATE xxxxx`).
- **R8 (cancel and timeout stop the server).** Cancel, a timeout, a dropped run (agent tool
  timeout, aborted task) and a closed result channel send `KILL QUERY <id>` from a separate,
  freshly authenticated connection, then close the connection. The kill is sent only when that
  connection reports the same server identity as the session it targets (recorded when the
  session opened; up to three tries), so behind a reader endpoint or proxy it cannot reach
  another instance's session with the same id. The identity is `@@server_uuid`, else (MariaDB,
  some proxies) `@@hostname`, `@@port` and `@@server_id`; both are read as raw text, never with
  a typed row conversion (those panic on a reply of the wrong shape). When neither is available
  no kill is sent: the connection is only closed, and the statement may run on until the server
  notices the closed socket or `max_execution_time` stops it (SELECT only). An identity read that
  times out or breaks makes the connection unusable: it is closed, never pooled, because the
  late reply would be read as the answer to the next request. The per-statement timeout is
  enforced on the client for every statement and by `max_execution_time` on the server for
  `SELECT`s. Agent SQL keeps the 30 s statement and 150 s script budgets.
- **R9 (validation in Rust; enums refused, not defaulted).** All rules above are in Rust;
  `engine: "mysql"` with another auth kind, a missing database or out-of-range options is
  refused on save and on use. The UI repeats them for convenience.

### 6.4 MySQL threats and the tests that use the hostile input

| Threat | Closed by | Test |
|---|---|---|
| Server reads a local file (`LOCAL INFILE`) | Capability not advertised; request never answered, connection closed (driver patch 1, 2) | `mysql_hostile::a_local_infile_request_gets_no_file` (fake server requests a secret file), `::the_client_takes_up_neither_local_files_nor_multi_statements`, `mysql_engine::load_data_local_is_refused_even_when_the_server_allows_it` (server `local_infile=ON`) |
| Auth downgrade: RSA key retrieval, cleartext, native hash over an unverified channel | Patch 3, 4; `verifyFull` required for remote passwords and IAM | `mysql_hostile::no_password_and_no_rsa_key_request_without_tls`, `::no_cleartext_password_without_tls_even_when_enabled`, `mysql_engine::full_authentication_never_happens_without_tls`, `mysql::validate` tests, `mysql_engine::manager::an_unsafe_profile_is_refused_before_connecting` |
| Read-only bypasses (`INTO OUTFILE`, `LOAD_FILE`, `SET GLOBAL`, `KILL`, `HANDLER`, `DO`, `CALL`, `:=`, `GET_LOCK`, `SLEEP`/`BENCHMARK`, UDFs, `/*! */`, `/*+ */`, backslash tricks, `#`/`--` lexing, `DELIMITER`) | `check_read_only` + `START TRANSACTION READ ONLY` + status check | `mysql::check::tests::hostile_reads_are_refused` (60+ inputs), `mysql_engine::read_only_runs_refuse_before_sending_and_inside_the_transaction`, `mysql::exec::tests::the_transaction_and_the_after_statement_check_hold_without_the_text_check`, agent `policy::tests::mysql_reads_with_side_effects_are_not_safe` |
| One approved statement smuggles a second | Client split; `CLIENT_MULTI_STATEMENTS` off (patch 1) | `mysql::exec::tests::the_server_runs_no_second_statement_in_one_query` (`SELECT 1; DROP TABLE …` in one COM_QUERY: 1064, table still there) |
| A stopped agent turn leaves a query running | `KILL QUERY` on cancel, timeout, drop | `mysql_engine::cancel_stops_the_statement_on_the_server`, `::a_timeout_stops_any_statement_on_the_server` (SELECT and DO), `::a_dropped_run_kills_its_statement`, `::manager::the_agent_backend_runs_pinned_reads_and_stops_on_cancel` (all check `PROCESSLIST`) |
| Unbounded results | Row cap while streaming, cell and page caps, 64 MiB packets | `mysql_engine::rows_are_capped_while_streaming`, `::odd_values_map_without_loss_or_panic` (1.1 MB cell) |
| Hostile or odd values | `mysql/cells.rs` (no panics, no lossy text) | `mysql::cells` tests (zero dates, invalid UTF-8, BIT, huge DECIMAL, deep JSON, binary), `mysql_hostile::hostile_rows_are_mapped_or_refused_without_panicking` (invalid UTF-8, 9-byte BIT, row lying about its length), `::an_unknown_column_type_is_an_error_not_a_panic`, `mysql_engine::odd_values_map_without_loss_or_panic` |

| A server aborts the client (ed25519 / parsec switch, repeated switch, huge column count) | Patch 5 | `mysql_hostile::ed25519_and_parsec_switches_are_refused_not_panics`, `::a_second_auth_switch_is_an_error`, `::a_huge_column_count_is_refused_before_allocating` |
| A bound password read through another engine | Origin includes the engine; `load_any_password`; `unbind_if_not_mysql` | `mysql::secret::tests::switching_the_engine_does_not_release_a_bound_password`, `aws_service::app_credential_source_binds_a_mysql_password_to_its_endpoint` |
| `ANSI_QUOTES` calls (`"SLEEP"(1)`) | Double-quoted name before `(` is a call; sessions are reset or closed | `check::tests::hostile_reads_are_refused`, live `mysql_engine::ansi_quotes_calls_are_refused_in_read_only_runs` |
| A kill reaching another instance; identity replies of the wrong shape | Identity match (`@@server_uuid`, else host/port/server_id), raw-row reads | `mysql_hostile::kill_query_is_sent_only_to_the_same_server` (same, moving, two-column, non-UTF-8 and missing replies) |
| Routine bodies hiding dynamic SQL or administration | `agent_refusal` scans the whole statement | `check::tests::agent_refusals` |
| Oversized column definitions | Patch 5 (16 MiB total) | `mysql_hostile::fat_column_definitions_are_refused` |
| Server bytes reaching any panic (the class) | Patches 5 and 6 | `mysql_hostile::fuzz::a_corrupting_server_never_panics_or_hangs_the_engine` (3000 iterations by default; `VG_FUZZ_SEED`, `VG_FUZZ_ITERS`) |
| A reply arriving after a timeout (connection one reply behind) | A timed-out or broken identity read closes the connection; every timeout, cancel, truncation or dropped run closes it too | `mysql_hostile::a_timed_out_identity_query_closes_the_connection` |
| Eviction of a pool a statement is about to use; one slow database blocking others | Only unheld, idle pools are evicted; a slot is reserved and the dial runs outside any lock | `mysql_hostile::database_pools_per_connection_are_capped`, `::one_slow_database_does_not_block_another` |
| Unbounded catalog text, oversized table cap, many pools | Value and byte caps, clamp, 8 pools | `mysql_hostile::catalog_listings_are_bounded_per_value_and_in_total`, `::an_oversized_table_cap_does_not_overflow`, `::database_pools_per_connection_are_capped` |

Removing patch 1, 3 or 4 makes the corresponding `mysql_hostile` tests fail (checked by reverting
them locally).

### 6.5 Residual risks

- **Functions inside views and the read-only transaction.** The text check sees the statement,
  not what it reaches. A read-only `SELECT` from an existing view (or a generated column) whose
  definition calls a stored function, or a loadable function (UDF) installed on the server,
  still runs that function. `START TRANSACTION READ ONLY` stops its table writes, but not what
  a UDF does outside the database (`sys_exec`-style), nor `GET_LOCK`, `SLEEP` or user-variable
  assignments inside a stored function. The role used with the agent should have no `EXECUTE`
  and the server no untrusted UDFs (the seed's `vibelens_ro` user shows the shape). This is the
  MySQL form of the Redshift Lambda UDF caveat.
- **What a read may read.** `SELECT authentication_string FROM mysql.user`,
  `performance_schema` statement history, `SHOW PROCESSLIST` and binary-log reads are reads;
  only privileges stop them. A least-privilege role is the boundary, as for Postgres.
- **Passwords to this machine without TLS.** Allowed (for local development); full
  authentication then fails by design until a TLS login has filled the server's cache.
- **The bastion-to-database hop.** With a tunnel `verifyFull` is still required (every MySQL
  login carries a credential), so the hop from the bastion to the database is encrypted and
  verified too. This is stricter than Postgres, where a password may cross it in the clear.
- **`require` (no verification) on this machine** is accepted, as on Postgres.
- **Lexer drift.** The checker follows MySQL 8.0/8.4's lexer; a future server version that
  lexes differently could disagree. Multi-statements being off limits the damage to the
  statement itself, and the read-only transaction and status check still apply.
- **The vendored driver** must be refreshed by hand for upstream fixes (its `PATCH.md` says how).
- **Not tested against a live server:** Aurora MySQL, RDS IAM end to end (token signing is
  tested offline), MariaDB, the SSH tunnel path for MySQL (shared code, tested with Postgres and
  Valkey). See `docs/TEST_PLAN.md` §11c.

### 6.6 Testing

- Unit: lexer, classifier and refusals, splitter, value mapping, validation, password binding,
  driver options, policy and approval text.
- Hostile server: `tests/mysql_hostile.rs`, an in-process fake speaking the protocol; runs
  everywhere.
- Live: `tests/mysql_engine.rs` and the database-backed tests in `mysql::exec`, gated by
  `VG_TEST_MYSQL="host=… port=… user=… password=… dbname=…"` (service `mysql` in
  `VG_TEST_REQUIRE`); the verified-TLS test also needs `VG_TEST_MYSQL_CA` (service `mysql-tls`).
  `docker-compose.dev.yml` has a `mysql` service (8.4, `127.0.0.1:53306`) seeded from
  `scripts/seed/mysql-shop.sql`.
- UI: Vitest for the form rules, tree model, dialect, splitter and dispatch.
