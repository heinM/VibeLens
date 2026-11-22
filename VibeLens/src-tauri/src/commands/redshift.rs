//! Amazon Redshift commands (Data API). Thin adapters over
//! `vibelens_core::redshift::RedshiftEngine`; the connection manager owns the engines.

use tauri::State;
use tauri::ipc::Channel;
use tokio::sync::mpsc;
use vibelens_core::ErrorPayload;
use vibelens_core::model::*;

use super::{CmdResult, err};
use crate::state::AppState;

#[tauri::command]
pub async fn rs_list_databases(
    state: State<'_, AppState>,
    profile_id: String,
) -> CmdResult<Vec<String>> {
    let e = state.connections.redshift(&profile_id).await.map_err(err)?;
    e.list_databases().await.map_err(err)
}

#[tauri::command]
pub async fn rs_list_schemas(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
) -> CmdResult<Vec<String>> {
    let e = state.connections.redshift(&profile_id).await.map_err(err)?;
    e.list_schemas(database.as_deref()).await.map_err(err)
}

#[tauri::command]
pub async fn rs_list_tables(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
    schema: String,
) -> CmdResult<Vec<RedshiftTableRef>> {
    let e = state.connections.redshift(&profile_id).await.map_err(err)?;
    e.list_tables(database.as_deref(), &schema)
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn rs_describe_table(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
    schema: String,
    name: String,
) -> CmdResult<Vec<RedshiftColumn>> {
    let e = state.connections.redshift(&profile_id).await.map_err(err)?;
    e.describe_table(database.as_deref(), &schema, &name)
        .await
        .map_err(err)
}

/// Whole-database catalog (tables, views, columns) for autocomplete and the palette. Capped at
/// `max_tables` (default 2000).
#[tauri::command]
pub async fn rs_schema_metadata(
    state: State<'_, AppState>,
    profile_id: String,
    database: Option<String>,
    max_tables: Option<usize>,
) -> CmdResult<SchemaMetadata> {
    let e = state.connections.redshift(&profile_id).await.map_err(err)?;
    e.schema_metadata(
        database.as_deref(),
        None,
        max_tables.unwrap_or(vibelens_core::redshift::DEFAULT_MAX_TABLES),
    )
    .await
    .map_err(err)
}

/// Streams results over `on_event` (same contract as `pg_execute` and `ice_execute`). Cancel with
/// `pg_cancel` (the running-query registry is shared), which also cancels the statement on the
/// server. The production write guard is Redshift's own (`redshift::prod_guard_reason`): a
/// script Redshift would read differently from the classifier (a backslash in a string literal)
/// is refused outright in every environment, and a call to a non-built-in function counts as a
/// possible write.
#[tauri::command]
pub async fn rs_execute(
    state: State<'_, AppState>,
    profile_id: String,
    sql: String,
    database: Option<String>,
    options: Option<ExecOptions>,
    confirmed: Option<bool>,
    on_event: Channel<QueryEvent>,
) -> CmdResult<String> {
    let profile = state.profiles.get(&profile_id).map_err(err)?;
    let opts = options.unwrap_or_default();
    let guard = vibelens_core::redshift::prod_guard_reason(&sql, opts.read_only).map_err(err)?;
    if profile.environment == Environment::Prod
        && !confirmed.unwrap_or(false)
        && let Some(reason) = guard
    {
        return Err(ErrorPayload::new(
            "confirm_required",
            format!("Production write guard: {reason}"),
        ));
    }
    let engine = state.connections.redshift(&profile_id).await.map_err(err)?;
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
        if let Err(e) = engine
            .execute_stream(qid, &sql, database.as_deref(), opts, cancel, tx)
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
