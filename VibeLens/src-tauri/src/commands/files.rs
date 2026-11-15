use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;
use vibelens_core::CoreError;

use super::{CmdResult, err};

/// Shows the native "Save" dialog and writes `contents` to the chosen path (exports: CSV, JSON,
/// SQL). The user picks the destination, so no path from the webview is trusted. Returns the
/// path written, or `None` if the user cancelled. Capped at 512 MiB.
#[tauri::command]
pub async fn export_file(
    app: AppHandle,
    suggested_name: String,
    contents: String,
) -> CmdResult<Option<String>> {
    const MAX: usize = 512 * 1024 * 1024;
    if contents.len() > MAX {
        return Err(err(CoreError::Invalid(
            "export is larger than 512 MiB".into(),
        )));
    }
    let name: String = suggested_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_file_name(&name)
            .blocking_save_file()
    })
    .await
    .map_err(|e| err(CoreError::Other(format!("save dialog failed: {e}"))))?;
    let Some(path) = picked else { return Ok(None) };
    let path = path.into_path().map_err(|e| {
        err(CoreError::Invalid(format!(
            "unsupported save location: {e}"
        )))
    })?;
    write_private(&path, contents.into_bytes())
        .await
        .map_err(|e| err(CoreError::Io(e)))?;
    Ok(Some(path.display().to_string()))
}

/// Exports often contain customer data: create them owner-read/write only (0600) on unix.
/// (Windows inherits the destination folder's ACL, normally the user's profile.)
async fn write_private(path: &std::path::Path, bytes: Vec<u8>) -> std::io::Result<()> {
    let path = path.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path)?;
        // `mode` only applies when the file is created; tighten an overwritten file too.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(&bytes)?;
        f.sync_all()
    })
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))?
}
