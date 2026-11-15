use tauri::State;
use vibelens_core::model::WorkspaceFile;

use super::{CmdResult, err};
use crate::state::AppState;

#[tauri::command]
pub async fn ws_list(state: State<'_, AppState>) -> CmdResult<Vec<WorkspaceFile>> {
    state.workspace.list().await.map_err(err)
}

#[tauri::command]
pub async fn ws_read(state: State<'_, AppState>, path: String) -> CmdResult<String> {
    state.workspace.read(&path).await.map_err(err)
}

#[tauri::command]
pub async fn ws_write(state: State<'_, AppState>, path: String, contents: String) -> CmdResult<()> {
    state.workspace.write(&path, &contents).await.map_err(err)
}
