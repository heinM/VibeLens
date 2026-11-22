//! MySQL commands (`my_*`). Thin adapters over `vibelens_core::mysql::MysqlHandle`; the
//! connection manager owns one pool per (profile, database). Cancel a run with `pg_cancel` (the
//! running-query registry is shared), which stops the statement on the server (`KILL QUERY`).

use tauri::State;
use tauri::ipc::Channel;
use tokio::sync::mpsc;
use vibelens_core::ErrorPayload;
use vibelens_core::model::*;

use super::{CmdResult, err};
use crate::state::AppState;

/// Databases the connection's user can see.
#[tauri::command]
pub async fn my_list_databases(
    state: State<'_, AppState>,
    profile_id: String,
) -> CmdResult<Vec<String>> {
    let h = state
        .connections
        .mysql(&profile_id, None)
        .await
        .map_err(err)?;
    h.list_databases().await.map_err(err)
}

/// Tables, views, functions and procedures of one database (`schema` of each object is the
/// database).
#[tauri::command]
pub async fn my_list_objects(
    state: State<'_, AppState>,
    profile_id: String,
    database: String,
) -> CmdResult<Vec<SchemaObject>> {
    let h = state
        .connections
        .mysql(&profile_id, None)
        .await
        .map_err(err)?;
    h.list_objects(&database).await.map_err(err)
}

/// Columns, indexes and foreign keys of one table or view.
#[tauri::command]
pub async fn my_describe_table(
    state: State<'_, AppState>,
    profile_id: String,
    database: String,
    table: String,
) -> CmdResult<TableDescription> {
    let h = state
        .connections
        .mysql(&profile_id, None)
        .await
        .map_err(err)?;
    h.describe_table(&database, &table).await.map_err(err)
}

/// Whole-database catalog (tables, views, columns, keys) for autocomplete and the palette.
/// Capped at `max_tables` (default 2000).
#[tauri::command]
pub async fn my_schema_metadata(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
    max_tables: Option<usize>,
) -> CmdResult<SchemaMetadata> {
    let h = state
        .connections
        .mysql(&profile_id, database.as_deref())
        .await
        .map_err(err)?;
    h.schema_metadata(
        database.as_deref(),
        max_tables.unwrap_or(vibelens_core::mysql::DEFAULT_MAX_TABLES),
    )
    .await
    .map_err(err)
}

/// Streams results over `on_event` (same contract as `pg_execute`). Returns the query id at
/// once; cancel with `pg_cancel`. The production write guard uses MySQL's own classifier
/// (`mysql::prod_guard_reason`).
#[tauri::command]
pub async fn my_execute(
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
        && let Some(reason) = vibelens_core::mysql::prod_guard_reason(&sql, opts.read_only)
    {
        return Err(ErrorPayload::new(
            "confirm_required",
            format!("Production write guard: {reason}"),
        ));
    }
    let handle = state
        .connections
        .mysql(&profile_id, database.as_deref())
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
        if let Err(e) = handle.execute_stream(qid, &sql, opts, cancel, tx).await {
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
