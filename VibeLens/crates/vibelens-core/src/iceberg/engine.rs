//! The local query engine: an embedded Apache DataFusion session over the Iceberg catalog.
//!
//! # Why not `IcebergCatalogProvider::try_new`
//!
//! `iceberg-datafusion` 0.10.1 builds its catalog provider eagerly: it lists every top-level
//! namespace and *loads every table* before the first query, only sees top-level namespaces, and
//! then serves stale metadata. On a real Glue or REST catalog that is minutes of start-up. Its
//! `IcebergTableProvider` (the one that can `INSERT`) has a `pub(crate)` constructor, so it cannot
//! be built from outside either. We therefore register our own lazy providers:
//!
//! * [`LazyCatalog`] / [`LazySchema`] resolve `ns.table` on demand (async `SchemaProvider::table`),
//!   loading fresh metadata per query. Nested namespaces are addressed as `"a.b".table`.
//! * `ns."table@<snapshot-id | branch | tag | 2026-10-01T00:00:00Z>"` is time travel;
//!   `ns."table$snapshots"` / `"table$manifests"` are the metadata tables.
//! * [`LiveTable`] reads through `IcebergStaticTableProvider` and, for `INSERT`, builds the real
//!   write path through `iceberg-datafusion` against a [`ScopedCatalog`] that exposes exactly one
//!   table (so only that table is loaded).

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::DateTime;
use datafusion::arrow::array::{Array, RecordBatch, UInt64Array};
use datafusion::arrow::datatypes::SchemaRef as ArrowSchemaRef;
use datafusion::catalog::{CatalogProvider, SchemaProvider, Session};
use datafusion::datasource::{MemTable, TableProvider, TableType};
use datafusion::error::{DataFusionError, Result as DFResult};
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::logical_expr::dml::InsertOp;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::prelude::{SQLOptions, SessionConfig, SessionContext};
use futures_util::{StreamExt, TryStreamExt};
use iceberg::arrow::{arrow_schema_to_schema_auto_assign_ids, schema_to_arrow_schema};
use iceberg::inspect::MetadataTableType;
use iceberg::table::Table;
use iceberg::{
    Catalog, ErrorKind, Namespace, NamespaceIdent, TableCommit, TableCreation, TableIdent,
};
use iceberg_datafusion::{IcebergCatalogProvider, IcebergStaticTableProvider, to_datafusion_error};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::catalog::{ManagedCatalog, to_core};
use super::codec;
use crate::error::{CoreError, ErrorPayload, Result};
use crate::model::{ExecOptions, PAGE_BYTE_BUDGET, QueryEvent, QueryResult, cell_weight};
use crate::sql::{StatementClass, classify};

/// Memory DataFusion may use for sorts/joins/aggregates before it errors or spills (it would otherwise be unbounded).
const MEMORY_LIMIT_BYTES: usize = 2 * 1024 * 1024 * 1024;

pub(crate) const NO_ROW_LEVEL_DML: &str = "row-level UPDATE/DELETE/MERGE isn't supported by the local Iceberg engine yet; switch this connection's query engine to Athena (Glue / S3 Tables) to run DML";

// ---------------------------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------------------------

/// `"a.b"` → `["a", "b"]`.
fn namespace_of(schema_name: &str) -> NamespaceIdent {
    let levels: Vec<String> = schema_name.split('.').map(str::to_string).collect();
    NamespaceIdent::from_vec(levels)
        .unwrap_or_else(|_| NamespaceIdent::new(schema_name.to_string()))
}

#[derive(Debug, PartialEq, Eq)]
enum Selector {
    Current,
    /// `table@<x>`: x is a snapshot id, a branch/tag name or an RFC 3339 timestamp.
    AsOf(String),
    /// `table$snapshots` / `table$manifests`.
    Meta(String),
}

/// Splits `orders`, `orders@123`, `orders$snapshots`.
fn parse_table_name(name: &str) -> (&str, Selector) {
    if let Some((base, meta)) = name.split_once('$') {
        return (base, Selector::Meta(meta.to_string()));
    }
    if let Some((base, at)) = name.rsplit_once('@')
        && !base.is_empty()
        && !at.is_empty()
    {
        return (base, Selector::AsOf(at.to_string()));
    }
    (name, Selector::Current)
}

