#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, QueryHandle, Status};
use loci_experiment::linux_inotify::process_usage;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// Resource assertions require a quiet process even when the harness is parallel.
static NATIVE: Mutex<()> = Mutex::new(());

fn paths(handle: &QueryHandle) -> Vec<PathBuf> {
    handle
        .lease()
        .unwrap()
        .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap()
        .paths
}

fn settle(engine: &mut Engine, expected: &[PathBuf]) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        engine.poll().unwrap();
        if engine.view().status == Status::Validated && paths(&engine.query()) == expected {
            let out = engine
                .query()
                .lease()
                .unwrap()
                .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
                .unwrap();
            assert!(out.complete && out.validated_at_start_and_finish);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "did not settle: {:?}, paths {:?}, metrics {:?}",
            engine.view(),
            paths(&engine.query()),
            engine.metrics()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn saved_engine_releases_its_database_parent_descriptor_on_stop_and_drop() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.root.join("seed.txt"), "x").unwrap();
    let db = f.base.join("state.loci");
    let descriptors = fd_count();
    for stop in [true, false] {
        let mut engine = Engine::open(&f.root, Some(&db)).unwrap();
        let handle = engine.query();
        // One inotify descriptor, one root identity and one database parent.
        assert_eq!(fd_count(), descriptors + 3);
        engine.save().unwrap();
        if stop {
            engine.stop().unwrap();
            assert_eq!(fd_count(), descriptors);
        }
        drop(engine);
        assert_eq!(handle.view().status, Status::Stopped);
        assert_eq!(fd_count(), descriptors);
    }
}

#[test]
fn linux_new_engine_opens_saves_and_reconciles_offline_changes() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    let db = f.base.join("inventory.loci");
    fs::write(f.root.join("中文 old.rs"), "x").unwrap();
    fs::write(f.root.join("deleted.txt"), "x").unwrap();
    let mut engine = Engine::open(&f.root, Some(&db)).unwrap();
    assert_eq!(engine.view().status, Status::Validated);
    engine.save().unwrap();
    engine.stop().unwrap();
    fs::rename(f.root.join("中文 old.rs"), f.root.join("中文 new.rs")).unwrap();
    fs::remove_file(f.root.join("deleted.txt")).unwrap();
    fs::write(f.root.join("added.txt"), "x").unwrap();
    let engine = Engine::open(&f.root, Some(&db)).unwrap();
    assert_eq!(engine.view().status, Status::Validated);
    let query = engine.query();
    let out = query
        .lease()
        .unwrap()
        .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(
        out.paths,
        [f.root.join("added.txt"), f.root.join("中文 new.rs")]
    );
    assert!(out.complete && out.validated_at_start_and_finish);
}

#[test]
fn new_recursive_directories_receive_later_child_changes() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    let mut engine = Engine::open(&f.root, None).unwrap();
    let scans = engine.metrics().full_scans;
    fs::create_dir_all(f.root.join("new/deep")).unwrap();
    fs::write(f.root.join("new/deep/early.txt"), "x").unwrap();
    settle(
        &mut engine,
        &[
            f.root.join("new"),
            f.root.join("new/deep"),
            f.root.join("new/deep/early.txt"),
        ],
    );
    fs::write(f.root.join("new/deep/later:合法.txt"), "x").unwrap();
    fs::remove_file(f.root.join("new/deep/early.txt")).unwrap();
    settle(
        &mut engine,
        &[
            f.root.join("new"),
            f.root.join("new/deep"),
            f.root.join("new/deep/later:合法.txt"),
        ],
    );
    assert_eq!(engine.metrics().full_scans, scans);
    assert!(engine.metrics().subtree_scans > 0);
}

#[test]
fn directory_rename_retains_recursive_watches_and_attribute_path() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("old/deep")).unwrap();
    fs::write(f.root.join("old/deep/kept.txt"), "x").unwrap();
    let mut engine = Engine::open(&f.root, None).unwrap();
    let scans = engine.metrics().full_scans;
    fs::rename(f.root.join("old"), f.root.join("renamed")).unwrap();
    settle(
        &mut engine,
        &[
            f.root.join("renamed"),
            f.root.join("renamed/deep"),
            f.root.join("renamed/deep/kept.txt"),
        ],
    );
    assert_eq!(
        engine.metrics().full_scans,
        scans,
        "standalone paired rename should be incremental"
    );
    fs::write(f.root.join("renamed/deep/later.txt"), "x").unwrap();
    settle(
        &mut engine,
        &[
            f.root.join("renamed"),
            f.root.join("renamed/deep"),
            f.root.join("renamed/deep/kept.txt"),
            f.root.join("renamed/deep/later.txt"),
        ],
    );
    let version = engine.view().version;
    fs::set_permissions(f.root.join("renamed"), fs::Permissions::from_mode(0o750)).unwrap();
    // Wait for the coalescing gate, then require a transaction for IN_ATTRIB.
    std::thread::sleep(Duration::from_millis(280));
    assert!(engine.poll().unwrap());
    assert_eq!(engine.view().status, Status::Validated);
    assert!(engine.view().version > version);
    assert_eq!(engine.metrics().full_scans, scans);
}

