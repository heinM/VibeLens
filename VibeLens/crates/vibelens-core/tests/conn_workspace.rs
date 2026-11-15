//! WorkspaceFs: the path jail, size cap, atomic writes and listing.

use std::fs;

use tempfile::tempdir;
use vibelens_core::error::CoreError;
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