/// Resolves a time-travel selector to a snapshot id of `table`.
fn resolve_snapshot(table: &Table, selector: &str) -> std::result::Result<i64, String> {
    let md = table.metadata();
    if let Ok(id) = selector.parse::<i64>() {
        return if md.snapshot_by_id(id).is_some() {
            Ok(id)
        } else {
            Err(format!(
                "snapshot {id} does not exist in table {}",
                table.identifier().name()
            ))
        };
    }
    if let Some(r) = super::meta::refs_of(md)
        .into_iter()
        .find(|r| r.name == selector)
    {
        return r.snapshot_id.parse::<i64>().map_err(|e| e.to_string());
    }
    if let Ok(ts) = DateTime::parse_from_rfc3339(selector) {
        let ms = ts.timestamp_millis();
        return md
            .snapshots()
            .filter(|s| s.timestamp_ms() <= ms)
            .max_by_key(|s| (s.timestamp_ms(), s.sequence_number()))
            .map(|s| s.snapshot_id())
            .ok_or_else(|| {
                format!(
                    "table {} has no snapshot at or before {selector}",
                    table.identifier().name()
                )
            });
    }
    Err(format!(
        "'{selector}' is neither a snapshot id, a branch/tag of {}, nor an RFC 3339 timestamp",
        table.identifier().name()
    ))
}

// ---------------------------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------------------------

fn to_df(e: iceberg::Error) -> DataFusionError {
    to_datafusion_error(e)
}

/// DataFusion error → [`CoreError`].
pub(crate) fn df_to_core(e: DataFusionError) -> CoreError {
    let root = e.find_root();
    if let DataFusionError::External(inner) = root {
        if let Some(ice) = inner.downcast_ref::<iceberg::Error>() {
            return to_core(
                iceberg::Error::new(ice.kind(), ice.message().to_string())
                    .with_context("detail", format!("{ice:#}")),
            );
        }
        if let Some(core) = inner.downcast_ref::<CoreError>() {
            return CoreError::Other(core.to_string());
        }
    }
    match root {
        DataFusionError::NotImplemented(m) => {
            CoreError::Invalid(format!("not supported by the local Iceberg engine: {m}"))
        }
        DataFusionError::Plan(_)
        | DataFusionError::SQL(..)
        | DataFusionError::SchemaError(..)
        | DataFusionError::Configuration(_) => CoreError::Invalid(clean(&root.to_string())),
        other => CoreError::Database(clean(&other.to_string())),
    }
}

fn clean(msg: &str) -> String {
    msg.trim_start_matches("Error during planning: ")
        .trim_start_matches("This feature is not implemented: ")
        .trim_start_matches("Execution error: ")
        .to_string()
}

// ---------------------------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------------------------

/// Catalog that resolves schemas lazily. Any schema name is accepted; a wrong one fails at table lookup.
#[derive(Debug)]
struct LazyCatalog {
    catalog: Arc<ManagedCatalog>,
    /// Known top-level namespaces, for `SHOW SCHEMAS`/information_schema only.
    names: RwLock<Vec<String>>,
}

impl CatalogProvider for LazyCatalog {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema_names(&self) -> Vec<String> {
        self.names.read().map(|g| g.clone()).unwrap_or_default()
    }

    fn schema(&self, name: &str) -> Option<Arc<dyn SchemaProvider>> {
        Some(Arc::new(LazySchema {
            catalog: self.catalog.clone(),
            ns: namespace_of(name),
        }))
    }
}

#[derive(Debug)]
struct LazySchema {
    catalog: Arc<ManagedCatalog>,
    ns: NamespaceIdent,
}

/// Runs a future to completion from a synchronous `SchemaProvider` method.
fn block_on_sync<F: std::future::Future>(f: F) -> Option<F::Output> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    if handle.runtime_flavor() != tokio::runtime::RuntimeFlavor::MultiThread {
        return None;
    }
    Some(tokio::task::block_in_place(|| handle.block_on(f)))
}

