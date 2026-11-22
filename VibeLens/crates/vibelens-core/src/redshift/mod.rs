//! Amazon Redshift through the Redshift Data API (provisioned clusters and Serverless
//! workgroups), authenticated with IAM. Design: `docs/REMOTE_ENGINES_AND_REDSHIFT.md` §3.
//!
//! `ExecuteStatement` (with a `ClientToken`) → poll `DescribeStatement` (100 ms doubling to 1 s)
//! → page `GetStatementResult` into `QueryEvent::Rows`. Cancel and timeout call
//! `CancelStatement`. The browser uses `ListDatabases`, `ListSchemas`, `ListTables` and
//! `DescribeTable`.
//!
//! # Behaviour worth knowing
//!
//! * **Identity.** Every call is SigV4-signed with the profile's AWS identity. No database
//!   password exists on the machine: `iamIdentity` lets Redshift map the IAM identity to a
//!   database user, `dbUser` asks for temporary credentials for one user (provisioned only),
//!   `secret` names a Secrets Manager secret that the Data API reads on the server side.
//! * **Event contract** (same as `PgHandle::execute_stream` and Athena): `Started` first, then
//!   per statement `Columns`, `Rows`*, `Stats`, `StatementDone`, and exactly one of `Finished` /
//!   `Failed` last. `Stats` carries `engine: "redshift"` and the Redshift query id.
//! * **No transactions.** The Data API commits each statement on its own, so `BEGIN`, `COMMIT`,
//!   `ROLLBACK`, `START TRANSACTION`, `END` and `ABORT` are refused in every mode, before
//!   anything is submitted.
//! * **`read_only`.** There is no read-only session to open, so the check *is* the enforcement:
//!   every statement must be provably a read before any is submitted (see [`check`]).
//! * **Bounds.** Rows stop at `max_rows` (and `truncated` says so), cells larger than
//!   `MAX_CELL_BYTES` become markers, row pages are cut at `PAGE_BYTE_BUDGET`, error text at
//!   4 KiB, and every listing at a fixed number of pages.
//! * **Retries.** Throttling and transient transport failures are retried up to 3 times with
//!   jittered exponential backoff. `ExecuteStatement` is safe to retry because it carries a
//!   `ClientToken`.

mod cells;
mod check;
mod engine;
mod lex;
mod validate;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use aws_credential_types::provider::SharedCredentialsProvider;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub use validate::validate_options;

use crate::error::{CoreError, Result};
use crate::model::*;

/// Sets the cluster or workgroup, the database and the authentication parameters on any Data API
/// request builder that takes them (they all use the same setter names).
macro_rules! with_target {
    ($req:expr, $opts:expr, $database:expr) => {{
        let req = $req.database($database);
        let req = match &$opts.target {
            $crate::model::RedshiftTarget::Provisioned { cluster_identifier } => {
                req.cluster_identifier(cluster_identifier.trim())
            }
            $crate::model::RedshiftTarget::Serverless { workgroup_name } => {
                req.workgroup_name(workgroup_name.trim())
            }
        };
        // `iamIdentity` sends neither a database user nor a secret: the Data API then uses
        // GetClusterCredentialsWithIAM / redshift-serverless:GetCredentials for the caller.
        match &$opts.auth {
            $crate::model::RedshiftAuth::IamIdentity => req,
            $crate::model::RedshiftAuth::DbUser { db_user } => req.db_user(db_user.trim()),
            $crate::model::RedshiftAuth::Secret { secret_arn } => req.secret_arn(secret_arn.trim()),
        }
    }};
}
pub(crate) use with_target;

/// Most items any listing returns (databases, schemas, tables, columns).
const MAX_LIST_ITEMS: usize = 10_000;
/// Most pages any listing reads, whatever the server keeps answering.
const MAX_LIST_PAGES: usize = 200;
/// Default table cap for [`RedshiftEngine::schema_metadata`].
pub const DEFAULT_MAX_TABLES: usize = 2000;

