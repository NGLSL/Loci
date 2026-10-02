//! Native incremental tests run on Linux; feature checks on Windows are not execution.
#![cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
mod common;
use common::Fixture;
use loci_experiment::incremental::Native;
use loci_experiment::live::Status;
use loci_experiment::watch::{self, Limits, Signal};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn kernel_usage() -> (usize, usize) {
    let mut sessions = 0;
    let mut watches = 0;
    for entry in fs::read_dir("/proc/self/fd").unwrap() {
        let entry = entry.unwrap();
        if fs::read_link(entry.path())
            .is_ok_and(|p| p.to_string_lossy().contains("anon_inode:inotify"))
        {
            sessions += 1;
            let info =
                fs::read_to_string(PathBuf::from("/proc/self/fdinfo").join(entry.file_name()))
                    .unwrap();
            watches += info
                .lines()
                .filter(|line| line.starts_with("inotify wd:"))
                .count();
        }
    }
    (watches, sessions)
}
fn query(n: &Native, q: &str) -> loci_experiment::live::QueryResult {
    n.store
        .handle()
        .lease()
        .unwrap()
        .search(q, false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap()
}
fn settle(n: &mut Native, require_events: bool) {
    let before = n.watch.events_observed;
    let started = Instant::now();
    loop {
        n.tick().unwrap();
        let actual = watch::scan(&n.watch.root, n.watch.limits, |_| Ok(()));
        if n.store.handle().view().status == Status::Validated
            && actual.complete
            && actual.entries == n.watch.state.inventory.entries
        {
            let expected: Vec<_> = actual.entries.keys().take(50).cloned().collect();
            assert_eq!(query(n, "").paths, expected);
            if require_events {
                assert!(n.watch.events_observed > before);
            }
            return;
        }
        assert!(started.elapsed() < Duration::from_secs(6));
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn native_file_events_reuse_session_and_only_rebuild_changed_partitions() {
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    for i in 0..256 {
        fs::write(f.root.join(format!("f{i:04}.rs")), "x").unwrap();
    }
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    let scanned = n.metrics.scanned_entries;
    fs::write(f.root.join("new.rs"), "x").unwrap();
    settle(&mut n, true);
    assert_eq!(query(&n, "new").paths, [PathBuf::from("new.rs")]);
    assert!(n.store.last_rebuilt_partitions <= 1);
    fs::rename(f.root.join("new.rs"), f.root.join("报告.rs")).unwrap();
    settle(&mut n, true);
    assert!(n.store.last_rebuilt_partitions <= 2);
    assert_eq!(query(&n, "报告").matches, 1);
    fs::remove_file(f.root.join("报告.rs")).unwrap();
    settle(&mut n, true);
    assert_eq!(n.metrics.full_scans, 1);
    assert_eq!(n.metrics.scanned_entries, scanned);
    assert_eq!(n.watch.watches(), 1);
}
#[test]
fn native_directory_rename_and_delete_update_subtree_without_root_rescan() {
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("old/nested")).unwrap();
    fs::write(f.root.join("old/nested/a.rs"), "x").unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    assert_eq!(n.watch.watches(), 3);
    assert_eq!(kernel_usage(), (3, 1));
    assert_eq!(loci_experiment::linux_inotify::process_usage(), (3, 1));
    fs::rename(f.root.join("old"), f.root.join("new")).unwrap();
    settle(&mut n, true);
    assert_eq!(query(&n, "new/nested/a").matches, 1);
    assert_eq!(n.metrics.full_scans, 1);
    assert_eq!(n.metrics.subtree_scans, 0);
    assert_eq!(kernel_usage(), (3, 1));
    fs::write(f.root.join("new/nested/b.rs"), "x").unwrap();
    settle(&mut n, true);
    assert_eq!(query(&n, "b.rs").matches, 1);
    fs::remove_file(f.root.join("new/nested/a.rs")).unwrap();
    fs::remove_file(f.root.join("new/nested/b.rs")).unwrap();
    fs::remove_dir(f.root.join("new/nested")).unwrap();
    fs::remove_dir(f.root.join("new")).unwrap();
    settle(&mut n, true);
    assert!(query(&n, "new").paths.is_empty());
    assert_eq!(n.watch.watches(), 1);
    assert_eq!(n.metrics.full_scans, 1);
    assert_eq!(kernel_usage(), (1, 1));
    assert_eq!(loci_experiment::linux_inotify::process_usage(), (1, 1));
    drop(n);
    assert_eq!(kernel_usage(), (0, 0));
    assert_eq!(loci_experiment::linux_inotify::process_usage(), (0, 0));
}
#[test]
fn native_move_in_admits_only_new_subtree_and_move_out_retires_watches() {
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    let outside = f.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("a.rs"), "x").unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    fs::rename(&outside, f.root.join("incoming")).unwrap();
    settle(&mut n, true);
    assert_eq!(n.metrics.full_scans, 1);
    assert_eq!(n.metrics.subtree_scans, 1);
    assert_eq!(n.watch.watches(), 2);
    fs::rename(f.root.join("incoming"), &outside).unwrap();
    settle(&mut n, true);
    assert!(query(&n, "").paths.is_empty());
    assert_eq!(n.watch.watches(), 1);
    assert_eq!(n.metrics.full_scans, 1);
}
#[test]
fn native_index_build_race_rejects_snapshot_and_uses_pending_events() {
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.root.join("seed.rs"), "x").unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    fs::write(f.root.join("first.rs"), "x").unwrap();
    std::thread::sleep(Duration::from_millis(260));
    assert!(!n
        .tick_with_hooks(
            || {},
            || fs::write(f.root.join("during-build.rs"), "x").unwrap()
        )
        .unwrap());
    assert_eq!(n.store.handle().view().version, 1);
    assert!(!query(&n, "").validated_at_start_and_finish);
    settle(&mut n, false);
    assert_eq!(query(&n, "ext:rs").matches, 3);
    assert_eq!(n.metrics.full_scans, 1);
}
#[test]
fn native_batch_overflow_backpressure_and_restart_recovery_are_bounded() {
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.root.join("v1.rs"), "x").unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    let old = n.store.handle().lease().unwrap();
    fs::rename(f.root.join("v1.rs"), f.root.join("v2.rs")).unwrap();
    settle(&mut n, true);
    fs::rename(f.root.join("v2.rs"), f.root.join("v3.rs")).unwrap();
    let start = Instant::now();
    while n.store.handle().view().status != Status::ReadersPinned {
        n.tick().unwrap();
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(n.store.builds, 2);
    assert_eq!(query(&n, "").paths, [PathBuf::from("v2.rs")]);
    drop(old);
    settle(&mut n, false);
    assert_eq!(query(&n, "").paths, [PathBuf::from("v3.rs")]);
    assert_eq!(n.metrics.full_scans, 1);
    for i in 0..300 {
        fs::write(f.root.join(format!("batch{i}.rs")), "x").unwrap();
    }
    n.tick().unwrap();
    assert!(n.watch.state.reasons.contains(&Signal::UserOverflow) || n.metrics.full_scans >= 2);
    settle(&mut n, false);
    assert_eq!(query(&n, "ext:rs").matches, 301);
    assert!(n.metrics.full_scans >= 2);
    let checkpoint = f.base.join("checkpoint");
    watch::save_checkpoint(&checkpoint, &n.watch.root, &n.watch.state.inventory).unwrap();
    drop(n);
    fs::remove_file(f.root.join("v3.rs")).unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    n.restore(watch::load_checkpoint(&checkpoint, &n.watch.root, n.watch.limits).unwrap());
    assert!(n.store.handle().lease().is_err());
    assert!(n.tick().unwrap());
    assert_eq!(query(&n, "ext:rs").matches, 300);
}
#[test]
fn native_cli_validation_entry_observes_real_mutation() {
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args([
            "live-check",
            f.root.to_str().unwrap(),
            "ext:rs",
            "1500",
            "incremental",
        ])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    fs::write(f.root.join("a.rs"), "x").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text
        .lines()
        .any(|line| line.contains("validated=true,matches=1,full_scans=1")));
}