#[async_trait]
impl SchemaProvider for LazySchema {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn table_names(&self) -> Vec<String> {
        block_on_sync(self.catalog.list_tables(&self.ns))
            .and_then(|r| r.ok())
            .map(|ts| ts.into_iter().map(|t| t.name().to_string()).collect())
            .unwrap_or_default()
    }

    async fn table(&self, name: &str) -> DFResult<Option<Arc<dyn TableProvider>>> {
        let (base, selector) = parse_table_name(name);
        let ident = TableIdent::new(self.ns.clone(), base.to_string());
        let table = match self.catalog.load_table(&ident).await {
            Ok(t) => t,
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::TableNotFound | ErrorKind::NamespaceNotFound
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(to_df(e)),
        };
        match selector {
            Selector::Current => {
                let read = IcebergStaticTableProvider::try_new_from_table(table)
                    .await
                    .map_err(to_df)?;
                Ok(Some(Arc::new(LiveTable {
                    read,
                    catalog: self.catalog.clone(),
                    ident,
                })))
            }
            Selector::AsOf(sel) => {
                let id = resolve_snapshot(&table, &sel).map_err(DataFusionError::Plan)?;
                let p = IcebergStaticTableProvider::try_new_from_table_snapshot(table, id)
                    .await
                    .map_err(to_df)?;
                Ok(Some(Arc::new(p)))
            }
            Selector::Meta(kind) => Ok(Some(metadata_table(&table, &kind).await?)),
        }
    }

    fn table_exist(&self, name: &str) -> bool {
        let (base, _) = parse_table_name(name);
        let ident = TableIdent::new(self.ns.clone(), base.to_string());
        block_on_sync(self.catalog.table_exists(&ident))
            .and_then(|r| r.ok())
            .unwrap_or(false)
    }

    /// `CREATE TABLE ns.t (…)` and `CREATE TABLE ns.t AS SELECT …`. DataFusion hands us a `MemTable`
    /// holding the (possibly empty) result; we create the Iceberg table from its schema and append the rows.
    fn register_table(
        &self,
        name: String,
        table: Arc<dyn TableProvider>,
    ) -> DFResult<Option<Arc<dyn TableProvider>>> {
        // DataFusion also lands here for `CREATE VIEW` and `CREATE EXTERNAL TABLE`, which must not
        // silently become a physical table holding a copy of the data.
        if table.as_any().downcast_ref::<MemTable>().is_none() {
            return Err(DataFusionError::NotImplemented(
                "the local Iceberg engine only creates tables (CREATE TABLE, CREATE TABLE AS SELECT)"
                    .into(),
            ));
        }
        let catalog = self.catalog.clone();
        let ns = self.ns.clone();
        let fut = async move {
            let arrow = table.schema();
            let schema = arrow_schema_to_schema_auto_assign_ids(arrow.as_ref()).map_err(to_df)?;
            let creation = TableCreation::builder()
                .name(name.clone())
                .schema(schema)
                .build();
            catalog.create_table(&ns, creation).await.map_err(to_df)?;
            // Rows from `CREATE TABLE … AS SELECT`.
            let ctx = SessionContext::new();
            let state = ctx.state();
            let input = table.scan(&state, None, &[], None).await?;
            if input.properties().output_partitioning().partition_count() > 0 {
                let rows =
                    datafusion::physical_plan::collect(input.clone(), ctx.task_ctx()).await?;
                if rows.iter().any(|b| b.num_rows() > 0) {
                    let ident = TableIdent::new(ns.clone(), name.clone());
                    let loaded = catalog.load_table(&ident).await.map_err(to_df)?;
                    let live = LiveTable {
                        read: IcebergStaticTableProvider::try_new_from_table(loaded)
                            .await
                            .map_err(to_df)?,
                        catalog: catalog.clone(),
                        ident,
                    };
                    let mem = MemTable::try_new(rows[0].schema(), vec![rows])?;
                    let src = mem.scan(&state, None, &[], None).await?;
                    let plan = live.insert_into(&state, src, InsertOp::Append).await?;
                    datafusion::physical_plan::collect(plan, ctx.task_ctx()).await?;
                }
            }
            Ok::<Option<Arc<dyn TableProvider>>, DataFusionError>(None)
        };
        block_on_sync(fut).unwrap_or_else(|| {
            Err(DataFusionError::NotImplemented(
                "CREATE TABLE needs a multi-threaded runtime".into(),
            ))
        })
    }