/// Pacing knobs. The defaults are the production values; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct RedshiftTiming {
    /// First `DescribeStatement` poll delay; doubles each poll up to [`poll_max`](Self::poll_max).
    pub poll_initial: Duration,
    pub poll_max: Duration,
    /// Base of the throttling retry backoff (`base * 2^attempt`, plus up to 50% jitter).
    pub retry_base: Duration,
    /// Ceiling for a statement run without `ExecOptions.timeout_ms`, so no run polls forever
    /// (R8). On expiry the statement is cancelled on the server.
    pub default_timeout: Duration,
    /// Deadline for a whole [`RedshiftEngine::schema_metadata`] call, which fans out into many
    /// listing requests.
    pub metadata_timeout: Duration,
}

impl Default for RedshiftTiming {
    fn default() -> Self {
        Self {
            poll_initial: Duration::from_millis(100),
            poll_max: Duration::from_secs(1),
            retry_base: Duration::from_millis(250),
            default_timeout: Duration::from_secs(60 * 60),
            metadata_timeout: Duration::from_secs(60),
        }
    }
}

/// `Redshift · {cluster or workgroup} · {database} · {region} · {who}`: the status line and the
/// agent's approval card. Built from the profile, never from anything a server or the model
/// said. `{who}` is `as the IAM identity`, `as {db user}` or `via secret {name}`.
pub fn describe_target(o: &RedshiftOptions, database: &str) -> String {
    let place = match &o.target {
        RedshiftTarget::Provisioned { cluster_identifier } => cluster_identifier.trim(),
        RedshiftTarget::Serverless { workgroup_name } => workgroup_name.trim(),
    };
    format!(
        "Redshift · {place} · {} · {} · {}",
        database.trim(),
        o.aws.region.trim(),
        auth_text(&o.auth)
    )
}

/// [`describe_target`] for the database a call resolves to ([`effective_database`]): the text
/// for the approval card's `detail`.
pub fn approval_detail(o: &RedshiftOptions, requested_database: Option<&str>) -> String {
    describe_target(o, &effective_database(o, requested_database))
}

/// Who statements run as, for people: never the account id or the secret's random suffix.
fn auth_text(a: &RedshiftAuth) -> String {
    match a {
        RedshiftAuth::IamIdentity => "as the IAM identity".to_string(),
        RedshiftAuth::DbUser { db_user } => format!("as {}", db_user.trim()),
        RedshiftAuth::Secret { secret_arn } => {
            format!("via secret {}", secret_display_name(secret_arn))
        }
    }
}

/// `arn:…:secret:prod/redshift-AbC123` → `prod/redshift`: the name part, without the six
/// characters Secrets Manager appends.
fn secret_display_name(arn: &str) -> String {
    let name = arn
        .trim()
        .split_once(":secret:")
        .map(|(_, n)| n)
        .unwrap_or("");
    match name.rsplit_once('-') {
        Some((base, suffix))
            if !base.is_empty()
                && suffix.len() == 6
                && suffix.bytes().all(|b| b.is_ascii_alphanumeric()) =>
        {
            base.to_string()
        }
        _ => name.to_string(),
    }
}

/// `Err(PolicyDenied)` with the reason when `sql` is refused on Redshift in every mode: a string
/// literal with a backslash (Redshift and the classifier would read it differently), a split
/// statement that still holds a `;`, or transaction control. For the agent policy and the
/// production guard, so the same rules apply before anyone is asked.
pub fn check_script(sql: &str) -> Result<()> {
    check::check_script(&crate::sql::split_statements(sql))
}

/// The first function call in `sql` that a Redshift read-only run refuses (not a built-in known
/// to only read, or qualified with a schema other than `pg_catalog`), as `name()`.
pub fn unknown_function(sql: &str) -> Option<String> {
    check::unknown_function(&crate::sql::split_statements(sql))
}

/// The production write guard for Redshift (`rs_execute`). `Err` when [`check_script`] refuses
/// the script outright; otherwise `Some(reason)` when the run needs the user's confirmation: a
/// write (class Write or above, judged only after the script was found unambiguous) or a call to
/// a function that is not a read-only built-in, in a run that may write; in a read-only run, a
/// statement that is not provably a read (the engine refuses the rest of what it would not run).
pub fn prod_guard_reason(sql: &str, read_only: bool) -> Result<Option<String>> {
    check_script(sql)?;
    let c = crate::sql::classify(sql);
    let unknown = unknown_function(sql);
    let needs = if read_only {
        c.overall == crate::sql::StatementClass::Unknown
    } else {
        c.overall >= crate::sql::StatementClass::Write || unknown.is_some()
    };
    Ok(needs.then(|| match unknown {
        Some(f) => format!("{} (calls {f})", c.summary),
        None => c.summary,
    }))
}

