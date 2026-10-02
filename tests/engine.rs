#![cfg(any(windows, target_os = "linux"))]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, QueryHandle, Status};
use loci_experiment::events::{Change, EventBatch, EventSource, Loss, SourceState};
use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Native tests share the backend's process-wide eight-source budget.
static NATIVE: Mutex<()> = Mutex::new(());
fn paths(handle: &QueryHandle, text: &str) -> Vec<PathBuf> {
    handle
        .lease()
        .unwrap()
        .search(text, false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap()
        .paths
}
fn settle(engine: &mut Engine, expected: &[PathBuf]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        engine
            .poll()
            .unwrap_or_else(|error| panic!("poll toward {expected:?}: {error}"));
        if engine.view().status == Status::Validated && paths(&engine.query(), "") == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "did not converge: {:?}, paths {:?}",
            engine.view(),
            paths(&engine.query(), "")
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// These explicit simulated EventSource inputs exercise only engine behavior.
// They are not evidence of kernel overflow, native delivery, or cancellation.
type Step = Box<dyn FnOnce() -> io::Result<EventBatch> + Send>;
#[derive(Clone, Default)]
struct Inputs(Arc<Mutex<VecDeque<Step>>>);
impl Inputs {
    fn batch(&self, batch: EventBatch) {
        self.step(move || Ok(batch));
    }
    fn change(&self, change: Change) {
        self.batch(EventBatch {
            changes: vec![change],
            ..EventBatch::default()
        });
    }
    fn step(&self, step: impl FnOnce() -> io::Result<EventBatch> + Send + 'static) {
        self.0.lock().unwrap().push_back(Box::new(step));
    }
}
struct SimulatedSource {
    inputs: Inputs,
    stopped: bool,
}
impl EventSource for SimulatedSource {
    fn poll(&mut self) -> io::Result<EventBatch> {
        if self.stopped {
            return Ok(EventBatch {
                state: SourceState::Stopped,
                ..EventBatch::default()
            });
        }
        let step = self.inputs.0.lock().unwrap().pop_front();
        step.map_or_else(|| Ok(EventBatch::default()), |step| step())
    }
    fn stop(&mut self) -> io::Result<()> {
        self.stopped = true;
        Ok(())
    }
}
fn simulated(f: &Fixture) -> (Engine, Inputs) {
    let inputs = Inputs::default();
    let engine = Engine::with_source(
        &f.root,
        None,
        SimulatedSource {
            inputs: inputs.clone(),
            stopped: false,
        },
    )
    .unwrap();
    (engine, inputs)
}
fn ready() {
    std::thread::sleep(Duration::from_millis(270));
}

#[test]
fn saved_root_reopens_after_offline_changes_and_readers_observe_stop() {
    let _native = NATIVE.lock().unwrap_or_else(|poison| poison.into_inner());
    let f = Fixture::new();
    let db = f.base.join("inventory.loci");
    fs::write(f.root.join("中文 old.rs"), "x").unwrap();
    fs::write(f.root.join("deleted.txt"), "x").unwrap();
    let mut engine = Engine::open(&f.root, Some(&db)).unwrap();
    let handle = engine.query();
    assert_eq!(handle.view().status, Status::Validated);
    assert_eq!(paths(&handle, "中文 ext:rs"), [f.root.join("中文 old.rs")]);
    engine.save().unwrap();
    engine.stop().unwrap();
    engine.stop().unwrap();
    assert_eq!(handle.view().status, Status::Stopped);
    fs::rename(f.root.join("中文 old.rs"), f.root.join("中文 new.rs")).unwrap();
    fs::remove_file(f.root.join("deleted.txt")).unwrap();
    fs::write(f.root.join("added.txt"), "x").unwrap();
    let reopened = Engine::open(&f.root, Some(&db)).unwrap();
    let handle = reopened.query();
    assert_eq!(handle.view().status, Status::Validated);
    assert!(paths(&handle, "old").is_empty());
    assert!(paths(&handle, "deleted").is_empty());
    assert_eq!(paths(&handle, "中文"), [f.root.join("中文 new.rs")]);
    assert_eq!(paths(&handle, "added"), [f.root.join("added.txt")]);
    drop(reopened);
    assert_eq!(handle.view().status, Status::Stopped);
    let out = handle
        .lease()
        .unwrap()
        .search("中文", false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert!(!out.validated_at_start_and_finish);
    assert!(out.complete);
}

#[test]
fn native_reliable_add_delete_file_and_directory_rename_stay_incremental() {
    let _native = NATIVE.lock().unwrap_or_else(|poison| poison.into_inner());
    let f = Fixture::new();
    fs::create_dir(f.root.join("旧目录")).unwrap();
    fs::write(f.root.join("旧目录/child.txt"), "x").unwrap();
    fs::write(f.root.join("old.rs"), "x").unwrap();
    let mut engine = Engine::open(&f.root, None).unwrap();
    let scans = engine.metrics().full_scans;
    fs::write(f.root.join("added.txt"), "x").unwrap();
    settle(
        &mut engine,
        &[
            f.root.join("added.txt"),
            f.root.join("old.rs"),
            f.root.join("旧目录"),
            f.root.join("旧目录/child.txt"),
        ],
    );
    fs::remove_file(f.root.join("added.txt")).unwrap();
    settle(
        &mut engine,
        &[
            f.root.join("old.rs"),
            f.root.join("旧目录"),
            f.root.join("旧目录/child.txt"),
        ],
    );
    fs::rename(f.root.join("old.rs"), f.root.join("new.rs")).unwrap();
    settle(
        &mut engine,
        &[
            f.root.join("new.rs"),
            f.root.join("旧目录"),
            f.root.join("旧目录/child.txt"),
        ],
    );
    fs::rename(f.root.join("旧目录"), f.root.join("新目录")).unwrap();
    settle(
        &mut engine,
        &[
            f.root.join("new.rs"),
            f.root.join("新目录"),
            f.root.join("新目录/child.txt"),
        ],
    );
    assert_eq!(
        engine.metrics().full_scans,
        scans,
        "ordinary reliable changes must not full-scan"
    );
    assert!(engine.metrics().transactions >= 4);
    assert!(paths(&engine.query(), "")
        .iter()
        .all(|path| path.is_absolute()));
}

#[test]
fn empty_native_root_is_validated_and_drop_stops_remaining_handles() {
    let _native = NATIVE.lock().unwrap_or_else(|poison| poison.into_inner());
    let f = Fixture::new();
    let engine = Engine::open(&f.root, None).unwrap();
    let handle = engine.query();
    assert_eq!(handle.view().status, Status::Validated);
    assert!(paths(&handle, "").is_empty());
    drop(engine);
    assert_eq!(handle.view().status, Status::Stopped);
}

#[test]
fn native_root_rename_and_replacement_fail_without_retargeting_old_query() {
    let _native = NATIVE.lock().unwrap_or_else(|poison| poison.into_inner());
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
        assert_eq!(paths(&handle, ""), [f.root.join("original.txt")]);
        assert!(
            !handle
                .lease()
                .unwrap()
                .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
                .unwrap()
                .validated_at_start_and_finish
        );
    }
}

#[test]
fn simulated_loss_discards_changes_and_reconciles_actual_root() {
    let f = Fixture::new();
    fs::write(f.root.join("kept.txt"), "x").unwrap();
    let (mut engine, inputs) = simulated(&f);
    let scans = engine.metrics().full_scans;
    inputs.batch(EventBatch {
        changes: vec![
            Change::Remove("kept.txt".into()),
            Change::Refresh("../unsafe".into()),
        ],
        losses: [Loss::KernelOverflow].into(),
        ..EventBatch::default()
    });
    assert!(!engine.poll().unwrap());
    assert_eq!(engine.view().status, Status::Pending);
    assert_eq!(paths(&engine.query(), ""), [f.root.join("kept.txt")]);
    settle(&mut engine, &[f.root.join("kept.txt")]);
    assert!(engine.metrics().full_scans > scans);
}

#[test]
fn simulated_capture_during_scan_and_build_rejects_stale_candidates() {
    for after_build in [false, true] {
        let f = Fixture::new();
        fs::write(f.root.join("old.txt"), "x").unwrap();
        let (mut engine, inputs) = simulated(&f);
        let handle = engine.query();
        let version = handle.view().version;
        ready();
        inputs.batch(EventBatch {
            losses: [Loss::BackendRestart].into(),
            ..EventBatch::default()
        });
        if after_build {
            inputs.batch(EventBatch::default());
        }
        let root = f.root.clone();
        inputs.step(move || {
            fs::write(root.join("racing.txt"), "x")?;
            Ok(EventBatch {
                changes: vec![Change::Refresh("racing.txt".into())],
                ..EventBatch::default()
            })
        });
        let published = engine.poll().unwrap();
        if published {
            assert_eq!(
                paths(&handle, ""),
                [f.root.join("old.txt"), f.root.join("racing.txt")]
            );
        } else {
            assert_eq!(handle.view().version, version);
            assert_eq!(paths(&handle, ""), [f.root.join("old.txt")]);
            assert_ne!(handle.view().status, Status::Validated);
        }
        settle(
            &mut engine,
            &[f.root.join("old.txt"), f.root.join("racing.txt")],
        );
    }
}

#[test]
fn simulated_fatal_error_watch_lost_and_stopped_keep_distinct_states() {
    for case in 0..4 {
        let f = Fixture::new();
        fs::write(f.root.join("kept.txt"), "x").unwrap();
        let (mut engine, inputs) = simulated(&f);
        let handle = engine.query();
        match case {
            0 => inputs.step(|| Err(io::Error::from_raw_os_error(5))),
            1 => inputs.batch(EventBatch {
                losses: [Loss::WatchLost].into(),
                ..EventBatch::default()
            }),
            3 => inputs.batch(EventBatch {
                state: SourceState::Stopped,
                losses: [Loss::WatchLost].into(),
                ..EventBatch::default()
            }),
            _ => inputs.batch(EventBatch {
                state: SourceState::Stopped,
                ..EventBatch::default()
            }),
        }
        let result = engine.poll();
        if case == 2 {
            assert!(!result.unwrap());
            assert_eq!(handle.view().status, Status::Stopped);
        } else {
            let error = result.unwrap_err();
            if case == 0 {
                assert_eq!(error.raw_os_error(), Some(5));
            }
            assert!(matches!(handle.view().status, Status::Failed(_)));
        }
        assert_eq!(paths(&handle, ""), [f.root.join("kept.txt")]);
    }
}

#[test]
fn simulated_whole_batch_validation_precedes_inspection() {
    let bad_paths = vec![
        "../escape.txt",
        "a/./b",
        "a//b",
        "a/",
        "nul\0name",
        "",
        "/absolute",
    ];
    #[cfg(windows)]
    let bad_paths = {
        let mut paths = bad_paths;
        paths.extend(["a\\.\\b", "a\\\\b", "a\\", "file:stream", "C:\\absolute"]);
        paths
    };
    for bad in bad_paths {
        let f = Fixture::new();
        fs::write(f.root.join("kept.txt"), "x").unwrap();
        let (mut engine, inputs) = simulated(&f);
        let calls = engine.metrics().metadata_calls;
        inputs.batch(EventBatch {
            changes: vec![
                Change::Refresh("kept.txt".into()),
                Change::Rename {
                    from: "kept.txt".into(),
                    to: bad.into(),
                },
            ],
            ..EventBatch::default()
        });
        assert!(engine.poll().is_err(), "accepted unsafe batch path {bad:?}");
        assert!(matches!(engine.view().status, Status::Failed(_)));
        assert_eq!(
            engine.metrics().metadata_calls,
            calls,
            "inspected filesystem before validating {bad:?}"
        );
        assert_eq!(paths(&engine.query(), ""), [f.root.join("kept.txt")]);
    }
}

#[test]
fn simulated_leases_are_immutable_and_two_generation_backpressure_recovers() {
    let f = Fixture::new();
    fs::write(f.root.join("old.txt"), "x").unwrap();
    let (mut engine, inputs) = simulated(&f);
    let handle = engine.query();
    let old = handle.lease().unwrap();
    let mut leases = (0..7).map(|_| handle.lease().unwrap()).collect::<Vec<_>>();
    assert!(handle.lease().is_err());
    leases.clear();
    assert_eq!(handle.view().leases, 1);
    fs::write(f.root.join("second.txt"), "x").unwrap();
    inputs.change(Change::Refresh("second.txt".into()));
    settle(
        &mut engine,
        &[f.root.join("old.txt"), f.root.join("second.txt")],
    );
    let old_result = old
        .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(old_result.paths, [f.root.join("old.txt")]);
    assert!(!old_result.validated_at_start_and_finish);
    let second_version = handle.view().version;
    fs::write(f.root.join("third.txt"), "x").unwrap();
    inputs.change(Change::Refresh("third.txt".into()));
    ready();
    assert!(!engine.poll().unwrap());
    assert_eq!(handle.view().status, Status::ReadersPinned);
    assert_eq!(handle.view().version, second_version);
    drop(old);
    settle(
        &mut engine,
        &[
            f.root.join("old.txt"),
            f.root.join("second.txt"),
            f.root.join("third.txt"),
        ],
    );
}

#[test]
fn queries_report_cancellation_first50_and_complete_counts() {
    let f = Fixture::new();
    for n in 0..70 {
        fs::write(f.root.join(format!("match-{n:02}.txt")), "x").unwrap();
    }
    let (engine, _) = simulated(&f);
    let lease = engine.query().lease().unwrap();
    let cancelled = lease
        .search("match", false, &AtomicBool::new(true), &AtomicUsize::new(0))
        .unwrap();
    assert!(cancelled.cancelled);
    assert!(!cancelled.complete);
    let first = lease
        .search("match", true, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(first.paths.len(), 50);
    assert!(!first.complete);
    let all = lease
        .search(
            "match",
            false,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap();
    assert_eq!(all.matches, 70);
    assert_eq!(
        all.paths,
        (0..50)
            .map(|n| f.root.join(format!("match-{n:02}.txt")))
            .collect::<Vec<_>>()
    );
    assert!(all.complete);
    assert!(all.validated_at_start_and_finish);
}

#[cfg(windows)]
#[test]
fn non_utf8_windows_name_rejects_candidate_and_preserves_old_query() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    let f = Fixture::new();
    fs::write(f.root.join("kept.txt"), "x").unwrap();
    let (mut engine, inputs) = simulated(&f);
    let path = PathBuf::from(OsString::from_wide(&[b'x' as u16, 0xd800]));
    fs::write(f.root.join(&path), "x").unwrap();
    inputs.change(Change::Refresh(path));
    ready();
    assert!(engine.poll().unwrap_err().to_string().contains("UTF-8"));
    assert!(matches!(engine.view().status, Status::Failed(_)));
    assert_eq!(paths(&engine.query(), ""), [f.root.join("kept.txt")]);
}

#[test]
fn corrupt_database_open_is_read_only_and_database_inside_root_is_rejected() {
    let f = Fixture::new();
    let db = f.base.join("corrupt.loci");
    let bytes = b"corrupt database bytes";
    fs::write(&db, bytes).unwrap();
    let inputs = Inputs::default();
    assert!(Engine::with_source(
        &f.root,
        Some(&db),
        SimulatedSource {
            inputs,
            stopped: false
        }
    )
    .is_err());
    assert_eq!(fs::read(&db).unwrap(), bytes);
    let inside = f.root.join("inventory.loci");
    let inputs = Inputs::default();
    let result = Engine::with_source(
        &f.root,
        Some(&inside),
        SimulatedSource {
            inputs,
            stopped: false,
        },
    );
    assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidInput));
    assert!(!inside.exists());
}

#[test]
fn engine_cross_process_child() {
    let Some(mode) = std::env::var_os("LOCI_ENGINE_CHILD_MODE") else {
        return;
    };
    let root = PathBuf::from(std::env::var_os("LOCI_ENGINE_CHILD_ROOT").unwrap());
    let db = PathBuf::from(std::env::var_os("LOCI_ENGINE_CHILD_DB").unwrap());
    let mut engine = Engine::open(&root, Some(&db)).unwrap();
    assert_eq!(engine.view().status, Status::Validated);
    if mode == "save" {
        assert_eq!(
            paths(&engine.query(), ""),
            [root.join("deleted.txt"), root.join("中文 old.rs")]
        );
        engine.save().unwrap();
    } else {
        assert_eq!(mode, "reopen");
        // Literal independent oracle: no scanner or matcher supplies expectations.
        assert_eq!(
            paths(&engine.query(), ""),
            [root.join("added.txt"), root.join("中文 new.rs")]
        );
        assert!(paths(&engine.query(), "deleted").is_empty());
        assert!(paths(&engine.query(), "old").is_empty());
    }
    engine.stop().unwrap();
    assert_eq!(engine.view().status, Status::Stopped);
}

#[test]
fn native_snapshot_reopens_in_another_process_after_offline_changes() {
    let f = Fixture::new();
    let db = f.base.join("cross-process.loci");
    fs::write(f.root.join("deleted.txt"), "x").unwrap();
    fs::write(f.root.join("中文 old.rs"), "x").unwrap();
    let run = |mode: &str| {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "engine_cross_process_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("LOCI_ENGINE_CHILD_MODE", mode)
            .env("LOCI_ENGINE_CHILD_ROOT", &f.root)
            .env("LOCI_ENGINE_CHILD_DB", &db)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("cross-process engine child {mode} exceeded 10 seconds");
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
    fs::write(f.root.join("added.txt"), "x").unwrap();
    run("reopen");
}

#[test]
fn simulated_stopped_during_scan_or_build_keeps_snapshot_and_stopped_state() {
    for after_build in [false, true] {
        let f = Fixture::new();
        fs::write(f.root.join("kept.txt"), "x").unwrap();
        let (mut engine, inputs) = simulated(&f);
        let handle = engine.query();
        let version = handle.view().version;
        ready();
        inputs.batch(EventBatch {
            losses: [Loss::BackendRestart].into(),
            ..EventBatch::default()
        });
        if after_build {
            inputs.batch(EventBatch::default());
        }
        inputs.batch(EventBatch {
            state: SourceState::Stopped,
            ..EventBatch::default()
        });
        assert!(!engine.poll().unwrap());
        assert_eq!(handle.view().status, Status::Stopped);
        assert_eq!(handle.view().version, version);
        assert_eq!(paths(&handle, ""), [f.root.join("kept.txt")]);
        assert!(!engine.poll().unwrap());
        assert!(engine.save().is_err());
    }
}

#[test]
fn simulated_stop_error_still_marks_remaining_query_handles_stopped() {
    struct StopErrorSource;
    impl EventSource for StopErrorSource {
        fn poll(&mut self) -> io::Result<EventBatch> {
            Ok(EventBatch::default())
        }
        fn stop(&mut self) -> io::Result<()> {
            Err(io::Error::from_raw_os_error(5))
        }
    }
    let f = Fixture::new();
    fs::write(f.root.join("kept.txt"), "x").unwrap();
    let mut engine = Engine::with_source(&f.root, None, StopErrorSource).unwrap();
    let handle = engine.query();
    assert_eq!(engine.stop().unwrap_err().raw_os_error(), Some(5));
    assert_eq!(handle.view().status, Status::Stopped);
    assert_eq!(paths(&handle, ""), [f.root.join("kept.txt")]);
    assert!(!engine.poll().unwrap());
    engine.stop().unwrap();
}
