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
