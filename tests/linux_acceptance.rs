//! Additional bounded Linux acceptance checks. All mutations stay in owned fixtures.
#![cfg(target_os = "linux")]
use loci_experiment::linux_inotify::{Runtime, Session};
use loci_experiment::watch::{self, Limits, Signal};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
struct Fixture {
    base: PathBuf,
    root: PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}
impl Fixture {
    fn new() -> Self {
        let guard = FIXTURE_LOCK.lock().unwrap();
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let work = std::env::current_dir().unwrap().join("work");
        fs::create_dir_all(&work).unwrap();
        let base = work.join(format!(
            "linux-acceptance-fixture-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let root = base.join("data");
        fs::create_dir(&root).unwrap();
        Self {
            base: fs::canonicalize(base).unwrap(),
            root: fs::canonicalize(root).unwrap(),
            _guard: guard,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let work = fs::canonicalize(std::env::current_dir().unwrap().join("work")).unwrap();
        let base = fs::canonicalize(&self.base).unwrap();
        assert!(
            base.starts_with(work)
                && base
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("linux-acceptance-fixture-")
        );
        fs::remove_dir_all(base).unwrap();
    }
}
fn settle(runtime: &mut Runtime) {
    settle_with_events(runtime, true);
}
fn settle_with_events(runtime: &mut Runtime, require_events: bool) {
    let before = runtime.events_observed;
    let start = Instant::now();
    loop {
        runtime.tick().unwrap();
        let actual = watch::scan(&runtime.root, runtime.limits, |_| Ok(()));
        if !runtime.state.dirty
            && actual.complete
            && actual.entries == runtime.state.inventory.entries
        {
            if require_events {
                assert!(
                    runtime.events_observed > before,
                    "native event must be observed"
                );
            }
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn inotify_fds() -> usize {
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| fs::read_link(e.path()).ok())
        .filter(|p| p == Path::new("anon_inode:inotify"))
        .count()
}
fn kernel_watches() -> usize {
    fs::read_dir("/proc/self/fdinfo")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .map(|s| s.lines().filter(|l| l.starts_with("inotify wd:")).count())
        .sum()
}

#[test]
fn live_file_and_directory_deletion_are_event_driven() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("child")).unwrap();
    fs::write(f.root.join("child/a"), "x").unwrap();
    let mut r = Runtime::new(&f.root, Limits::default()).unwrap();
    assert!(r.reconcile().unwrap());
    assert_eq!(r.watches(), 2);
    fs::remove_file(f.root.join("child/a")).unwrap();
    settle(&mut r);
    assert!(!r.state.inventory.entries.contains_key(Path::new("child/a")));
    fs::remove_dir(f.root.join("child")).unwrap();
    settle(&mut r);
    assert_eq!(r.watches(), 1);
    assert!(r.state.inventory.entries.is_empty());
    println!(
        "PASS: live deletions observed {} native events",
        r.events_observed
    );
}

#[test]
fn repeated_scan_races_stop_at_retry_budget() {
    let f = Fixture::new();
    let limits = Limits {
        retries: 4,
        ..Limits::default()
    };
    let mut r = Runtime::new(&f.root, limits).unwrap();
    assert!(r.reconcile().unwrap());
    let mut attempts = 0;
    assert!(!r
        .reconcile_with_hook(|| {
            fs::write(f.root.join(format!("race-{attempts}")), "x").unwrap();
            attempts += 1;
        })
        .unwrap());
    assert_eq!(attempts, 4);
    assert!(r.state.dirty && r.state.reasons.contains(&Signal::RetryLimit));
    assert!(r.state.inventory.entries.is_empty());
    assert!(r.state.pending() <= limits.queue);
    assert!(r.events_observed >= 4);
    assert!(r.reconcile().unwrap());
    assert_eq!(r.state.inventory.entries.len(), 4);
    println!("PASS: initial-scan mutation stopped after {attempts} retries, then recovered");
}

#[test]
fn exceeded_entry_budget_keeps_last_good_snapshot_then_recovers() {
    let f = Fixture::new();
    fs::write(f.root.join("seed"), "x").unwrap();
    let mut r = Runtime::new(
        &f.root,
        Limits {
            entries: 8,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(r.reconcile().unwrap());
    let previous = r.state.inventory.entries.clone();
    for i in 0..9 {
        fs::write(f.root.join(format!("extra-{i}")), "x").unwrap();
    }
    assert!(!r.tick().unwrap());
    assert!(r.events_observed > 0);
    assert!(r.state.dirty && r.state.reasons.contains(&Signal::ScanIncomplete));
    assert_eq!(r.state.inventory.entries, previous);
    for i in 0..9 {
        fs::remove_file(f.root.join(format!("extra-{i}"))).unwrap();
    }
    // Failed scans intentionally drop the session. The later deletions are
    // recovered by a bounded rescan; they cannot claim native event delivery.
    assert_eq!(r.watches(), 0);
    settle_with_events(&mut r, false);
    assert_eq!(r.state.inventory.entries, previous);
    println!("PASS: failed bounded scan retained previous snapshot and recovered");
}

#[test]
fn bounded_tree_churn_and_session_drop_release_inotify_fds() {
    // Fixtures are serialized so process-wide fd assertions also work in default test mode.
    let f = Fixture::new();
    for d in 0..32 {
        fs::create_dir(f.root.join(format!("d{d}"))).unwrap();
        for n in 0..32 {
            fs::write(f.root.join(format!("d{d}/f{n}")), "x").unwrap();
        }
    }
    let before = inotify_fds();
    let before_watches = kernel_watches();
    let started = Instant::now();
    let mut r = Runtime::new(&f.root, Limits::default()).unwrap();
    assert!(r.reconcile().unwrap());
    assert_eq!(r.watches(), 33);
    assert_eq!(r.state.inventory.entries.len(), 1056);
    assert_eq!(inotify_fds(), before + 1);
    assert_eq!(kernel_watches(), before_watches + 33);
    // Old session is dropped first: peak is the same 33-watch/1-fd budget.
    assert!(r
        .reconcile_with_hook(|| {
            assert_eq!(inotify_fds(), before + 1);
            assert_eq!(kernel_watches(), before_watches + 33);
        })
        .unwrap());
    assert_eq!(inotify_fds(), before + 1);
    assert_eq!(kernel_watches(), before_watches + 33);
    for i in 0..20 {
        let from = if i % 2 == 0 { "d0/f0" } else { "d0/renamed" };
        let to = if i % 2 == 0 { "d0/renamed" } else { "d0/f0" };
        fs::rename(f.root.join(from), f.root.join(to)).unwrap();
        settle(&mut r);
        assert_eq!(r.watches(), 33);
        assert_eq!(inotify_fds(), before + 1);
        assert_eq!(kernel_watches(), before_watches + 33);
        assert!(r.state.pending() <= r.limits.queue);
    }
    let observed = r.events_observed;
    drop(r);
    assert_eq!(inotify_fds(), before);
    assert_eq!(kernel_watches(), before_watches);
    for _ in 0..64 {
        let mut s = Session::new(1).unwrap();
        s.add(&f.root).unwrap();
        assert_eq!(inotify_fds(), before + 1);
        drop(s);
        assert_eq!(inotify_fds(), before);
    }
    println!("PASS: 1024 files, 33 watched directories, 20 renames, {observed} native events, 64 session lifecycles, no retained inotify fd; elapsed_ms={:.3}", started.elapsed().as_secs_f64()*1000.0);
    println!("PASS: /proc fdinfo confirmed 33 steady-state watches, 33 peak watches and 1 inotify fd while replacing the session, then 0 after drop");
    for line in fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .filter(|l| l.starts_with("VmRSS:") || l.starts_with("VmHWM:"))
    {
        println!("resource: {line}");
    }
}

#[test]
fn linux_symlink_is_skipped_and_cannot_be_watched_as_directory() {
    use std::os::unix::fs::symlink;
    let f = Fixture::new();
    let outside = f.base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("not-in-scope"), "x").unwrap();
    symlink(&outside, f.root.join("link")).unwrap();
    let mut r = Runtime::new(&f.root, Limits::default()).unwrap();
    assert!(r.reconcile().unwrap());
    assert!(r.state.inventory.entries.is_empty());
    assert_eq!(r.watches(), 1);
    let mut s = Session::new(1).unwrap();
    assert!(s.add(&f.root.join("link")).is_err());
    assert_eq!(s.count(), 0);
    println!("PASS: symlink skipped; DONT_FOLLOW rejects symlink watch target");
}

#[test]
fn checkpoint_atomic_replacement_roundtrips_current_inventory() {
    let f = Fixture::new();
    let checkpoint = f.base.join("checkpoint.bin");
    let mut r = Runtime::new(&f.root, Limits::default()).unwrap();
    assert!(r.reconcile().unwrap());
    watch::save_checkpoint(&checkpoint, &r.root, &r.state.inventory).unwrap();
    fs::write(f.root.join("new"), "x").unwrap();
    settle(&mut r);
    watch::save_checkpoint(&checkpoint, &r.root, &r.state.inventory).unwrap();
    let restored = watch::load_checkpoint(&checkpoint, &r.root, r.limits).unwrap();
    assert_eq!(restored.entries, r.state.inventory.entries);
    assert_eq!(restored.entries.len(), 1);
    println!("PASS: existing checkpoint replaced and reloaded on Linux");
}

#[test]
fn process_watch_and_fd_budgets_cover_multiple_sessions() {
    use loci_experiment::linux_inotify::{process_usage, PROCESS_SESSION_CAP, PROCESS_WATCH_CAP};
    let f = Fixture::new();
    assert_eq!(process_usage(), (0, 0));
    for i in 0..=PROCESS_WATCH_CAP {
        fs::create_dir(f.root.join(format!("budget-{i}"))).unwrap();
    }
    let mut a = Session::new(PROCESS_WATCH_CAP).unwrap();
    let mut b = Session::new(PROCESS_WATCH_CAP).unwrap();
    for i in 0..PROCESS_WATCH_CAP {
        if i % 2 == 0 {
            a.add(&f.root.join(format!("budget-{i}"))).unwrap();
        } else {
            b.add(&f.root.join(format!("budget-{i}"))).unwrap();
        }
    }
    assert_eq!(process_usage(), (PROCESS_WATCH_CAP, 2));
    assert!(b
        .add(&f.root.join(format!("budget-{}", PROCESS_WATCH_CAP)))
        .is_err());
    assert_eq!(kernel_watches(), PROCESS_WATCH_CAP);
    assert_eq!(inotify_fds(), 2);
    drop(a);
    drop(b);
    assert_eq!(process_usage(), (0, 0));
    assert_eq!(kernel_watches(), 0);
    assert_eq!(inotify_fds(), 0);
    let mut sessions = vec![];
    for _ in 0..PROCESS_SESSION_CAP {
        sessions.push(Session::new(1).unwrap());
    }
    assert!(Session::new(1).is_err());
    assert_eq!(process_usage(), (0, PROCESS_SESSION_CAP));
    assert_eq!(inotify_fds(), PROCESS_SESSION_CAP);
    drop(sessions);
    assert_eq!(process_usage(), (0, 0));
    assert_eq!(inotify_fds(), 0);
    println!(
        "PASS: aggregate cap {} watches / {} sessions enforced and fully released",
        PROCESS_WATCH_CAP, PROCESS_SESSION_CAP
    );
}
