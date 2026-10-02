#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, QueryHandle, Status};
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

fn query(handle: &QueryHandle, raw: &str) -> Vec<PathBuf> {
    let lease = handle.lease().unwrap();
    let mut cursor = None;
    let mut paths = Vec::new();
    loop {
        let page = lease
            .page(
                raw,
                cursor.as_ref(),
                17,
                &AtomicBool::new(false),
                &AtomicUsize::new(0),
            )
            .unwrap();
        paths.extend(page.paths);
        if page.complete {
            break;
        }
        cursor = page.next;
    }
    paths.sort();
    paths
}
fn settle(engine: &mut Engine, expected: &[PathBuf]) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        engine.poll().unwrap();
        if engine.view().status == Status::Validated && query(&engine.query(), "") == expected {
            return;
        }
        assert!(Instant::now() < deadline, "{:?}", engine.view());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn raw_names_preserve_bytes_search_valid_runs_and_never_bridge_invalid_bytes() {
    let fixture = Fixture::new();
    let raw = fixture.root.join(OsString::from_vec(
        b"ABC\xffDEF-\xe6\x8a\xa5\xe5\x91\x8a.TXT".to_vec(),
    ));
    let punct = fixture.root.join("中文 space:\nline.txt");
    fs::write(&raw, b"x").unwrap();
    fs::write(&punct, b"x").unwrap();
    let mut expected: Vec<_> = fs::read_dir(&fixture.root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    expected.sort();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    assert_eq!(query(&engine.query(), ""), expected);
    assert_eq!(
        query(&engine.query(), "abc def 报告 ext:txt"),
        [raw.clone()]
    );
    assert!(query(&engine.query(), "abcdef").is_empty());
    assert_eq!(query(&engine.query(), "中文 space: line ext:txt"), [punct]);
    let next = fixture
        .root
        .join(OsString::from_vec(b"later\xfe.TXT".to_vec()));
    fs::write(&next, b"x").unwrap();
    expected.push(next);
    expected.sort();
    settle(&mut engine, &expected);
}

#[test]
fn links_are_typed_snapshot_entries_without_following_external_targets_or_cycles() {
    use loci_experiment::engine::EntryKind;
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    fs::create_dir(fixture.base.join("outside")).unwrap();
    fs::write(fixture.base.join("outside/secret.txt"), b"x").unwrap();
    symlink(fixture.base.join("outside"), fixture.root.join("external")).unwrap();
    symlink(".", fixture.root.join("cycle")).unwrap();
    symlink("missing", fixture.root.join("dangling")).unwrap();
    fs::write(fixture.root.join("file.txt"), b"x").unwrap();
    fs::create_dir(fixture.root.join("dir")).unwrap();
    let mut expected: Vec<_> = fs::read_dir(&fixture.root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    expected.sort();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    assert_eq!(query(&engine.query(), ""), expected);
    assert!(query(&engine.query(), "secret").is_empty());
    let lease = engine.query().lease().unwrap();
    assert_eq!(
        lease.entry_kind(&fixture.root.join("external")).unwrap(),
        EntryKind::Symlink
    );
    assert_eq!(
        lease.entry_kind(&fixture.root.join("cycle")).unwrap(),
        EntryKind::Symlink
    );
    assert_eq!(
        lease.entry_kind(&fixture.root.join("dangling")).unwrap(),
        EntryKind::Symlink
    );
    assert_eq!(
        lease.entry_kind(&fixture.root.join("file.txt")).unwrap(),
        EntryKind::File
    );
    assert_eq!(
        lease.entry_kind(&fixture.root.join("dir")).unwrap(),
        EntryKind::Directory
    );
    fs::remove_file(fixture.root.join("external")).unwrap();
    fs::write(fixture.root.join("external"), b"replacement").unwrap();
    settle(&mut engine, &expected);
    // The leased kind remains the old symlink after publishing a file replacement.
    let deadline = Instant::now() + Duration::from_secs(8);
    while engine
        .query()
        .lease()
        .unwrap()
        .entry_kind(&fixture.root.join("external"))
        .unwrap()
        != EntryKind::File
    {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        lease.entry_kind(&fixture.root.join("external")).unwrap(),
        EntryKind::Symlink
    );
}

#[test]
fn scale_exclusions_are_explicit_relative_subtrees_and_default_is_empty() {
    let fixture = Fixture::new();
    for directory in ["target", ".git", "skip/deep"] {
        fs::create_dir_all(fixture.root.join(directory)).unwrap();
    }
    for name in ["target/source.txt", ".git/config", "skip/deep/hidden.txt"] {
        fs::write(fixture.root.join(name), b"x").unwrap();
    }
    let engine = Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    assert!(query(&engine.query(), "").contains(&fixture.root.join("target/source.txt")));
    assert!(query(&engine.query(), "").contains(&fixture.root.join(".git/config")));
    drop(engine);
    let options = EngineOptions {
        exclusions: vec![PathBuf::from("skip")],
        ..EngineOptions::scale()
    };
    let engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    assert!(query(&engine.query(), "hidden").is_empty());
    assert!(!query(&engine.query(), "").contains(&fixture.root.join("skip")));
    for invalid in [
        PathBuf::from("../outside"),
        fixture.base.clone(),
        PathBuf::new(),
    ] {
        let options = EngineOptions {
            exclusions: vec![invalid],
            ..EngineOptions::scale()
        };
        assert!(Engine::open_with_options(&fixture.root, None, options).is_err());
    }
}

#[test]
fn native_bind_mount_scope_in_private_namespace() {
    use std::process::Command;
    let probe = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount", "true"])
        .output();
    if !probe.as_ref().is_ok_and(|output| output.status.success()) {
        eprintln!(
            "UNVERIFIED: private user/mount namespaces unavailable for native bind-mount scope"
        );
        return;
    }
    let output = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "native_bind_mount_scope_child", "--nocapture"])
        .env("LOCI_BIND_SCOPE_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native_bind_scope_verified"));
}

