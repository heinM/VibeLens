//! [`IcebergHandle`]: one per connection profile.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use futures_util::{StreamExt, stream};
use iceberg::transaction::{ApplyTransactionAction, Transaction};
use iceberg::{Catalog, NamespaceIdent, TableIdent};
use tokio::sync::{OnceCell, mpsc};
use tokio_util::sync::CancellationToken;

use super::IcebergDeps;
use super::catalog::{CatalogFactory, ManagedCatalog, ProfileFactory, table_bucket_name, to_core};
use super::engine::{Collector, End, LocalEngine, failed_event};
use super::meta;
use crate::athena::{AthenaConfig, AthenaEngine};
use crate::error::{CoreError, Result};
use crate::model::{
    ConnectionProfile, EngineKind, ExecOptions, ExpireSnapshotsResult, IcebergCatalog,
    IcebergNamespace, IcebergObjectKind, IcebergOptions, IcebergQueryEngine, IcebergTableInfo,
    IcebergTableRef, QueryEvent, QueryResult, SchemaMetadata, TableDescription,
};
use crate::sql::split_statements;

/// Namespaces fetched concurrently for their properties / tables loaded concurrently for `schema_metadata`.
const FANOUT: usize = 8;
/// Upper bounds for catalog walks.
const MAX_NAMESPACES: usize = 500;
const MAX_NAMESPACE_DEPTH: usize = 8;
const MAX_PROPERTY_FETCHES: usize = 64;

const FIVE_DAYS_MS: i64 = 5 * 24 * 3600 * 1000;

/// A connected Iceberg catalog with its query engine.
pub struct IcebergHandle {
    opts: IcebergOptions,
    profile_name: String,
    target: String,
    catalog: Arc<ManagedCatalog>,
    /// Top-level namespaces seen at connect (used to seed `SHOW SCHEMAS`).
    top_level: Vec<String>,
    local: OnceCell<Arc<LocalEngine>>,
    athena: Option<AthenaEngine>,
    started: Instant,
    last_used_ms: AtomicU64,
}

impl IcebergHandle {
    /// Builds the catalog + engine and verifies the catalog answers (lists top-level namespaces).
    /// `profile.iceberg` must be `Some`.
    pub async fn connect(
        profile: ConnectionProfile,
        deps: IcebergDeps,
    ) -> Result<Arc<IcebergHandle>> {
        if profile.engine != EngineKind::Iceberg {
            return Err(CoreError::Invalid("not an Iceberg connection".into()));
        }
        let opts = profile
            .iceberg
            .clone()
            .ok_or_else(|| CoreError::Invalid("this connection has no Iceberg options".into()))?;
        // The stored profile may have been edited by hand; the rules a save applies run again.
        crate::cleartext::check_profile(&profile)?;
        let athena_cfg = athena_target(&opts)?;

        let factory: Arc<dyn CatalogFactory> = Arc::new(ProfileFactory::new(&profile, &deps)?);
        let aws = deps.aws.clone();
        let clock: crate::aws::Clock = Arc::new(move || aws.now());
        let catalog = ManagedCatalog::new(factory, clock).await?;

        // Verify the catalog answers before declaring the connection good.
        let top = catalog.list_namespaces(None).await.map_err(to_core)?;
        let top_level: Vec<String> = top.iter().map(|n| n.as_ref().join(".")).collect();

        let athena = match athena_cfg {
            Some((cfg, target)) => {
                let creds = deps.aws.credentials_provider(&target).await?;
                Some(AthenaEngine::new(cfg, creds))
            }
            None => None,
        };

        Ok(Arc::new(IcebergHandle {
            target: describe(&opts),
            opts,
            profile_name: profile.name,
            catalog,
            top_level,
            local: OnceCell::new(),
            athena,
            started: Instant::now(),
            last_used_ms: AtomicU64::new(0),
        }))
    }

    /// Human-readable target for the status bar, e.g. "Glue · eu-west-1 · Athena (primary)".
    pub fn describe_target(&self) -> String {
        self.target.clone()
    }

    fn touch(&self) {
        let ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_used_ms.store(ms, Ordering::Relaxed);
    }

