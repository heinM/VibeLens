//! Apache Iceberg commands. Thin adapters over `vibelens_core::iceberg::IcebergHandle`.

use tauri::State;
use tauri::ipc::Channel;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use vibelens_core::ErrorPayload;
use vibelens_core::model::*;

use super::{CmdResult, err};
use crate::state::AppState;

#[tauri::command]
pub async fn ice_list_namespaces(
    state: State<'_, AppState>,
    profile_id: String,
    parent: Option<Vec<String>>,
) -> CmdResult<Vec<IcebergNamespace>> {
    let h = state.connections.iceberg(&profile_id).await.map_err(err)?;
    h.list_namespaces(parent).await.map_err(err)
}

#[tauri::command]
pub async fn ice_list_tables(
    state: State<'_, AppState>,
    profile_id: String,
    namespace: Vec<String>,
) -> CmdResult<Vec<IcebergTableRef>> {
    let h = state.connections.iceberg(&profile_id).await.map_err(err)?;
    h.list_tables(namespace).await.map_err(err)
}

#[tauri::command]
pub async fn ice_describe_table(
    state: State<'_, AppState>,
    profile_id: String,
    namespace: Vec<String>,
    name: String,
) -> CmdResult<IcebergTableInfo> {
    let h = state.connections.iceberg(&profile_id).await.map_err(err)?;
    h.describe_table(namespace, &name).await.map_err(err)
}

#[tauri::command]
pub async fn ice_schema_metadata(
    state: State<'_, AppState>,
    profile_id: String,
    namespace: Option<Vec<String>>,
    max_tables: Option<usize>,
) -> CmdResult<SchemaMetadata> {
    let h = state.connections.iceberg(&profile_id).await.map_err(err)?;
    h.schema_metadata(namespace, max_tables.unwrap_or(2000))
        .await
        .map_err(err)
}

/// Streams results over `on_event` (same contract as `pg_execute`). Cancel with `pg_cancel`
/// (the running-query registry is shared). Prod write-guard identical to Postgres.
#[tauri::command]
pub async fn ice_execute(
    state: State<'_, AppState>,
    profile_id: String,
    sql: String,
    options: Option<ExecOptions>,
    confirmed: Option<bool>,
    on_event: Channel<QueryEvent>,
) -> CmdResult<String> {
    let profile = state.profiles.get(&profile_id).map_err(err)?;
    let opts = options.unwrap_or_default();
    if profile.environment == Environment::Prod
        && !confirmed.unwrap_or(false)
        && let Some(reason) = super::pg::prod_guard_reason(&sql, opts.read_only)
    {
        return Err(ErrorPayload {
            code: "confirm_required".into(),
            message: format!("Production write guard: {reason}"),
        });
    }
    let handle = state.connections.iceberg(&profile_id).await.map_err(err)?;
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

/// Expiring snapshots permanently removes time-travel history, so it always needs `confirmed`.
#[tauri::command]
pub async fn ice_expire_snapshots(
    state: State<'_, AppState>,
    profile_id: String,
    namespace: Vec<String>,
    name: String,
    older_than_ms: Option<i64>,
    retain_last: Option<u32>,
    confirmed: Option<bool>,
) -> CmdResult<ExpireSnapshotsResult> {
    if !confirmed.unwrap_or(false) {
        return Err(ErrorPayload {
            code: "confirm_required".into(),
            message: format!(
                "Expiring snapshots of {name} permanently removes time-travel history."
            ),
        });
    }
    let h = state.connections.iceberg(&profile_id).await.map_err(err)?;
    h.expire_snapshots(namespace, &name, older_than_ms, retain_last)
        .await
        .map_err(err)
}