    fn deregister_table(&self, _name: &str) -> DFResult<Option<Arc<dyn TableProvider>>> {
        Err(DataFusionError::NotImplemented(
            "DROP TABLE through the local engine (use your catalog's tooling)".into(),
        ))
    }
}

/// `table$snapshots` / `table$manifests` as an in-memory table.
async fn metadata_table(table: &Table, kind: &str) -> DFResult<Arc<dyn TableProvider>> {
    let kind = MetadataTableType::try_from(kind).map_err(DataFusionError::Plan)?;
    let inspect = table.inspect();
    let (iceberg_schema, stream) = match kind {
        MetadataTableType::Snapshots => {
            let t = inspect.snapshots();
            (t.schema(), t.scan().await.map_err(to_df)?)
        }
        MetadataTableType::Manifests => {
            let t = inspect.manifests();
            (t.schema(), t.scan().await.map_err(to_df)?)
        }
    };
    let batches: Vec<RecordBatch> = stream.try_collect().await.map_err(to_df)?;
    let arrow: ArrowSchemaRef = match batches.first() {
        Some(b) => b.schema(),
        None => Arc::new(schema_to_arrow_schema(&iceberg_schema).map_err(to_df)?),
    };
    Ok(Arc::new(MemTable::try_new(arrow, vec![batches])?))
}

/// Read path via the static provider (fresh metadata was loaded when the name was resolved);
/// write path via `iceberg-datafusion`'s catalog-backed provider, created on demand.
#[derive(Debug)]
struct LiveTable {
    read: IcebergStaticTableProvider,
    catalog: Arc<ManagedCatalog>,
    ident: TableIdent,
}

#[async_trait]
impl TableProvider for LiveTable {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn schema(&self) -> ArrowSchemaRef {
        self.read.schema()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> DFResult<Arc<dyn ExecutionPlan>> {
        self.read.scan(state, projection, filters, limit).await
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DFResult<Vec<TableProviderFilterPushDown>> {
        self.read.supports_filters_pushdown(filters)
    }

    async fn insert_into(
        &self,
        state: &dyn Session,
        input: Arc<dyn ExecutionPlan>,
        insert_op: InsertOp,
    ) -> DFResult<Arc<dyn ExecutionPlan>> {
        let scoped = Arc::new(ScopedCatalog::new(self.catalog.clone(), self.ident.clone()));
        let joined = scoped.joined.clone();
        let name = self.ident.name().to_string();
        let provider = IcebergCatalogProvider::try_new(scoped)
            .await
            .map_err(to_df)?;
        let schema = provider
            .schema(&joined)
            .ok_or_else(|| DataFusionError::Plan(format!("namespace {joined} not found")))?;
        let writer = schema
            .table(&name)
            .await?
            .ok_or_else(|| DataFusionError::Plan(format!("table {name} not found")))?;
        writer.insert_into(state, input, insert_op).await
    }
}

/// A catalog view that contains exactly one table, so `IcebergCatalogProvider::try_new` loads only that one.
/// Namespaces with several levels are presented as one level named `a.b` (the provider flattens levels).
#[derive(Debug)]
struct ScopedCatalog {
    inner: Arc<ManagedCatalog>,
    ns: NamespaceIdent,
    joined: String,
    name: String,
}

impl ScopedCatalog {
    fn new(inner: Arc<ManagedCatalog>, ident: TableIdent) -> Self {
        let ns = ident.namespace().clone();
        let joined = ns.as_ref().join(".");
        Self {
            inner,
            ns,
            joined,
            name: ident.name().to_string(),
        }
    }

