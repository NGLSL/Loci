#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, Status};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};
static NATIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn background_owner_updates_without_foreground_poll_and_preserves_stopped_lease() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    fs::write(f.root.join("old.txt"), "").unwrap();
    let engine = Engine::open_with_options(&f.root, None, EngineOptions::scale()).unwrap();
    let mut owner = engine.spawn().unwrap();
    let query = owner.query();
    let old = query.lease().unwrap();
    fs::write(f.root.join("new.txt"), "").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = query
            .lease()
            .unwrap()
            .page(
                "new",
                None,
                50,
                &AtomicBool::new(false),
                &AtomicUsize::new(0),
            )
            .unwrap();
        if page.paths == [f.root.join("new.txt")] && page.finished.status == Status::Validated {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owner did not advance: {:?}",
            query.view()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    owner.stop(Duration::from_secs(2)).unwrap();
    owner.stop(Duration::ZERO).unwrap();
    assert_eq!(query.view().status, Status::Stopped);
    let page = old
        .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(page.paths, [f.root.join("old.txt")]);
    assert_eq!(page.finished.status, Status::Stopped);
    assert!(!page.validated_at_start_and_finish);
}

#[test]
fn owner_drop_releases_native_descriptors_watches_and_writer_lock_with_pinned_reader() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    let db = f.base.join("state.loci");
    fs::write(f.root.join("saved.txt"), "").unwrap();
    let descriptors = fs::read_dir("/proc/self/fd").unwrap().count();
    let usage = loci_experiment::linux_inotify::process_usage();
    let owner = Engine::open_with_options(&f.root, Some(&db), EngineOptions::scale())
        .unwrap()
        .spawn()
        .unwrap();
    let handle = owner.query();
    let lease = handle.lease().unwrap();
    owner.save().unwrap().wait(Duration::from_secs(2)).unwrap();
    assert!(fs::read_dir("/proc/self/fd").unwrap().count() > descriptors);
    drop(owner);
    assert_eq!(handle.view().status, Status::Stopped);
    assert_eq!(loci_experiment::linux_inotify::process_usage(), usage);
    assert_eq!(fs::read_dir("/proc/self/fd").unwrap().count(), descriptors);
    let mut reopened =
        Engine::open_with_options(&f.root, Some(&db), EngineOptions::scale()).unwrap();
    reopened.stop().unwrap();
    assert_eq!(
        lease
            .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap()
            .paths,
        [f.root.join("saved.txt")]
    );
}

