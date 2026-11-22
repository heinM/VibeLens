use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;
use vibelens_core::CoreError;
use vibelens_core::model::WorkspaceFile;
use vibelens_core::workspace::{ALLOWED_EXTENSIONS, ImportOutcome, ImportSkip};

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

/// Native multi-file picker (filtered to the workspace's file types), then copies each chosen
/// file into the workspace. The user picks the files here in Rust: the webview sends no path.
/// Core decides what is accepted, never overwrites (a clash gets a numeric suffix) and reports
/// a reason per skipped file. `None` = the dialog was cancelled.
#[tauri::command]
pub async fn ws_import_files(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Option<ImportOutcome>> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Workspace files", ALLOWED_EXTENSIONS)
            .blocking_pick_files()
    })
    .await
    .map_err(|e| err(CoreError::Other(format!("open dialog failed: {e}"))))?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let mut paths = Vec::new();
    let mut unsupported = Vec::new();
    for p in picked {
        match p.into_path() {
            Ok(path) => paths.push(path),
            Err(e) => unsupported.push(ImportSkip {
                name: String::new(),
                reason: format!("unsupported file location: {e}"),
            }),
        }
    }
    let mut outcome = state.workspace.import_files(paths).await;
    outcome.skipped.extend(unsupported);
    Ok(Some(outcome))
}

/// Shows the workspace folder in the system file manager. The path is the app's own workspace
/// root; nothing from the webview is used, so the webview needs no opener or fs permission.
#[tauri::command]
pub fn ws_reveal(app: AppHandle, state: State<'_, AppState>) -> CmdResult<()> {
    let root = state.workspace.root().to_string_lossy().into_owned();
    app.opener().open_path(root, None::<&str>).map_err(|e| {
        err(CoreError::Other(format!(
            "cannot open the workspace folder: {e}"
        )))
    })
}
