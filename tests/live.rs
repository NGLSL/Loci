//! Windows tests use actual fixture mutations plus explicitly simulated notifications.
mod common;
use common::Fixture;
use loci_experiment::live::{Portable, Status, Store, MAX_LEASES};
use loci_experiment::watch::{self, Inventory, Kind, Limits, Recovery, Signal};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

fn query(
    handle: &loci_experiment::live::QueryHandle,
    raw: &str,
) -> loci_experiment::live::QueryResult {
    handle
        .lease()
        .unwrap()
        .search(raw, false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap()
}
fn inventory(names: &[&str]) -> Inventory {
    Inventory {
        entries: names
            .iter()
            .map(|n| (PathBuf::from(n), Kind::File))
            .collect(),
        complete: true,
        ..Inventory::default()
    }
}
fn oracle(path: &Path, raw: &str) -> bool {
    let text = path
        .components()
        .map(|p| p.as_os_str().to_str().unwrap())
        .collect::<Vec<_>>()
        .join("/")
        .to_lowercase();
    raw.split_whitespace().all(|t| {
        if let Some(e) = t.strip_prefix("ext:") {
            text.rsplit_once('.').is_some_and(|(_, ext)| ext == e)
        } else {
            text.contains(&t.to_lowercase())
        }
    })
}
fn assert_oracle(p: &Portable) {
    let actual = watch::scan(&p.root, p.limits, |_| Ok(()));
    assert!(actual.complete);
    for raw in [
        "",
        "报告",
        "ext:rs",
        "old",
        "renamed/",
        "gone",
        "ext:pdf report",
    ] {
        let out = query(&p.store.handle(), raw);
        let expected: Vec<_> = actual
            .entries
            .keys()
            .filter(|path| oracle(path, raw))
            .cloned()
            .collect();
        assert_eq!(out.matches, expected.len(), "{raw}");
        assert_eq!(
            out.paths,
            expected.into_iter().take(50).collect::<Vec<_>>(),
            "{raw}"
        );
        assert!(out.validated_at_start_and_finish);
    }
}
#[test]
fn fixture_add_delete_file_and_directory_rename_flow_to_query() {
    let f = Fixture::new();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    assert_oracle(&p);
    fs::create_dir(f.root.join("old")).unwrap();
    fs::write(f.root.join("old/a.rs"), "x").unwrap();
    fs::write(f.root.join("gone.pdf"), "x").unwrap();
    p.signal(Signal::Change);
    assert!(!query(&p.store.handle(), "").validated_at_start_and_finish);
    assert!(p.tick(Duration::from_secs(1)).unwrap());
    assert_oracle(&p);
    fs::rename(f.root.join("old/a.rs"), f.root.join("old/报告.rs")).unwrap();
    fs::rename(f.root.join("old"), f.root.join("renamed")).unwrap();
    fs::remove_file(f.root.join("gone.pdf")).unwrap();
    p.signal(Signal::Change);
    assert!(p.tick(Duration::from_secs(2)).unwrap());
    assert_oracle(&p);
    assert_eq!(
        query(&p.store.handle(), "报告").paths,
        [PathBuf::from("renamed/报告.rs")]
    );
    fs::remove_file(f.root.join("renamed/报告.rs")).unwrap();
    fs::remove_dir(f.root.join("renamed")).unwrap();
    p.signal(Signal::Change);
    assert!(p.tick(Duration::from_secs(3)).unwrap());
    assert_oracle(&p);
}
#[test]
fn race_between_scan_and_publish_does_not_replace_snapshot() {
    let f = Fixture::new();
    fs::write(f.root.join("seed"), "x").unwrap();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    let handle = p.store.handle();
    let old = handle.lease().unwrap();
    p.signal(Signal::Change);
    let mut races = 0;
    assert!(!p
        .tick_with_hook(Duration::from_secs(1), |state| {
            fs::write(f.root.join(format!("race-{races}")), "x").unwrap();
            races += 1;
            state.signal(Signal::Change);
        })
        .unwrap());
    assert_eq!(races, p.limits.retries);
    assert_eq!(handle.view().version, 1);
    assert_eq!(query(&handle, "").paths, [PathBuf::from("seed")]);
    assert!(
        !old.search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap()
            .validated_at_start_and_finish
    );
    drop(old);
    assert!(p.tick(Duration::from_secs(5)).unwrap());
    assert_oracle(&p);
}
#[test]
fn race_during_index_build_rejects_candidate() {
    let mut state = Recovery::new(16);
    assert!(state.publish(state.ticket(), inventory(&["old.rs"])));
    let mut store = Store::new();
    assert!(store.stage(&state).unwrap());
    assert!(store.commit(&state));
    state.signal(Signal::Change);
    assert!(state.publish(state.ticket(), inventory(&["new.rs"])));
    assert!(store.stage(&state).unwrap());
    state.signal(Signal::Change);
    assert!(!store.commit(&state));
    assert_eq!(query(&store.handle(), "").paths, [PathBuf::from("old.rs")]);
    assert!(!query(&store.handle(), "").validated_at_start_and_finish);
}
#[test]
fn concurrent_old_reader_returns_one_version_and_pins_only_two_snapshots() {
    let mut state = Recovery::new(16);
    assert!(state.publish(state.ticket(), inventory(&["old.rs"])));
    let mut store = Store::new();
    assert!(store.stage(&state).unwrap());
    assert!(store.commit(&state));
    let handle = store.handle();
    let lease = handle.lease().unwrap();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        go_rx.recv().unwrap();
        let result = lease
            .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap();
        done_tx
            .send((
                result.version,
                result.paths,
                result.validated_at_start_and_finish,
            ))
            .unwrap();
        go_rx.recv().unwrap(); // Retain first generation while main attempts third.
    });
    state.signal(Signal::Change);
    assert!(state.publish(state.ticket(), inventory(&["new.rs"])));
    assert!(store.stage(&state).unwrap());
    assert!(store.commit(&state));
    go_tx.send(()).unwrap();
    assert_eq!(
        done_rx.recv().unwrap(),
        (1, vec![PathBuf::from("old.rs")], false)
    );
    assert_eq!(query(&handle, "").paths, [PathBuf::from("new.rs")]);
    state.signal(Signal::Change);
    assert!(state.publish(state.ticket(), inventory(&["third.rs"])));
    assert!(!store.stage(&state).unwrap());
    assert_eq!(handle.view().status, Status::ReadersPinned);
    assert_eq!(store.builds, 2);
    go_tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(store.stage(&state).unwrap());
    assert!(store.commit(&state));
    assert_eq!(handle.view().version, 3);
}
#[test]
fn query_budget_and_precancel_hold_for_present_and_absent_queries() {
    let mut state = Recovery::new(16);
    assert!(state.publish(state.ticket(), inventory(&["a.rs"])));
    let mut store = Store::new();
    assert!(store.stage(&state).unwrap());
    assert!(store.commit(&state));
    let handle = store.handle();
    let leases: Vec<_> = (0..MAX_LEASES).map(|_| handle.lease().unwrap()).collect();
    assert!(handle.lease().is_err());
    for raw in ["a", "absent"] {
        let out = leases[0]
            .search(raw, false, &AtomicBool::new(true), &AtomicUsize::new(0))
            .unwrap();
        assert!(out.cancelled && out.paths.is_empty());
    }
    assert!(leases[0]
        .search(
            &"a".repeat(513),
            false,
            &AtomicBool::new(false),
            &AtomicUsize::new(0)
        )
        .is_err());
    drop(leases);
    assert_eq!(handle.view().leases, 0);
}
#[test]
fn cancellation_while_query_runs_has_no_mixed_or_unvalidated_completion() {
    let mut state = Recovery::new(16);
    let entries = (0..4096)
        .map(|i| {
            (
                PathBuf::from(format!("{}/a{i:04}.rs", "a".repeat(180))),
                Kind::File,
            )
        })
        .collect();
    assert!(state.publish(
        state.ticket(),
        Inventory {
            entries,
            complete: true,
            ..Inventory::default()
        }
    ));
    let mut store = Store::new();
    assert!(store.stage(&state).unwrap());
    assert!(store.commit(&state));
    let lease = store.handle().lease().unwrap();
    let cancel = AtomicBool::new(false);
    let progress = AtomicUsize::new(0);
    let done = AtomicBool::new(false);
    let start = Instant::now();
    let out = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let out = lease.search("a", false, &cancel, &progress).unwrap();
            done.store(true, Ordering::Release);
            out
        });
        while progress.load(Ordering::Acquire) == 0 && !done.load(Ordering::Acquire) {
            assert!(start.elapsed() < Duration::from_secs(2));
            std::thread::yield_now();
        }
        cancel.store(true, Ordering::Release);
        worker.join().unwrap()
    });
    assert_eq!(out.version, 1);
    assert!(out.validated_at_start_and_finish);
    assert!(out.cancelled || out.matches == 4096); // A completed search is allowed to win the cancellation race.
    assert!(out
        .paths
        .iter()
        .all(|p| p.to_string_lossy().ends_with(".rs")));
    println!(
        "cancel_live,cancelled={},matches={},elapsed_us={:.3}",
        out.cancelled,
        out.matches,
        start.elapsed().as_secs_f64() * 1e6
    );
}
#[test]
fn restart_calibrates_checkpoint_before_any_query_is_available() {
    let f = Fixture::new();
    fs::write(f.root.join("old"), "x").unwrap();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    let checkpoint = f.base.join("checkpoint");
    watch::save_checkpoint(&checkpoint, &p.root, &p.state.inventory).unwrap();
    drop(p);
    fs::remove_file(f.root.join("old")).unwrap();
    fs::write(f.root.join("offline-new"), "x").unwrap();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    p.restore(watch::load_checkpoint(&checkpoint, &p.root, p.limits).unwrap());
    assert!(p.store.handle().lease().is_err());
    assert!(p.tick(Duration::ZERO).unwrap());
    assert_eq!(
        query(&p.store.handle(), "").paths,
        [PathBuf::from("offline-new")]
    );
    assert_oracle(&p);
}
#[test]
fn simulated_event_loss_and_periodic_rescan_repair_query_snapshot() {
    let f = Fixture::new();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    fs::write(f.root.join("missed"), "x").unwrap();
    // Before observing loss, validated refers only to the previous scan cut.
    p.signal(Signal::KernelOverflow);
    let out = query(&p.store.handle(), "missed");
    assert_eq!(out.matches, 0);
    assert!(!out.validated_at_start_and_finish);
    assert!(out.finished.reasons.contains(&Signal::KernelOverflow));
    assert!(p.tick(Duration::from_secs(1)).unwrap());
    assert_eq!(query(&p.store.handle(), "missed").matches, 1);
    fs::remove_file(f.root.join("missed")).unwrap();
    p.signal(Signal::Periodic);
    assert!(p.tick(Duration::from_secs(2)).unwrap());
    assert_oracle(&p);
}
#[test]
fn failed_scan_keeps_previous_results_with_visible_reason_and_recovers() {
    let f = Fixture::new();
    fs::write(f.root.join("seed"), "x").unwrap();
    let mut p = Portable::new(
        &f.root,
        Limits {
            entries: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    for i in 0..3 {
        fs::write(f.root.join(format!("extra{i}")), "x").unwrap();
    }
    p.signal(Signal::Change);
    assert!(!p.tick(Duration::from_secs(1)).unwrap());
    let out = query(&p.store.handle(), "");
    assert_eq!(out.version, 1);
    assert_eq!(out.paths, [PathBuf::from("seed")]);
    assert!(!out.validated_at_start_and_finish);
    assert!(out.finished.reasons.contains(&Signal::ScanIncomplete));
    for i in 0..3 {
        fs::remove_file(f.root.join(format!("extra{i}"))).unwrap();
    }
    p.signal(Signal::Change);
    assert!(p.tick(Duration::from_secs(5)).unwrap());
    assert_oracle(&p);
}
#[test]
fn event_storm_has_finite_scan_attempts_and_backoff_then_converges() {
    let f = Fixture::new();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    let builds = p.store.builds;
    for i in 1..=2000 {
        p.signal(Signal::Change);
        assert!(!p
            .tick_with_hook(Duration::from_millis(i), |state| state
                .signal(Signal::Change))
            .unwrap());
    }
    assert!(p.gate.attempts <= 4);
    assert_eq!(p.store.builds, builds);
    assert!(p.state.dirty);
    assert!(p.state.pending() <= p.limits.queue);
    assert!(p.tick(Duration::from_secs(6)).unwrap());
    assert!(query(&p.store.handle(), "").validated_at_start_and_finish);
}
#[test]
fn incomplete_or_oversized_index_input_is_rejected_as_a_whole() {
    let mut state = Recovery::new(16);
    let mut store = Store::new();
    assert!(!store.stage(&state).unwrap());
    let paths: BTreeMap<_, _> = (0..300)
        .map(|i| {
            (
                PathBuf::from(format!("{i}/{}", "a".repeat(4090))),
                Kind::File,
            )
        })
        .collect();
    assert!(state.publish(
        state.ticket(),
        Inventory {
            entries: paths,
            complete: true,
            ..Inventory::default()
        }
    ));
    assert!(store.stage(&state).is_err());
    assert!(store.handle().lease().is_err());
    assert_eq!(store.builds, 0);
}
fn percentile(mut values: Vec<f64>, p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).ceil() as usize]
}
#[test]
#[ignore = "explicit bounded Windows fixture latency/memory measurement"]
fn measure_live_fixture_1024_records_31_renames() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("dir")).unwrap();
    for i in 0..1024 {
        fs::write(f.root.join(format!("dir/file{i:04}.rs")), "x").unwrap();
    }
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    let handle = p.store.handle();
    let mut end_to_end = vec![];
    let mut rebuild = vec![];
    let mut queries = vec![];
    for i in 0..31 {
        let old = if i % 2 == 0 {
            "dir/file0000.rs"
        } else {
            "dir/renamed.rs"
        };
        let new = if i % 2 == 0 {
            "dir/renamed.rs"
        } else {
            "dir/file0000.rs"
        };
        let lease = handle.lease().unwrap(); // Retain old snapshot during replacement to include coexistence.
        let start = Instant::now();
        fs::rename(f.root.join(old), f.root.join(new)).unwrap();
        p.signal(Signal::Change);
        let build = Instant::now();
        assert!(p.tick(Duration::from_secs(i + 1)).unwrap());
        rebuild.push(build.elapsed().as_secs_f64() * 1000.0);
        let query_start = Instant::now();
        let out = query(&handle, new);
        queries.push(query_start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(out.paths, [PathBuf::from(new)]);
        assert!(out.validated_at_start_and_finish);
        end_to_end.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(query(&handle, "ext:rs").matches, 1024);
        drop(lease);
    }
    for (name, values) in [
        ("mutation_signal_rebuild_query", end_to_end),
        ("scan_index_publish", rebuild),
        ("query", queries),
    ] {
        println!(
            "live_metric,{name},samples=31,p50_ms={:.6},p95_ms={:.6}",
            percentile(values.clone(), 0.5),
            percentile(values, 0.95)
        );
    }
    println!("live_bounds,records=1025,builds={},leases={},max_snapshots=2,notifications=simulated_windows",p.store.builds,handle.view().leases);
}
