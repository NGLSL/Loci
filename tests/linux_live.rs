//! Real inotify -> index -> query tests, executed only on Linux.
#![cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
mod common;
use common::Fixture;
use loci_experiment::live::{Native, Status};
use loci_experiment::watch::{self, Limits, Signal};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};
fn paths(native: &Native, raw: &str) -> Vec<PathBuf> {
    native
        .store
        .handle()
        .lease()
        .unwrap()
        .search(raw, false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap()
        .paths
}
fn settle(native: &mut Native, require_events: bool) {
    let before = native.watch.events_observed;
    let started = Instant::now();
    loop {
        native.tick().unwrap();
        let actual = watch::scan(&native.watch.root, native.watch.limits, |_| Ok(()));
        if native.store.handle().view().status == Status::Validated
            && actual.complete
            && actual.entries == native.watch.state.inventory.entries
        {
            let expected: Vec<_> = actual.entries.keys().take(50).cloned().collect();
            assert_eq!(paths(native, ""), expected);
            if require_events {
                assert!(native.watch.events_observed > before);
            }
            println!(
                "native_live_settle,elapsed_ms={:.3},events={},version={}",
                started.elapsed().as_secs_f64() * 1000.0,
                native.watch.events_observed - before,
                native.store.handle().view().version
            );
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "native live bridge did not converge"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn native_mutations_reach_query_and_old_reader_never_mixes_versions() {
    let f = Fixture::new();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    let old = n.store.handle().lease().unwrap();
    fs::create_dir(f.root.join("old")).unwrap();
    fs::write(f.root.join("old/a.rs"), "x").unwrap();
    settle(&mut n, true);
    assert_eq!(paths(&n, "ext:rs"), [PathBuf::from("old/a.rs")]);
    let stale = old
        .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(stale.version, 1);
    assert!(stale.paths.is_empty());
    assert!(!stale.validated_at_start_and_finish);
    drop(old);
    fs::rename(f.root.join("old/a.rs"), f.root.join("old/报告.rs")).unwrap();
    fs::rename(f.root.join("old"), f.root.join("renamed")).unwrap();
    settle(&mut n, true);
    assert_eq!(paths(&n, "报告"), [PathBuf::from("renamed/报告.rs")]);
    fs::remove_file(f.root.join("renamed/报告.rs")).unwrap();
    fs::remove_dir(f.root.join("renamed")).unwrap();
    settle(&mut n, true);
    assert!(paths(&n, "").is_empty());
    assert_eq!(n.watch.watches(), 1);
}
#[test]
fn native_event_during_index_build_rejects_old_candidate_and_converges() {
    let f = Fixture::new();
    fs::write(f.root.join("seed"), "x").unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    n.watch.state.signal(Signal::Change);
    std::thread::sleep(Duration::from_millis(260));
    assert!(!n
        .tick_with_hook(|| fs::write(f.root.join("during-build"), "x").unwrap())
        .unwrap());
    assert!(n.watch.events_observed > 0);
    assert_ne!(n.store.handle().view().status, Status::Validated);
    assert_eq!(n.store.handle().view().version, 1);
    assert_eq!(paths(&n, ""), [PathBuf::from("seed")]);
    settle(&mut n, false);
    assert_eq!(paths(&n, "during-build"), [PathBuf::from("during-build")]);
}
#[test]
fn native_restart_recalibrates_query_before_publication() {
    let f = Fixture::new();
    fs::write(f.root.join("old.rs"), "x").unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    let checkpoint = f.base.join("checkpoint");
    watch::save_checkpoint(&checkpoint, &n.watch.root, &n.watch.state.inventory).unwrap();
    drop(n);
    fs::remove_file(f.root.join("old.rs")).unwrap();
    fs::write(f.root.join("offline-new.rs"), "x").unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    n.restore(watch::load_checkpoint(&checkpoint, &n.watch.root, n.watch.limits).unwrap());
    assert!(n.store.handle().lease().is_err());
    assert!(n.tick().unwrap());
    assert_eq!(paths(&n, "ext:rs"), [PathBuf::from("offline-new.rs")]);
}
#[test]
fn native_budget_failure_is_visible_and_keeps_previous_query() {
    let f = Fixture::new();
    fs::write(f.root.join("seed.rs"), "x").unwrap();
    let mut n = Native::new(
        &f.root,
        Limits {
            entries: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(n.tick().unwrap());
    for i in 0..3 {
        fs::write(f.root.join(format!("extra{i}")), "x").unwrap();
    }
    let before = n.watch.events_observed;
    let start = Instant::now();
    while !n.watch.state.reasons.contains(&Signal::ScanIncomplete) {
        n.tick().unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(n.watch.events_observed > before);
    let out = n
        .store
        .handle()
        .lease()
        .unwrap()
        .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(out.version, 1);
    assert_eq!(out.paths, [PathBuf::from("seed.rs")]);
    assert!(!out.validated_at_start_and_finish);
    for i in 0..3 {
        fs::remove_file(f.root.join(format!("extra{i}"))).unwrap();
    }
    // Failed scans drop the watcher; recovery is a bounded rescan, not a deletion event claim.
    settle(&mut n, false);
    assert_eq!(paths(&n, ""), [PathBuf::from("seed.rs")]);
}
#[test]
#[cfg(target_os = "linux")]
fn native_non_utf8_candidate_is_rejected_visibly_without_dropping_files() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new();
    fs::write(f.root.join("seed"), "x").unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    let invalid = f.root.join(std::ffi::OsString::from_vec(vec![0xff, 0xfe]));
    fs::write(&invalid, "x").unwrap();
    let started = Instant::now();
    let mut failure = false;
    while started.elapsed() < Duration::from_secs(3) {
        if n.tick().is_err() {
            failure = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(failure);
    assert!(matches!(n.store.handle().view().status, Status::Failed(_)));
    assert_eq!(n.store.handle().view().version, 1);
    assert_eq!(paths(&n, ""), [PathBuf::from("seed")]);
    fs::remove_file(invalid).unwrap();
    settle(&mut n, true);
}

#[test]
fn native_initial_scan_race_reaches_query_and_storm_is_throttled() {
    let f = Fixture::new();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    let mut once = true;
    assert!(n
        .tick_with_hooks(
            || {
                if once {
                    fs::write(f.root.join("initial-race"), "x").unwrap();
                    once = false;
                }
            },
            || {}
        )
        .unwrap());
    assert!(n.watch.events_observed > 0);
    assert_eq!(paths(&n, "initial-race"), [PathBuf::from("initial-race")]);
    n.watch.state.signal(Signal::Change);
    std::thread::sleep(Duration::from_millis(260));
    let mut attempts = 0;
    assert!(!n
        .tick_with_hooks(
            || {
                fs::write(f.root.join(format!("race-{attempts}")), "x").unwrap();
                attempts += 1;
            },
            || {}
        )
        .unwrap());
    assert_eq!(attempts, n.watch.limits.retries);
    assert!(n.watch.state.reasons.contains(&Signal::RetryLimit));
    assert_eq!(n.store.handle().view().version, 1);
    assert_eq!(paths(&n, ""), [PathBuf::from("initial-race")]);
    let scans = n.gate.attempts;
    let builds = n.store.builds;
    for _ in 0..100 {
        if n.gate.ready(Duration::from_millis(0)) {
            panic!("backoff should remain active");
        }
        n.tick().unwrap();
    }
    assert_eq!(n.gate.attempts, scans);
    assert_eq!(n.store.builds, builds);
    settle(&mut n, false);
    assert_eq!(n.watch.state.inventory.entries.len(), 1 + attempts);
}

fn percentile(mut values: Vec<f64>, p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).ceil() as usize]
}
#[test]
#[ignore = "explicit bounded native inotify-to-query latency measurement"]
fn measure_native_live_1024_records_31_renames() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("dir")).unwrap();
    for i in 0..1024 {
        fs::write(f.root.join(format!("dir/file{i:04}.rs")), "x").unwrap();
    }
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    let mut latency = vec![];
    let mut query_times = vec![];
    for i in 0..31 {
        let from = if i % 2 == 0 {
            "dir/file0000.rs"
        } else {
            "dir/renamed.rs"
        };
        let to = if i % 2 == 0 {
            "dir/renamed.rs"
        } else {
            "dir/file0000.rs"
        };
        let old = n.store.handle().lease().unwrap();
        let start = Instant::now();
        fs::rename(f.root.join(from), f.root.join(to)).unwrap();
        settle(&mut n, true);
        let q = Instant::now();
        let result = paths(&n, to);
        query_times.push(q.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(result, [PathBuf::from(to)]);
        latency.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(
            n.store
                .handle()
                .lease()
                .unwrap()
                .search(
                    "ext:rs",
                    false,
                    &AtomicBool::new(false),
                    &AtomicUsize::new(0)
                )
                .unwrap()
                .matches,
            1024
        );
        drop(old);
    }
    for (name, values) in [
        ("native_mutation_poll_throttle_rebuild_query", latency),
        ("native_selective_query", query_times),
    ] {
        println!(
            "native_live_metric,{name},samples=31,p50_ms={:.6},p95_ms={:.6}",
            percentile(values.clone(), 0.5),
            percentile(values, 0.95)
        );
    }
    println!(
        "native_live_bounds,files=1024,directories=2,builds={},events={},attempts={}",
        n.store.builds, n.watch.events_observed, n.gate.attempts
    );
    for line in fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("VmRSS:") || l.starts_with("VmHWM:"))
    {
        println!("resource: {line}");
    }
}
