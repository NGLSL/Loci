//! Real Linux tests are compiled/run only on Linux. Windows does not claim them.
#![cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
use loci_experiment::linux_inotify::{Runtime, Session};
use loci_experiment::watch::{self, Limits, Recovery, Signal};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
struct Fixture {
    base: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let work = std::env::current_dir().unwrap().join("work");
        fs::create_dir_all(&work).unwrap();
        let base = work.join(format!(
            "linux-watch-fixture-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let root = base.join("data");
        fs::create_dir(&root).unwrap();
        Self {
            base: fs::canonicalize(base).unwrap(),
            root: fs::canonicalize(root).unwrap(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let work = fs::canonicalize(std::env::current_dir().unwrap().join("work")).unwrap();
        let base = fs::canonicalize(&self.base).unwrap();
        assert!(
            base.starts_with(work)
                && base
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("linux-watch-fixture-")
        );
        fs::remove_dir_all(base).unwrap();
    }
}
fn settle(runtime: &mut Runtime) {
    let before_events = runtime.events_observed;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        runtime.tick().unwrap();
        let actual = watch::scan(&runtime.root, runtime.limits, |_| Ok(()));
        if !runtime.state.dirty
            && actual.complete
            && actual.entries == runtime.state.inventory.entries
        {
            assert!(
                runtime.events_observed > before_events,
                "periodic rescan alone is not a native event pass"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "watch/recovery did not converge within 3 seconds"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn native_add_delete_file_directory_rename_and_restart() {
    let f = Fixture::new();
    let mut runtime = Runtime::new(&f.root, Limits::default()).unwrap();
    assert!(runtime.reconcile().unwrap());
    fs::create_dir(f.root.join("old")).unwrap();
    fs::write(f.root.join("old/a.rs"), "x").unwrap();
    settle(&mut runtime);
    assert!(runtime
        .state
        .inventory
        .entries
        .contains_key(&PathBuf::from("old/a.rs")));
    assert_eq!(runtime.watches(), 2);
    fs::rename(f.root.join("old/a.rs"), f.root.join("old/报告.rs")).unwrap();
    settle(&mut runtime);
    fs::rename(f.root.join("old"), f.root.join("renamed")).unwrap();
    settle(&mut runtime);
    assert!(runtime
        .state
        .inventory
        .entries
        .contains_key(&PathBuf::from("renamed/报告.rs")));
    assert_eq!(runtime.watches(), 2);
    let checkpoint = f.base.join("state.bin");
    watch::save_checkpoint(&checkpoint, &runtime.root, &runtime.state.inventory).unwrap();
    drop(runtime);
    fs::remove_file(f.root.join("renamed/报告.rs")).unwrap();
    fs::remove_dir(f.root.join("renamed")).unwrap();
    fs::write(f.root.join("offline-new"), "x").unwrap();
    let mut runtime = Runtime::new(&f.root, Limits::default()).unwrap();
    runtime.restore(watch::load_checkpoint(&checkpoint, &runtime.root, runtime.limits).unwrap());
    assert!(runtime.state.dirty);
    assert!(runtime.reconcile().unwrap());
    assert_eq!(runtime.watches(), 1);
    assert!(runtime
        .state
        .inventory
        .entries
        .contains_key(&PathBuf::from("offline-new")));
}
#[test]
fn native_initial_scan_race_is_retried() {
    let f = Fixture::new();
    let mut runtime = Runtime::new(&f.root, Limits::default()).unwrap();
    let mut once = true;
    assert!(runtime
        .reconcile_with_hook(|| {
            if once {
                fs::write(f.root.join("during-scan"), "x").unwrap();
                once = false;
            }
        })
        .unwrap());
    assert!(runtime.state.generation > 1);
    assert!(runtime.events_observed > 0);
    assert!(runtime
        .state
        .inventory
        .entries
        .contains_key(&PathBuf::from("during-scan")));
}
#[test]
fn native_watch_budget_failure_preserves_stale_state() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("child")).unwrap();
    let mut runtime = Runtime::new(
        &f.root,
        Limits {
            directories: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(!runtime.reconcile().unwrap());
    assert!(runtime.state.dirty);
    assert!(runtime.state.reasons.contains(&Signal::ScanIncomplete));
}
#[test]
fn native_move_out_and_move_in_rebuild_scope() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("d")).unwrap();
    fs::write(f.root.join("d/a"), "x").unwrap();
    let mut runtime = Runtime::new(&f.root, Limits::default()).unwrap();
    assert!(runtime.reconcile().unwrap());
    fs::rename(f.root.join("d"), f.base.join("outside")).unwrap();
    settle(&mut runtime);
    assert_eq!(runtime.watches(), 1);
    fs::rename(f.base.join("outside"), f.root.join("returned")).unwrap();
    settle(&mut runtime);
    assert_eq!(runtime.watches(), 2);
    assert!(runtime
        .state
        .inventory
        .entries
        .contains_key(&PathBuf::from("returned/a")));
}
#[test]
#[cfg(target_os = "linux")]
fn native_non_utf8_names_survive_scan_and_checkpoint() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    let name = std::ffi::OsString::from_vec(vec![0xff, 0xfe]);
    let path = f.root.join(&name);
    let mut runtime = Runtime::new(&f.root, Limits::default()).unwrap();
    assert!(runtime.reconcile().unwrap());
    fs::write(path, "x").unwrap();
    settle(&mut runtime);
    assert!(runtime
        .state
        .inventory
        .entries
        .contains_key(&PathBuf::from(name)));
    let checkpoint = f.base.join("bytes.bin");
    watch::save_checkpoint(&checkpoint, &runtime.root, &runtime.state.inventory).unwrap();
    assert_eq!(
        watch::load_checkpoint(&checkpoint, &runtime.root, runtime.limits)
            .unwrap()
            .entries,
        runtime.state.inventory.entries
    );
}
#[test]
#[ignore = "opt-in real kernel overflow; bounded fixture only, never changes sysctl"]
fn native_kernel_overflow_bounded_fixture() {
    let f = Fixture::new();
    let queue: usize = fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let cycles = queue / 2 + 32;
    if cycles > 10000 {
        println!("SKIP: queue size {queue} exceeds 10000-cycle safety budget");
        return;
    }
    let mut session = Session::new(1).unwrap();
    session.add(&f.root).unwrap();
    let started = Instant::now();
    // Alternate CREATE and DELETE; consecutive identical event coalescing cannot erase every pair.
    for _ in 0..cycles {
        if started.elapsed() > Duration::from_secs(10) {
            println!("SKIP: generation exceeded 10-second budget");
            return;
        }
        let path = f.root.join("storm");
        fs::write(&path, "x").unwrap();
        fs::remove_file(path).unwrap();
    }
    fs::write(f.root.join("survivor-after-loss"), "x").unwrap();
    let mut state = Recovery::new(256);
    for _ in 0..4 {
        session.pump(&mut state).unwrap();
        if state.reasons.contains(&Signal::KernelOverflow) {
            break;
        }
    }
    if !state.reasons.contains(&Signal::KernelOverflow) {
        println!(
            "SKIP: no real IN_Q_OVERFLOW observed; user-space truncation is not kernel overflow"
        );
        return;
    }
    println!("PASS: real IN_Q_OVERFLOW observed with queue={queue}, cycles={cycles}");
    drop(session);
    let mut native = loci_experiment::live::Native::new(&f.root, Limits::default()).unwrap();
    // The exact Recovery which observed real overflow now drives the query snapshot.
    native.watch.state = state;
    assert!(native.watch.state.reasons.contains(&Signal::KernelOverflow));
    assert!(native.tick().unwrap());
    assert!(!native.watch.state.dirty);
    let result = native
        .store
        .handle()
        .lease()
        .unwrap()
        .search(
            "survivor-after-loss",
            false,
            &std::sync::atomic::AtomicBool::new(false),
            &std::sync::atomic::AtomicUsize::new(0),
        )
        .unwrap();
    assert_eq!(result.paths, [PathBuf::from("survivor-after-loss")]);
    assert!(result.validated_at_start_and_finish);
    println!("PASS: same real-overflow Recovery converged through watcher/index/query bridge");
}
