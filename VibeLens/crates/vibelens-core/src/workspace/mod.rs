//! Sandboxed workspace files (OWNER: connections agent).
//!
//! Both the UI's worksheet editor and the AI agent's `list_files` / `read_file` / `write_file`
//! tools go through this type, so it is a security boundary (docs/DESIGN.md §6).
//!
//! The jail, enforced by [`WorkspaceFs::resolve`]:
//! - paths are **relative** and `/`-separated (`\` is accepted as a separator and normalised);
//!   absolute paths, UNC paths, drive letters / `:` anywhere (also blocks NTFS alternate data
//!   streams), `..` components, Windows device names (`NUL.txt`), NUL / control characters and empty paths are rejected;
//! - no path component may start with `.` (hidden files are never listed, so they are never
//!   reachable either; this also keeps our `.tmp-` files and `.git` out of reach);
//! - the extension must be in [`ALLOWED_EXTENSIONS`] (case-insensitive);
//! - the deepest *existing* ancestor of the target is canonicalised and must still be under the
//!   canonicalised root, which defeats symlink escapes (a symlink inside the workspace pointing
//!   outside it makes everything beneath it unreachable);
//! - files larger than [`MAX_FILE_BYTES`] can be neither read nor written.
//!
//! [`WorkspaceFs::import_files`] copies files the *user* picked (outside the jail) into the
//! workspace root. It applies the same extension list and size cap to the source, accepts only
//! regular files (a symlink, directory, FIFO or device is refused), never overwrites (a clash
//! gets a numeric suffix, and a symlink already sitting at the target name counts as taken, so
//! nothing is ever written through it), and reports a reason for everything it skipped.
//!
//! Known limit: like any path-based check there is a small window between `resolve` and the
//! subsequent open in which another local process could swap a directory for a symlink. The
//! workspace lives in the user's own Documents folder, so that attacker already owns the account.

use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::error::{CoreError, Result};
use crate::model::WorkspaceFile;
use crate::ports::FileVersion;
use crate::profiles::atomic_write;

pub const ALLOWED_EXTENSIONS: &[&str] = &["sql", "valkey", "redis", "md", "txt", "json"];
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Deepest directory nesting `list`/`resolve` will deal with.
const MAX_DEPTH: usize = 16;
/// Longest relative path accepted, in bytes.
const MAX_REL_LEN: usize = 1024;
/// Longest single path component accepted, in bytes (the common filesystem limit).
const MAX_COMPONENT_LEN: usize = 255;
/// `list` stops after this many files so a pathological tree can't stall the UI or flood the model.
const MAX_LISTED_FILES: usize = 10_000;

/// Most files one import call will look at; the rest are reported as skipped.
pub const MAX_IMPORT_FILES: usize = 200;
/// Most name-clash suffixes tried (`name-2.sql` .. `name-N.sql`) before giving up on a file.
const MAX_CLASH_SUFFIX: u32 = 1000;

/// A file an import left out, and why. `name` is the file name only, never a full path.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportSkip {
    pub name: String,
    pub reason: String,
}

/// What an import did: the workspace-relative paths created (in the order picked) and what was skipped.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub imported: Vec<String>,
    pub skipped: Vec<ImportSkip>,
}

/// Path-jailed file access under a single root directory.
pub struct WorkspaceFs {
    root: PathBuf,
}

impl WorkspaceFs {
    /// Creates the root directory if missing.
    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validates and resolves a user/agent-supplied relative path to a real path that is
    /// guaranteed (modulo the race noted in the module docs) to lie under the workspace root.
    /// The file need not exist yet.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let components = validate_relative(rel)?;
        let canonical_root = self.root.canonicalize().map_err(|e| {
            CoreError::Io(std::io::Error::new(
                e.kind(),
                format!("workspace root is not accessible: {e}"),
            ))
        })?;

        let mut full = canonical_root.clone();
        full.extend(&components);

        // Find the deepest ancestor of `full` that exists (without following a final symlink
        // so a dangling link is noticed), canonicalise it, and re-attach the missing tail.
        let mut existing: &Path = &full;
        let mut missing: Vec<&std::ffi::OsStr> = Vec::new();
        let canonical_existing = loop {
            match fs::symlink_metadata(existing) {
                Ok(_) => {
                    break existing.canonicalize().map_err(|e| {
                        CoreError::Invalid(format!(
                            "path cannot be resolved ({e}); dangling symlink?"
                        ))
                    })?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let (Some(parent), Some(name)) = (existing.parent(), existing.file_name())
                    else {
                        return Err(CoreError::Invalid("path escapes the workspace".into()));
                    };
                    missing.push(name);
                    existing = parent;
                }
                Err(e) => return Err(e.into()),
            }
        };

