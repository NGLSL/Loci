#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, Status};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};
fn all(engine: &Engine) -> Vec<PathBuf> {
    let lease = engine.query().lease().unwrap();
    let mut cursor = None;
    let mut paths = vec![];
    loop {
        let page = lease
            .page(
                "",
                cursor.as_ref(),
                32,
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
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let _ = engine.poll();
        if engine.view().status == Status::Validated && all(engine) == expected {
            return;
        }
        assert!(Instant::now() < deadline, "{:?}", engine.view());
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn correction_can_be_cancelled_without_losing_old_queries_then_explicitly_restarted() {
    let fixture = Fixture::new();
    for n in 0..40 {
        fs::write(fixture.root.join(format!("item-{n:03}.txt")), b"x").unwrap();
    }
    let mut expected: Vec<_> = fs::read_dir(&fixture.root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    expected.sort();
    let mut options = EngineOptions::scale();
    options.scan_batch = 8;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    settle(&mut engine, &expected);
    let version = engine.view().version;
    engine.request_rebuild().unwrap();
    assert!(!engine.poll_with_cancel(&AtomicBool::new(true)).unwrap());
    assert_eq!(all(&engine), expected);
    let scanned = engine.metrics().scanned_entries;
    for _ in 0..20 {
        assert!(!engine.poll().unwrap());
    }
    assert_eq!(engine.metrics().scanned_entries, scanned);
    assert_eq!(engine.view().version, version);
    engine.request_rebuild().unwrap();
    settle(&mut engine, &expected);
    assert!(engine.view().version > version);
}

#[test]
fn actual_kernel_overflow_marker_is_observed_before_replacing_source_and_correcting() {
    use loci_experiment::events::{EventLimits, Loss};
    let fixture = Fixture::new();
    fs::write(fixture.root.join("kept.txt"), b"x").unwrap();
    let capacity: usize = fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        (1..=131072).contains(&capacity),
        "native fixture kernel queue budget: {capacity}"
    );
    let mut options = EngineOptions::scale();
    options.event_limits = EventLimits::new(8, 4096).unwrap();
    options.scan_batch = 1;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    settle(&mut engine, &[fixture.root.join("kept.txt")]);
    let old = engine.query().lease().unwrap();
    let transient = fixture.root.join("transient.txt");
    for _ in 0..capacity + 512 {
        fs::write(&transient, b"x").unwrap();
        fs::remove_file(&transient).unwrap();
    }
    fs::write(fixture.root.join("survivor.txt"), b"x").unwrap();
    for _ in 0..512 {
        let _ = engine.poll();
        if engine
            .view()
            .observed_losses
            .contains(&Loss::KernelOverflow)
        {
            break;
        }
    }
    assert!(
        engine
            .view()
            .observed_losses
            .contains(&Loss::KernelOverflow),
        "actual native IN_Q_OVERFLOW marker missing: {:?}",
        engine.view()
    );
    let page = old
        .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(page.paths, [fixture.root.join("kept.txt")]);
    assert!(!page.validated_at_start_and_finish);
    drop(old);
    let mut expected: Vec<_> = fs::read_dir(&fixture.root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    expected.sort();
    settle(&mut engine, &expected);
    assert!(
        engine
            .view()
            .observed_losses
            .contains(&Loss::KernelOverflow),
        "history remains observable after reliable recovery"
    );
    eprintln!("native_scale_kernel_overflow_verified capacity={capacity}");
}

#[derive(Clone, Default)]
struct Simulated(
    std::sync::Arc<
        std::sync::Mutex<std::collections::VecDeque<loci_experiment::events::EventBatch>>,
    >,
);
impl loci_experiment::events::EventSource for Simulated {
    fn poll(&mut self) -> std::io::Result<loci_experiment::events::EventBatch> {
        Ok(self.0.lock().unwrap().pop_front().unwrap_or_default())
    }
    fn stop(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[test]
fn simulated_loss_discards_changes_and_corrects_real_directory_with_old_pages_retained() {
    use loci_experiment::events::{Change, EventBatch, Loss};
    let fixture = Fixture::new();
    fs::write(fixture.root.join("old.txt"), b"x").unwrap();
    let source = Simulated::default();
    let injection = source.clone();
    let mut options = EngineOptions::scale();
    options.scan_batch = 1;
    let mut engine = Engine::with_source_and_options(&fixture.root, None, source, options).unwrap();
    settle(&mut engine, &[fixture.root.join("old.txt")]);
    let old = engine.query().lease().unwrap();
    fs::remove_file(fixture.root.join("old.txt")).unwrap();
    fs::write(fixture.root.join("new.txt"), b"x").unwrap();
    injection.0.lock().unwrap().push_back(EventBatch {
        changes: vec![Change::Remove(PathBuf::from("new.txt"))],
        losses: [Loss::UserOverflow, Loss::InvalidEvent].into(),
        ..Default::default()
    });
    engine.poll().unwrap();
    let page = old
        .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(page.paths, [fixture.root.join("old.txt")]);
    assert!(!page.validated_at_start_and_finish);
    drop(old);
    settle(&mut engine, &[fixture.root.join("new.txt")]);
    assert!(engine.view().observed_losses.contains(&Loss::UserOverflow));
    assert!(engine.view().observed_losses.contains(&Loss::InvalidEvent));
}

#[test]
fn native_permission_revocation_invalidates_directory_before_it_can_stay_validated() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let blocked = fixture.root.join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("old.txt"), b"x").unwrap();
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let restore = Restore(blocked.clone());
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let old = all(&engine);
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let _ = engine.poll();
        if !engine.view().coverage_gaps.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "permission revocation stayed {:?}",
            engine.view()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(engine.view().status, Status::Failed(_)));
    assert!(engine
        .view()
        .coverage_gaps
        .iter()
        .any(|gap| gap.path == blocked
            && gap.kind == loci_experiment::engine::CoverageGapKind::Permission
            && gap.errno == Some(13)));
    assert_eq!(all(&engine), old);
    drop(restore);
    engine.request_rebuild().unwrap();
    settle(&mut engine, &old);
}

#[test]
fn rotating_coverage_audit_is_bounded_without_full_scans_and_detects_unreported_permission_loss() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let blocked = fixture.root.join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("old.txt"), b"x").unwrap();
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let restore = Restore(blocked.clone());
    let mut options = EngineOptions::scale();
    options.recovery.audit_interval = Duration::ZERO;
    options.recovery.audit_batch = 1;
    let mut engine =
        Engine::with_source_and_options(&fixture.root, None, Simulated::default(), options)
            .unwrap();
    let expected = all(&engine);
    let scans = engine.metrics().full_scans;
    for _ in 0..20 {
        let before = engine.metrics().audited_directories;
        engine.poll().unwrap();
        assert!(engine.metrics().audited_directories - before <= 1);
    }
    assert_eq!(
        engine.metrics().full_scans,
        scans,
        "quiet audits don't enumerate whole root"
    );
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0)).unwrap();
    for _ in 0..4 {
        let _ = engine.poll();
        if matches!(engine.view().status, Status::Failed(_)) {
            break;
        }
    }
    assert!(matches!(engine.view().status, Status::Failed(_)));
    assert_eq!(all(&engine), expected);
    assert!(engine
        .view()
        .coverage_gaps
        .iter()
        .any(|gap| gap.path == blocked && gap.errno == Some(13)));
    drop(restore);
    engine.request_rebuild().unwrap();
    settle(&mut engine, &expected);
}