    /// Child namespaces of `parent` (top level when `None`).
    pub async fn list_namespaces(
        &self,
        parent: Option<Vec<String>>,
    ) -> Result<Vec<IcebergNamespace>> {
        self.touch();
        let parent = parent.filter(|p| !p.is_empty()).map(ident).transpose()?;
        let found = self
            .catalog
            .list_namespaces(parent.as_ref())
            .await
            .map_err(to_core)?;
        let cat = self.catalog.clone();
        let mut out: Vec<IcebergNamespace> = stream::iter(found.into_iter().enumerate())
            .map(|(i, ns)| {
                let cat = cat.clone();
                async move {
                    // Properties are a courtesy: skip them (one round trip each) past a cap, ignore failures.
                    let properties = if i < MAX_PROPERTY_FETCHES {
                        cat.get_namespace(&ns)
                            .await
                            .map(|n| {
                                n.properties()
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect::<BTreeMap<_, _>>()
                            })
                            .unwrap_or_default()
                    } else {
                        BTreeMap::new()
                    };
                    IcebergNamespace {
                        levels: ns.inner(),
                        properties,
                    }
                }
            })
            .buffer_unordered(FANOUT)
            .collect()
            .await;
        out.sort_by(|a, b| a.levels.cmp(&b.levels));
        Ok(out)
    }