    /// The real namespace behind what the provider asks for.
    fn real(&self, ns: &NamespaceIdent) -> NamespaceIdent {
        if ns.as_ref().len() == 1 && ns.as_ref()[0] == self.joined {
            self.ns.clone()
        } else {
            ns.clone()
        }
    }

    fn real_table(&self, t: &TableIdent) -> TableIdent {
        TableIdent::new(self.real(t.namespace()), t.name().to_string())
    }
}

#[async_trait]
impl Catalog for ScopedCatalog {
    async fn list_namespaces(
        &self,
        _parent: Option<&NamespaceIdent>,
    ) -> iceberg::Result<Vec<NamespaceIdent>> {
        Ok(vec![NamespaceIdent::new(self.joined.clone())])
    }
    async fn create_namespace(
        &self,
        n: &NamespaceIdent,
        p: HashMap<String, String>,
    ) -> iceberg::Result<Namespace> {
        self.inner.create_namespace(&self.real(n), p).await
    }
    async fn get_namespace(&self, n: &NamespaceIdent) -> iceberg::Result<Namespace> {
        self.inner.get_namespace(&self.real(n)).await
    }
    async fn namespace_exists(&self, n: &NamespaceIdent) -> iceberg::Result<bool> {
        self.inner.namespace_exists(&self.real(n)).await
    }
    async fn update_namespace(
        &self,
        n: &NamespaceIdent,
        p: HashMap<String, String>,
    ) -> iceberg::Result<()> {
        self.inner.update_namespace(&self.real(n), p).await
    }
    async fn drop_namespace(&self, n: &NamespaceIdent) -> iceberg::Result<()> {
        self.inner.drop_namespace(&self.real(n)).await
    }
    async fn list_tables(&self, _n: &NamespaceIdent) -> iceberg::Result<Vec<TableIdent>> {
        Ok(vec![TableIdent::new(
            NamespaceIdent::new(self.joined.clone()),
            self.name.clone(),
        )])
    }
    async fn create_table(&self, n: &NamespaceIdent, c: TableCreation) -> iceberg::Result<Table> {
        self.inner.create_table(&self.real(n), c).await
    }
    async fn load_table(&self, t: &TableIdent) -> iceberg::Result<Table> {
        self.inner.load_table(&self.real_table(t)).await
    }
    async fn drop_table(&self, t: &TableIdent) -> iceberg::Result<()> {
        self.inner.drop_table(&self.real_table(t)).await
    }
    async fn purge_table(&self, t: &TableIdent) -> iceberg::Result<()> {
        self.inner.purge_table(&self.real_table(t)).await
    }
    async fn table_exists(&self, t: &TableIdent) -> iceberg::Result<bool> {
        self.inner.table_exists(&self.real_table(t)).await
    }
    async fn rename_table(&self, a: &TableIdent, b: &TableIdent) -> iceberg::Result<()> {
        self.inner
            .rename_table(&self.real_table(a), &self.real_table(b))
            .await
    }
    async fn register_table(&self, t: &TableIdent, m: String) -> iceberg::Result<Table> {
        self.inner.register_table(&self.real_table(t), m).await
    }
    async fn update_table(&self, commit: TableCommit) -> iceberg::Result<Table> {
        self.inner.update_table(commit).await
    }
}

// ---------------------------------------------------------------------------------------------
// Statement handling
// ---------------------------------------------------------------------------------------------

/// First keyword of a statement, upper-cased, skipping whitespace, comments and opening parentheses.
pub(crate) fn leading_keywords(sql: &str, n: usize) -> Vec<String> {
    let b = sql.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() && out.len() < n {
        match b[i] {
            c if c.is_ascii_whitespace() || c == b'(' => i += 1,
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 1;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let s = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push(sql[s..i].to_ascii_uppercase());
            }
            _ => break,
        }
    }
    out
}

/// Statements DataFusion cannot execute against Iceberg tables.
fn unsupported_dml(sql: &str) -> bool {
    matches!(
        leading_keywords(sql, 1).first().map(String::as_str),
        Some("UPDATE" | "DELETE" | "MERGE" | "TRUNCATE")
    )
}

/// How a run ended.
pub(crate) enum End {
    Finished,
    Failed {
        statement_index: Option<usize>,
        error: CoreError,
    },
    /// The receiver went away; nobody to tell.
    Gone,
}

enum StmtErr {
    Failed(CoreError),
    Gone,
}

impl From<CoreError> for StmtErr {
    fn from(e: CoreError) -> Self {
        StmtErr::Failed(e)
    }
}

struct StmtDone {
    command_tag: Option<String>,
    rows_affected: Option<u64>,
    row_count: usize,
    truncated: bool,
}

pub(crate) struct LocalEngine {
    ctx: SessionContext,
}

impl LocalEngine {
    /// Builds the session. `default_schema` is the Iceberg namespace that unqualified names resolve in.
    pub fn new(
        catalog: Arc<ManagedCatalog>,
        default_schema: Option<&str>,
        known_schemas: Vec<String>,
    ) -> Result<Self> {
        let schema = default_schema
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("default");
        let config = SessionConfig::new()
            .with_default_catalog_and_schema("iceberg", schema)
            .with_information_schema(true);
        let runtime = RuntimeEnvBuilder::new()
            .with_memory_limit(MEMORY_LIMIT_BYTES, 0.9)
            .build_arc()
            .map_err(|e| CoreError::Other(format!("query engine: {e}")))?;
        let ctx = SessionContext::new_with_config_rt(config, runtime);
        let mut names = known_schemas;
        if !names.iter().any(|n| n == schema) {
            names.push(schema.to_string());
        }
        ctx.register_catalog(
            "iceberg",
            Arc::new(LazyCatalog {
                catalog,
                names: RwLock::new(names),
            }),
        );
        Ok(Self { ctx })
    }

