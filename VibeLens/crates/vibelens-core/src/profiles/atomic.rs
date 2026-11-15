//! Crash-safe file replacement shared by the profile store and the workspace.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::error::{CoreError, Result};

/// Writes `bytes` to `path` atomically: a temp file in the same directory is written and
/// fsynced, then renamed over the target (and the directory is fsynced on unix). Readers see
/// either the old or the new content, never a torn file. Creates missing parent directories.
///
/// With `private`, the temp file (and therefore the result) is created `0600` on unix.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            CoreError::Invalid(format!("path has no parent directory: {}", path.display()))
        })?;
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| CoreError::Invalid(format!("path has no file name: {}", path.display())))?
        .to_string_lossy();
    let tmp = parent.join(format!(
        ".{file_name}.tmp-{}",
        uuid::Uuid::new_v4().simple()
    ));

    let result = write_tmp_then_rename(&tmp, path, bytes, private);
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;

    // Persist the rename itself. Not possible/needed on Windows.
    #[cfg(unix)]
    if let Ok(dir) = File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn write_tmp_then_rename(tmp: &Path, dest: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut file: File = opts.open(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(tmp, dest)?;
    Ok(())
}