    /// Tables of a namespace. `iceberg-rust` 0.10 has no view API, so views are not listed.
    pub async fn list_tables(&self, namespace: Vec<String>) -> Result<Vec<IcebergTableRef>> {
        self.touch();
        let ns = ident(namespace.clone())?;
        let mut tables: Vec<IcebergTableRef> = self
            .catalog
            .list_tables(&ns)
            .await
            .map_err(to_core)?
            .into_iter()
            .map(|t| IcebergTableRef {
                namespace: t.namespace().as_ref().clone(),
                name: t.name().to_string(),
                kind: IcebergObjectKind::Table,
            })
            .collect();
        tables.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tables)
    }

    pub async fn describe_table(
        &self,
        namespace: Vec<String>,
        name: &str,
    ) -> Result<IcebergTableInfo> {
        self.touch();
        let table = self
            .catalog
            .load_table(&table_ident(&namespace, name)?)
            .await
            .map_err(to_core)?;
        Ok(meta::describe(&table, namespace, name))
    }

    /// The FileIO properties a table was loaded with, secrets masked. Diagnostic: lets tests and
    /// support see which `s3.endpoint` really applies after the profile/catalog merge.
    #[doc(hidden)]
    pub async fn debug_file_io_props(
        &self,
        namespace: Vec<String>,
        name: &str,
    ) -> Result<BTreeMap<String, String>> {
        let table = self
            .catalog
            .load_table(&table_ident(&namespace, name)?)
            .await
            .map_err(to_core)?;
        Ok(table
            .file_io()
            .config()
            .props()
            .iter()
            .map(|(k, v)| {
                let secret = ["secret", "token", "credential", "key-id"]
                    .iter()
                    .any(|s| k.contains(s));
                (
                    k.clone(),
                    if secret { "***".to_string() } else { v.clone() },
                )
            })
            .collect())
    }

    /// Catalog as `SchemaMetadata` for editor autocomplete / agent `get_schema`. `TableDescription.schema`
    /// is the namespace joined with `.`; columns are top-level fields with Iceberg type names.
    /// Walks nested namespaces; `truncated` is set when more than `max_tables` tables exist.
    pub async fn schema_metadata(
        &self,
        namespace: Option<Vec<String>>,
        max_tables: usize,
    ) -> Result<SchemaMetadata> {
        self.touch();
        let roots: Vec<Vec<String>> = match namespace.filter(|n| !n.is_empty()) {
            Some(n) => vec![n],
            None => self
                .catalog
                .list_namespaces(None)
                .await
                .map_err(to_core)?
                .into_iter()
                .map(NamespaceIdent::inner)
                .collect(),
        };

        // Breadth-first over namespaces, collecting table identifiers until the cap is hit.
        let mut queue: VecDeque<Vec<String>> = roots.into();
        let mut seen = 0usize;
        let mut idents: Vec<TableIdent> = Vec::new();
        let mut truncated = false;
        'walk: while let Some(levels) = queue.pop_front() {
            seen += 1;
            if seen > MAX_NAMESPACES {
                truncated = true;
                break;
            }
            let Ok(ns) = ident(levels.clone()) else {
                continue;
            };
            match self.catalog.list_tables(&ns).await {
                Ok(ts) => {
                    for t in ts {
                        if idents.len() >= max_tables {
                            truncated = true;
                            break 'walk;
                        }
                        idents.push(t);
                    }
                }
                Err(e) => tracing::debug!("iceberg: list_tables({}) failed: {e}", levels.join(".")),
            }
            if levels.len() < MAX_NAMESPACE_DEPTH {
                // Catalogs without nested namespaces (Glue, S3 Tables) answer with an error or nothing.
                if let Ok(children) = self.catalog.list_namespaces(Some(&ns)).await {
                    queue.extend(children.into_iter().map(NamespaceIdent::inner));
                }
            }
        }

        let cat = self.catalog.clone();
        let mut tables: Vec<TableDescription> = stream::iter(idents)
            .map(|id| {
                let cat = cat.clone();
                async move {
                    match cat.load_table(&id).await {
                        Ok(t) => Some(meta::table_description(
                            &t,
                            id.namespace().as_ref(),
                            id.name(),
                        )),
                        Err(e) => {
                            tracing::debug!("iceberg: load_table({id}) failed: {e}");
                            None
                        }
                    }
                }
            })
            .buffer_unordered(FANOUT)
            .filter_map(|x| async move { x })
            .collect()
            .await;
        tables.sort_by(|a, b| (&a.schema, &a.name).cmp(&(&b.schema, &b.name)));
        Ok(SchemaMetadata {
            database: self.profile_name.clone(),
            server_version: None,
            tables,
            truncated,
        })
    }

    async fn local_engine(&self) -> Result<Arc<LocalEngine>> {
        self.local
            .get_or_try_init(|| async {
                LocalEngine::new(
                    self.catalog.clone(),
                    self.opts.default_namespace.as_deref(),
                    self.top_level.clone(),
                )
                .map(Arc::new)
            })
            .await
            .cloned()
    }

    /// Same event contract as `PgHandle::execute_stream` (Started … Finished|Failed), plus optional `Stats`.
    /// With the Athena engine this delegates to [`AthenaEngine`].
    pub async fn execute_stream(
        &self,
        query_id: String,
        sql: &str,
        opts: ExecOptions,
        cancel: CancellationToken,
        tx: mpsc::Sender<QueryEvent>,
    ) -> Result<()> {
        self.touch();
        if let Some(a) = &self.athena {
            return a.execute_stream(query_id, sql, opts, cancel, tx).await;
        }
        let statements = split_statements(sql);
        tx.send(QueryEvent::Started {
            query_id,
            statement_count: statements.len(),
        })
        .await
        .map_err(|_| CoreError::Other("query result receiver was dropped".into()))?;
        let t0 = Instant::now();
        let end = match self.local_engine().await {
            Ok(engine) => engine.run(&statements, &opts, &cancel, &tx).await,
            Err(error) => End::Failed {
                statement_index: None,
                error,
            },
        };
        let terminal = match end {
            End::Finished => QueryEvent::Finished {
                elapsed_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
            },
            End::Failed {
                statement_index,
                error,
            } => failed_event(statement_index, &error),
            End::Gone => return Ok(()),
        };
        let _ = tx.send(terminal).await;
        self.touch();
        Ok(())
    }

    /// Bounded, materialised execution: the result of the last statement that returns rows
    /// (or of the last statement when none does). Errors are returned as `Err`.
    pub async fn execute_collect(&self, sql: &str, opts: ExecOptions) -> Result<QueryResult> {
        self.touch();
        if let Some(a) = &self.athena {
            return a.execute_collect(sql, opts).await;
        }
        let statements = split_statements(sql);
        if statements.is_empty() {
            return Ok(QueryResult::default());
        }
        let engine = self.local_engine().await?;
        let (tx, mut rx) = mpsc::channel::<QueryEvent>(32);
        let cancel = CancellationToken::new();
        let runner = async {
            let end = engine.run(&statements, &opts, &cancel, &tx).await;
            drop(tx);
            end
        };
        let collector = async {
            let mut c = Collector::default();
            while let Some(ev) = rx.recv().await {
                c.push(ev);
            }
            c
        };
        let (end, collected) = tokio::join!(runner, collector);
        match end {
            End::Finished => collected
                .into_result()
                .ok_or_else(|| CoreError::Other("no statement produced a result".into())),
            End::Failed { error, .. } => Err(error),
            End::Gone => Err(CoreError::Other("result collector went away".into())),
        }
    }

    /// Expire snapshots older than `older_than_ms` (default: 5 days ago), always keeping the newest
    /// `retain_last` (default 1) and every snapshot referenced by a branch/tag.
    /// Only table *metadata* is rewritten; data files are left for a separate clean-up.
    pub async fn expire_snapshots(
        &self,
        namespace: Vec<String>,
        name: &str,
        older_than_ms: Option<i64>,
        retain_last: Option<u32>,
    ) -> Result<ExpireSnapshotsResult> {
        self.touch();
        let retain = retain_last.unwrap_or(1);
        if retain == 0 {
            return Err(CoreError::Invalid("retain_last must be at least 1".into()));
        }
        let table = self
            .catalog
            .load_table(&table_ident(&namespace, name)?)
            .await
            .map_err(to_core)?;
        let before = table.metadata().snapshots().len() as u64;
        let cutoff =
            older_than_ms.unwrap_or_else(|| chrono::Utc::now().timestamp_millis() - FIVE_DAYS_MS);
        let tx = Transaction::new(&table);
        let tx = tx
            .expire_snapshots()
            .expire_older_than_ms(cutoff)
            .retain_last(retain as usize)
            .apply(tx)
            .map_err(to_core)?;
        let updated = tx.commit(self.catalog.as_ref()).await.map_err(to_core)?;
        let remaining = updated.metadata().snapshots().len() as u64;
        Ok(ExpireSnapshotsResult {
            expired: before.saturating_sub(remaining),
            remaining,
        })
    }

    pub fn idle_secs(&self) -> u64 {
        let used = self.last_used_ms.load(Ordering::Relaxed);
        let now = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        now.saturating_sub(used) / 1000
    }

    pub async fn close(&self) {
        // Pools (SQL catalog) and HTTP clients close when the last reference drops; nothing to flush.
    }
}