#[test]
fn dependent_child_creation_and_directory_rename_reconciles_without_stale_publish() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    fs::create_dir(f.root.join("old")).unwrap();
    fs::write(f.root.join("old/kept.txt"), "x").unwrap();
    let mut engine = Engine::open(&f.root, None).unwrap();
    let scans = engine.metrics().full_scans;
    fs::write(f.root.join("old/added.txt"), "x").unwrap();
    fs::rename(f.root.join("old"), f.root.join("new")).unwrap();
    settle(
        &mut engine,
        &[
            f.root.join("new"),
            f.root.join("new/added.txt"),
            f.root.join("new/kept.txt"),
        ],
    );
    assert!(engine.metrics().full_scans > scans);
}

#[test]
fn moved_or_replaced_root_fails_and_retains_last_good_query() {
    let _guard = NATIVE.lock().unwrap();
    for replace in [false, true] {
        let f = Fixture::new();
        fs::write(f.root.join("original.txt"), "x").unwrap();
        let mut engine = Engine::open(&f.root, None).unwrap();
        let handle = engine.query();
        fs::rename(&f.root, f.base.join("moved")).unwrap();
        if replace {
            fs::create_dir(&f.root).unwrap();
            fs::write(f.root.join("replacement.txt"), "x").unwrap();
        }
        assert!(engine.poll().is_err());
        assert!(matches!(handle.view().status, Status::Failed(_)));
        assert_eq!(paths(&handle), [f.root.join("original.txt")]);
        engine.stop().unwrap();
    }
}

#[test]
fn non_utf8_native_candidate_retains_snapshot_then_recovers_after_removal() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.root.join("kept.txt"), "x").unwrap();
    let mut engine = Engine::open(&f.root, None).unwrap();
    let invalid = f.root.join(OsString::from_vec(vec![b'x', 0xff]));
    fs::write(&invalid, "x").unwrap();
    std::thread::sleep(Duration::from_millis(280));
    let error = engine.poll().unwrap_err();
    assert!(error.to_string().contains("UTF-8"), "{error}");
    assert!(matches!(engine.view().status, Status::Failed(_)));
    assert_eq!(paths(&engine.query()), [f.root.join("kept.txt")]);
    fs::remove_file(invalid).unwrap();
    settle(&mut engine, &[f.root.join("kept.txt")]);
}

fn fd_count() -> usize {
    fs::read_dir("/proc/self/fd").unwrap().count()
}

fn kernel_watch_count() -> usize {
    fs::read_dir("/proc/self/fdinfo")
        .unwrap()
        .filter_map(|entry| fs::read_to_string(entry.ok()?.path()).ok())
        .map(|info| {
            info.lines()
                .filter(|line| line.starts_with("inotify wd:"))
                .count()
        })
        .sum()
}

#[test]
fn native_stop_and_drop_release_process_and_kernel_resources() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("one/two")).unwrap();
    let usage = process_usage();
    let descriptors = fd_count();
    let watches = kernel_watch_count();
    for explicit_stop in [true, false] {
        let mut engine = Engine::open(&f.root, None).unwrap();
        let handle = engine.query();
        assert_eq!(process_usage(), (usage.0 + 3, usage.1 + 1));
        assert_eq!(kernel_watch_count(), watches + 3);
        assert_eq!(fd_count(), descriptors + 2);
        if explicit_stop {
            engine.stop().unwrap();
            engine.stop().unwrap();
            assert!(!engine.poll().unwrap());
            assert_eq!(process_usage(), usage);
            assert_eq!(kernel_watch_count(), watches);
            assert_eq!(fd_count(), descriptors);
        }
        drop(engine);
        assert_eq!(handle.view().status, Status::Stopped);
        assert_eq!(process_usage(), usage);
        assert_eq!(kernel_watch_count(), watches);
        assert_eq!(fd_count(), descriptors);
    }
    // A failed recursive open must release every partially registered watch.
    for n in 0..126 {
        fs::create_dir(f.root.join(format!("limit-{n:03}"))).unwrap();
    }
    assert!(Engine::open(&f.root, None).is_err());
    assert_eq!(process_usage(), usage);
    assert_eq!(kernel_watch_count(), watches);
    assert_eq!(fd_count(), descriptors);
}

#[test]
fn bounded_user_queue_overflow_preserves_last_good_and_reconciles() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.root.join("kept.txt"), "x").unwrap();
    let mut engine = Engine::open(&f.root, None).unwrap();
    let old_lease = engine.query().lease().unwrap();
    fs::write(f.root.join("baseline.txt"), "x").unwrap();
    settle(
        &mut engine,
        &[f.root.join("baseline.txt"), f.root.join("kept.txt")],
    );
    let scans = engine.metrics().full_scans;
    // >=800 distinct CREATE/DELETE events exceed the source's 256-event
    // bounded user queue. This does not claim IN_Q_OVERFLOW from the kernel.
    for n in 0..400 {
        let path = f.root.join(format!("transient-{n:03}.txt"));
        fs::write(&path, "x").unwrap();
        fs::remove_file(path).unwrap();
    }
    fs::write(f.root.join("survivor.txt"), "x").unwrap();
    assert!(!engine.poll().unwrap());
    assert!(matches!(
        engine.view().status,
        Status::Pending | Status::ReadersPinned
    ));
    assert_eq!(
        paths(&engine.query()),
        [f.root.join("baseline.txt"), f.root.join("kept.txt")]
    );
    drop(old_lease);
    settle(
        &mut engine,
        &[
            f.root.join("baseline.txt"),
            f.root.join("kept.txt"),
            f.root.join("survivor.txt"),
        ],
    );
    assert!(engine.metrics().full_scans > scans);
}

