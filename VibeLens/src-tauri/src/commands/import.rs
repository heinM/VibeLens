//! Importing connections from DataGrip, DBeaver, Azure Data Studio, pgAdmin and pg_service.conf.
//! Parsing lives in `vibelens_core::import`; these commands find the files, mark what already
//! exists, and save the profiles the user ticked. No secret is ever imported or returned.

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;
use vibelens_core::CoreError;
use vibelens_core::import::{self, ImportReport, ImportSource};
use vibelens_core::model::ConnectionProfile;

use super::{CmdResult, err};
use crate::state::AppState;

fn home() -> CmdResult<std::path::PathBuf> {
    dirs::home_dir().ok_or_else(|| err(CoreError::Other("cannot find the home folder".into())))
}

fn with_duplicates(
    state: &AppState,
    mut reports: Vec<ImportReport>,
) -> CmdResult<Vec<ImportReport>> {
    let existing = state.profiles.list().map_err(err)?;
    import::mark_duplicates(&mut reports, &existing);
    Ok(reports)
}

/// Looks in the usual places of every supported tool and returns what it found.
#[tauri::command]
pub async fn import_scan(state: State<'_, AppState>) -> CmdResult<Vec<ImportReport>> {
    let home = home()?;
    let reports = tauri::async_runtime::spawn_blocking(move || import::scan(&home))
        .await
        .map_err(|e| err(CoreError::Other(format!("scan failed: {e}"))))?;
    with_duplicates(&state, reports)
}

/// Native "Open" dialog, then parses the chosen file (format detected from name and content).
/// The user picks the file, so no path from the webview is trusted. `None` = cancelled.
#[tauri::command]
pub async fn import_choose_file(
    app: AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<Option<ImportReport>> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Connection files", &["xml", "json", "conf", "txt"])
            .add_filter("All files", &["*"])
            .blocking_pick_file()
    })
    .await
    .map_err(|e| err(CoreError::Other(format!("open dialog failed: {e}"))))?;
    let Some(path) = picked else { return Ok(None) };
    let path = path.into_path().map_err(|e| {
        err(CoreError::Invalid(format!(
            "unsupported file location: {e}"
        )))
    })?;
    let home = dirs::home_dir();
    let report = tauri::async_runtime::spawn_blocking(move || {
        import::parse_file(None, &path, home.as_deref())
    })
    .await
    .map_err(|e| err(CoreError::Other(format!("import failed: {e}"))))?;
    Ok(with_duplicates(&state, vec![report])?.pop())
}

/// Parses pasted text: DataGrip's "Copy" XML, or the contents of any supported file.
#[tauri::command]
pub async fn import_text(state: State<'_, AppState>, text: String) -> CmdResult<ImportReport> {
    if text.len() > 8 * 1024 * 1024 {
        return Err(err(CoreError::Invalid("pasted text is too large".into())));
    }
    let source = import::detect("", &text).ok_or_else(|| {
        err(CoreError::Invalid(
            "the pasted text is not something VibeLens can import (DataGrip data source XML, DBeaver, Azure Data Studio, pgAdmin or pg_service.conf content)".into(),
        ))
    })?;
    let report = import::parse_text(source, &text, "pasted text", &import::Extra::default());
    Ok(with_duplicates(&state, vec![report])?
        .pop()
        .expect("one report"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSelection {
    pub profile: ConnectionProfile,
    pub source: Option<ImportSource>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFailure {
    pub name: String,
    pub error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSaveResult {
    pub saved: Vec<ConnectionProfile>,
    pub failed: Vec<ImportFailure>,
}

/// Saves the chosen candidates as new profiles. Names that clash with an existing profile get
/// the source appended (`orders (DBeaver)`), then a number. Secrets are never written: the user
/// adds passwords in the connection editor.
#[tauri::command]
pub async fn import_save(
    state: State<'_, AppState>,
    selections: Vec<ImportSelection>,
) -> CmdResult<ImportSaveResult> {
    let mut taken: Vec<String> = state
        .profiles
        .list()
        .map_err(err)?
        .into_iter()
        .map(|p| p.name)
        .collect();
    let mut saved = Vec::new();
    let mut failed = Vec::new();
    for sel in selections {
        let mut profile = sel.profile;
        // Always a new profile: never overwrite an existing one through a reused id.
        profile.id = String::new();
        profile.created_at = None;
        profile.updated_at = None;
        if let Some(vibelens_core::model::TunnelConfig::Ssh {
            known_host_fingerprint,
            ..
        }) = profile.tunnel.as_mut()
        {
            *known_host_fingerprint = None;
        }
        profile.name = import::unique_name(&profile.name, sel.source, &taken);
        let name = profile.name.clone();
        match state.profiles.upsert(profile) {
            Ok(p) => {
                taken.push(p.name.clone());
                saved.push(p);
            }
            Err(e) => failed.push(ImportFailure {
                name,
                error: e.to_string(),
            }),
        }
    }
    Ok(ImportSaveResult { saved, failed })
}
