#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, QueryLease, Status};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

fn paths(lease: &QueryLease) -> Vec<PathBuf> {
    let mut cursor = None;
    let mut out = Vec::new();
    loop {
        let page = lease
            .page(
                "",
                cursor.as_ref(),
                2,
                &AtomicBool::new(false),
                &AtomicUsize::new(0),
            )
            .unwrap();
        out.extend(page.paths);
        if page.complete {
            break;
        }
        cursor = page.next;
    }
    out.sort();
    out
}
fn settle(engine: &mut Engine, version: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.view().version <= version || engine.view().status != Status::Validated {
        engine.poll().unwrap();
        assert!(
            !matches!(engine.view().status, Status::Failed(_)),
            "{:?}",
            engine.view()
        );
        assert!(Instant::now() < deadline, "{:?}", engine.view());
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn sustained_native_churn_reclaims_slots_without_rescanning_the_root() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed"), b"").unwrap();
    let mut options = EngineOptions::scale();
    options.scale_budgets.max_slots = 6;
    options.scale_budgets.max_name_bytes = 64;
    options.scan_batch = 1;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    while engine.view().status != Status::Validated {
        engine.poll().unwrap();
    }
    let scans = engine.metrics().full_scans;
    for number in 0..24 {
        let path = fixture.root.join(format!("file{number:02}"));
        let version = engine.view().version;
        fs::write(&path, b"").unwrap();
        settle(&mut engine, version);
        assert_eq!(
            paths(&engine.query().lease().unwrap()),
            [path.clone(), fixture.root.join("seed")]
        );
        let version = engine.view().version;
        fs::remove_file(path).unwrap();
        settle(&mut engine, version);
        assert_eq!(
            paths(&engine.query().lease().unwrap()),
            [fixture.root.join("seed")]
        );
        assert_eq!(
            engine.metrics().full_scans,
            scans,
            "garbage reclamation rescanned the root"
        );
        assert!(engine.view().resources.inventory_slots <= 6);
        assert!(engine.view().resources.inventory_name_bytes <= 64);
    }
}

#[test]
fn an_old_snapshot_keeps_directory_paths_through_compaction_and_reopen() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("old")).unwrap();
    fs::write(fixture.root.join("old/leaf"), b"").unwrap();
    let database = fixture.base.join("index.loci");
    let mut options = EngineOptions::scale();
    options.scan_batch = 1;
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), options.clone()).unwrap();
    while engine.view().status != Status::Validated {
        engine.poll().unwrap();
    }
    let old = engine.query().lease().unwrap();
    let old_paths = [fixture.root.join("old"), fixture.root.join("old/leaf")];
    let version = engine.view().version;
    fs::rename(fixture.root.join("old"), fixture.root.join("new")).unwrap();
    settle(&mut engine, version);
    engine.request_compaction().unwrap();
    for _ in 0..4 {
        engine.poll().unwrap();
    }
    assert_eq!(paths(&old), old_paths);
    assert_eq!(engine.view().status, Status::ReadersPinned);
    drop(old);
    let version = engine.view().version;
    settle(&mut engine, version);
    assert_eq!(engine.metrics().compactions, 1);
    let expected = [fixture.root.join("new"), fixture.root.join("new/leaf")];
    assert_eq!(paths(&engine.query().lease().unwrap()), expected);
    engine.save().unwrap();
    engine.stop().unwrap();
    drop(engine);
    let mut reopened = Engine::open_with_options(&fixture.root, Some(&database), options).unwrap();
    assert_eq!(paths(&reopened.query().lease().unwrap()), expected);
    let version = reopened.view().version;
    settle(&mut reopened, version);
    assert_eq!(paths(&reopened.query().lease().unwrap()), expected);
}

