use crate::watch::*;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
struct Fixture {
    base: PathBuf,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let work = std::env::current_dir().unwrap().join("work");
        fs::create_dir_all(&work).unwrap();
        let base = work.join(format!(
            "watch-fixture-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let root = base.join("data");
        fs::create_dir(&root).unwrap();
        Self {
            base: fs::canonicalize(base).unwrap(),
            root: fs::canonicalize(root).unwrap(),
        }
    }
    fn inventory(&self) -> Inventory {
        scan(&self.root, Limits::default(), |_| Ok(()))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Verify the resolved path is the disposable fixture inside this project's work/.
        let work = fs::canonicalize(std::env::current_dir().unwrap().join("work")).unwrap();
        let base = fs::canonicalize(&self.base).unwrap();
        assert!(
            base.starts_with(&work)
                && base
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("watch-fixture-")
        );
        fs::remove_dir_all(base).unwrap();
    }
}

#[test]
fn real_add_delete_file_and_directory_rename_reconcile() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("old")).unwrap();
    fs::write(f.root.join("old/a.rs"), "x").unwrap();
    let mut state = Recovery::new(4);
    assert!(state.publish(state.ticket(), f.inventory()));
    fs::write(f.root.join("new.txt"), "x").unwrap();
    state.signal(Signal::Change);
    assert!(state.publish(state.ticket(), f.inventory()));
    assert!(state
        .inventory
        .entries
        .contains_key(&PathBuf::from("new.txt")));
    fs::rename(f.root.join("old/a.rs"), f.root.join("old/b.rs")).unwrap();
    state.signal(Signal::Change);
    assert!(state.publish(state.ticket(), f.inventory()));
    fs::rename(f.root.join("old"), f.root.join("renamed")).unwrap();
    state.signal(Signal::Change);
    assert!(state.publish(state.ticket(), f.inventory()));
    assert!(!state
        .inventory
        .entries
        .contains_key(&PathBuf::from("old/b.rs")));
    assert!(state
        .inventory
        .entries
        .contains_key(&PathBuf::from("renamed/b.rs")));
    fs::remove_file(f.root.join("renamed/b.rs")).unwrap();
    fs::remove_dir(f.root.join("renamed")).unwrap();
    state.signal(Signal::Change);
    assert!(state.publish(state.ticket(), f.inventory()));
    assert_eq!(state.inventory.entries.len(), 1);
}
#[test]
fn simulated_loss_and_user_queue_overflow_remain_dirty_until_rescan() {
    let f = Fixture::new();
    let mut state = Recovery::new(2);
    assert!(state.publish(state.ticket(), f.inventory()));
    fs::write(f.root.join("lost.rs"), "x").unwrap(); // Event deliberately never sent.
    state.signal(Signal::KernelOverflow);
    assert!(state.dirty);
    for _ in 0..10 {
        state.signal(Signal::Change);
        assert!(state.pending() <= 2);
    }
    assert!(state.reasons.contains(&Signal::UserOverflow));
    assert!(state.reasons.contains(&Signal::KernelOverflow));
    assert!(state.publish(state.ticket(), f.inventory()));
    assert!(!state.dirty);
    assert!(state
        .inventory
        .entries
        .contains_key(&PathBuf::from("lost.rs")));
}
#[test]
fn scan_race_rejects_old_generation_and_keeps_last_good_snapshot() {
    let f = Fixture::new();
    fs::write(f.root.join("a"), "x").unwrap();
    let mut state = Recovery::new(2);
    assert!(state.publish(state.ticket(), f.inventory()));
    let ticket = state.ticket();
    let old = f.inventory();
    fs::write(f.root.join("racing"), "x").unwrap();
    state.signal(Signal::Change);
    assert!(!state.publish(ticket, old));
    assert!(state.dirty);
    assert_eq!(state.inventory.entries.len(), 1);
    assert!(state.publish(state.ticket(), f.inventory()));
    assert_eq!(state.inventory.entries.len(), 2);
}
#[test]
fn restart_checkpoint_is_stale_and_offline_changes_are_corrected() {
    let f = Fixture::new();
    fs::write(f.root.join("old"), "x").unwrap();
    let file = f.base.join("state.bin");
    save_checkpoint(&file, &f.root, &f.inventory()).unwrap();
    fs::remove_file(f.root.join("old")).unwrap();
    fs::write(f.root.join("while-offline"), "x").unwrap();
    let loaded = load_checkpoint(&file, &f.root, Limits::default()).unwrap();
    let mut state = Recovery::restored(loaded, 2);
    assert!(state.dirty && state.reasons.contains(&Signal::Restart));
    assert!(state.inventory.entries.contains_key(&PathBuf::from("old")));
    assert!(state.publish(state.ticket(), f.inventory()));
    assert_eq!(state.inventory.entries.len(), 1);
    assert!(state
        .inventory
        .entries
        .contains_key(&PathBuf::from("while-offline")));
    let mut bytes = fs::read(&file).unwrap();
    bytes[12] ^= 1;
    fs::write(&file, bytes).unwrap();
    assert!(load_checkpoint(&file, &f.root, Limits::default()).is_err());
}
#[test]
fn watch_scan_depth_entry_and_failure_budgets_cannot_publish_partial() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("d")).unwrap();
    fs::write(f.root.join("d/a"), "x").unwrap();
    fs::write(f.root.join("b"), "x").unwrap();
    for limits in [
        Limits {
            entries: 1,
            ..Limits::default()
        },
        Limits {
            directories: 1,
            ..Limits::default()
        },
        Limits {
            depth: 0,
            ..Limits::default()
        },
    ] {
        let out = scan(&f.root, limits, |_| Ok(()));
        assert!(!out.complete);
        let mut state = Recovery::new(1);
        assert!(!state.publish(state.ticket(), out));
        assert!(state.dirty);
        assert!(!state.errors.is_empty());
    }
    let out = scan(&f.root, Limits::default(), |_| {
        Err(std::io::Error::other("injected ENOSPC/permission error"))
    });
    assert!(!out.complete);
}
#[test]
fn watcher_callback_precedes_each_directory_enumeration() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("child")).unwrap();
    let mut registered = BTreeSet::new();
    let out = scan(&f.root, Limits::default(), |dir| {
        registered.insert(dir.to_path_buf());
        fs::write(dir.join("after-watch"), "x")?;
        Ok(())
    });
    assert!(out.complete);
    assert_eq!(registered.len(), 2);
    assert!(out
        .entries
        .contains_key(&PathBuf::from("child/after-watch")));
}