        if !canonical_existing.starts_with(&canonical_root) {
            return Err(CoreError::Invalid("path escapes the workspace".into()));
        }
        let mut resolved = canonical_existing;
        resolved.extend(missing.iter().rev());
        Ok(resolved)
    }

    /// Recursive listing of every allowed, non-hidden file, sorted by path. Paths are relative
    /// to the root and `/`-separated. Symlinks are not followed or listed.
    pub async fn list(&self) -> Result<Vec<WorkspaceFile>> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            walk(&root, "", 0, &mut out)?;
            out.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(out)
        })
        .await
        .map_err(|e| CoreError::Other(format!("workspace task failed: {e}")))?
    }

    /// Reads a UTF-8 file of at most [`MAX_FILE_BYTES`].
    pub async fn read(&self, rel: &str) -> Result<String> {
        let path = self.resolve(rel)?;
        let shown = rel.to_string();
        tokio::task::spawn_blocking(move || read_capped(&path, &shown))
            .await
            .map_err(|e| CoreError::Other(format!("workspace task failed: {e}")))?
    }

    /// Copies the given files (chosen by the user, anywhere on disk) into the workspace root.
    /// Never fails as a whole: each file either lands in `imported` or in `skipped` with a reason.
    /// See the module docs for the rules.
    pub async fn import_files(&self, sources: Vec<PathBuf>) -> ImportOutcome {
        let this = Self {
            root: self.root.clone(),
        };
        match tokio::task::spawn_blocking(move || this.import_files_blocking(&sources)).await {
            Ok(out) => out,
            Err(e) => ImportOutcome {
                imported: Vec::new(),
                skipped: vec![ImportSkip {
                    name: String::new(),
                    reason: format!("workspace task failed: {e}"),
                }],
            },
        }
    }

    fn import_files_blocking(&self, sources: &[PathBuf]) -> ImportOutcome {
        let mut out = ImportOutcome::default();
        for (i, src) in sources.iter().enumerate() {
            let name = src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if i >= MAX_IMPORT_FILES {
                out.skipped.push(ImportSkip {
                    name,
                    reason: format!("at most {MAX_IMPORT_FILES} files can be imported at once"),
                });
                continue;
            }
            match self.import_one(src) {
                Ok(rel) => out.imported.push(rel),
                Err(e) => out.skipped.push(ImportSkip {
                    name,
                    reason: match e {
                        CoreError::Invalid(m) => m,
                        other => other.to_string(),
                    },
                }),
            }
        }
        out
    }

    fn import_one(&self, src: &Path) -> Result<String> {
        use std::io::{Read, Write};

        let invalid = |why: String| CoreError::Invalid(why);
        // The name must be one plain, allowed workspace file name: this is the same check as
        // for any other workspace path, and rules out hidden files, device names and odd forms.
        let name = src
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| invalid("the file name is not valid UTF-8".into()))?;
        if validate_relative(name)?.as_slice() != [name] {
            return Err(invalid("the file name is not a plain file name".into()));
        }

        // Look at the entry itself first (no symlink following), and only open a regular file:
        // opening a FIFO or device could block or read forever.
        let link_meta = fs::symlink_metadata(src).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => invalid("the file no longer exists".into()),
            _ => CoreError::Io(e),
        })?;
        if link_meta.file_type().is_symlink() {
            return Err(invalid(
                "a symbolic link is not imported; pick the file itself".into(),
            ));
        }
        if !link_meta.is_file() {
            return Err(invalid("not a regular file".into()));
        }
        // The entry could be swapped between the check above and this open, so the open itself
        // refuses a symbolic link and never blocks on a FIFO; the checks below see what was opened.
        let mut open = fs::OpenOptions::new();
        open.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = open.open(src)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(invalid("not a regular file".into()));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(invalid(format!(
                "the file is {} bytes; the limit is {MAX_FILE_BYTES}",
                meta.len()
            )));
        }
        // Bounded read in case the file grows between the stat and the read.
        let mut bytes = Vec::with_capacity(meta.len() as usize);
        file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(invalid(format!(
                "the file exceeds the {MAX_FILE_BYTES} byte limit"
            )));
        }
        if std::str::from_utf8(&bytes).is_err() {
            return Err(invalid("the file is not valid UTF-8 text".into()));
        }

        // `create_new` fails if anything (file, directory, dangling or outward-pointing
        // symlink) is already at the name, so an import can neither overwrite nor write
        // through a link; the next numbered name is tried instead.
        let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
        for n in 1..=MAX_CLASH_SUFFIX {
            let candidate = if n == 1 {
                name.to_string()
            } else {
                format!("{stem}-{n}.{ext}")
            };
            // Anything already at the name (a symlink included, dangling or not) means taken.
            if fs::symlink_metadata(self.root.join(&candidate)).is_ok() {
                continue;
            }
            let target = self.resolve(&candidate)?;
            let mut dest = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
            {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            };
            let written = dest.write_all(&bytes).and_then(|()| dest.sync_all());
            drop(dest);
            if let Err(e) = written {
                let _ = fs::remove_file(&target);
                return Err(e.into());
            }
            return Ok(candidate);
        }
        Err(invalid(format!(
            "too many files named like {name} already exist in the workspace"
        )))
    }

    /// Atomic write (tmp + fsync + rename). Creates parent dirs inside the root.
    pub async fn write(&self, rel: &str, contents: &str) -> Result<()> {
        self.write_checked(rel, contents, None).await
    }

    /// Like [`write`](Self::write), but only if the file is still at `expected`: the version
    /// somebody was shown before agreeing to this write. If the file has changed, appeared or
    /// gone since, nothing is written and the error says which.
    ///
    /// The check and the rename are two steps, so a change in the instant between them is
    /// still overwritten. What this closes is the wait before it: the approval has no time
    /// limit, and the file on disk used to be replaced whatever had happened to it meanwhile.
    pub async fn write_if_unchanged(
        &self,
        rel: &str,
        contents: &str,
        expected: &FileVersion,
    ) -> Result<()> {
        self.write_checked(rel, contents, Some(expected.clone()))
            .await
    }

    async fn write_checked(
        &self,
        rel: &str,
        contents: &str,
        expected: Option<FileVersion>,
    ) -> Result<()> {
        if contents.len() as u64 > MAX_FILE_BYTES {
            return Err(CoreError::Invalid(format!(
                "file would be {} bytes; the limit is {MAX_FILE_BYTES}",
                contents.len()
            )));
        }
        let path = self.resolve(rel)?;
        let bytes = contents.as_bytes().to_vec();
        let shown = rel.to_string();
        tokio::task::spawn_blocking(move || {
            if path.is_dir() {
                return Err(CoreError::Invalid("path is a directory".into()));
            }
            if let Some(expected) = expected {
                let now = version_on_disk(&path)?;
                if now != expected {
                    return Err(changed_since_shown(&shown, &expected, &now));
                }
            }
            atomic_write(&path, &bytes, false)
        })
        .await
        .map_err(|e| CoreError::Other(format!("workspace task failed: {e}")))?
    }
}

