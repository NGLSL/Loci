#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, QueryHandle, Status};
use loci_experiment::linux_inotify::process_usage;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Mutex;
use std::time::{Duration, Instant};
static NATIVE: Mutex<()> = Mutex::new(());
fn all(handle: &QueryHandle) -> Vec<PathBuf> {
    let lease = handle.lease().unwrap();
    let mut cursor = None;
    let mut paths = Vec::new();
    loop {
        let page = lease
            .page(
                "",
                cursor.as_ref(),
                256,
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
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        engine.poll().unwrap();
        if engine.view().status == Status::Validated && all(&engine.query()) == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "did not settle: {:?}",
            engine.view()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn kernel_watches() -> usize {
    fs::read_dir("/proc/self/fdinfo")
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| {
            fs::read_to_string(entry.path())
                .unwrap_or_default()
                .lines()
                .filter(|line| line.starts_with("inotify wd:"))
                .count()
        })
        .sum()
}
#[test]
fn two_thousand_real_directories_are_watched_and_new_subtrees_keep_children() {
    let _guard = NATIVE.lock().unwrap();
    // This dedicated fixture contains ~2000 empty dirs (<16 MiB, 2100 inodes).
    // Run preparation checks free disk/inodes before this opt-in native fixture.
    let fixture = Fixture::new();
    let baseline = process_usage();
    let kernel_baseline = kernel_watches();
    let mut expected = Vec::new();
    for n in 0..2000 {
        let path = fixture.root.join(format!("directory-{n:04}"));
        fs::create_dir(&path).unwrap();
        expected.push(path);
    }
    let mut options = EngineOptions::scale();
    options.limits.directories = 4096;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    settle(&mut engine, &expected);
    assert_eq!(process_usage(), (baseline.0 + 2001, baseline.1 + 1));
    assert_eq!(kernel_watches(), kernel_baseline + 2001);
    fs::create_dir_all(fixture.root.join("incoming/deep")).unwrap();
    fs::write(fixture.root.join("incoming/deep/early.txt"), "x").unwrap();
    expected.extend([
        fixture.root.join("incoming"),
        fixture.root.join("incoming/deep"),
        fixture.root.join("incoming/deep/early.txt"),
    ]);
    expected.sort();
    settle(&mut engine, &expected);
    fs::write(fixture.root.join("incoming/deep/later.txt"), "x").unwrap();
    expected.push(fixture.root.join("incoming/deep/later.txt"));
    expected.sort();
    settle(&mut engine, &expected);
    fs::remove_dir_all(fixture.root.join("incoming")).unwrap();
    expected.retain(|path| !path.starts_with(fixture.root.join("incoming")));
    settle(&mut engine, &expected);
    assert_eq!(kernel_watches(), kernel_baseline + 2001);
    fs::create_dir_all(fixture.base.join("staging/deep")).unwrap();
    fs::write(fixture.base.join("staging/deep/moved.txt"), "x").unwrap();
    fs::rename(fixture.base.join("staging"), fixture.root.join("moved-in")).unwrap();
    expected.extend([
        fixture.root.join("moved-in"),
        fixture.root.join("moved-in/deep"),
        fixture.root.join("moved-in/deep/moved.txt"),
    ]);
    expected.sort();
    settle(&mut engine, &expected);
    fs::write(fixture.root.join("moved-in/deep/later.txt"), "x").unwrap();
    expected.push(fixture.root.join("moved-in/deep/later.txt"));
    expected.sort();
    settle(&mut engine, &expected);
    fs::remove_dir_all(fixture.root.join("moved-in")).unwrap();
    expected.retain(|path| !path.starts_with(fixture.root.join("moved-in")));
    settle(&mut engine, &expected);
    assert_eq!(kernel_watches(), kernel_baseline + 2001);
    engine.stop().unwrap();
    assert_eq!(process_usage(), baseline);
    assert_eq!(kernel_watches(), kernel_baseline);
}

#[test]
fn a_watch_budget_gap_keeps_old_results_unvalidated_and_reports_exact_scope() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed.txt"), "x").unwrap();
    let baseline = process_usage();
    let mut options = EngineOptions::scale();
    options.watch_limit = 1;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    let handle = engine.query();
    assert_eq!(handle.view().status, Status::Validated);
    fs::create_dir(fixture.root.join("uncovered")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let _ = engine.poll();
        let view = handle.view();
        if !view.coverage_gaps.is_empty() {
            assert!(matches!(view.status, Status::Failed(_)));
            assert_eq!(view.coverage_gaps[0].path, fixture.root.join("uncovered"));
            assert_eq!(
                view.coverage_gaps[0].kind,
                loci_experiment::engine::CoverageGapKind::WatchBudget
            );
            assert_eq!(view.resources.session_watch_limit, 1);
            assert_eq!(view.resources.process_watch_limit, 65536);
            assert_eq!(all(&handle), [fixture.root.join("seed.txt")]);
            let page = handle
                .lease()
                .unwrap()
                .page(
                    "seed",
                    None,
                    50,
                    &AtomicBool::new(false),
                    &AtomicUsize::new(0),
                )
                .unwrap();
            assert!(!page.validated_at_start_and_finish);
            break;
        }
        assert!(Instant::now() < deadline, "missing watch gap: {:?}", view);
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::remove_dir(fixture.root.join("uncovered")).unwrap();
    settle(&mut engine, &[fixture.root.join("seed.txt")]);
    assert!(engine.view().coverage_gaps.is_empty());
    drop(engine);
    assert_eq!(process_usage(), baseline);
}

#[test]
fn permission_gap_is_reported_and_restoration_allows_bounded_recovery() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = NATIVE.lock().unwrap();
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    assert_ne!(
        unsafe { geteuid() },
        0,
        "permission coverage requires an unprivileged test user"
    );
    let fixture = Fixture::new();
    let baseline = process_usage();
    let blocked = fixture.root.join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("hidden.txt"), "x").unwrap();
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let restore = Restore(blocked.clone());
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0)).unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let view = engine.view();
    assert!(matches!(view.status, Status::Failed(_)));
    assert_eq!(view.coverage_gaps[0].path, blocked);
    assert_eq!(
        view.coverage_gaps[0].kind,
        loci_experiment::engine::CoverageGapKind::Permission
    );
    assert_eq!(view.coverage_gaps[0].errno, Some(13));
    assert!(
        engine.query().lease().is_err(),
        "an incomplete initial scan cannot publish a snapshot"
    );
    let scans = engine.metrics().full_scans;
    for _ in 0..20 {
        assert!(!engine.poll().unwrap());
    }
    assert_eq!(
        engine.metrics().full_scans,
        scans,
        "coverage failures must not spin retries"
    );
    drop(restore);
    settle(&mut engine, &[blocked.clone(), blocked.join("hidden.txt")]);
    assert!(engine.view().coverage_gaps.is_empty());
    drop(engine);
    assert_eq!(process_usage(), baseline);
}

#[test]
fn scale_root_replacement_fails_with_scope_and_releases_native_resources() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    let baseline = process_usage();
    fs::write(fixture.root.join("kept.txt"), "x").unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let handle = engine.query();
    fs::rename(&fixture.root, fixture.base.join("old-root")).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    assert!(engine.poll().is_err());
    let view = handle.view();
    assert!(matches!(view.status, Status::Failed(_)));
    assert_eq!(view.coverage_gaps[0].path, fixture.root);
    assert_eq!(view.resources.inotify_fds, 0);
    assert_eq!(view.resources.session_watches, 0);
    assert_eq!(process_usage(), baseline);
    assert_eq!(all(&handle), [fixture.root.join("kept.txt")]);
}