#[test]
fn process_memory_admission_includes_snapshots_after_the_writer_drops() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed"), b"").unwrap();
    let mut options = EngineOptions::scale();
    options.limits.entries = 16;
    options.limits.directories = 16;
    options.watch_limit = 16;
    options.scale_budgets.max_slots = 32;
    options.scale_budgets.max_retained_bytes =
        loci_experiment::engine::PROCESS_MEMORY_LIMIT * 2 / 3;
    let engine = Engine::open_with_options(&fixture.root, None, options.clone()).unwrap();
    let handle = engine.query();
    let lease = handle.lease().unwrap();
    assert!(
        matches!(Engine::open_with_options(&fixture.root, None, options.clone()),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    drop(engine);
    assert_eq!(paths(&lease), [fixture.root.join("seed")]);
    assert!(
        matches!(Engine::open_with_options(&fixture.root, None, options.clone()),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    drop(lease);
    drop(handle);
    let reopened = Engine::open_with_options(&fixture.root, None, options).unwrap();
    assert_eq!(
        paths(&reopened.query().lease().unwrap()),
        [fixture.root.join("seed")]
    );
}

#[test]
fn quiet_polls_budget_mount_table_checks_while_native_changes_remain_visible() {
    let fixture = Fixture::new();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let checks = engine.metrics().scope_checks;
    for _ in 0..100 {
        engine.poll().unwrap();
    }
    assert!(
        engine.metrics().scope_checks - checks <= 1,
        "quiet polls repeatedly parsed mount tables"
    );
    let version = engine.view().version;
    fs::write(fixture.root.join("new"), b"").unwrap();
    settle(&mut engine, version);
    assert_eq!(
        paths(&engine.query().lease().unwrap()),
        [fixture.root.join("new")]
    );
}

#[test]
fn mutation_and_cancellation_of_a_batched_compaction_preserve_queryable_cuts() {
    let fixture = Fixture::new();
    let mut expected = Vec::new();
    for number in 0..40 {
        let path = fixture.root.join(format!("seed{number:02}"));
        fs::write(&path, b"").unwrap();
        expected.push(path);
    }
    let mut options = EngineOptions::scale();
    options.scan_batch = 1;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    while engine.view().status != Status::Validated {
        engine.poll().unwrap();
    }
    let old = engine.query().lease().unwrap();
    let version = engine.view().version;
    engine.request_compaction().unwrap();
    engine.poll().unwrap();
    assert!(engine.view().resources.compaction_in_progress);
    engine.poll_with_cancel(&AtomicBool::new(true)).unwrap();
    assert!(!engine.view().resources.compaction_in_progress);
    assert_eq!(engine.view().version, version);
    assert_eq!(paths(&old), expected);
    engine.poll().unwrap();
    let added = fixture.root.join("added");
    fs::write(&added, b"").unwrap();
    engine.poll().unwrap();
    assert!(engine.view().version > version);
    assert!(
        !old.page("", None, 2, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap()
            .validated_at_start_and_finish
    );
    drop(old);
    settle(&mut engine, version);
    expected.push(added);
    expected.sort();
    assert_eq!(paths(&engine.query().lease().unwrap()), expected);
    assert_eq!(engine.metrics().full_scans, 1);
    assert!(engine.metrics().compaction_restarts > 0);
    let deadline = Instant::now() + Duration::from_secs(2);
    while engine.metrics().compactions == 0 {
        assert!(Instant::now() < deadline);
        engine.poll().unwrap();
    }
    assert_eq!(paths(&engine.query().lease().unwrap()), expected);
}

#[test]
fn repeated_native_renames_reclaim_obsolete_names_within_the_arena_budget() {
    let fixture = Fixture::new();
    let mut path = fixture.root.join("name00");
    fs::write(&path, b"").unwrap();
    let mut options = EngineOptions::scale();
    options.scale_budgets.max_name_bytes = 12;
    options.scan_batch = 1;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    while engine.view().status != Status::Validated {
        engine.poll().unwrap();
    }
    for number in 1..24 {
        let next = fixture.root.join(format!("name{number:02}"));
        let version = engine.view().version;
        fs::rename(&path, &next).unwrap();
        settle(&mut engine, version);
        assert_eq!(paths(&engine.query().lease().unwrap()), [next.clone()]);
        assert!(engine.view().resources.inventory_name_bytes <= 12);
        path = next;
    }
    assert_eq!(engine.metrics().full_scans, 1);
    assert!(engine.metrics().reclaimed_name_bytes > 0);
}

#[test]
fn cli_compaction_can_pause_resume_and_query_while_reporting_resource_admission() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    let fixture = Fixture::new();
    for number in 0..40 {
        fs::write(fixture.root.join(format!("seed{number:02}")), b"").unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "watch"])
        .arg(&fixture.root)
        .arg(fixture.base.join("index.loci"))
        .args(["--scale", "--scan-batch", "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    let mut input = child.stdin.take().unwrap();
    let wait = |needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut logs = Vec::new();
        loop {
            let line = rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            let matched = line.contains(needle);
            logs.push(line);
            if matched {
                return logs;
            }
        }
    };
    wait("watch-ready");
    writeln!(input, "compact").unwrap();
    wait("command=compact,ok=true");
    std::thread::sleep(Duration::from_millis(40));
    writeln!(input, "query seed00\nstatus").unwrap();
    let logs = wait("command=status,ok=true");
    let memory_reported = logs
        .iter()
        .any(|line| line.contains("process_memory_reserved_bytes="));
    assert!(logs
        .iter()
        .any(|line| line.contains("compaction_in_progress=true")));
    if !logs
        .iter()
        .any(|line| line.contains("command=query,ok=true"))
    {
        wait("command=query,ok=true");
    }
    writeln!(input, "cancel").unwrap();
    wait("command=cancel,ok=true");
    std::thread::sleep(Duration::from_millis(50));
    writeln!(input, "status").unwrap();
    assert!(wait("command=status,ok=true")
        .iter()
        .any(|line| line.contains("compaction_in_progress=false")));
    writeln!(input, "compact").unwrap();
    wait("command=compact,ok=true");
    wait("version=2,state=Validated");
    writeln!(input, "stop").unwrap();
    drop(input);
    let status = child.wait().unwrap();
    reader.join().unwrap();
    assert!(status.success());
    assert!(
        memory_reported,
        "CLI did not report its process memory admission"
    );
}

#[test]
fn ordinary_native_updates_publish_within_500ms_during_a_long_compaction() {
    let fixture = Fixture::new();
    for number in 0..60 {
        fs::write(fixture.root.join(format!("seed{number:02}")), b"").unwrap();
    }
    let mut options = EngineOptions::scale();
    options.scan_batch = 1;
    let mut owner = Engine::open_with_options(&fixture.root, None, options)
        .unwrap()
        .spawn()
        .unwrap();
    let ready = Instant::now() + Duration::from_secs(5);
    while owner.view().status != Status::Validated {
        assert!(Instant::now() < ready);
        std::thread::sleep(Duration::from_millis(5));
    }
    owner
        .request_compaction()
        .unwrap()
        .wait(Duration::from_secs(2))
        .unwrap();
    while !owner.view().resources.compaction_in_progress {
        assert!(Instant::now() < ready);
        std::thread::sleep(Duration::from_millis(2));
    }
    let query = owner.query();
    for number in 0..3 {
        let added = fixture.root.join(format!("new-visible-{number}"));
        let began = Instant::now();
        fs::write(&added, b"").unwrap();
        loop {
            let page = query
                .lease()
                .unwrap()
                .page(
                    &format!("new-visible-{number}"),
                    None,
                    50,
                    &AtomicBool::new(false),
                    &AtomicUsize::new(0),
                )
                .unwrap();
            if page.paths == [added.clone()] && page.validated_at_start_and_finish {
                break;
            }
            assert!(
                began.elapsed() < Duration::from_millis(500),
                "ordinary event waited behind compaction: {:?}, {:?}",
                owner.view(),
                owner.metrics()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    assert_eq!(owner.metrics().full_scans, 1);
    owner.stop(Duration::from_secs(2)).unwrap();
}

#[test]
fn simulated_reliable_batches_arriving_during_apply_do_not_rescan_the_root() {
    use loci_experiment::events::{Change, EventBatch, EventSource};
    use std::sync::{atomic::Ordering, Arc};
    struct Consecutive {
        enabled: Arc<AtomicBool>,
        polls: usize,
    }
    impl EventSource for Consecutive {
        fn stop(&mut self) -> std::io::Result<()> {
            Ok(())
        }
        fn poll(&mut self) -> std::io::Result<EventBatch> {
            if !self.enabled.load(Ordering::Relaxed) {
                return Ok(EventBatch::default());
            }
            self.polls += 1;
            Ok(EventBatch {
                changes: match self.polls {
                    1 => vec![Change::Refresh(PathBuf::from("a"))],
                    2 => vec![Change::Refresh(PathBuf::from("b"))],
                    _ => vec![],
                },
                ..EventBatch::default()
            })
        }
    }
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed"), b"").unwrap();
    let enabled = Arc::new(AtomicBool::new(false));
    let mut engine = Engine::with_source_and_options(
        &fixture.root,
        None,
        Consecutive {
            enabled: enabled.clone(),
            polls: 0,
        },
        EngineOptions::scale(),
    )
    .unwrap();
    let old = engine.query().lease().unwrap();
    let version = engine.view().version;
    fs::write(fixture.root.join("a"), b"").unwrap();
    fs::write(fixture.root.join("b"), b"").unwrap();
    enabled.store(true, Ordering::Relaxed);
    engine.poll().unwrap();
    assert_eq!(engine.view().version, version);
    assert_eq!(paths(&old), [fixture.root.join("seed")]);
    settle(&mut engine, version);
    assert_eq!(
        paths(&engine.query().lease().unwrap()),
        [
            fixture.root.join("a"),
            fixture.root.join("b"),
            fixture.root.join("seed")
        ]
    );
    assert_eq!(engine.metrics().full_scans, 1);
}
