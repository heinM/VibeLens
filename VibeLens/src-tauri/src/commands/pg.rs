use tauri::State;
use tauri::ipc::Channel;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vibelens_core::model::*;
use vibelens_core::sql::{StatementClass, classify};
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
    let query_id = uuid::Uuid::new_v4().to_string();
    let cancel = CancellationToken::new();
    state
        .running_queries
        .lock()
        .unwrap()
        .insert(query_id.clone(), cancel.clone());

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
    let c = classify(sql);
    let needs = if read_only {
        c.overall == StatementClass::Unknown
    } else {
        c.overall >= StatementClass::Write
    };
    needs.then_some(c.summary)
}

#[tauri::command]
pub async fn pg_cancel(state: State<'_, AppState>, query_id: String) -> CmdResult<()> {
    match state.running_queries.lock().unwrap().remove(&query_id) {
        Some(tok) => {
            tok.cancel();
            Ok(())
        }
        None => Err(CoreError::NotFound(format!("query {query_id}")).into()),
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
