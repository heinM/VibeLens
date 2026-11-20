use tauri::State;
use vibelens_core::model::*;

use vibelens_core::ErrorPayload;

use super::{CmdResult, err};
use crate::state::AppState;

#[tauri::command]
pub async fn vk_server_info(
    state: State<'_, AppState>,
    profile_id: String,
) -> CmdResult<ValkeyServerInfo> {
    let h = state.connections.valkey(&profile_id).await.map_err(err)?;
    h.server_info().await.map_err(err)
}

#[tauri::command]
pub async fn vk_list_dbs(
    state: State<'_, AppState>,
    profile_id: String,
) -> CmdResult<Vec<ValkeyDbInfo>> {
    let h = state.connections.valkey(&profile_id).await.map_err(err)?;
    h.list_dbs().await.map_err(err)
}

#[tauri::command]
pub async fn vk_scan(
    state: State<'_, AppState>,
    profile_id: String,
    db: u8,
    pattern: Option<String>,
    cursor: Option<String>,
    count: Option<u32>,
) -> CmdResult<ScanPage> {
    let profile = state.profiles.get(&profile_id).map_err(err)?;
    let h = state.connections.valkey(&profile_id).await.map_err(err)?;
    h.scan(
        db,
        pattern.as_deref().unwrap_or("*"),
        cursor.as_deref().unwrap_or("0"),
        count.unwrap_or(profile.valkey.scan_count),
    )
    .await
    .map_err(err)
}

#[tauri::command]
pub async fn vk_get_value(
    state: State<'_, AppState>,
    profile_id: String,
    db: u8,
    key: String,
    max_items: Option<usize>,
) -> CmdResult<ValkeyValue> {
    let h = state.connections.valkey(&profile_id).await.map_err(err)?;
    h.get_value(db, &key, max_items.unwrap_or(1000))
        .await
        .map_err(err)
}

#[tauri::command]
pub async fn vk_command(
    state: State<'_, AppState>,
    profile_id: String,
    db: u8,
    argv: Vec<String>,
    allow_write: Option<bool>,
    confirmed: Option<bool>,
) -> CmdResult<CommandOutcome> {
    let profile = state.profiles.get(&profile_id).map_err(err)?;
    if profile.environment == Environment::Prod
        && allow_write.unwrap_or(false)
        && !confirmed.unwrap_or(false)
    {
        return Err(ErrorPayload::new(
            "confirm_required",
            "Production write guard: confirm this write",
        ));
    }
    let h = state.connections.valkey(&profile_id).await.map_err(err)?;
    h.command(db, argv, allow_write.unwrap_or(false))
        .await
        .map_err(err)
}
