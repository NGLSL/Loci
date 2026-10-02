//! Bounded native bridge checks added during Linux stage3 validation.
#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::linux_inotify::{process_usage, Session, PROCESS_SESSION_CAP};
use loci_experiment::live::{Native, QueryHandle, QueryResult, Status};
use loci_experiment::watch::{Limits, Signal};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn query(handle: &QueryHandle) -> QueryResult {
    handle
        .lease()
        .unwrap()
        .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap()
}
fn settle(n: &mut Native, expected: &str) {
    let start = Instant::now();
    loop {
        n.tick().unwrap();
        if n.store.handle().view().status == Status::Validated {
            let out = query(&n.store.handle());
            if out.paths == [PathBuf::from(expected)] {
                assert!(out.validated_at_start_and_finish);
                return;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(6),
            "native query convergence budget"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn failed_native_session_allocation_exposes_latest_reason_and_generation() {
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.root.join("seed.rs"), "x").unwrap();
    let mut held = vec![];
    for _ in 0..PROCESS_SESSION_CAP {
        held.push(Session::new(1).unwrap());
    }
    assert_eq!(process_usage(), (0, PROCESS_SESSION_CAP));
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().is_err());
    let view = n.store.handle().view();
    assert!(matches!(view.status, Status::Failed(_)));
    assert!(n.watch.state.reasons.contains(&Signal::WatchLost));
    assert_eq!(
        view.observed_generation, n.watch.state.generation,
        "Failed view must expose the latest recovery generation immediately"
    );
    assert_eq!(
        view.reasons, n.watch.state.reasons,
        "Failed view must preserve the latest WatchLost reason immediately"
    );
    drop(held);
    assert_eq!(process_usage(), (0, 0));
    settle(&mut n, "seed.rs");
    assert_eq!(n.store.handle().view().version, 1);
    drop(n);
    assert_eq!(process_usage(), (0, 0));
    println!("PASS: native allocation failure exposes exact generation/reasons, then recovers when session capacity is released");
}
#[test]
fn native_concurrent_old_reader_blocks_third_generation_until_release() {
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.root.join("v1.rs"), "x").unwrap();
    let mut n = Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    let old = n.store.handle().lease().unwrap();
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
        drop(old);
    });
    fs::rename(f.root.join("v1.rs"), f.root.join("v2.rs")).unwrap();
    settle(&mut n, "v2.rs");
    go_tx.send(()).unwrap();
    assert_eq!(
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        (1, vec![PathBuf::from("v1.rs")], false)
    );
    fs::rename(f.root.join("v2.rs"), f.root.join("v3.rs")).unwrap();
    let started = Instant::now();
    while n.store.handle().view().status != Status::ReadersPinned {
        n.tick().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        n.store.builds, 2,
        "third index allocation must be backpressured"
    );
    let current = query(&n.store.handle());
    assert_eq!(current.version, 2);
    assert_eq!(current.paths, [PathBuf::from("v2.rs")]);
    assert!(!current.validated_at_start_and_finish);
    go_tx.send(()).unwrap();
    worker.join().unwrap();
    settle(&mut n, "v3.rs");
    assert_eq!(n.store.handle().view().version, 3);
    assert_eq!(n.store.builds, 3);
    assert!(n.watch.events_observed >= 4);
    drop(n);
    assert_eq!(process_usage(), (0, 0));
    println!("PASS: real rename/query pipeline preserved threaded v1 reader, blocked v3 allocation at two generations, then published v3 after reader release");
}
#[test]
fn watcherless_failed_scan_gap_is_reconciled_into_new_query_contents() {
    let _lock = LOCK.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.root.join("old.rs"), "x").unwrap();
    let mut n = Native::new(
        &f.root,
        Limits {
            entries: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(n.tick().unwrap());
    fs::write(f.root.join("extra.rs"), "x").unwrap();
    let started = Instant::now();
    while !n.watch.state.reasons.contains(&Signal::ScanIncomplete) {
        n.tick().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(n.watch.watches(), 0);
    assert_eq!(process_usage(), (0, 0));
    let before_events = n.watch.events_observed;
    let old = query(&n.store.handle());
    assert_eq!(old.paths, [PathBuf::from("old.rs")]);
    assert!(!old.validated_at_start_and_finish);
    fs::remove_file(f.root.join("old.rs")).unwrap();
    fs::rename(f.root.join("extra.rs"), f.root.join("reborn.rs")).unwrap();
    settle(&mut n, "reborn.rs");
    assert_eq!(
        n.watch.events_observed, before_events,
        "gap mutations must be recovered by scan, not credited as native events"
    );
    assert_eq!(n.store.handle().view().version, 2);
    drop(n);
    assert_eq!(process_usage(), (0, 0));
    println!("PASS: unobserved delete/rename while failed-scan watcher was absent reached corrected v2 query via rescan");
}