    /// Runs statements one after another, streaming events. Does not send `Started`/`Finished`.
    pub async fn run(
        &self,
        statements: &[String],
        opts: &ExecOptions,
        cancel: &CancellationToken,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> End {
        for (idx, stmt) in statements.iter().enumerate() {
            if cancel.is_cancelled() {
                return End::Failed {
                    statement_index: Some(idx),
                    error: CoreError::Cancelled,
                };
            }
            let t0 = Instant::now();
            let work = self.run_statement(idx, stmt, opts, tx);
            let work = async {
                match opts.timeout_ms {
                    Some(ms) => tokio::time::timeout(Duration::from_millis(ms), work)
                        .await
                        .unwrap_or_else(|_| {
                            Err(StmtErr::Failed(CoreError::Timeout(format!(
                                "statement exceeded {ms} ms"
                            ))))
                        }),
                    None => work.await,
                }
            };
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(StmtErr::Failed(CoreError::Cancelled)),
                r = work => r,
            };
            match result {
                Ok(done) => {
                    let ev = QueryEvent::StatementDone {
                        statement_index: idx,
                        command_tag: done.command_tag,
                        rows_affected: done.rows_affected,
                        row_count: done.row_count,
                        truncated: done.truncated,
                        elapsed_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
                    };
                    if tx.send(ev).await.is_err() {
                        return End::Gone;
                    }
                }
                Err(StmtErr::Gone) => return End::Gone,
                Err(StmtErr::Failed(error)) => {
                    return End::Failed {
                        statement_index: Some(idx),
                        error,
                    };
                }
            }
        }
        End::Finished
    }