/// What is at `path` now. Reads at most one byte more than a workspace file may hold: a file
/// that has grown past the limit is a different file whatever the rest of it says.
fn version_on_disk(path: &Path) -> Result<FileVersion> {
    use sha2::Digest as _;
    use std::io::Read;

    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileVersion::Absent),
        Err(e) => return Err(e.into()),
    };
    let mut hasher = sha2::Sha256::new();
    let mut limited = file.take(MAX_FILE_BYTES + 1);
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = limited.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(FileVersion::Sha256(hasher.finalize().into()))
}

/// The refusal of [`WorkspaceFs::write_if_unchanged`], worded for whoever has to act on it
/// (the agent's model reads it and can read the file again).
fn changed_since_shown(shown: &str, expected: &FileVersion, now: &FileVersion) -> CoreError {
    let what = match (expected, now) {
        (FileVersion::Absent, _) => {
            "did not exist when this change was shown for approval, and it exists now"
        }
        (_, FileVersion::Absent) => {
            "has been deleted or moved since this change was shown for approval"
        }
        _ => "has changed since this change was shown for approval",
    };
    CoreError::Other(format!(
        "{shown} {what}. Nothing was written. Read the file again and propose the change against what it holds now."
    ))
}

/// Syntactic checks. Returns the normal components (no `.`/empty segments) on success.
fn validate_relative(rel: &str) -> Result<Vec<String>> {
    let invalid = |why: &str| CoreError::Invalid(format!("invalid workspace path: {why}"));

    if rel.trim().is_empty() {
        return Err(invalid("empty"));
    }
    if rel.len() > MAX_REL_LEN {
        return Err(invalid("too long"));
    }
    if rel.chars().any(char::is_control) {
        return Err(invalid("contains a NUL or control character"));
    }
    let normalised = rel.replace('\\', "/");
    if normalised.starts_with('/') {
        return Err(invalid("must be relative to the workspace"));
    }
    if normalised.contains(':') {
        return Err(invalid("drive letters and ':' are not allowed"));
    }
    // Belt and braces: let std decide what is absolute / prefixed on this OS too.
    let std_path = Path::new(&normalised);
    if std_path.is_absolute() || std_path.has_root() {
        return Err(invalid("must be relative to the workspace"));
    }

    let mut parts: Vec<String> = Vec::new();
    for raw in normalised.split('/') {
        match raw {
            "" | "." => continue,
            ".." => return Err(invalid("'..' is not allowed")),
            seg if seg.starts_with('.') => {
                return Err(invalid("hidden files and folders are not allowed"));
            }
            seg if seg.len() > MAX_COMPONENT_LEN => {
                return Err(invalid("a path segment is too long"));
            }
            seg if seg.ends_with(' ') || seg.ends_with('.') => {
                // Windows silently strips these, which could alias another name.
                return Err(invalid("segments must not end with a space or '.'"));
            }
            seg if is_windows_device_name(seg) => {
                // `NUL.txt` opens the NUL device on Windows; reject everywhere for consistency.
                return Err(invalid("reserved device name"));
            }
            seg => parts.push(seg.to_string()),
        }
    }
    if normalised.ends_with('/') {
        return Err(invalid("must name a file, not a directory"));
    }
    if parts.is_empty() {
        return Err(invalid("empty"));
    }
    if parts.len() > MAX_DEPTH {
        return Err(invalid("nested too deeply"));
    }
    // Every component must be a plain name to std too (catches platform-specific prefixes).
    if Path::new(&parts.join("/"))
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(invalid("unsupported path form"));
    }

    let file = parts.last().map(String::as_str).unwrap_or_default();
    let ext = file.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match ext {
        Some(ext) if ALLOWED_EXTENSIONS.contains(&ext.as_str()) => Ok(parts),
        _ => Err(CoreError::Invalid(format!(
            "file type not allowed; use one of: {}",
            ALLOWED_EXTENSIONS
                .iter()
                .map(|e| format!(".{e}"))
                .collect::<Vec<_>>()
                .join(" ")
        ))),
    }
}

