//! WorkspaceFs: the path jail, size cap, atomic writes and listing.

mod services;

use std::fs;

use tempfile::tempdir;
use vibelens_core::error::CoreError;
use vibelens_core::ports::FileVersion;
use vibelens_core::workspace::{ALLOWED_EXTENSIONS, MAX_FILE_BYTES, WorkspaceFs};

fn ws() -> (tempfile::TempDir, WorkspaceFs) {
    let dir = tempdir().unwrap();
    let root = dir.path().join("ws");
    let fs = WorkspaceFs::new(root).unwrap();
    (dir, fs)
}

fn is_invalid<T: std::fmt::Debug>(r: Result<T, CoreError>) -> bool {
    matches!(r, Err(CoreError::Invalid(_)))
}

#[test]
fn conn_new_creates_the_root() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("deep").join("ws");
    let w = WorkspaceFs::new(root.clone()).unwrap();
    assert!(root.is_dir());
    assert_eq!(w.root(), root);
}

#[tokio::test]
async fn conn_write_read_round_trip_creates_parent_dirs() {
    let (_d, w) = ws();
    w.write("reports/2026/q1.sql", "SELECT 1;\n").await.unwrap();
    assert_eq!(w.read("reports/2026/q1.sql").await.unwrap(), "SELECT 1;\n");
    assert!(w.root().join("reports/2026/q1.sql").is_file());
    // Overwrite.
    w.write("reports/2026/q1.sql", "SELECT 2;").await.unwrap();
    assert_eq!(w.read("reports/2026/q1.sql").await.unwrap(), "SELECT 2;");
}

#[tokio::test]
async fn conn_read_missing_is_not_found() {
    let (_d, w) = ws();
    assert!(matches!(
        w.read("nope.sql").await,
        Err(CoreError::NotFound(_))
    ));
    assert!(matches!(
        w.read("no/such/dir/x.sql").await,
        Err(CoreError::NotFound(_))
    ));
}

#[test]
fn conn_rejects_traversal() {
    let (_d, w) = ws();
    for bad in [
        "../x.sql",
        "..",
        "a/../../x.sql",
        "a/../b.sql",
        "./../x.sql",
        "..\\x.sql",
        "a\\..\\..\\x.sql",
        "sub/..",
        "%2e%2e/x.sql".replace("%2e%2e", "..").as_str(),
    ] {
        assert!(is_invalid(w.resolve(bad)), "should reject {bad:?}");
    }
}

#[test]
fn conn_rejects_absolute_and_drive_paths() {
    let (_d, w) = ws();
    for bad in [
        "/etc/passwd.txt",
        "/x.sql",
        "\\x.sql",
        "\\\\server\\share\\x.sql",
        "C:\\x.sql",
        "C:/x.sql",
        "c:x.sql",
        "dir/C:/x.sql",
        "file.sql:stream",
    ] {
        assert!(is_invalid(w.resolve(bad)), "should reject {bad:?}");
    }
}

#[test]
fn conn_rejects_nul_empty_hidden_and_odd_names() {
    let (_d, w) = ws();
    for bad in [
        "",
        "   ",
        "/",
        "./",
        ".",
        "a\0.sql",
        "\0",
        "a\n.sql",
        ".hidden.sql",
        "dir/.hidden.sql",
        ".git/config.json",
        "dir/",
        "dir//",
        "trailing.dot.",
        "trailing.sql ",
        "NUL.txt",
        "dir/con.sql",
        "Aux",
        "com1.md",
        "LPT9.json",
    ] {
        assert!(is_invalid(w.resolve(bad)), "should reject {bad:?}");
    }
    let long = format!("{}.sql", "a".repeat(300));
    assert!(is_invalid(w.resolve(&long)));
    let deep = format!("{}x.sql", "d/".repeat(40));
    assert!(is_invalid(w.resolve(&deep)));
}

#[test]
fn conn_device_name_lookalikes_are_fine() {
    let (_d, w) = ws();
    for ok in [
        "console.sql",
        "nullable.txt",
        "com10.sql",
        "lpt.md",
        "aux_notes.md",
        "dir.con/a.sql",
    ] {
        assert!(w.resolve(ok).is_ok(), "{ok} should be allowed");
    }
    // Microsoft lists COM0/LPT0 and the superscript forms as reserved too.
    for bad in ["com0.sql", "LPT0.md", "COM\u{00B9}.sql", "lpt\u{00B2}.txt"] {
        assert!(w.resolve(bad).is_err(), "{bad} should be rejected");
    }
}

