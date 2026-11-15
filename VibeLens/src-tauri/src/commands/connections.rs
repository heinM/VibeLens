use tauri::State;
use vibelens_core::model::ConnectionStatus;

use super::{CmdResult, err};
use crate::state::AppState;

#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    profile_id: String,
) -> CmdResult<ConnectionStatus> {
    state.connections.connect(&profile_id).await.map_err(err)
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>, profile_id: String) -> CmdResult<()> {
    state.connections.disconnect(&profile_id).await.map_err(err)
}

#[tauri::command]
pub async fn connection_status(state: State<'_, AppState>) -> CmdResult<Vec<ConnectionStatus>> {
    Ok(state.connections.status())
}
