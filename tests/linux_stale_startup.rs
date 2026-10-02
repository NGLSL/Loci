#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, QueryLease, Status};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

fn paths(lease: &QueryLease) -> Vec<PathBuf> {
    let mut cursor = None;
    let mut paths = vec![];
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
        paths.extend(page.paths);
        if page.complete {
            break;
        }
        cursor = page.next;
    }
    paths.sort();
    paths
}

fn oracle(root: &Path) -> Vec<PathBuf> {
    let mut todo = vec![root.to_path_buf()];
    let mut paths = vec![];
    while let Some(directory) = todo.pop() {
        for child in fs::read_dir(directory).unwrap() {
            let child = child.unwrap();
            if child.file_type().unwrap().is_dir() {
                todo.push(child.path());
            }
            paths.push(child.path());
        }
    }
    paths.sort();
    paths
}

fn settle(engine: &mut Engine) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.view().status != Status::Validated {
        // A path can disappear from the listing during a real rename. Such a
        // candidate fails explicitly and bounded retry must still converge.
        let _ = engine.poll();
        assert!(Instant::now() < deadline, "{:?}", engine.view());
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn native_reopen_returns_saved_pending_results_before_any_correction_and_recovers_offline_changes()
{
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("old-dir")).unwrap();
    fs::write(fixture.root.join("old-dir/kept.txt"), "x").unwrap();
    fs::write(fixture.root.join("deleted.txt"), "x").unwrap();
    let saved = oracle(&fixture.root);
    let database = fixture.base.join("saved.loci");
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    settle(&mut engine);
    engine.save().unwrap();
    let version = engine.view().version;
    engine.stop().unwrap();
    fs::rename(
        fixture.root.join("old-dir"),
        fixture.root.join("renamed-dir"),
    )
    .unwrap();
    fs::remove_file(fixture.root.join("deleted.txt")).unwrap();
    fs::write(fixture.root.join("offline-added.txt"), "x").unwrap();

    let mut options = EngineOptions::scale();
    options.scan_batch = 2;
    let mut reopened = Engine::open_with_options(&fixture.root, Some(&database), options).unwrap();
    assert_eq!(reopened.view().status, Status::Pending);
    assert_eq!(reopened.view().version, version);
    assert_eq!(reopened.metrics().scanned_entries, 0);
    assert_eq!(reopened.view().resources.session_watches, 1);
    let stale = reopened.query().lease().unwrap();
    assert_eq!(paths(&stale), saved);
    let page = stale
        .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert!(!page.validated_at_start_and_finish);
    assert!(!reopened.poll().unwrap());
    assert!(reopened.metrics().scanned_entries <= 2);
    assert_eq!(reopened.view().status, Status::Pending);
    // Native root watch is active while the bounded candidate is incomplete.
    fs::create_dir(fixture.root.join("startup-dir")).unwrap();
    fs::write(fixture.root.join("startup-dir/early.txt"), "x").unwrap();
    fs::write(fixture.root.join("renamed-dir/during-scan.txt"), "x").unwrap();
    fs::rename(
        fixture.root.join("offline-added.txt"),
        fixture.root.join("startup-renamed.txt"),
    )
    .unwrap();
    settle(&mut reopened);
    assert_eq!(
        paths(&reopened.query().lease().unwrap()),
        oracle(&fixture.root)
    );
    assert_eq!(
        paths(&stale),
        saved,
        "saved lease is immutable during correction"
    );
}

#[test]
fn cancelled_or_failed_startup_keeps_saved_pending_results_and_preserves_checkpoint() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("saved.txt"), "x").unwrap();
    let database = fixture.base.join("saved.loci");
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    engine.save().unwrap();
    engine.stop().unwrap();
    let checkpoint = fs::read(&database).unwrap();
    fs::write(fixture.root.join("offline.txt"), "x").unwrap();
    let mut options = EngineOptions::scale();
    options.limits.entries = 1;
    options.scan_batch = 1;
    let mut engine = Engine::open_with_options(&fixture.root, Some(&database), options).unwrap();
    engine.poll_with_cancel(&AtomicBool::new(true)).unwrap();
    assert_eq!(engine.view().status, Status::Pending);
    assert!(engine
        .view()
        .coverage_gaps
        .iter()
        .any(|gap| gap.kind == loci_experiment::engine::CoverageGapKind::Cancelled));
    for _ in 0..8 {
        engine.poll().unwrap();
    }
    assert_eq!(engine.metrics().scanned_entries, 0);
    assert_eq!(
        paths(&engine.query().lease().unwrap()),
        [fixture.root.join("saved.txt")]
    );
    assert!(engine.save().is_err());
    engine.request_rebuild().unwrap();
    for _ in 0..8 {
        if engine.poll().is_err() {
            break;
        }
    }
    assert!(matches!(engine.view().status, Status::Failed(_)));
    assert_eq!(
        paths(&engine.query().lease().unwrap()),
        [fixture.root.join("saved.txt")]
    );
    assert!(engine.save().is_err());
    engine.stop().unwrap();
    assert_eq!(fs::read(&database).unwrap(), checkpoint);
    fs::rename(&fixture.root, fixture.base.join("original-root")).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    let error = Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale())
        .err()
        .expect("replaced root must reject saved source");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error.to_string().contains("source identity mismatch")
            && error.to_string().contains("rebuild"),
        "{error}"
    );
    assert_eq!(fs::read(&database).unwrap(), checkpoint);
}