/// The database a call runs in: the requested one, else the profile's.
pub fn effective_database(o: &RedshiftOptions, requested: Option<&str>) -> String {
    requested
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(o.database.trim())
        .to_string()
}

pub struct RedshiftEngine {
    opts: RedshiftOptions,
    /// Kept only to rebuild the client for an endpoint override (test builds).
    #[cfg(any(test, feature = "test-util"))]
    creds: SharedCredentialsProvider,
    timing: RedshiftTiming,
    client: aws_sdk_redshiftdata::Client,
}

impl RedshiftEngine {
    /// Validates `opts` (identifiers, ARN, region, `dbUser` only for provisioned) and builds a
    /// client for the regional Data API endpoint. Makes no request.
    pub fn new(opts: RedshiftOptions, creds: SharedCredentialsProvider) -> Result<Self> {
        validate_options(&opts)?;
        let client = engine::build_client(&opts.aws.region, &creds, None);
        Ok(Self {
            opts,
            #[cfg(any(test, feature = "test-util"))]
            creds,
            timing: RedshiftTiming::default(),
            client,
        })
    }

    /// Sends every request to `url` instead of the regional endpoint. A hook for tests and for
    /// the connection manager's test wiring: it is deliberately not a profile setting (R3, R4),
    /// so nothing a user or a hand-edited file says can point signed requests elsewhere.
    #[cfg(any(test, feature = "test-util"))]
    #[doc(hidden)]
    #[must_use]
    pub fn with_endpoint_url(mut self, url: impl Into<String>) -> Self {
        let url = url.into();
        self.client = engine::build_client(&self.opts.aws.region, &self.creds, Some(&url));
        self
    }

    /// Overrides polling and retry pacing.
    #[must_use]
    pub fn with_timing(mut self, timing: RedshiftTiming) -> Self {
        self.timing = timing;
        self
    }

    pub fn options(&self) -> &RedshiftOptions {
        &self.opts
    }

    /// `Redshift · {cluster or workgroup} · {database} · {region}` for the profile's database.
    pub fn describe_target(&self) -> String {
        describe_target(&self.opts, &self.opts.database)
    }