#[test]
fn continuous_simulated_loss_exhausts_finite_attempts_then_explicit_retry_can_recover() {
    use loci_experiment::events::{EventBatch, EventSource, Loss};
    #[derive(Clone)]
    struct Storm(std::sync::Arc<AtomicBool>);
    impl EventSource for Storm {
        fn poll(&mut self) -> std::io::Result<EventBatch> {
            Ok(if self.0.load(std::sync::atomic::Ordering::Relaxed) {
                EventBatch {
                    losses: [Loss::InvalidEvent].into(),
                    ..Default::default()
                }
            } else {
                EventBatch::default()
            })
        }
        fn stop(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let fixture = Fixture::new();
    fs::write(fixture.root.join("kept.txt"), b"x").unwrap();
    let active = std::sync::Arc::new(AtomicBool::new(false));
    let mut options = EngineOptions::scale();
    options.recovery.retry_limit = 2;
    options.recovery.retry_delay = Duration::ZERO;
    let mut engine =
        Engine::with_source_and_options(&fixture.root, None, Storm(active.clone()), options)
            .unwrap();
    active.store(true, std::sync::atomic::Ordering::Relaxed);
    engine.request_rebuild().unwrap();
    for _ in 0..20 {
        let _ = engine.poll();
    }
    assert!(matches!(engine.view().status, Status::Failed(_)));
    let scans = engine.metrics().full_scans;
    for _ in 0..20 {
        assert!(!engine.poll().unwrap());
    }
    assert_eq!(
        engine.metrics().full_scans,
        scans,
        "exhausted correction can't spin"
    );
    assert_eq!(all(&engine), [fixture.root.join("kept.txt")]);
    active.store(false, std::sync::atomic::Ordering::Relaxed);
    engine.request_rebuild().unwrap();
    settle(&mut engine, &[fixture.root.join("kept.txt")]);
}

#[test]
fn cli_watch_can_cancel_rebuild_show_old_results_and_recover_permission_failure() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    let fixture = Fixture::new();
    for n in 0..40 {
        fs::write(fixture.root.join(format!("item-{n:03}")), b"x").unwrap();
    }
    fs::write(fixture.root.join("kept.txt"), b"x").unwrap();
    let blocked = fixture.root.join("blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("child.txt"), b"x").unwrap();
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let restore = Restore(blocked.clone());
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
            .args(["engine", "watch"])
            .arg(&fixture.root)
            .arg(fixture.base.join("state.loci"))
            .args(["--scale", "--scan-batch", "1", "--null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut input = child.0.stdin.take().unwrap();
    let mut output = child.0.stdout.take().unwrap();
    let stderr = child.0.stderr.take().unwrap();
    let (send, recv) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if send.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let wait = |needle: &str| {
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut lines = vec![];
        loop {
            let line = recv
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("CLI status timeout");
            let matched = line.contains(needle);
            lines.push(line);
            if matched {
                return lines;
            }
        }
    };
    wait("watch-ready");
    writeln!(input, "rebuild\ncancel").unwrap();
    wait("command=cancel,ok=true");
    writeln!(input, "query kept").unwrap();
    wait("command=query,ok=false");
    writeln!(input, "rebuild").unwrap();
    wait("version=2,state=Validated");
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0)).unwrap();
    wait("reason=Permission");
    writeln!(input, "query kept").unwrap();
    wait("command=query,ok=false");
    drop(restore);
    writeln!(input, "rebuild").unwrap();
    wait("version=3,state=Validated");
    writeln!(input, "stop").unwrap();
    drop(input);
    let _ = child.0.wait().unwrap();
    reader.join().unwrap();
    let mut bytes = vec![];
    output.read_to_end(&mut bytes).unwrap();
    let mut one = fixture
        .root
        .join("kept.txt")
        .as_os_str()
        .as_encoded_bytes()
        .to_vec();
    one.push(0);
    let mut expected = one.clone();
    expected.extend(one);
    assert_eq!(bytes, expected);
}

#[test]
fn bounded_correction_updates_watch_and_retained_resources_and_preserves_cancelled_reader() {
    let fixture = Fixture::new();
    for id in 0..24 {
        let directory = fixture.root.join(format!("directory-{id:03}"));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("kept.txt"), b"").unwrap();
    }
    let mut expected = Vec::new();
    for item in fs::read_dir(&fixture.root).unwrap() {
        let directory = item.unwrap().path();
        expected.push(directory.join("kept.txt"));
        expected.push(directory);
    }
    expected.sort();
    let mut options = EngineOptions::scale();
    options.scan_batch = 2;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    settle(&mut engine, &expected);
    let old = engine.query().lease().unwrap();
    let version = engine.view().version;
    let retained_before = engine.view().resources.snapshot_bytes;
    engine.request_rebuild().unwrap();
    for _ in 0..24 {
        engine.poll().unwrap();
        if engine.view().resources.session_watches >= 3 {
            break;
        }
    }
    let partial = engine.view();
    assert_eq!(partial.status, Status::Pending);
    assert_eq!(partial.version, version);
    assert!(partial.resources.inventory_slots > 1);
    assert!(partial.resources.session_watches > 1);
    assert!(partial.resources.session_watches < 25);
    assert_eq!(
        partial.resources.session_watches,
        partial.resources.process_watches
    );
    assert!(partial.resources.retained_snapshot_bytes >= retained_before);
    engine.poll_with_cancel(&AtomicBool::new(true)).unwrap();
    let cancelled = engine.view();
    assert_eq!(cancelled.status, Status::Pending);
    assert_eq!(cancelled.version, version);
    assert_eq!(
        cancelled.resources.session_watches,
        partial.resources.session_watches
    );
    let page = old
        .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    let mut paths = page.paths;
    paths.sort();
    assert_eq!(paths, expected);
    assert!(!page.validated_at_start_and_finish);
    drop(old);
    engine.request_rebuild().unwrap();
    settle(&mut engine, &expected);
    let final_view = engine.view();
    assert_eq!(final_view.resources.session_watches, 25);
    assert_eq!(final_view.resources.inventory_slots, 49);
    assert!(final_view.coverage_gaps.is_empty());
}