    async fn run_statement(
        &self,
        idx: usize,
        stmt: &str,
        opts: &ExecOptions,
        tx: &mpsc::Sender<QueryEvent>,
    ) -> std::result::Result<StmtDone, StmtErr> {
        // 1. Policy before anything is planned: DataFusion has no read-only transactions.
        if opts.read_only {
            let c = classify(stmt);
            if c.overall != StatementClass::ReadOnly {
                return Err(CoreError::PolicyDenied(format!(
                    "this run is read-only; {} is not a query",
                    c.summary
                ))
                .into());
            }
        }
        if unsupported_dml(stmt) {
            return Err(CoreError::Invalid(NO_ROW_LEVEL_DML.into()).into());
        }
        let words = leading_keywords(stmt, 2);
        let is_insert = words.first().map(String::as_str) == Some("INSERT");

        // 2. Plan. Read-only runs also tell DataFusion itself to refuse DDL/DML/statements.
        let df = if opts.read_only {
            let o = SQLOptions::new()
                .with_allow_ddl(false)
                .with_allow_dml(false)
                .with_allow_statements(false);
            self.ctx.sql_with_options(stmt, o).await
        } else {
            self.ctx.sql(stmt).await
        }
        .map_err(df_to_core)?;

        // 3. Execute and stream.
        let mut stream = df.execute_stream().await.map_err(df_to_core)?;
        let schema = stream.schema();
        let returns_rows = !is_insert && !schema.fields().is_empty();
        if returns_rows {
            let ev = QueryEvent::Columns {
                statement_index: idx,
                columns: codec::columns_of(&schema),
            };
            tx.send(ev).await.map_err(|_| StmtErr::Gone)?;
        }

        let page_size = opts.page_size.max(1);
        let mut page: Vec<Vec<crate::model::Cell>> = Vec::new();
        let mut page_bytes = 0usize;
        let mut row_count = 0usize;
        let mut truncated = false;
        let mut affected: Option<u64> = None;

        while let Some(batch) = stream.next().await {
            let batch = batch.map_err(df_to_core)?;
            if is_insert {
                affected = Some(affected.unwrap_or(0) + insert_count(&batch));
                continue;
            }
            if !returns_rows || batch.num_rows() == 0 {
                continue;
            }
            let room = opts.max_rows.saturating_sub(row_count);
            let take = batch.num_rows().min(room);
            if take < batch.num_rows() {
                truncated = true;
            }
            if take > 0 {
                let sliced = batch.slice(0, take);
                for row in codec::batch_to_rows(&sliced) {
                    page_bytes += row.iter().map(cell_weight).sum::<usize>();
                    page.push(row);
                    row_count += 1;
                    // A page of large cells goes out early (same rule as the Postgres engine).
                    if page.len() >= page_size || page_bytes >= PAGE_BYTE_BUDGET {
                        page_bytes = 0;
                        let rows = std::mem::take(&mut page);
                        tx.send(QueryEvent::Rows {
                            statement_index: idx,
                            rows,
                        })
                        .await
                        .map_err(|_| StmtErr::Gone)?;
                    }
                }
            }
            if truncated {
                break; // drop the stream: stop scanning once the cap is exceeded
            }
            if row_count >= opts.max_rows {
                // Exactly at the cap: it is truncated only if another row exists.
                if let Some(next) = stream.next().await {
                    let next = next.map_err(df_to_core)?;
                    if next.num_rows() > 0 {
                        truncated = true;
                    }
                }
                break;
            }
        }
        if !page.is_empty() {
            tx.send(QueryEvent::Rows {
                statement_index: idx,
                rows: page,
            })
            .await
            .map_err(|_| StmtErr::Gone)?;
        }

        let command_tag = if is_insert {
            Some(format!("INSERT 0 {}", affected.unwrap_or(0)))
        } else if returns_rows {
            Some(format!("SELECT {row_count}"))
        } else {
            Some(words.join(" ")).filter(|t| !t.is_empty())
        };
        Ok(StmtDone {
            command_tag,
            rows_affected: affected,
            row_count,
            truncated,
        })
    }
}

/// The `count` column of an `INSERT`'s result batch.
fn insert_count(batch: &RecordBatch) -> u64 {
    batch
        .columns()
        .first()
        .and_then(|c| c.as_any().downcast_ref::<UInt64Array>())
        .map(|a| {
            (0..a.len())
                .filter(|&i| !a.is_null(i))
                .map(|i| a.value(i))
                .sum()
        })
        .unwrap_or(0)
}

/// Folds events into the result of the last row-returning statement (or the last statement).
#[derive(Default)]
pub(crate) struct Collector {
    current: QueryResult,
    last_rows: Option<QueryResult>,
    last_any: Option<QueryResult>,
}

impl Collector {
    pub fn push(&mut self, ev: QueryEvent) {
        match ev {
            QueryEvent::Columns { columns, .. } => {
                self.current = QueryResult {
                    columns,
                    ..QueryResult::default()
                }
            }
            QueryEvent::Rows { rows, .. } => self.current.rows.extend(rows),
            QueryEvent::StatementDone {
                command_tag,
                rows_affected,
                row_count,
                truncated,
                elapsed_ms,
                ..
            } => {
                let mut done = std::mem::take(&mut self.current);
                done.command_tag = command_tag;
                done.rows_affected = rows_affected;
                done.row_count = row_count;
                done.truncated = truncated;
                done.elapsed_ms = elapsed_ms;
                if done.columns.is_empty() {
                    self.last_any = Some(done)
                } else {
                    self.last_rows = Some(done)
                }
            }
            _ => {}
        }
    }