#[test]
fn native_bind_mount_scope_child() {
    if std::env::var_os("LOCI_BIND_SCOPE_CHILD").is_none() {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    use std::process::Command;
    struct Mounted(PathBuf);
    impl Mounted {
        fn bind(source: &std::path::Path, target: &std::path::Path) -> Self {
            assert!(Command::new("mount")
                .arg("--bind")
                .arg(source)
                .arg(target)
                .status()
                .unwrap()
                .success());
            Self(target.to_path_buf())
        }
    }
    impl Drop for Mounted {
        fn drop(&mut self) {
            assert!(Command::new("umount")
                .arg(&self.0)
                .status()
                .unwrap()
                .success());
        }
    }
    let fixture = Fixture::new();
    let outside = fixture.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"x").unwrap();
    let target = fixture
        .root
        .join(OsString::from_vec(b"mounted space\nraw\xff".to_vec()));
    fs::create_dir(&target).unwrap();
    let mounted = Mounted::bind(&outside, &target);
    assert_eq!(
        fs::metadata(&target).unwrap().dev(),
        fs::metadata(&fixture.root).unwrap().dev(),
        "bind is same-device case"
    );
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    assert_eq!(query(&engine.query(), ""), [target.clone()]);
    drop(mounted);
    fs::write(target.join("local.txt"), b"x").unwrap();
    settle(&mut engine, &[target.clone(), target.join("local.txt")]);
    let old = engine.query().lease().unwrap();
    let root_overlay = Mounted::bind(&fixture.root, &fixture.root);
    assert!(
        engine.poll().is_err(),
        "selected root mount identity changed despite unchanged device/inode"
    );
    assert!(matches!(engine.view().status, Status::Failed(_)));
    assert_eq!(
        old.page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap()
            .paths
            .len(),
        2
    );
    drop(root_overlay);
    drop(engine);
    let mut fresh = Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let old = fresh.query().lease().unwrap();
    let invalid_scope = fixture.base.join("invalid-mountinfo");
    fs::write(&invalid_scope, b"unrecognized scope record\n").unwrap();
    let proc_scope = PathBuf::from(format!("/proc/{}/mountinfo", std::process::id()));
    let mask = Mounted::bind(&invalid_scope, &proc_scope);
    assert!(
        fresh.poll().is_err(),
        "unknown scope must not remain validated"
    );
    assert!(matches!(fresh.view().status, Status::Failed(_)));
    let page = old
        .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert!(page.complete && !page.validated_at_start_and_finish);
    drop(mask);
    println!("native_bind_scope_verified");
}

#[test]
fn cli_scale_exports_exact_raw_paths_and_accepts_raw_root_and_explicit_exclusions() {
    use std::process::Command;
    let fixture = Fixture::new();
    let root = fixture
        .root
        .join(OsString::from_vec(b"raw-root\xff".to_vec()));
    fs::create_dir(&root).unwrap();
    let raw = root.join(OsString::from_vec(b"visible\xfe\nspace:.txt".to_vec()));
    fs::write(&raw, b"x").unwrap();
    fs::create_dir(root.join("skip")).unwrap();
    fs::write(root.join("skip/secret.txt"), b"x").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "query"])
        .arg(&root)
        .arg(fixture.base.join("unused.loci"))
        .args(["", "--scale", "--exclude", "skip", "--all", "--null"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut expected = raw.as_os_str().as_bytes().to_vec();
    expected.push(0);
    assert_eq!(output.stdout, expected);
    assert!(!fixture.base.join("unused.loci").exists());
    let display = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "query"])
        .arg(&root)
        .arg(fixture.base.join("unused.loci"))
        .args(["", "--scale", "--exclude", "skip", "--all"])
        .output()
        .unwrap();
    assert!(display.status.success());
    let text = String::from_utf8(display.stdout).unwrap();
    assert!(
        text.contains("\\xFF") && text.contains("\\xFE") && text.contains("\\n"),
        "{text}"
    );
}