#[test]
fn cli_exposes_stale_first_search_and_explicit_fresh_correction_with_separate_timings() {
    use std::process::Command;
    let fixture = Fixture::new();
    fs::write(fixture.root.join("offline-deleted.txt"), "x").unwrap();
    let database = fixture.base.join("saved.loci");
    let build = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "build"])
        .arg(&fixture.root)
        .arg(&database)
        .args(["--scale", "--scan-batch", "1"])
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let build_log = String::from_utf8(build.stderr).unwrap();
    assert!(
        build_log.find("first_searchable_ms=").unwrap()
            < build_log.find("full_correction_ms=").unwrap(),
        "{build_log}"
    );
    fs::remove_file(fixture.root.join("offline-deleted.txt")).unwrap();
    fs::write(fixture.root.join("offline-added.txt"), "x").unwrap();
    let run = |fresh: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_loci-experiment"));
        command
            .args(["engine", "query"])
            .arg(&fixture.root)
            .arg(&database)
            .args(["", "--scale", "--null"]);
        if fresh {
            command.arg("--fresh");
        }
        command.output().unwrap()
    };
    let stale = run(false);
    assert_eq!(stale.status.code(), Some(4));
    assert_eq!(
        stale.stdout,
        [
            fixture
                .root
                .join("offline-deleted.txt")
                .as_os_str()
                .as_encoded_bytes(),
            &[0]
        ]
        .concat()
    );
    let fresh = run(true);
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    assert_eq!(
        fresh.stdout,
        [
            fixture
                .root
                .join("offline-added.txt")
                .as_os_str()
                .as_encoded_bytes(),
            &[0]
        ]
        .concat()
    );
    let stale_log = String::from_utf8(stale.stderr).unwrap();
    assert!(stale_log.contains("phase=stale,first_searchable_ms="));
    assert!(!stale_log.contains("full_correction_ms="));
    let fresh_log = String::from_utf8(fresh.stderr).unwrap();
    let saved = fresh_log.find("phase=stale,first_searchable_ms=").unwrap();
    let correcting = fresh_log.find("phase=correcting").unwrap();
    let validated = fresh_log
        .find("phase=validated,full_correction_ms=")
        .unwrap();
    assert!(saved < correcting && correcting < validated, "{fresh_log}");
}

#[test]
fn watch_is_ready_with_saved_queries_while_startup_correction_is_in_progress() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    let fixture = Fixture::new();
    for n in 0..40 {
        fs::write(fixture.root.join(format!("entry-{n:02}.txt")), "x").unwrap();
    }
    fs::write(fixture.root.join("offline-deleted.txt"), "x").unwrap();
    let database = fixture.base.join("saved.loci");
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    engine.save().unwrap();
    engine.stop().unwrap();
    fs::remove_file(fixture.root.join("offline-deleted.txt")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "watch"])
        .arg(&fixture.root)
        .arg(&database)
        .args(["--scale", "--scan-batch", "1", "--null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let stderr = child.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut logs = vec![];
    let wait = |target: &str, logs: &mut Vec<String>| loop {
        let line = receiver
            .recv_timeout(Duration::from_secs(8))
            .expect("watch diagnostic timeout");
        let matched = line.contains(target);
        logs.push(line);
        if matched {
            break;
        }
    };
    let mut input = child.stdin.take().unwrap();
    wait("watch-ready", &mut logs);
    assert!(logs
        .iter()
        .any(|line| line.contains("phase=stale,first_searchable_ms=")));
    assert!(!logs.iter().any(|line| line.contains("full_correction_ms=")));
    writeln!(input, "query offline-deleted").unwrap();
    wait("command=query,ok=false", &mut logs);
    wait("phase=validated,full_correction_ms=", &mut logs);
    writeln!(input, "query offline-deleted").unwrap();
    wait("command=query,ok=true", &mut logs);
    writeln!(input, "stop").unwrap();
    drop(input);
    assert!(child.wait().unwrap().success());
    reader.join().unwrap();
    let mut output = vec![];
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    assert_eq!(
        output,
        [
            fixture
                .root
                .join("offline-deleted.txt")
                .as_os_str()
                .as_encoded_bytes(),
            &[0]
        ]
        .concat()
    );
    assert!(logs.iter().any(|line| line.contains("phase=correcting")));
}