fn ident(levels: Vec<String>) -> Result<NamespaceIdent> {
    NamespaceIdent::from_vec(levels)
        .map_err(|e| CoreError::Invalid(format!("namespace: {}", e.message())))
}

fn table_ident(namespace: &[String], name: &str) -> Result<TableIdent> {
    if name.trim().is_empty() {
        return Err(CoreError::Invalid("table name is empty".into()));
    }
    Ok(TableIdent::new(
        ident(namespace.to_vec())?,
        name.to_string(),
    ))
}

/// For the Athena engine: its config and the AWS identity to use. `Ok(None)` for the local engine.
fn athena_target(opts: &IcebergOptions) -> Result<Option<(AthenaConfig, crate::model::AwsTarget)>> {
    let IcebergQueryEngine::Athena {
        workgroup,
        output_location,
        aws,
    } = &opts.query_engine
    else {
        return Ok(None);
    };
    let (catalog_aws, athena_catalog) = match &opts.catalog {
        IcebergCatalog::Glue { aws, .. } => (aws, "AwsDataCatalog".to_string()),
        IcebergCatalog::S3Tables {
            aws,
            table_bucket_arn,
        } => {
            let bucket = table_bucket_name(table_bucket_arn)
                .ok_or_else(|| CoreError::Invalid("table bucket ARN is not valid (expected arn:aws:s3tables:<region>:<account>:bucket/<name>)".into()))?;
            (aws, format!("s3tablescatalog/{bucket}"))
        }
        _ => {
            return Err(CoreError::Invalid(
                "the Athena query engine works with Glue and S3 Tables catalogs only; use the local engine for REST, SQL and Hive catalogs".into(),
            ));
        }
    };
    if workgroup.trim().is_empty() {
        return Err(CoreError::Invalid("Athena workgroup is empty".into()));
    }
    let target = aws.clone().unwrap_or_else(|| catalog_aws.clone());
    let cfg = AthenaConfig {
        region: target.region.trim().to_string(),
        workgroup: workgroup.trim().to_string(),
        output_location: output_location
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        catalog: athena_catalog,
        database: opts
            .default_namespace
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    };
    Ok(Some((cfg, target)))
}

/// "REST · localhost:8181 · Local engine".
pub(crate) fn describe(opts: &IcebergOptions) -> String {
    let (kind, place) = match &opts.catalog {
        IcebergCatalog::Rest { uri, .. } => ("REST", host_port(uri)),
        IcebergCatalog::Glue { aws, .. } => ("Glue", aws.region.clone()),
        IcebergCatalog::S3Tables { aws, .. } => ("S3 Tables", aws.region.clone()),
        IcebergCatalog::Sql { uri, .. } => ("SQL", sql_place(uri)),
        IcebergCatalog::Hms { address, .. } => ("Hive Metastore", address.clone()),
    };
    let engine = match &opts.query_engine {
        IcebergQueryEngine::Local => "Local engine".to_string(),
        IcebergQueryEngine::Athena { workgroup, .. } => format!("Athena ({workgroup})"),
    };
    format!("{kind} · {place} · {engine}")
}

fn host_port(uri: &str) -> String {
    match url::Url::parse(uri.trim()) {
        Ok(u) => match (u.host_str(), u.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => uri.to_string(),
        },
        Err(_) => uri.to_string(),
    }
}