    fn session(&self) -> engine::Session<'_> {
        engine::Session {
            client: &self.client,
            opts: &self.opts,
            timing: &self.timing,
        }
    }

    fn database(&self, requested: Option<&str>) -> Result<String> {
        let db = effective_database(&self.opts, requested);
        if requested.is_some_and(|r| !r.trim().is_empty()) {
            validate::requested_database_ok(&db)?;
        } else {
            validate::database_name_ok(&db)?;
        }
        Ok(db)
    }

    /// Same event contract as `PgHandle::execute_stream`. Runs in `database` (default: the
    /// profile's). Transaction control is refused in every mode and, with `opts.read_only`,
    /// anything that is not provably a read, before anything is submitted.
    ///
    /// Emits `Started` first and exactly one of `Finished` / `Failed` last (unless the receiver
    /// disappears). Returns `Err` only if `Started` could not be delivered.
    pub async fn execute_stream(
        &self,
        query_id: String,
        sql: &str,
        database: Option<&str>,
        opts: ExecOptions,
        cancel: CancellationToken,
        tx: mpsc::Sender<QueryEvent>,
    ) -> Result<()> {
        let statements = crate::sql::split_statements(sql);
        tx.send(QueryEvent::Started {
            query_id,
            statement_count: statements.len(),
        })
        .await
        .map_err(|_| CoreError::Other("query result receiver was dropped".into()))?;
        let t0 = Instant::now();
        let end = match self.database(database) {
            Ok(db) => {
                self.session()
                    .run(&statements, &db, &opts, &cancel, &tx)
                    .await
            }
            Err(error) => engine::End::Failed {
                statement_index: None,
                error,
            },
        };
        if let Some(terminal) = end.into_event(engine::elapsed_ms(t0)) {
            // If the receiver vanished since the last event there is nobody to tell.
            let _ = tx.send(terminal).await;
        }
        Ok(())
    }

    /// Bounded, materialised execution: the result of the last statement that returns rows
    /// (or of the last statement when none does). Errors are returned as `Err`.
    ///
    /// `cancel` stops the run, after `CancelStatement` has been sent for a live statement. The
    /// run itself is a task of its own, and dropping this future cancels it: the task then
    /// still sends `CancelStatement` before it ends, so an agent Stop or a timeout higher up
    /// never leaves a statement running in the warehouse.
    pub async fn execute_collect(
        &self,
        sql: &str,
        database: Option<&str>,
        opts: ExecOptions,
        cancel: CancellationToken,
    ) -> Result<QueryResult> {
        /// Cancels the run when the caller's future is dropped.
        struct CancelOnDrop(CancellationToken);
        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                self.0.cancel();
            }
        }

        let statements = crate::sql::split_statements(sql);
        if statements.is_empty() {
            return Ok(QueryResult::default());
        }
        let db = self.database(database)?;
        let run_cancel = cancel.child_token();
        let _cancel_on_drop = CancelOnDrop(run_cancel.clone());
        let client = self.client.clone();
        let ropts = self.opts.clone();
        let timing = self.timing;
        let task = tokio::spawn(async move {
            let session = engine::Session {
                client: &client,
                opts: &ropts,
                timing: &timing,
            };
            let (tx, mut rx) = mpsc::channel::<QueryEvent>(32);
            let runner = async {
                let end = session.run(&statements, &db, &opts, &run_cancel, &tx).await;
                drop(tx);
                end
            };
            let collector = async {
                let mut collected = engine::Collector::default();
                while let Some(ev) = rx.recv().await {
                    collected.push(ev);
                }
                collected
            };
            let (end, collected) = tokio::join!(runner, collector);
            match end {
                engine::End::Finished => collected
                    .into_result()
                    .ok_or_else(|| CoreError::Other("no statement produced a result".into())),
                engine::End::Failed { error, .. } => Err(error),
                engine::End::Gone => Err(CoreError::Other("result collector went away".into())),
            }
        });
        task.await
            .unwrap_or_else(|e| Err(CoreError::Other(format!("the Redshift run failed: {e}"))))
    }

    /// One `ListDatabases` call of one item: it goes through the whole path (IAM signature,
    /// credentials for the database, the cluster or workgroup answering), so a connection that
    /// passes it works. Used by connect and test.
    pub async fn ping(&self) -> Result<()> {
        let db = self.database(None)?;
        let cancel = CancellationToken::new();
        self.session()
            .call(&cancel, "ListDatabases", true, || {
                let req = self.client.list_databases().max_results(1);
                with_target!(req, self.opts, &db).send()
            })
            .await
            .map_err(engine::Stop::into_core)?;
        Ok(())
    }

    // ---- catalog browsing --------------------------------------------------------------------

    /// Databases of the cluster or workgroup (`ListDatabases`, paginated and bounded).
    pub async fn list_databases(&self) -> Result<Vec<String>> {
        let db = self.database(None)?;
        let session = self.session();
        let cancel = CancellationToken::new();
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let page = session
                .call(&cancel, "ListDatabases", true, || {
                    let req = self.client.list_databases().set_next_token(token.clone());
                    with_target!(req, self.opts, &db).send()
                })
                .await
                .map_err(engine::Stop::into_core)?;
            out.extend(page.databases().iter().cloned());
            token = page
                .next_token()
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            if token.is_none() || out.len() >= MAX_LIST_ITEMS {
                break;
            }
        }
        out.truncate(MAX_LIST_ITEMS);
        Ok(out)
    }

    /// Schemas of `database` (default: the profile's), `ListSchemas`, paginated and bounded.
    pub async fn list_schemas(&self, database: Option<&str>) -> Result<Vec<String>> {
        let db = self.database(database)?;
        let session = self.session();
        let cancel = CancellationToken::new();
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let page = session
                .call(&cancel, "ListSchemas", true, || {
                    let req = self.client.list_schemas().set_next_token(token.clone());
                    with_target!(req, self.opts, &db).send()
                })
                .await
                .map_err(engine::Stop::into_core)?;
            out.extend(page.schemas().iter().cloned());
            token = page
                .next_token()
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            if token.is_none() || out.len() >= MAX_LIST_ITEMS {
                break;
            }
        }
        out.truncate(MAX_LIST_ITEMS);
        Ok(out)
    }

    /// Tables and views of one schema (`ListTables`), paginated and bounded.
    pub async fn list_tables(
        &self,
        database: Option<&str>,
        schema: &str,
    ) -> Result<Vec<RedshiftTableRef>> {
        Ok(self
            .list_tables_bounded(database, schema, MAX_LIST_ITEMS)
            .await?
            .0)
    }

    /// Up to `limit` tables of `schema`, and whether there were more.
    async fn list_tables_bounded(
        &self,
        database: Option<&str>,
        schema: &str,
        limit: usize,
    ) -> Result<(Vec<RedshiftTableRef>, bool)> {
        let db = self.database(database)?;
        validate::object_name_ok("schema", schema)?;
        let session = self.session();
        let cancel = CancellationToken::new();
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        let mut more = false;
        for page_no in 0..MAX_LIST_PAGES {
            let page = session
                .call(&cancel, "ListTables", true, || {
                    // `SchemaPattern` is a LIKE pattern; `_` and `%` in a name match more than the
                    // name, so the answer is filtered to the exact schema below.
                    let req = self
                        .client
                        .list_tables()
                        .schema_pattern(schema)
                        .set_next_token(token.clone());
                    with_target!(req, self.opts, &db).send()
                })
                .await
                .map_err(engine::Stop::into_core)?;
            for t in page.tables() {
                let (Some(name), Some(s)) = (t.name(), t.schema()) else {
                    continue;
                };
                if s != schema {
                    continue;
                }
                if out.len() >= limit {
                    more = true;
                    break;
                }
                out.push(RedshiftTableRef {
                    schema: s.to_string(),
                    name: name.to_string(),
                    kind: t.r#type().unwrap_or("TABLE").to_string(),
                });
            }
            token = page
                .next_token()
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            if more || token.is_none() {
                break;
            }
            if page_no + 1 == MAX_LIST_PAGES {
                more = true;
            }
        }
        Ok((out, more || token.is_some()))
    }

    /// Columns of one table or view (`DescribeTable`), paginated and bounded.
    pub async fn describe_table(
        &self,
        database: Option<&str>,
        schema: &str,
        name: &str,
    ) -> Result<Vec<RedshiftColumn>> {
        validate::object_name_ok("table", name)?;
        let (mut by_table, _) = self.columns(database, schema, Some(name)).await?;
        let cols = by_table.remove(name).unwrap_or_default();
        if cols.is_empty() {
            return Err(CoreError::NotFound(format!("table {schema}.{name}")));
        }
        Ok(cols)
    }

    /// Columns of `table` in `schema`, or of every table in it when `table` is `None`, grouped
    /// by table name (`DescribeTable` returns them in column order). The flag is true when the
    /// page cap stopped the listing.
    async fn columns(
        &self,
        database: Option<&str>,
        schema: &str,
        table: Option<&str>,
    ) -> Result<(HashMap<String, Vec<RedshiftColumn>>, bool)> {
        let db = self.database(database)?;
        validate::object_name_ok("schema", schema)?;
        let session = self.session();
        let cancel = CancellationToken::new();
        let mut out: HashMap<String, Vec<RedshiftColumn>> = HashMap::new();
        let mut count = 0usize;
        let mut token: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let page = session
                .call(&cancel, "DescribeTable", true, || {
                    let req = self
                        .client
                        .describe_table()
                        .schema(schema)
                        .set_table(table.map(str::to_string))
                        .set_next_token(token.clone());
                    with_target!(req, self.opts, &db).send()
                })
                .await
                .map_err(engine::Stop::into_core)?;
            for c in page.column_list() {
                if c.schema_name().is_some_and(|s| s != schema) {
                    continue;
                }
                let owner = c
                    .table_name()
                    .or(table)
                    .or(page.table_name())
                    .unwrap_or_default();
                if table.is_some_and(|t| t != owner) {
                    continue;
                }
                count += 1;
                out.entry(owner.to_string()).or_default().push(column(c));
            }
            token = page
                .next_token()
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            if token.is_none() || count >= MAX_LIST_ITEMS {
                break;
            }
        }
        Ok((out, token.is_some()))
    }

    /// Compact catalog for the editor and the agent: tables and views with their columns, of
    /// one schema or of every non-system schema, at most `max_tables`. `truncated` says when
    /// the cap (or a page cap) cut it. Redshift has no indexes or enforced keys to report.
    ///
    /// `max_tables` is clamped to [`DEFAULT_MAX_TABLES`], and the whole call has a deadline
    /// (`RedshiftTiming::metadata_timeout`, 60 s): it fans out into a request per schema and
    /// page, and the UI can ask for it directly.
    pub async fn schema_metadata(
        &self,
        database: Option<&str>,
        schema_filter: Option<&str>,
        max_tables: usize,
    ) -> Result<SchemaMetadata> {
        let limit = self.timing.metadata_timeout;
        let max_tables = max_tables.min(DEFAULT_MAX_TABLES);
        match tokio::time::timeout(
            limit,
            self.schema_metadata_unbounded(database, schema_filter, max_tables),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => Err(CoreError::Timeout(format!(
                "reading the Redshift catalog took longer than {} s; name a schema to read less",
                limit.as_secs()
            ))),
        }
    }

    async fn schema_metadata_unbounded(
        &self,
        database: Option<&str>,
        schema_filter: Option<&str>,
        max_tables: usize,
    ) -> Result<SchemaMetadata> {
        let db = self.database(database)?;
        let schemas: Vec<String> = match schema_filter.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => vec![s.to_string()],
            None => self
                .list_schemas(Some(&db))
                .await?
                .into_iter()
                .filter(|s| !is_system_schema(s))
                .collect(),
        };
        let mut tables = Vec::new();
        let mut truncated = false;
        for schema in schemas {
            let room = max_tables.saturating_sub(tables.len());
            if room == 0 {
                truncated = true;
                break;
            }
            let (refs, more) = self.list_tables_bounded(Some(&db), &schema, room).await?;
            truncated |= more;
            if refs.is_empty() {
                continue;
            }
            let (mut cols, cut) = self.columns(Some(&db), &schema, None).await?;
            truncated |= cut;
            for r in refs {
                let columns = cols.remove(&r.name).unwrap_or_default();
                tables.push(TableDescription {
                    kind: object_kind(&r.kind),
                    columns: columns
                        .into_iter()
                        .enumerate()
                        .map(|(i, c)| ColumnInfo {
                            name: c.name,
                            data_type: c.type_name,
                            nullable: c.nullable,
                            default: c.default_value,
                            is_primary_key: false,
                            ordinal: i32::try_from(i + 1).unwrap_or(i32::MAX),
                            comment: None,
                        })
                        .collect(),
                    schema: r.schema,
                    name: r.name,
                    comment: None,
                    estimated_rows: None,
                    indexes: Vec::new(),
                    foreign_keys: Vec::new(),
                });
            }
        }
        Ok(SchemaMetadata {
            database: db,
            server_version: None,
            tables,
            truncated,
        })
    }
}

