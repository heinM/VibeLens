use tauri::State;
use tauri::ipc::Channel;
use tokio::sync::mpsc;
use vibelens_core::model::*;
use vibelens_core::sql::StatementClass;
use vibelens_core::{CoreError, ErrorPayload};

use super::{CmdResult, err};
use crate::state::AppState;

#[tauri::command]
pub async fn pg_list_databases(
    state: State<'_, AppState>,
    profile_id: String,
) -> CmdResult<Vec<DatabaseInfo>> {
    let h = state.connections.pg(&profile_id, None).await.map_err(err)?;
    h.list_databases().await.map_err(err)
}

#[tauri::command]
pub async fn pg_list_schemas(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
) -> CmdResult<Vec<SchemaInfo>> {
    let h = state
        .connections
        .pg(&profile_id, database.as_deref())
        .await
        .map_err(err)?;
    h.list_schemas().await.map_err(err)
}

#[tauri::command]
pub async fn pg_list_objects(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
    schema: String,
) -> CmdResult<Vec<SchemaObject>> {
    let h = state
        .connections
        .pg(&profile_id, database.as_deref())
        .await
        .map_err(err)?;
    h.list_objects(&schema).await.map_err(err)
}

/// Partitions (and pg_partman settings, when present) of one partitioned table.
#[tauri::command]
pub async fn pg_list_partitions(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
    schema: String,
    table: String,
) -> CmdResult<PartitionListing> {
    let h = state
        .connections
        .pg(&profile_id, database.as_deref())
        .await
        .map_err(err)?;
    h.list_partitions(&schema, &table).await.map_err(err)
}

#[tauri::command]
pub async fn pg_describe_table(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
    schema: String,
    table: String,
) -> CmdResult<TableDescription> {
    let h = state
        .connections
        .pg(&profile_id, database.as_deref())
        .await
        .map_err(err)?;
    h.describe_table(&schema, &table).await.map_err(err)
}

/// Streams results over `on_event`. Returns the query id immediately (use with `pg_cancel`).
/// Prod write-guard: on `environment = prod`, writes/DDL/unparseable SQL need `confirmed = true`
/// (UI shows a typed-confirmation dialog and resends).
#[tauri::command]
pub async fn pg_execute(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
    sql: String,
    options: Option<ExecOptions>,
    confirmed: Option<bool>,
    on_event: Channel<QueryEvent>,
) -> CmdResult<String> {
    let profile = state.profiles.get(&profile_id).map_err(err)?;
    let opts = options.unwrap_or_default();
    if profile.environment == Environment::Prod
        && !confirmed.unwrap_or(false)
        && let Some(reason) = prod_guard_reason(&sql, opts.read_only)
    {
        return Err(ErrorPayload::new(
            "confirm_required",
            format!("Production write guard: {reason}"),
        ));
    }

    let handle = state
        .connections
        .pg(&profile_id, database.as_deref())
        .await
        .map_err(err)?;
    let run = state.running_queries.start();
    let (query_id, cancel) = (run.id.clone(), run.cancel.clone());

    let (tx, mut rx) = mpsc::channel::<QueryEvent>(16);
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if on_event.send(ev).is_err() {
                break;
            }
        }
    });

    let qid = query_id.clone();
    let tx_err = tx.clone();
    tauri::async_runtime::spawn(async move {
        // Registered for as long as this task runs: the entry goes when the statement ends.
        let _run = run;
        if let Err(e) = handle
            .execute_stream(qid.clone(), &sql, opts, cancel, tx)
            .await
        {
            let _ = tx_err
                .send(QueryEvent::Failed {
                    statement_index: None,
                    error: (&e).into(),
                })
                .await;
        }
    });
    Ok(query_id)
}

/// Why a statement on a prod connection needs typed confirmation, or `None` if it doesn't.
/// A READ ONLY transaction blocks ordinary writes, but NOT side-effecting functions
/// (`dblink_exec`, `lo_export`, `pg_terminate_backend`, advisory locks, …), which the classifier
/// reports as `Unknown`. So read-only runs are still classified on prod.
pub(crate) fn prod_guard_reason(sql: &str, read_only: bool) -> Option<String> {
    prod_guard_reason_on(None, sql, read_only)
}

/// [`prod_guard_reason`] for an Iceberg connection on `engine`: Trino and Spark SQL the
/// PostgreSQL parser cannot read is judged by the engine's own read-only check
/// (`remote::classify_on`), so ordinary reads do not hit the write guard.
pub(crate) fn prod_guard_reason_on(
    engine: Option<&vibelens_core::model::IcebergQueryEngine>,
    sql: &str,
    read_only: bool,
) -> Option<String> {
    let c = vibelens_core::remote::classify_on(engine, sql);
    let needs = if read_only {
        c.overall == StatementClass::Unknown
    } else {
        c.overall >= StatementClass::Write
    };
    needs.then_some(c.summary)
}

#[tauri::command]
pub async fn pg_cancel(state: State<'_, AppState>, query_id: String) -> CmdResult<()> {
    // Not found: no such run, or it has already ended or been cancelled.
    if state.running_queries.cancel(&query_id) {
        Ok(())
    } else {
        Err(CoreError::NotFound(format!("query {query_id}")).into())
    }
}

/// Whole-schema catalog (tables, columns, indexes, FKs) in one call. Powers editor
/// autocomplete, the command palette and ER diagrams. Capped at `max_tables` (default 2000).
#[tauri::command]
pub async fn pg_schema_metadata(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
    schema: Option<String>,
    max_tables: Option<usize>,
) -> CmdResult<SchemaMetadata> {
    let h = state
        .connections
        .pg(&profile_id, database.as_deref())
        .await
        .map_err(err)?;
    h.schema_metadata(schema.as_deref(), max_tables.unwrap_or(2000))
        .await
        .map_err(err)
}