#[test]
fn conn_extension_allowlist() {
    let (_d, w) = ws();
    for ext in ALLOWED_EXTENSIONS {
        assert!(
            w.resolve(&format!("file.{ext}")).is_ok(),
            ".{ext} should be allowed"
        );
        assert!(
            w.resolve(&format!("file.{}", ext.to_uppercase())).is_ok(),
            ".{ext} is case-insensitive"
        );
    }
    for bad in [
        "file.exe",
        "file.sh",
        "file",
        "file.",
        "sql",
        "file.sql.exe",
        "file.sqlx",
        "archive.tar.gz",
        "x.json.bak",
    ] {
        assert!(is_invalid(w.resolve(bad)), "should reject {bad:?}");
    }
}

#[test]
fn conn_resolve_stays_under_root_and_normalises() {
    let (_d, w) = ws();
    let canonical_root = w.root().canonicalize().unwrap();
    for ok in [
        "a.sql",
        "dir/a.sql",
        "./dir//a.sql",
        "dir\\a.sql",
        "deep/er/est/a.md",
    ] {
        let p = w.resolve(ok).unwrap();
        assert!(p.starts_with(&canonical_root), "{ok} -> {p:?}");
    }
    assert_eq!(
        w.resolve("dir\\a.sql").unwrap(),
        w.resolve("dir/a.sql").unwrap()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn conn_symlinked_directory_escape_is_blocked() {
    use std::os::unix::fs::symlink;
    let (d, w) = ws();
    let outside = d.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "top secret").unwrap();
    symlink(&outside, w.root().join("link")).unwrap();

    // Read through the link.
    assert!(is_invalid(w.resolve("link/secret.txt")));
    assert!(is_invalid(w.read("link/secret.txt").await));
    // Write through the link (existing target file, and a new one).
    assert!(is_invalid(w.write("link/secret.txt", "pwned").await));
    assert!(is_invalid(w.write("link/new.txt", "pwned").await));
    // Creating nested dirs below the link must not create anything outside either.
    assert!(is_invalid(w.write("link/sub/new.txt", "pwned").await));

    assert_eq!(
        fs::read_to_string(outside.join("secret.txt")).unwrap(),
        "top secret"
    );
    assert!(!outside.join("new.txt").exists());
    assert!(!outside.join("sub").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn conn_symlinked_file_escape_is_blocked() {
    use std::os::unix::fs::symlink;
    let (d, w) = ws();
    let secret = d.path().join("secret.txt");
    fs::write(&secret, "top secret").unwrap();
    symlink(&secret, w.root().join("leak.txt")).unwrap();

    assert!(is_invalid(w.read("leak.txt").await));
    assert!(is_invalid(w.write("leak.txt", "pwned").await));
    assert_eq!(fs::read_to_string(&secret).unwrap(), "top secret");
}

#[cfg(unix)]
#[tokio::test]
async fn conn_dangling_symlink_is_rejected() {
    use std::os::unix::fs::symlink;
    let (d, w) = ws();
    symlink(
        d.path().join("does-not-exist.txt"),
        w.root().join("dangling.txt"),
    )
    .unwrap();
    assert!(is_invalid(w.write("dangling.txt", "x").await));
    assert!(!d.path().join("does-not-exist.txt").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn conn_symlink_that_stays_inside_the_root_is_allowed() {
    use std::os::unix::fs::symlink;
    let (_d, w) = ws();
    w.write("real/a.sql", "SELECT 1").await.unwrap();
    symlink(w.root().join("real"), w.root().join("alias")).unwrap();
    assert_eq!(w.read("alias/a.sql").await.unwrap(), "SELECT 1");
}

#[cfg(unix)]
#[tokio::test]
async fn conn_symlinked_root_still_works() {
    use std::os::unix::fs::symlink;
    let dir = tempdir().unwrap();
    let real = dir.path().join("real_root");
    fs::create_dir_all(&real).unwrap();
    let linked = dir.path().join("linked_root");
    symlink(&real, &linked).unwrap();
    let w = WorkspaceFs::new(linked).unwrap();
    w.write("a.sql", "SELECT 1").await.unwrap();
    assert_eq!(w.read("a.sql").await.unwrap(), "SELECT 1");
    assert!(real.join("a.sql").is_file());
}

#[tokio::test]
async fn conn_size_cap_on_write_and_read() {
    let (_d, w) = ws();
    let exactly = "a".repeat(MAX_FILE_BYTES as usize);
    w.write("max.txt", &exactly).await.unwrap();
    assert_eq!(
        w.read("max.txt").await.unwrap().len(),
        MAX_FILE_BYTES as usize
    );

    let too_big = "a".repeat(MAX_FILE_BYTES as usize + 1);
    assert!(is_invalid(w.write("big.txt", &too_big).await));
    assert!(!w.root().join("big.txt").exists(), "nothing written");

    // A file that got big behind our back is refused on read.
    fs::write(w.root().join("sneaky.txt"), &too_big).unwrap();
    assert!(is_invalid(w.read("sneaky.txt").await));
}

#[tokio::test]
async fn conn_non_utf8_is_a_clean_error() {
    let (_d, w) = ws();
    fs::write(w.root().join("bin.txt"), [0xff, 0xfe, 0x00, 0x80]).unwrap();
    assert!(is_invalid(w.read("bin.txt").await));
}

#[tokio::test]
async fn conn_reading_a_directory_is_an_error() {
    let (_d, w) = ws();
    fs::create_dir_all(w.root().join("folder.sql")).unwrap();
    assert!(w.read("folder.sql").await.is_err());
    assert!(is_invalid(w.write("folder.sql", "x").await));
}

#[tokio::test]
async fn conn_write_is_atomic_and_leaves_no_temp_files() {
    let (_d, w) = ws();
    for i in 0..10 {
        w.write("a.sql", &format!("SELECT {i}")).await.unwrap();
    }
    let names: Vec<_> = fs::read_dir(w.root())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a.sql".to_string()]);
}

#[tokio::test]
async fn conn_list_is_recursive_sorted_filtered_and_slash_separated() {
    let (_d, w) = ws();
    for (p, c) in [
        ("zeta.sql", "z"),
        ("alpha.md", "aa"),
        ("dir/b.json", "bbb"),
        ("dir/a.redis", "a"),
        ("dir/sub/deep.valkey", "d"),
        ("dir/sub/Notes.TXT", "n"),
    ] {
        w.write(p, c).await.unwrap();
    }
    // Things that must not show up.
    fs::write(w.root().join("ignored.exe"), "x").unwrap();
    fs::write(w.root().join("noext"), "x").unwrap();
    fs::write(w.root().join(".hidden.sql"), "x").unwrap();
    fs::create_dir_all(w.root().join(".git")).unwrap();
    fs::write(w.root().join(".git/config.json"), "x").unwrap();
    fs::create_dir_all(w.root().join("empty_dir")).unwrap();

    let files = w.list().await.unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "alpha.md",
            "dir/a.redis",
            "dir/b.json",
            "dir/sub/Notes.TXT",
            "dir/sub/deep.valkey",
            "zeta.sql"
        ]
    );

    let alpha = &files[0];
    assert_eq!(alpha.size, 2);
    assert!(alpha.modified.is_some());
    assert!(files.iter().all(|f| !f.path.contains('\\')));
    // Everything listed can be read back through the jail.
    for f in &files {
        w.read(&f.path).await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn conn_list_does_not_follow_or_show_symlinks() {
    use std::os::unix::fs::symlink;
    let (d, w) = ws();
    let outside = d.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "s").unwrap();
    symlink(&outside, w.root().join("linkdir")).unwrap();
    symlink(outside.join("secret.txt"), w.root().join("linkfile.txt")).unwrap();
    w.write("real.sql", "x").await.unwrap();
    let paths: Vec<_> = w
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(paths, ["real.sql"]);
}

#[tokio::test]
async fn conn_list_on_empty_workspace() {
    let (_d, w) = ws();
    assert!(w.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn conn_non_ascii_names_work() {
    let (_d, w) = ws();
    w.write("rapporte/übersicht-日本語.sql", "SELECT 'é'")
        .await
        .unwrap();
    assert_eq!(
        w.read("rapporte/übersicht-日本語.sql").await.unwrap(),
        "SELECT 'é'"
    );
    assert_eq!(
        w.list().await.unwrap()[0].path,
        "rapporte/übersicht-日本語.sql"
    );
}

// ---------------------------------------------------------------------------------------------
// Writing only what was approved (review finding M4)
// ---------------------------------------------------------------------------------------------

/// The agent's write carries the file as it was when the diff was shown. If the file is no
/// longer that, nothing is written: the user approved a change to something else.
#[tokio::test]
async fn conn_a_conditional_write_refuses_a_file_that_changed_since() {
    let (_d, w) = ws();
    w.write("q.sql", "select 1;\n").await.unwrap();
    let shown = FileVersion::of(Some("select 1;\n"));

    // Somebody edits the file while the approval is waiting.
    w.write("q.sql", "select 1;\n-- my own work\n")
        .await
        .unwrap();
    let err = w
        .write_if_unchanged("q.sql", "select 2;\n", &shown)
        .await
        .expect_err("the file is not what was shown");
    assert!(err.to_string().contains("changed"), "{err}");
    assert!(err.to_string().contains("q.sql"), "{err}");
    assert_eq!(
        w.read("q.sql").await.unwrap(),
        "select 1;\n-- my own work\n",
        "the newer content is still there"
    );
}

#[tokio::test]
async fn conn_a_conditional_write_refuses_a_file_that_appeared_or_went_away() {
    let (_d, w) = ws();
    // Shown as a new file, but one exists by the time of the write.
    w.write("new.sql", "-- written by the user meanwhile\n")
        .await
        .unwrap();
    let err = w
        .write_if_unchanged("new.sql", "select 2;\n", &FileVersion::Absent)
        .await
        .expect_err("the file was shown as new");
    assert!(err.to_string().contains("did not exist"), "{err}");
    assert_eq!(
        w.read("new.sql").await.unwrap(),
        "-- written by the user meanwhile\n"
    );

    // Shown as a change to a file that has since been removed.
    w.write("gone.sql", "select 1;\n").await.unwrap();
    let shown = FileVersion::of(Some("select 1;\n"));
    fs::remove_file(w.root().join("gone.sql")).unwrap();
    let err = w
        .write_if_unchanged("gone.sql", "select 2;\n", &shown)
        .await
        .expect_err("the file is gone");
    assert!(err.to_string().contains("deleted or moved"), "{err}");
    assert!(!w.root().join("gone.sql").exists(), "nothing was written");
}

#[tokio::test]
async fn conn_a_conditional_write_goes_ahead_when_nothing_changed() {
    let (_d, w) = ws();
    // A new file, in a folder that does not exist yet.
    w.write_if_unchanged("notes/a.md", "# a\n", &FileVersion::Absent)
        .await
        .unwrap();
    assert_eq!(w.read("notes/a.md").await.unwrap(), "# a\n");
    // An existing file, still as shown.
    w.write_if_unchanged("notes/a.md", "# b\n", &FileVersion::of(Some("# a\n")))
        .await
        .unwrap();
    assert_eq!(w.read("notes/a.md").await.unwrap(), "# b\n");
    // The same rules as any write: the jail, the extension list, the size cap.
    assert!(is_invalid(
        w.write_if_unchanged("../x.sql", "x", &FileVersion::Absent)
            .await
    ));
    assert!(is_invalid(
        w.write_if_unchanged("run.sh", "x", &FileVersion::Absent)
            .await
    ));
    let big = "x".repeat(MAX_FILE_BYTES as usize + 1);
    assert!(is_invalid(
        w.write_if_unchanged("big.sql", &big, &FileVersion::Absent)
            .await
    ));
}

/// A file that turned into something unreadable as text is "changed", not an I/O puzzle.
#[tokio::test]
async fn conn_a_conditional_write_treats_new_binary_content_as_a_change() {
    let (_d, w) = ws();
    w.write("q.sql", "select 1;\n").await.unwrap();
    let shown = FileVersion::of(Some("select 1;\n"));
    fs::write(w.root().join("q.sql"), [0xff, 0xfe, 0x00, 0x01]).unwrap();
    let err = w
        .write_if_unchanged("q.sql", "select 2;\n", &shown)
        .await
        .expect_err("the file is not what was shown");
    assert!(err.to_string().contains("changed"), "{err}");
    assert_eq!(
        fs::read(w.root().join("q.sql")).unwrap(),
        [0xff, 0xfe, 0x00, 0x01]
    );
}

/// The agent refuses a `write_file` when the file is there but cannot be read. That rests on
/// this: such a file is an error, not "no file", and a conditional write does not replace it.
#[cfg(unix)]
#[tokio::test]
async fn conn_an_unreadable_file_is_an_error_not_a_missing_file_and_is_not_replaced() {
    use std::os::unix::fs::PermissionsExt;

    let (_d, w) = ws();
    w.write("locked.sql", "select 1;\n").await.unwrap();
    let path = w.root().join("locked.sql");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&path).is_ok() {
        services::skip(
            "non-root",
            "this user can read a file that has no permissions (root)",
        );
        return;
    }

    let err = w.read("locked.sql").await.unwrap_err();
    assert!(
        matches!(&err, CoreError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied),
        "{err:?}"
    );
    for shown in [FileVersion::Absent, FileVersion::of(Some("select 1;\n"))] {
        let err = w
            .write_if_unchanged("locked.sql", "select 2;\n", &shown)
            .await
            .expect_err("its content cannot be compared, so it is not replaced");
        assert!(matches!(err, CoreError::Io(_)), "{err:?}");
    }

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "select 1;\n");
}

// ---- import_files ---------------------------------------------------------------------------

/// A scratch folder outside the workspace, where "picked" files live.
fn picked(d: &tempfile::TempDir, name: &str, contents: &[u8]) -> std::path::PathBuf {
    let dir = d.path().join("picked");
    fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    fs::write(&p, contents).unwrap();
    p
}

fn reasons(out: &vibelens_core::workspace::ImportOutcome) -> Vec<(&str, &str)> {
    out.skipped
        .iter()
        .map(|s| (s.name.as_str(), s.reason.as_str()))
        .collect()
}

#[tokio::test]
async fn conn_import_copies_allowed_files_in_order() {
    let (d, w) = ws();
    let a = picked(&d, "a.sql", b"select 1;\n");
    let b = picked(&d, "Notes.MD", b"# hi\n");
    let out = w.import_files(vec![a.clone(), b]).await;
    assert_eq!(out.imported, vec!["a.sql", "Notes.MD"]);
    assert!(out.skipped.is_empty());
    assert_eq!(w.read("a.sql").await.unwrap(), "select 1;\n");
    assert_eq!(w.read("Notes.MD").await.unwrap(), "# hi\n");
    // The source is copied, not moved.
    assert!(a.is_file());
}

#[tokio::test]
async fn conn_import_rejects_other_extensions_and_hidden_names() {
    let (d, w) = ws();
    let exe = picked(&d, "run.exe", b"MZ");
    let none = picked(&d, "Makefile", b"all:");
    let hidden = picked(&d, ".env.sql", b"secret");
    let ok = picked(&d, "ok.json", b"{}");
    let out = w.import_files(vec![exe, none, hidden, ok]).await;
    assert_eq!(out.imported, vec!["ok.json"]);
    let skipped: Vec<_> = out.skipped.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(skipped, ["run.exe", "Makefile", ".env.sql"]);
    assert!(out.skipped.iter().all(|s| !s.reason.is_empty()));
    assert!(out.skipped[0].reason.contains("not allowed"));
    assert!(!w.root().join("run.exe").exists());
    assert!(!w.root().join(".env.sql").exists());
}

#[tokio::test]
async fn conn_import_enforces_the_size_cap_on_the_source() {
    let (d, w) = ws();
    let big = picked(&d, "big.sql", &vec![b'x'; MAX_FILE_BYTES as usize + 1]);
    let exact = picked(&d, "exact.sql", &vec![b'y'; MAX_FILE_BYTES as usize]);
    let out = w.import_files(vec![big, exact]).await;
    assert_eq!(out.imported, vec!["exact.sql"]);
    assert_eq!(reasons(&out).len(), 1);
    assert_eq!(out.skipped[0].name, "big.sql");
    assert!(out.skipped[0].reason.contains("limit"), "{:?}", out.skipped);
    assert!(!w.root().join("big.sql").exists());
}

#[tokio::test]
async fn conn_import_rejects_text_that_is_not_utf8() {
    let (d, w) = ws();
    let bin = picked(&d, "bin.txt", &[0xff, 0xfe, 0x00, 0x80]);
    let out = w.import_files(vec![bin]).await;
    assert!(out.imported.is_empty());
    assert!(out.skipped[0].reason.contains("UTF-8"));
    assert!(!w.root().join("bin.txt").exists());
}

#[tokio::test]
async fn conn_import_rejects_directories_and_missing_files() {
    let (d, w) = ws();
    let dir = d.path().join("picked").join("folder.sql");
    fs::create_dir_all(&dir).unwrap();
    let gone = d.path().join("picked").join("gone.sql");
    let out = w.import_files(vec![dir, gone]).await;
    assert!(out.imported.is_empty());
    assert_eq!(out.skipped.len(), 2);
    assert!(out.skipped[0].reason.contains("regular file"));
    assert!(out.skipped[1].reason.contains("no longer exists"));
    assert!(!w.root().join("folder.sql").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn conn_import_rejects_fifos_without_opening_them() {
    let (d, w) = ws();
    let fifo = d.path().join("picked");
    fs::create_dir_all(&fifo).unwrap();
    let fifo = fifo.join("pipe.sql");
    let status = std::process::Command::new("mkfifo").arg(&fifo).status();
    if !status.map(|s| s.success()).unwrap_or(false) {
        services::skip("mkfifo", "mkfifo is not available");
        return;
    }
    // Opening a FIFO for reading blocks until a writer appears; the import must not try.
    let out = w.import_files(vec![fifo]).await;
    assert!(out.imported.is_empty());
    assert!(out.skipped[0].reason.contains("regular file"));
}

#[tokio::test]
async fn conn_import_never_overwrites_a_name_clash() {
    let (d, w) = ws();
    w.write("q.sql", "mine\n").await.unwrap();
    let first = picked(&d, "q.sql", b"theirs 1\n");
    let out = w.import_files(vec![first.clone()]).await;
    assert_eq!(out.imported, vec!["q-2.sql"]);
    // Again, and twice in one batch: the numbers keep going.
    let out = w.import_files(vec![first.clone(), first]).await;
    assert_eq!(out.imported, vec!["q-3.sql", "q-4.sql"]);
    assert_eq!(w.read("q.sql").await.unwrap(), "mine\n");
    assert_eq!(w.read("q-2.sql").await.unwrap(), "theirs 1\n");
    assert_eq!(w.read("q-4.sql").await.unwrap(), "theirs 1\n");
}

#[cfg(unix)]
#[tokio::test]
async fn conn_import_never_writes_through_a_symlink_in_the_workspace() {
    use std::os::unix::fs::symlink;
    let (d, w) = ws();
    let secret = d.path().join("secret.sql");
    fs::write(&secret, "top secret").unwrap();
    symlink(&secret, w.root().join("leak.sql")).unwrap();
    // A dangling link at the target name is just as taken.
    symlink(d.path().join("nowhere.sql"), w.root().join("dangling.sql")).unwrap();
    let a = picked(&d, "leak.sql", b"pwned\n");
    let b = picked(&d, "dangling.sql", b"pwned\n");
    let out = w.import_files(vec![a, b]).await;
    assert_eq!(out.imported, vec!["leak-2.sql", "dangling-2.sql"]);
    assert_eq!(fs::read_to_string(&secret).unwrap(), "top secret");
    assert!(!d.path().join("nowhere.sql").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn conn_import_rejects_a_picked_symlink_that_points_outside() {
    use std::os::unix::fs::symlink;
    let (d, w) = ws();
    let target = d.path().join("elsewhere.sql");
    fs::write(&target, "select 1;\n").unwrap();
    let dir = d.path().join("picked");
    fs::create_dir_all(&dir).unwrap();
    symlink(&target, dir.join("link.sql")).unwrap();
    symlink(d.path().join("missing.sql"), dir.join("broken.sql")).unwrap();
    let out = w
        .import_files(vec![dir.join("link.sql"), dir.join("broken.sql")])
        .await;
    assert!(out.imported.is_empty());
    assert_eq!(out.skipped.len(), 2);
    assert!(out.skipped[0].reason.contains("symbolic link"));
    assert!(out.skipped[1].reason.contains("symbolic link"));
    assert!(!w.root().join("link.sql").exists());
}

#[tokio::test]
async fn conn_import_caps_the_number_of_files() {
    use vibelens_core::workspace::MAX_IMPORT_FILES;
    let (d, w) = ws();
    let one = picked(&d, "x.sql", b"select 1;\n");
    let out = w.import_files(vec![one; MAX_IMPORT_FILES + 2]).await;
    assert_eq!(out.imported.len(), MAX_IMPORT_FILES);
    assert_eq!(out.skipped.len(), 2);
    assert!(out.skipped[0].reason.contains("at once"));
}
