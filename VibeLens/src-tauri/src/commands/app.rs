use serde::Serialize;
use tauri::State;

use super::CmdResult;
use crate::state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub version: String,
    pub config_dir: String,
    pub workspace_dir: String,
    /// "keychain" | "memory" (memory = secrets not persisted; shown as a warning banner)
    pub vault_backend: String,
}

#[tauri::command]
pub async fn app_info(state: State<'_, AppState>) -> CmdResult<AppInfo> {
    Ok(AppInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        config_dir: state.config_dir.display().to_string(),
        workspace_dir: state.workspace.root().display().to_string(),
        vault_backend: state.vault_backend.to_string(),
    })
}

/// Classifies `sql` with the rules of `engine` (MySQL has its own lexer and classifier; every
/// other engine, and no engine, uses the PostgreSQL one).
#[tauri::command]
pub fn sql_classify(
    sql: String,
    engine: Option<vibelens_core::model::EngineKind>,
) -> vibelens_core::sql::SqlClassification {
    match engine {
        Some(vibelens_core::model::EngineKind::Mysql) => vibelens_core::mysql::classify(&sql),
        _ => vibelens_core::sql::classify(&sql),
    }
}