/// Windows reserved device names: `CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`,
/// `COM0-9`/`LPT0-9` **and their superscript-digit forms** (`COM¹`, `LPT²`, `COM³`), in any case,
/// with or without an extension, and with trailing spaces before the dot (Windows strips them).
fn is_windows_device_name(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .trim_end_matches(' ')
        .to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) {
        return true;
    }
    let mut chars = stem.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    if prefix != "COM" && prefix != "LPT" {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    matches!(rest.as_slice(), [c] if c.is_ascii_digit() || matches!(c, '¹' | '²' | '³'))
}

fn read_capped(path: &Path, shown: &str) -> Result<String> {
    use std::io::Read;

    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CoreError::NotFound(format!("file {shown}")));
        }
        Err(e) => return Err(e.into()),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(CoreError::Invalid(format!("{shown} is not a regular file")));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(CoreError::Invalid(format!(
            "{shown} is {} bytes; the limit is {MAX_FILE_BYTES}",
            meta.len()
        )));
    }
    // Bounded read in case the file grows between the stat and the read.
    let mut buf = Vec::with_capacity(meta.len() as usize);
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_FILE_BYTES {
        return Err(CoreError::Invalid(format!(
            "{shown} exceeds the {MAX_FILE_BYTES} byte limit"
        )));
    }
    String::from_utf8(buf)
        .map_err(|_| CoreError::Invalid(format!("{shown} is not valid UTF-8 text")))
}

fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<WorkspaceFile>) -> Result<()> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        // A directory that vanished or is unreadable mid-walk shouldn't fail the whole listing.
        Err(e)
            if depth > 0
                && matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
        {
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        if out.len() >= MAX_LISTED_FILES {
            tracing::warn!("workspace listing truncated at {MAX_LISTED_FILES} files");
            return Ok(());
        }
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.to_string()
        } else {
            format!("{prefix}/{name}")
        };
        if file_type.is_dir() {
            walk(&entry.path(), &rel, depth + 1, out)?;
        } else if file_type.is_file() {
            let allowed = name
                .rsplit_once('.')
                .map(|(_, e)| ALLOWED_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false);
            if !allowed {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            out.push(WorkspaceFile {
                path: rel,
                size: meta.len(),
                modified: meta.modified().ok().map(DateTime::<Utc>::from),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod device_name_tests {
    use super::is_windows_device_name as dev;

    #[test]
    fn conn_reserved_device_names() {
        for n in [
            "con",
            "CON.sql",
            "nul.txt",
            "Aux",
            "prn.md",
            "COM1.sql",
            "lpt9",
            "COM0",
            "COM¹.sql",
            "lpt².txt",
            "COM³",
            "CONIN$",
            "conout$.txt",
            "NUL .sql",
            "com1 .sql",
        ] {
            assert!(dev(n), "{n} should be reserved");
        }
        for n in [
            "console.sql",
            "COM10.sql",
            "LPT.sql",
            "compose.sql",
            "nullable.sql",
            "COM¹0.sql",
            "lptx.sql",
            "CONFIG.json",
        ] {
            assert!(!dev(n), "{n} should be allowed");
        }
    }
}