/// Redshift's own schemas, left out of a whole-database catalog.
fn is_system_schema(s: &str) -> bool {
    s == "information_schema" || s.starts_with("pg_")
}

fn object_kind(kind: &str) -> ObjectKind {
    let k = kind.to_ascii_uppercase();
    if k.contains("MATERIALIZED") {
        ObjectKind::MaterializedView
    } else if k.contains("VIEW") {
        ObjectKind::View
    } else if k.contains("EXTERNAL") {
        ObjectKind::ForeignTable
    } else {
        ObjectKind::Table
    }
}

fn column(c: &aws_sdk_redshiftdata::types::ColumnMetadata) -> RedshiftColumn {
    let ty = c.type_name().unwrap_or("");
    let positive = |n: i32| (n > 0).then_some(n);
    RedshiftColumn {
        name: c.name().unwrap_or("").to_string(),
        type_name: cells::display_type(ty, c.precision(), c.scale()),
        // 0 = NOT NULL, 1 = nullable, 2 = unknown (treated as nullable).
        nullable: c.nullable() != 0,
        length: positive(c.length()),
        precision: positive(c.precision()),
        scale: positive(c.precision()).map(|_| c.scale().max(0)),
        default_value: c
            .column_default()
            .map(|d| engine::bounded(d, engine::MAX_ERROR_BYTES)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(target: RedshiftTarget) -> RedshiftOptions {
        RedshiftOptions {
            target,
            database: "dev".into(),
            aws: AwsTarget {
                identity: AwsIdentity::DefaultChain,
                region: "eu-west-1".into(),
            },
            auth: RedshiftAuth::IamIdentity,
        }
    }

    #[test]
    fn target_description() {
        let p = opts(RedshiftTarget::Provisioned {
            cluster_identifier: "analytics".into(),
        });
        assert_eq!(
            describe_target(&p, "dev"),
            "Redshift · analytics · dev · eu-west-1 · as the IAM identity"
        );
        let mut s = opts(RedshiftTarget::Serverless {
            workgroup_name: "wg".into(),
        });
        assert_eq!(
            describe_target(&s, "sales"),
            "Redshift · wg · sales · eu-west-1 · as the IAM identity"
        );
        s.auth = RedshiftAuth::Secret {
            secret_arn: "arn:aws:secretsmanager:eu-west-1:123456789012:secret:prod/rs-app-AbC123"
                .into(),
        };
        let d = approval_detail(&s, None);
        assert_eq!(
            d,
            "Redshift · wg · dev · eu-west-1 · via secret prod/rs-app"
        );
        assert!(!d.contains("123456789012") && !d.contains("AbC123"));
        let mut p = p;
        p.auth = RedshiftAuth::DbUser {
            db_user: "reader".into(),
        };
        assert_eq!(
            approval_detail(&p, Some(" sales ")),
            "Redshift · analytics · sales · eu-west-1 · as reader"
        );
        // A name without the random suffix is shown whole.
        assert_eq!(
            secret_display_name("arn:aws:secretsmanager:r:1:secret:plain"),
            "plain"
        );
    }

    /// HIGH 1 (c): the guard `rs_execute` uses refuses the backslash payloads outright, and asks
    /// for confirmation where the Postgres guard would see no write.
    #[test]
    fn production_guard_is_redshift_aware() {
        for sql in [
            r"SELECT '\' , ' AS x INTO public.pwned FROM secrets -- '",
            r"SELECT '\' , ' , pg_terminate_backend(123) -- '",
        ] {
            for ro in [false, true] {
                assert!(prod_guard_reason(sql, ro).is_err(), "{sql}");
            }
        }
        assert_eq!(prod_guard_reason("SELECT 1", false).unwrap(), None);
        assert!(prod_guard_reason("DELETE FROM t", false).unwrap().is_some());
        let r = prod_guard_reason("SELECT f_notify(x) FROM t", false).unwrap();
        assert!(r.is_some_and(|r| r.contains("f_notify()")));
        assert!(prod_guard_reason("BEGIN", false).is_err());
    }

    #[test]
    fn database_precedence() {
        let p = opts(RedshiftTarget::Serverless {
            workgroup_name: "wg1".into(),
        });
        assert_eq!(effective_database(&p, None), "dev");
        assert_eq!(effective_database(&p, Some("  ")), "dev");
        assert_eq!(effective_database(&p, Some(" sales ")), "sales");
    }

    #[test]
    fn object_kinds() {
        assert_eq!(object_kind("TABLE"), ObjectKind::Table);
        assert_eq!(object_kind("VIEW"), ObjectKind::View);
        assert_eq!(
            object_kind("MATERIALIZED VIEW"),
            ObjectKind::MaterializedView
        );
        assert_eq!(object_kind("EXTERNAL TABLE"), ObjectKind::ForeignTable);
        assert!(is_system_schema("pg_catalog") && is_system_schema("information_schema"));
        assert!(!is_system_schema("public"));
    }
}