/// File name for sqlite, `host/db` for postgres. Never the user or password.
fn sql_place(uri: &str) -> String {
    match url::Url::parse(uri.trim()) {
        Ok(u) if u.scheme() == "sqlite" => std::path::Path::new(u.path())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| u.path().to_string()),
        Ok(u) => format!("{}{}", host_port(uri), u.path()),
        Err(_) => "database".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{AwsIdentity, AwsTarget, IcebergStorage, RestAuth};

    fn opts(catalog: IcebergCatalog, engine: IcebergQueryEngine) -> IcebergOptions {
        IcebergOptions {
            catalog,
            storage: IcebergStorage::Auto,
            query_engine: engine,
            default_namespace: Some("analytics".into()),
        }
    }

    fn glue() -> IcebergCatalog {
        IcebergCatalog::Glue {
            aws: AwsTarget {
                identity: AwsIdentity::DefaultChain,
                region: "eu-west-1".into(),
            },
            catalog_id: None,
            warehouse: None,
        }
    }

    #[test]
    fn targets_read_well() {
        let rest = IcebergCatalog::Rest {
            uri: "http://localhost:8181/".into(),
            warehouse: None,
            auth: RestAuth::None,
            headers: BTreeMap::new(),
        };
        assert_eq!(
            describe(&opts(rest, IcebergQueryEngine::Local)),
            "REST · localhost:8181 · Local engine"
        );
        let athena = IcebergQueryEngine::Athena {
            workgroup: "primary".into(),
            output_location: None,
            aws: None,
        };
        assert_eq!(
            describe(&opts(glue(), athena)),
            "Glue · eu-west-1 · Athena (primary)"
        );
        let sql = IcebergCatalog::Sql {
            uri: "sqlite:////data/ice/catalog.db".into(),
            warehouse: "/w".into(),
            catalog_name: "c".into(),
        };
        assert_eq!(
            describe(&opts(sql, IcebergQueryEngine::Local)),
            "SQL · catalog.db · Local engine"
        );
        let pg = IcebergCatalog::Sql {
            uri: "postgres://user:pw@db.internal:5432/cat".into(),
            warehouse: "s3://w".into(),
            catalog_name: "c".into(),
        };
        let d = describe(&opts(pg, IcebergQueryEngine::Local));
        assert_eq!(d, "SQL · db.internal:5432/cat · Local engine");
        assert!(!d.contains("pw"));
    }

    #[test]
    fn athena_config_for_glue_and_s3_tables() {
        let athena = IcebergQueryEngine::Athena {
            workgroup: "wg".into(),
            output_location: Some("s3://res/".into()),
            aws: None,
        };
        let (cfg, target) = athena_target(&opts(glue(), athena.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(cfg.catalog, "AwsDataCatalog");
        assert_eq!(cfg.region, "eu-west-1");
        assert_eq!(cfg.workgroup, "wg");
        assert_eq!(cfg.output_location.as_deref(), Some("s3://res/"));
        assert_eq!(cfg.database.as_deref(), Some("analytics"));
        assert_eq!(target.region, "eu-west-1");

        let s3t = IcebergCatalog::S3Tables {
            aws: AwsTarget {
                identity: AwsIdentity::DefaultChain,
                region: "af-south-1".into(),
            },
            table_bucket_arn: "arn:aws:s3tables:af-south-1:123456789012:bucket/lake".into(),
        };
        let (cfg, _) = athena_target(&opts(s3t, athena)).unwrap().unwrap();
        assert_eq!(cfg.catalog, "s3tablescatalog/lake");
        assert_eq!(cfg.region, "af-south-1");
    }

    #[test]
    fn athena_with_other_catalogs_is_rejected() {
        let athena = IcebergQueryEngine::Athena {
            workgroup: "wg".into(),
            output_location: None,
            aws: None,
        };
        for c in [
            IcebergCatalog::Rest {
                uri: "http://x".into(),
                warehouse: None,
                auth: RestAuth::None,
                headers: BTreeMap::new(),
            },
            IcebergCatalog::Sql {
                uri: "sqlite://x".into(),
                warehouse: "w".into(),
                catalog_name: "c".into(),
            },
            IcebergCatalog::Hms {
                address: "h:9083".into(),
                warehouse: "w".into(),
            },
        ] {
            assert!(matches!(
                athena_target(&opts(c, athena.clone())),
                Err(CoreError::Invalid(_))
            ));
        }
        assert!(
            athena_target(&opts(glue(), IcebergQueryEngine::Local))
                .unwrap()
                .is_none()
        );
    }
}