#[test]
fn repeated_moves_into_excluded_directory_retire_watches_before_recreation() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    let usage = process_usage();
    let watches = kernel_watch_count();
    let descriptors = fd_count();
    fs::create_dir(f.root.join("active")).unwrap();
    fs::write(f.root.join("active/child.txt"), "x").unwrap();
    let mut engine = Engine::open(&f.root, None).unwrap();
    for _ in 0..12 {
        fs::rename(f.root.join("active"), f.root.join("target")).unwrap();
        settle(&mut engine, &[]);
        assert_eq!(process_usage(), (usage.0 + 1, usage.1 + 1));
        assert_eq!(kernel_watch_count(), watches + 1);
        fs::remove_dir_all(f.root.join("target")).unwrap();
        fs::create_dir(f.root.join("active")).unwrap();
        fs::write(f.root.join("active/child.txt"), "x").unwrap();
        settle(
            &mut engine,
            &[f.root.join("active"), f.root.join("active/child.txt")],
        );
        assert_eq!(process_usage(), (usage.0 + 2, usage.1 + 1));
        assert_eq!(kernel_watch_count(), watches + 2);
        fs::write(f.root.join("active/later.txt"), "x").unwrap();
        settle(
            &mut engine,
            &[
                f.root.join("active"),
                f.root.join("active/child.txt"),
                f.root.join("active/later.txt"),
            ],
        );
        assert_eq!(fd_count(), descriptors + 2);
    }
    drop(engine);
    assert_eq!(process_usage(), usage);
    assert_eq!(kernel_watch_count(), watches);
    assert_eq!(fd_count(), descriptors);
}

#[test]
fn linux_snapshot_child() {
    let Some(mode) = std::env::var_os("LOCI_LINUX_ENGINE_CHILD_MODE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("LOCI_LINUX_ENGINE_CHILD_ROOT").unwrap());
    let db = PathBuf::from(std::env::var_os("LOCI_LINUX_ENGINE_CHILD_DB").unwrap());
    // Parent supplies a guarded Fixture; children may access only its data root.
    let work = fs::canonicalize(std::env::current_dir().unwrap().join("work")).unwrap();
    assert!(root.starts_with(&work));
    assert_eq!(root.file_name().unwrap(), "data");
    assert!(root
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("live-fixture-"));
    assert_eq!(db.parent(), root.parent());
    let mut engine = Engine::open(&root, Some(&db)).unwrap();
    assert_eq!(engine.view().status, Status::Validated);
    if mode == "save" {
        assert_eq!(
            paths(&engine.query()),
            [
                root.join("deleted.txt"),
                root.join("old-dir"),
                root.join("old-dir/child.txt"),
                root.join("中文 old.rs")
            ]
        );
        engine.save().unwrap();
    } else {
        assert_eq!(mode, "reopen");
        assert_eq!(
            paths(&engine.query()),
            [
                root.join("added.txt"),
                root.join("new-dir"),
                root.join("new-dir/child.txt"),
                root.join("中文 new.rs")
            ]
        );
        let out = engine
            .query()
            .lease()
            .unwrap()
            .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap();
        assert!(out.complete && out.validated_at_start_and_finish);
    }
    engine.stop().unwrap();
}

#[test]
fn saved_inventory_reopens_in_fresh_process_after_offline_file_and_directory_changes() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    let db = f.base.join("cross-process.loci");
    fs::create_dir(f.root.join("old-dir")).unwrap();
    fs::write(f.root.join("old-dir/child.txt"), "x").unwrap();
    fs::write(f.root.join("deleted.txt"), "x").unwrap();
    fs::write(f.root.join("中文 old.rs"), "x").unwrap();
    let run = |mode: &str| {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "linux_snapshot_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("LOCI_LINUX_ENGINE_CHILD_MODE", mode)
            .env("LOCI_LINUX_ENGINE_CHILD_ROOT", &f.root)
            .env("LOCI_LINUX_ENGINE_CHILD_DB", &db)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("Linux engine child {mode} exceeded 10 seconds");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "child {mode}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run("save");
    assert!(db.is_file());
    fs::remove_file(f.root.join("deleted.txt")).unwrap();
    fs::rename(f.root.join("中文 old.rs"), f.root.join("中文 new.rs")).unwrap();
    fs::rename(f.root.join("old-dir"), f.root.join("new-dir")).unwrap();
    fs::write(f.root.join("added.txt"), "x").unwrap();
    run("reopen");
}