fn percentile(mut values: Vec<f64>, p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).ceil() as usize]
}
#[test]
#[ignore = "explicit bounded native incremental/rescan comparison"]
fn measure_incremental_comparison() {
    let _lock = LOCK.lock().unwrap();
    let mode = std::env::var("LOCI_COMPARE_MODE").unwrap_or("incremental".into());
    let files: usize = std::env::var("LOCI_COMPARE_FILES")
        .unwrap_or("1024".into())
        .parse()
        .unwrap();
    assert!([256, 1024, 3072].contains(&files));
    let f = Fixture::new();
    fs::create_dir(f.root.join("dir")).unwrap();
    for i in 0..files {
        fs::write(f.root.join(format!("dir/file{i:04}.rs")), "x").unwrap();
    }
    let mut latencies = vec![];
    let (scans, subtrees, scanned, metadata, reindexed, partitions, events);
    match mode.as_str() {
        "incremental" => {
            let mut p = Native::new(&f.root, Limits::default()).unwrap();
            assert!(p.tick().unwrap());
            for i in 0..31 {
                let (from, to) = if i % 2 == 0 {
                    ("dir/file0000.rs", "dir/renamed.rs")
                } else {
                    ("dir/renamed.rs", "dir/file0000.rs")
                };
                let old = p.store.handle().lease().unwrap();
                let before = p.watch.events_observed;
                let started = Instant::now();
                fs::rename(f.root.join(from), f.root.join(to)).unwrap();
                loop {
                    p.tick().unwrap();
                    let out = query(&p, to);
                    if out.validated_at_start_and_finish && out.paths == [PathBuf::from(to)] {
                        break;
                    }
                    assert!(started.elapsed() < Duration::from_secs(5));
                    std::thread::sleep(Duration::from_millis(5));
                }
                latencies.push(started.elapsed().as_secs_f64() * 1000.0);
                assert!(p.watch.events_observed > before);
                assert_eq!(query(&p, "ext:rs").matches, files);
                drop(old);
            }
            scans = p.metrics.full_scans;
            subtrees = p.metrics.subtree_scans;
            scanned = p.metrics.scanned_entries;
            metadata = p.metrics.metadata_calls;
            reindexed = p.store.rebuilt_records;
            partitions = p.store.rebuilt_partitions;
            events = p.watch.events_observed;
            assert_eq!(scans, 1);
            assert_eq!(subtrees, 0);
        }
        "rescan" => {
            let mut p = loci_experiment::live::Native::new(&f.root, Limits::default()).unwrap();
            assert!(p.tick().unwrap());
            for i in 0..31 {
                let (from, to) = if i % 2 == 0 {
                    ("dir/file0000.rs", "dir/renamed.rs")
                } else {
                    ("dir/renamed.rs", "dir/file0000.rs")
                };
                let old = p.store.handle().lease().unwrap();
                let before = p.watch.events_observed;
                let started = Instant::now();
                fs::rename(f.root.join(from), f.root.join(to)).unwrap();
                loop {
                    p.tick().unwrap();
                    let out = p
                        .store
                        .handle()
                        .lease()
                        .unwrap()
                        .search(to, false, &AtomicBool::new(false), &AtomicUsize::new(0))
                        .unwrap();
                    if out.validated_at_start_and_finish && out.paths == [PathBuf::from(to)] {
                        break;
                    }
                    assert!(started.elapsed() < Duration::from_secs(5));
                    std::thread::sleep(Duration::from_millis(5));
                }
                latencies.push(started.elapsed().as_secs_f64() * 1000.0);
                assert!(p.watch.events_observed > before);
                assert_eq!(
                    p.store
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
                    files
                );
                drop(old);
            }
            scans = p.watch.full_scans;
            subtrees = 0;
            scanned = p.watch.scanned_entries;
            metadata = 0;
            reindexed = p.store.rebuilt_records;
            partitions = p.store.rebuilt_partitions;
            events = p.watch.events_observed;
            assert_eq!(scans, 32);
        }
        _ => panic!("unknown comparison mode"),
    }
    println!("incremental_compare,platform=native,mode={mode},files={files},samples=31,p50_ms={:.6},p95_ms={:.6},full_scans={scans},subtree_scans={subtrees},scanned_entries={scanned},incremental_metadata_calls={metadata},reindexed_records={reindexed},rebuilt_partitions={partitions},native_events={events}",
        percentile(latencies.clone(),0.5),percentile(latencies,0.95));
}
#[test]
#[ignore = "opt-in bounded true kernel overflow to incremental query recovery"]
fn native_incremental_kernel_overflow() {
    use loci_experiment::linux_inotify::Session;
    use loci_experiment::watch::Recovery;
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    let queue: usize = fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let cycles = queue / 2 + 32;
    if cycles > 10000 {
        println!("SKIP: queue exceeds 10000-cycle budget");
        return;
    }
    let mut session = Session::new(1).unwrap();
    session.add(&f.root).unwrap();
    let started = Instant::now();
    for _ in 0..cycles {
        if started.elapsed() > Duration::from_secs(10) {
            println!("SKIP: 10s generation budget");
            return;
        }
        fs::write(f.root.join("storm"), "x").unwrap();
        fs::remove_file(f.root.join("storm")).unwrap();
    }
    fs::write(f.root.join("survivor.rs"), "x").unwrap();
    let mut state = Recovery::new(256);
    for _ in 0..4 {
        session.pump(&mut state).unwrap();
        if state.reasons.contains(&Signal::KernelOverflow) {
            break;
        }
    }
    if !state.reasons.contains(&Signal::KernelOverflow) {
        println!("SKIP: no real IN_Q_OVERFLOW observed");
        return;
    }
    drop(session);
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    n.watch.state = state;
    assert!(n.tick().unwrap());
    let out = query(&n, "survivor");
    assert_eq!(out.paths, [PathBuf::from("survivor.rs")]);
    assert!(out.validated_at_start_and_finish);
    println!("PASS: real IN_Q_OVERFLOW recovered through incremental snapshot engine; queue={queue},cycles={cycles}");
}