    pub fn into_result(self) -> Option<QueryResult> {
        self.last_rows.or(self.last_any)
    }
}

/// `Failed` event for an error.
pub(crate) fn failed_event(statement_index: Option<usize>, error: &CoreError) -> QueryEvent {
    QueryEvent::Failed {
        statement_index,
        error: ErrorPayload::from(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_name_selectors() {
        assert_eq!(parse_table_name("orders"), ("orders", Selector::Current));
        assert_eq!(
            parse_table_name("orders@123"),
            ("orders", Selector::AsOf("123".into()))
        );
        assert_eq!(
            parse_table_name("orders@audit-2026"),
            ("orders", Selector::AsOf("audit-2026".into()))
        );
        assert_eq!(
            parse_table_name("orders@2026-10-01T00:00:00Z"),
            ("orders", Selector::AsOf("2026-10-01T00:00:00Z".into()))
        );
        assert_eq!(
            parse_table_name("orders$snapshots"),
            ("orders", Selector::Meta("snapshots".into()))
        );
        assert_eq!(parse_table_name("@weird"), ("@weird", Selector::Current));
    }

    #[test]
    fn namespaces_split_on_dots() {
        assert_eq!(namespace_of("shop").as_ref(), &vec!["shop".to_string()]);
        assert_eq!(
            namespace_of("a.b").as_ref(),
            &vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn leading_keywords_skip_comments_and_parens() {
        assert_eq!(
            leading_keywords("  -- hi\n /* a /* nested */ b */ update t set x = 1", 1),
            vec!["UPDATE"]
        );
        assert_eq!(
            leading_keywords("(select 1) union select 2", 1),
            vec!["SELECT"]
        );
        assert_eq!(
            leading_keywords("create table x (a int)", 2),
            vec!["CREATE", "TABLE"]
        );
        assert!(leading_keywords("", 1).is_empty());
        assert!(unsupported_dml("DELETE FROM t"));
        assert!(unsupported_dml(
            "-- c\nmerge into t using s on true when matched then delete"
        ));
        assert!(!unsupported_dml("INSERT INTO t VALUES (1)"));
        assert!(!unsupported_dml("SELECT 'update'"));
    }

    #[test]
    fn datafusion_errors_map_to_core_variants() {
        assert!(matches!(
            df_to_core(DataFusionError::Plan("table 'x' not found".into())),
            CoreError::Invalid(_)
        ));
        assert!(
            matches!(df_to_core(DataFusionError::NotImplemented("MERGE".into())), CoreError::Invalid(m) if m.contains("local Iceberg engine"))
        );
        assert!(matches!(
            df_to_core(DataFusionError::Execution("boom".into())),
            CoreError::Database(_)
        ));
        let ice = iceberg::Error::new(ErrorKind::TableNotFound, "gone");
        assert!(matches!(df_to_core(to_df(ice)), CoreError::NotFound(_)));
    }

    #[test]
    fn insert_count_sums_the_count_column() {
        use datafusion::arrow::datatypes::{DataType, Field, Schema};
        let schema = Arc::new(Schema::new(vec![Field::new(
            "count",
            DataType::UInt64,
            false,
        )]));
        let b =
            RecordBatch::try_new(schema, vec![Arc::new(UInt64Array::from(vec![5, 7]))]).unwrap();
        assert_eq!(insert_count(&b), 12);
    }
}
