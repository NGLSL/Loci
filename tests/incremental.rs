mod common;
use common::Fixture;
use loci_experiment::incremental::{Change, Portable};
use loci_experiment::live::{QueryHandle, Status, Store};
use loci_experiment::watch::{self, Inventory, Kind, Limits, RawEvent, Recovery, Signal};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Duration;
fn result(handle: &QueryHandle, q: &str, first50: bool) -> loci_experiment::live::QueryResult {
    handle
        .lease()
        .unwrap()
        .search(q, first50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap()
}
fn paths(p: &Portable, q: &str) -> Vec<PathBuf> {
    result(&p.store.handle(), q, false).paths
}
fn initialize(f: &Fixture) -> Portable {
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    p
}
#[test]
fn file_add_delete_rename_updates_only_affected_partitions_without_rescan() {
    let f = Fixture::new();
    for i in 0..256 {
        fs::write(f.root.join(format!("f{i:04}.rs")), "x").unwrap();
    }
    let mut p = initialize(&f);
    let scans = p.metrics.full_scans;
    let scanned = p.metrics.scanned_entries;
    fs::write(f.root.join("报告.pdf"), "x").unwrap();
    p.enqueue(Change::Refresh("报告.pdf".into()));
    assert!(p.tick(Duration::from_secs(1)).unwrap());
    assert_eq!(paths(&p, "报告"), [PathBuf::from("报告.pdf")]);
    assert!(p.store.last_rebuilt_partitions <= 1 && p.store.last_rebuilt_records < 256);
    fs::rename(f.root.join("报告.pdf"), f.root.join("renamed.pdf")).unwrap();
    p.enqueue(Change::Rename {
        from: "报告.pdf".into(),
        to: "renamed.pdf".into(),
    });
    assert!(p.tick(Duration::from_secs(2)).unwrap());
    assert!(paths(&p, "报告").is_empty());
    assert_eq!(paths(&p, "renamed"), [PathBuf::from("renamed.pdf")]);
    assert!(p.store.last_rebuilt_partitions <= 2 && p.store.last_rebuilt_records < 256);
    fs::remove_file(f.root.join("renamed.pdf")).unwrap();
    p.enqueue(Change::Remove("renamed.pdf".into()));
    assert!(p.tick(Duration::from_secs(3)).unwrap());
    assert!(paths(&p, "renamed").is_empty());
    assert_eq!(
        (p.metrics.full_scans, p.metrics.scanned_entries),
        (scans, scanned)
    );
    assert_eq!(p.metrics.subtree_scans, 0);
}
#[test]
fn directory_subtree_rename_preserves_descendants_and_unaffected_records() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("old/nested")).unwrap();
    fs::write(f.root.join("old/a.rs"), "x").unwrap();
    fs::write(f.root.join("old/nested/报告.txt"), "x").unwrap();
    for i in 0..256 {
        fs::write(f.root.join(format!("unchanged{i}.rs")), "x").unwrap();
    }
    let mut p = initialize(&f);
    fs::rename(f.root.join("old"), f.root.join("new")).unwrap();
    p.enqueue(Change::Rename {
        from: "old".into(),
        to: "new".into(),
    });
    assert!(p.tick(Duration::from_secs(1)).unwrap());
    assert_eq!(
        paths(&p, "new"),
        vec!["new", "new/a.rs", "new/nested", "new/nested/报告.txt"]
            .into_iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>()
    );
    assert!(paths(&p, "old").is_empty());
    assert_eq!(result(&p.store.handle(), "unchanged", false).matches, 256);
    assert_eq!(p.metrics.full_scans, 1);
    assert_eq!(p.metrics.subtree_scans, 0);
    assert!(p.store.last_rebuilt_records < 260);
}
#[test]
fn newly_admitted_directory_scans_only_its_subtree_and_delete_is_incremental() {
    let f = Fixture::new();
    for i in 0..128 {
        fs::write(f.root.join(format!("base{i}.rs")), "x").unwrap();
    }
    let mut p = initialize(&f);
    let before = p.metrics.scanned_entries;
    fs::create_dir_all(f.root.join("incoming/child")).unwrap();
    fs::write(f.root.join("incoming/a.rs"), "x").unwrap();
    fs::write(f.root.join("incoming/child/b.rs"), "x").unwrap();
    p.enqueue(Change::Refresh("incoming".into()));
    assert!(p.tick(Duration::from_secs(1)).unwrap());
    assert_eq!(p.metrics.full_scans, 1);
    assert_eq!(p.metrics.subtree_scans, 1);
    assert_eq!(p.metrics.scanned_entries - before, 3);
    assert_eq!(result(&p.store.handle(), "incoming", false).matches, 4);
    fs::remove_file(f.root.join("incoming/a.rs")).unwrap();
    fs::remove_file(f.root.join("incoming/child/b.rs")).unwrap();
    fs::remove_dir(f.root.join("incoming/child")).unwrap();
    fs::remove_dir(f.root.join("incoming")).unwrap();
    p.enqueue(Change::Remove("incoming".into()));
    assert!(p.tick(Duration::from_secs(2)).unwrap());
    assert!(paths(&p, "incoming").is_empty());
    assert_eq!(p.metrics.full_scans, 1);
}
#[test]
fn ordered_batch_and_transient_paths_converge_without_full_scan() {
    let f = Fixture::new();
    fs::write(f.root.join("stable.rs"), "x").unwrap();
    let mut p = initialize(&f);
    fs::write(f.root.join("transient.rs"), "x").unwrap();
    fs::rename(f.root.join("transient.rs"), f.root.join("gone.rs")).unwrap();
    fs::remove_file(f.root.join("gone.rs")).unwrap();
    p.enqueue(Change::Refresh("transient.rs".into()));
    p.enqueue(Change::Rename {
        from: "transient.rs".into(),
        to: "gone.rs".into(),
    });
    p.enqueue(Change::Remove("gone.rs".into()));
    p.enqueue(Change::Refresh("stable.rs".into()));
    assert!(p.tick(Duration::from_secs(1)).unwrap());
    assert_eq!(paths(&p, ""), [PathBuf::from("stable.rs")]);
    assert_eq!(p.store.last_rebuilt_partitions, 0);
    assert_eq!(p.metrics.full_scans, 1);
}
#[test]
fn queue_overflow_and_explicit_loss_require_bounded_correction() {
    let f = Fixture::new();
    let mut p = Portable::new(
        &f.root,
        Limits {
            queue: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    for i in 0..4 {
        fs::write(f.root.join(format!("f{i}.rs")), "x").unwrap();
        p.enqueue(Change::Refresh(format!("f{i}.rs").into()));
    }
    assert!(p.state.reasons.contains(&Signal::UserOverflow));
    assert!(!result(&p.store.handle(), "", false).validated_at_start_and_finish);
    assert!(p.tick(Duration::from_secs(1)).unwrap());
    assert_eq!(result(&p.store.handle(), "", false).matches, 4);
    assert_eq!(p.metrics.full_scans, 2);
    fs::remove_file(f.root.join("f0.rs")).unwrap();
    p.invalidate(Signal::KernelOverflow);
    assert!(p.tick(Duration::from_secs(2)).unwrap());
    assert_eq!(result(&p.store.handle(), "", false).matches, 3);
    assert_eq!(p.metrics.full_scans, 3);
}
#[test]
fn race_during_inventory_transaction_rejects_candidate_then_corrects() {
    let f = Fixture::new();
    fs::write(f.root.join("seed.rs"), "x").unwrap();
    let mut p = initialize(&f);
    fs::write(f.root.join("new.rs"), "x").unwrap();
    p.enqueue(Change::Refresh("new.rs".into()));
    assert!(!p
        .tick_with_hook(Duration::from_secs(1), |state| {
            fs::write(f.root.join("missed.rs"), "x").unwrap();
            state.signal(Signal::Change);
        })
        .unwrap());
    assert_eq!(p.store.handle().view().version, 1);
    assert_eq!(paths(&p, ""), [PathBuf::from("seed.rs")]);
    assert!(!result(&p.store.handle(), "", false).validated_at_start_and_finish);
    assert!(p.tick(Duration::from_secs(5)).unwrap());
    assert_eq!(result(&p.store.handle(), "", false).matches, 3);
    assert_eq!(p.metrics.full_scans, 2);
}
#[test]
fn threaded_old_reader_pins_two_generations_without_more_index_allocations() {
    let f = Fixture::new();
    fs::write(f.root.join("v1.rs"), "x").unwrap();
    let mut p = initialize(&f);
    let old = p.store.handle().lease().unwrap();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        go_rx.recv().unwrap();
        let out = old
            .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap();
        done_tx
            .send((out.version, out.paths, out.validated_at_start_and_finish))
            .unwrap();
        go_rx.recv().unwrap();
    });
    fs::rename(f.root.join("v1.rs"), f.root.join("v2.rs")).unwrap();
    p.enqueue(Change::Rename {
        from: "v1.rs".into(),
        to: "v2.rs".into(),
    });
    assert!(p.tick(Duration::from_secs(1)).unwrap());
    go_tx.send(()).unwrap();
    assert_eq!(
        done_rx.recv().unwrap(),
        (1, vec![PathBuf::from("v1.rs")], false)
    );
    fs::rename(f.root.join("v2.rs"), f.root.join("v3.rs")).unwrap();
    p.enqueue(Change::Rename {
        from: "v2.rs".into(),
        to: "v3.rs".into(),
    });
    assert!(!p.tick(Duration::from_secs(2)).unwrap());
    assert_eq!(p.store.handle().view().status, Status::ReadersPinned);
    assert_eq!(p.store.builds, 2);
    assert_eq!(paths(&p, ""), [PathBuf::from("v2.rs")]);
    go_tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(p.tick(Duration::from_secs(6)).unwrap());
    assert_eq!(paths(&p, ""), [PathBuf::from("v3.rs")]);
    assert_eq!(p.metrics.full_scans, 1);
}
#[test]
fn budget_failure_keeps_visible_old_query_and_restart_recalibrates() {
    let f = Fixture::new();
    fs::write(f.root.join("seed.rs"), "x").unwrap();
    let mut p = Portable::new(
        &f.root,
        Limits {
            entries: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    for n in ["extra1.rs", "extra2.rs"] {
        fs::write(f.root.join(n), "x").unwrap();
        p.enqueue(Change::Refresh(n.into()));
    }
    assert!(p.tick(Duration::from_secs(1)).is_err());
    let view = p.store.handle().view();
    assert!(matches!(view.status, Status::Failed(_)));
    assert_eq!(view.observed_generation, p.state.generation);
    assert_eq!(paths(&p, ""), [PathBuf::from("seed.rs")]);
    fs::remove_file(f.root.join("extra2.rs")).unwrap();
    assert!(p.tick(Duration::from_secs(5)).unwrap());
    let checkpoint = f.base.join("checkpoint");
    watch::save_checkpoint(&checkpoint, &p.root, &p.state.inventory).unwrap();
    drop(p);
    fs::remove_file(f.root.join("seed.rs")).unwrap();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    p.restore(watch::load_checkpoint(&checkpoint, &p.root, p.limits).unwrap());
    assert!(p.store.handle().lease().is_err());
    assert!(p.tick(Duration::ZERO).unwrap());
    assert_eq!(paths(&p, ""), [PathBuf::from("extra1.rs")]);
}
#[test]
fn untrusted_paths_and_raised_limits_are_rejected_without_external_io() {
    let f = Fixture::new();
    let mut p = initialize(&f);
    p.enqueue(Change::Refresh("../outside".into()));
    assert!(p.tick(Duration::from_secs(1)).is_err());
    assert_eq!(p.metrics.metadata_calls, 0);
    assert_eq!(p.store.handle().view().version, 1);
    assert!(Portable::new(
        &f.root,
        Limits {
            entries: 4097,
            ..Limits::default()
        }
    )
    .is_err());
}
#[test]
fn partitioned_queries_preserve_first50_order_complete_counts_and_cancellation() {
    let mut state = Recovery::new(256);
    let entries: BTreeMap<_, _> = (0..4096)
        .map(|i| (PathBuf::from(format!("报告_{i:04}.rs")), Kind::File))
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
    assert!(store.stage_delta(&state).unwrap());
    assert!(store.commit(&state));
    let handle = store.handle();
    for q in ["", "报", "ext:rs", "报告_00", "absent"] {
        let complete = result(&handle, q, false);
        let first = result(&handle, q, true);
        assert_eq!(first.paths, complete.paths);
        assert_eq!(first.matches, complete.matches.min(50));
        assert!(first.paths.windows(2).all(|p| p[0] < p[1]));
        assert!(!first.cancelled);
    }
    assert_eq!(result(&handle, "报", false).matches, 4096);
    for q in ["absent", "报"] {
        let out = handle
            .lease()
            .unwrap()
            .search(q, false, &AtomicBool::new(true), &AtomicUsize::new(0))
            .unwrap();
        assert!(out.cancelled && out.paths.is_empty());
    }
}
fn event(wd: i32, mask: u32, cookie: u32, name: &str) -> RawEvent {
    RawEvent {
        wd,
        mask,
        cookie,
        name: name.as_bytes().to_vec(),
    }
}
#[test]
fn raw_directory_rename_remaps_later_child_events_and_handles_expected_retirement() {
    use loci_experiment::incremental::events::*;
    let f = Fixture::new();
    let watched = BTreeMap::from([
        (1, f.root.clone()),
        (2, f.root.join("old")),
        (3, f.root.join("old/nested")),
    ]);
    let raw = [
        event(1, FROM | IS_DIR, 7, "old"),
        event(1, TO | IS_DIR, 7, "new"),
        event(2, MOVE_SELF, 0, ""),
        event(3, CREATE, 0, "a.rs"),
    ];
    let out = translate(&f.root, &watched, &BTreeSet::new(), &raw).unwrap();
    assert_eq!(
        out.changes,
        vec![
            Change::Rename {
                from: "old".into(),
                to: "new".into()
            },
            Change::Refresh("new/nested/a.rs".into())
        ]
    );
    let raw = [
        event(1, DELETE | IS_DIR, 0, "old"),
        event(2, DELETE_SELF, 0, ""),
        event(2, watch::IN_IGNORED, 0, ""),
    ];
    let out = translate(&f.root, &watched, &BTreeSet::new(), &raw).unwrap();
    assert_eq!(out.ignored, [2]);
    assert_eq!(out.changes, [Change::Remove("old".into())]);
}
#[test]
fn malformed_reordered_and_lost_raw_batches_request_correction() {
    use loci_experiment::incremental::events::*;
    let f = Fixture::new();
    let watched = BTreeMap::from([(1, f.root.clone())]);
    for (raw, expected) in [
        (
            vec![event(-1, watch::IN_Q_OVERFLOW, 0, "")],
            Signal::KernelOverflow,
        ),
        (vec![event(99, CREATE, 0, "a")], Signal::UnknownWatch),
        (
            vec![event(1, TO, 7, "b"), event(1, FROM, 7, "a")],
            Signal::GenerationRace,
        ),
        (vec![event(1, CREATE, 0, "../x")], Signal::GenerationRace),
        (
            vec![
                event(1, FROM, 7, "a"),
                event(1, FROM, 7, "a"),
                event(1, TO, 7, "b"),
            ],
            Signal::GenerationRace,
        ),
    ] {
        assert_eq!(
            translate(&f.root, &watched, &BTreeSet::new(), &raw).err(),
            Some(expected)
        );
    }
}

fn percentile(mut values: Vec<f64>, p: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * p).ceil() as usize]
}
#[test]
#[ignore = "explicit bounded incremental/rescan comparison; mode/files supplied by measurement script"]
fn measure_incremental_comparison() {
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
    let mut update_times = vec![];
    let (scans, subtrees, scanned, metadata, reindexed, partitions);
    match mode.as_str() {
        "incremental" => {
            let mut p = initialize(&f);
            for i in 0..31 {
                let (from, to) = if i % 2 == 0 {
                    ("dir/file0000.rs", "dir/renamed.rs")
                } else {
                    ("dir/renamed.rs", "dir/file0000.rs")
                };
                let old = p.store.handle().lease().unwrap();
                let started = std::time::Instant::now();
                fs::rename(f.root.join(from), f.root.join(to)).unwrap();
                p.enqueue(Change::Rename {
                    from: from.into(),
                    to: to.into(),
                });
                let update = std::time::Instant::now();
                assert!(p.tick(Duration::from_secs(i + 1)).unwrap());
                update_times.push(update.elapsed().as_secs_f64() * 1000.0);
                let out = result(&p.store.handle(), to, false);
                assert_eq!(out.paths, [PathBuf::from(to)]);
                assert!(out.validated_at_start_and_finish);
                latencies.push(started.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(result(&p.store.handle(), "ext:rs", false).matches, files);
                drop(old);
            }
            scans = p.metrics.full_scans;
            subtrees = p.metrics.subtree_scans;
            scanned = p.metrics.scanned_entries;
            metadata = p.metrics.metadata_calls;
            reindexed = p.store.rebuilt_records;
            partitions = p.store.rebuilt_partitions;
            assert_eq!(scans, 1);
            assert_eq!(subtrees, 0);
        }
        "rescan" => {
            let mut p = loci_experiment::live::Portable::new(&f.root, Limits::default()).unwrap();
            assert!(p.tick(Duration::ZERO).unwrap());
            for i in 0..31 {
                let (from, to) = if i % 2 == 0 {
                    ("dir/file0000.rs", "dir/renamed.rs")
                } else {
                    ("dir/renamed.rs", "dir/file0000.rs")
                };
                let old = p.store.handle().lease().unwrap();
                let started = std::time::Instant::now();
                fs::rename(f.root.join(from), f.root.join(to)).unwrap();
                p.signal(Signal::Change);
                let update = std::time::Instant::now();
                assert!(p.tick(Duration::from_secs(i + 1)).unwrap());
                update_times.push(update.elapsed().as_secs_f64() * 1000.0);
                let out = result(&p.store.handle(), to, false);
                assert_eq!(out.paths, [PathBuf::from(to)]);
                assert!(out.validated_at_start_and_finish);
                latencies.push(started.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(result(&p.store.handle(), "ext:rs", false).matches, files);
                drop(old);
            }
            scans = p.full_scans;
            subtrees = 0;
            scanned = p.scanned_entries;
            metadata = 0;
            reindexed = p.store.rebuilt_records;
            partitions = p.store.rebuilt_partitions;
            assert_eq!(scans, 32);
        }
        _ => panic!("unknown comparison mode"),
    }
    println!("incremental_compare,platform=portable,mode={mode},files={files},samples=31,p50_ms={:.6},p95_ms={:.6},update_p50_ms={:.6},update_p95_ms={:.6},full_scans={scans},subtree_scans={subtrees},scanned_entries={scanned},incremental_metadata_calls={metadata},reindexed_records={reindexed},rebuilt_partitions={partitions}",
        percentile(latencies.clone(),0.5),percentile(latencies,0.95),percentile(update_times.clone(),0.5),percentile(update_times,0.95));
}

#[test]
fn cancellation_during_partitioned_query_keeps_one_snapshot_and_bounded_results() {
    use std::sync::atomic::Ordering;
    let mut state = Recovery::new(16);
    assert!(state.publish(
        state.ticket(),
        Inventory {
            entries: (0..4096)
                .map(|i| (
                    PathBuf::from(format!("{}/a{i:04}.rs", "a".repeat(180))),
                    Kind::File
                ))
                .collect(),
            complete: true,
            ..Inventory::default()
        }
    ));
    let mut store = Store::new();
    assert!(store.stage_delta(&state).unwrap());
    assert!(store.commit(&state));
    let lease = store.handle().lease().unwrap();
    let cancel = AtomicBool::new(false);
    let progress = AtomicUsize::new(0);
    let done = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let out = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let out = lease.search("a", false, &cancel, &progress).unwrap();
            done.store(true, Ordering::Release);
            out
        });
        while progress.load(Ordering::Acquire) == 0 && !done.load(Ordering::Acquire) {
            assert!(started.elapsed() < Duration::from_secs(2));
            std::thread::yield_now();
        }
        cancel.store(true, Ordering::Release);
        worker.join().unwrap()
    });
    assert_eq!(out.version, 1);
    assert!(out.validated_at_start_and_finish);
    // A fully finished query is allowed to win the cancellation race.
    assert!(out.cancelled || out.matches == 4096);
    assert!(out.matches <= 4096 && out.paths.len() <= 50);
    assert!(out.paths.windows(2).all(|p| p[0] < p[1]));
    assert!(out
        .paths
        .iter()
        .all(|p| p.to_string_lossy().ends_with(".rs")));
    println!(
        "cancel_incremental,cancelled={},matches={},elapsed_us={:.3}",
        out.cancelled,
        out.matches,
        started.elapsed().as_secs_f64() * 1e6
    );
}