#[test]
fn cancellation_preserves_checkpoint_and_rebuild_resumes_monitoring() {
    let _guard = NATIVE.lock().unwrap();
    let f = Fixture::new();
    let db = f.base.join("state.loci");
    fs::write(f.root.join("saved.txt"), "").unwrap();
    let mut first = Engine::open_with_options(&f.root, Some(&db), EngineOptions::scale()).unwrap();
    first.save().unwrap();
    first.stop().unwrap();
    let saved = fs::read(&db).unwrap();
    for n in 0..100 {
        fs::write(f.root.join(format!("added-{n}")), "").unwrap();
    }
    let options = EngineOptions {
        scan_batch: 1,
        ..EngineOptions::scale()
    };
    let mut owner = Engine::open_with_options(&f.root, Some(&db), options)
        .unwrap()
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while owner.metrics().scanned_entries == 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    owner.cancel().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !owner
        .view()
        .coverage_gaps
        .iter()
        .any(|gap| gap.kind == loci_experiment::engine::CoverageGapKind::Cancelled)
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        owner
            .save()
            .unwrap()
            .wait(Duration::from_secs(1))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(fs::read(&db).unwrap(), saved);
    owner
        .request_rebuild()
        .unwrap()
        .wait(Duration::from_secs(1))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while owner.view().status != Status::Validated {
        assert!(Instant::now() < deadline, "{:?}", owner.view());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(owner.metrics().scanned_entries >= 101);
    owner.save().unwrap().wait(Duration::from_secs(1)).unwrap();
    assert_ne!(fs::read(&db).unwrap(), saved);
    owner.stop(Duration::from_secs(1)).unwrap();
}

#[test]
fn timed_out_stop_retains_owned_resources_and_saturated_commands_cannot_hide_stop() {
    let _guard = NATIVE.lock().unwrap();
    use loci_experiment::events::{EventBatch, EventSource};
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Condvar, Mutex};
    struct BlockingSource {
        block: Arc<AtomicBool>,
        entered: Arc<AtomicBool>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }
    impl EventSource for BlockingSource {
        fn poll(&mut self) -> std::io::Result<EventBatch> {
            if self.block.load(Ordering::Acquire) {
                self.entered.store(true, Ordering::Release);
                let (lock, ready) = &*self.release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = ready.wait(released).unwrap();
                }
            }
            Ok(EventBatch::default())
        }
        fn stop(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new();
    fs::write(f.root.join("kept.txt"), "").unwrap();
    let block = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let engine = Engine::with_source(
        &f.root,
        None,
        BlockingSource {
            block: block.clone(),
            entered: entered.clone(),
            release: release.clone(),
        },
    )
    .unwrap();
    block.store(true, Ordering::Release);
    let mut owner = engine.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !entered.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let requests: Vec<_> = (0..loci_experiment::engine::MONITOR_COMMAND_CAPACITY)
        .map(|_| owner.save().unwrap())
        .collect();
    assert!(matches!(owner.save(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    assert_eq!(
        requests[0].wait(Duration::ZERO).unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
    assert_eq!(
        owner.stop(Duration::ZERO).unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
    assert_ne!(owner.view().status, Status::Stopped);
    let (lock, ready) = &*release;
    *lock.lock().unwrap() = true;
    ready.notify_all();
    owner.stop(Duration::from_secs(2)).unwrap();
    assert_eq!(owner.view().status, Status::Stopped);
}

#[test]
fn owner_threads_have_a_process_budget_including_external_sources() {
    let _guard = NATIVE.lock().unwrap();
    use loci_experiment::events::{EventBatch, EventSource};
    struct Quiet;
    impl EventSource for Quiet {
        fn poll(&mut self) -> std::io::Result<EventBatch> {
            Ok(EventBatch::default())
        }
        fn stop(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new();
    let owners: Vec<_> = (0..8)
        .map(|_| {
            Engine::with_source(&f.root, None, Quiet)
                .unwrap()
                .spawn()
                .unwrap()
        })
        .collect();
    let ninth = Engine::with_source(&f.root, None, Quiet).unwrap().spawn();
    assert!(matches!(ninth, Err(error) if error.kind() == std::io::ErrorKind::WouldBlock));
    drop(owners);
    let owner = Engine::with_source(&f.root, None, Quiet)
        .unwrap()
        .spawn()
        .unwrap();
    drop(owner);
}

#[test]
fn source_stop_error_still_releases_root_database_descriptors_and_lock() {
    let _guard = NATIVE.lock().unwrap();
    use loci_experiment::events::{EventBatch, EventSource};
    struct StopError;
    impl EventSource for StopError {
        fn poll(&mut self) -> std::io::Result<EventBatch> {
            Ok(EventBatch::default())
        }
        fn stop(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("simulated stop failure"))
        }
    }
    let f = Fixture::new();
    fs::write(f.root.join("kept.txt"), "").unwrap();
    let db = f.base.join("state.loci");
    let descriptors = fs::read_dir("/proc/self/fd").unwrap().count();
    let mut engine =
        Engine::with_source_and_options(&f.root, Some(&db), StopError, EngineOptions::scale())
            .unwrap();
    assert!(engine.stop().is_err());
    let stopped = engine
        .query()
        .lease()
        .unwrap()
        .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(stopped.paths, [f.root.join("kept.txt")]);
    assert_eq!(stopped.finished.status, Status::Stopped);
    assert!(!stopped.validated_at_start_and_finish);
    assert_eq!(fs::read_dir("/proc/self/fd").unwrap().count(), descriptors);
    let mut reopened =
        Engine::open_with_options(&f.root, Some(&db), EngineOptions::scale()).unwrap();
    reopened.stop().unwrap();
    // The still-live stopped engines retain searchable cuts and their capacity
    // credits. Close this phase before admitting another full-capacity owner.
    drop(reopened);
    drop(engine);
    let mut owner =
        Engine::with_source_and_options(&f.root, Some(&db), StopError, EngineOptions::scale())
            .unwrap()
            .spawn()
            .unwrap();
    assert!(owner.stop(Duration::from_secs(1)).is_err());
    assert!(owner.is_joined());
    let stopped = owner
        .query()
        .lease()
        .unwrap()
        .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(stopped.paths, [f.root.join("kept.txt")]);
    assert_eq!(stopped.finished.status, Status::Stopped);
    assert!(!stopped.validated_at_start_and_finish);
    assert_eq!(fs::read_dir("/proc/self/fd").unwrap().count(), descriptors);
}
