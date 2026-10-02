//! Dependent same-batch child changes must not be lost when an ancestor is renamed.
mod common;
use common::Fixture;
use loci_experiment::incremental::{Change, Portable};
use loci_experiment::live::{QueryHandle, Status};
use loci_experiment::watch::{Limits, Signal};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::Duration;
fn paths(handle: &QueryHandle, q: &str) -> Vec<PathBuf> {
    handle
        .lease()
        .unwrap()
        .search(q, false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap()
        .paths
}
fn initial() -> Fixture {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("old/nested")).unwrap();
    fs::write(f.root.join("old/nested/seed.rs"), "x").unwrap();
    f
}
#[test]
fn portable_child_create_before_parent_rename_requires_correction() {
    let f = initial();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    fs::write(f.root.join("old/new.rs"), "x").unwrap();
    p.enqueue(Change::Refresh("old/new.rs".into()));
    fs::rename(f.root.join("old"), f.root.join("new")).unwrap();
    p.enqueue(Change::Rename {
        from: "old".into(),
        to: "new".into(),
    });
    let published = p.tick(Duration::from_secs(1)).unwrap();
    println!(
        "prior-child-create published={published},status={:?},new_file_query={:?}",
        p.store.handle().view().status,
        paths(&p.store.handle(), "new.rs")
    );
    assert!(
        !published,
        "a dependent batch must not publish incomplete Validated contents"
    );
    assert_eq!(p.store.handle().view().version, 1);
    assert_eq!(p.store.handle().view().status, Status::Pending);
    assert!(p.state.reasons.contains(&Signal::GenerationRace));
    assert!(p.tick(Duration::from_secs(2)).unwrap());
    assert_eq!(
        paths(&p.store.handle(), "new.rs"),
        [PathBuf::from("new/new.rs")]
    );
    assert_eq!(
        paths(&p.store.handle(), "seed.rs"),
        [PathBuf::from("new/nested/seed.rs")]
    );
    assert_eq!(p.metrics.full_scans, 2);
}
#[test]
fn portable_child_rename_before_parent_rename_requires_correction() {
    let f = initial();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    fs::rename(
        f.root.join("old/nested/seed.rs"),
        f.root.join("old/nested/renamed.rs"),
    )
    .unwrap();
    p.enqueue(Change::Rename {
        from: "old/nested/seed.rs".into(),
        to: "old/nested/renamed.rs".into(),
    });
    fs::rename(f.root.join("old"), f.root.join("new")).unwrap();
    p.enqueue(Change::Rename {
        from: "old".into(),
        to: "new".into(),
    });
    assert!(!p.tick(Duration::from_secs(1)).unwrap());
    assert_eq!(p.store.handle().view().version, 1);
    assert!(p.tick(Duration::from_secs(2)).unwrap());
    assert_eq!(
        paths(&p.store.handle(), "renamed.rs"),
        [PathBuf::from("new/nested/renamed.rs")]
    );
    assert!(paths(&p.store.handle(), "seed.rs").is_empty());
    assert_eq!(p.metrics.full_scans, 2);
}
#[cfg(target_os = "linux")]
fn native_settle(n: &mut loci_experiment::incremental::Native, wanted: &str) {
    let started = std::time::Instant::now();
    loop {
        n.tick().unwrap();
        if n.store.handle().view().status == Status::Validated
            && paths(&n.store.handle(), wanted) == [PathBuf::from(wanted)]
        {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "native correction did not converge"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
#[cfg(target_os = "linux")]
fn native_child_create_and_subtree_before_parent_rename_are_not_omitted() {
    let f = initial();
    let mut n = loci_experiment::incremental::Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    fs::write(f.root.join("old/new.rs"), "x").unwrap();
    fs::create_dir(f.root.join("old/new-subtree")).unwrap();
    fs::write(f.root.join("old/new-subtree/child.rs"), "x").unwrap();
    fs::rename(f.root.join("old"), f.root.join("new")).unwrap();
    std::thread::sleep(Duration::from_millis(270));
    let published = n.tick().unwrap();
    println!("native prior-child-create published={published},status={:?},new_file_query={:?},full_scans={}",n.store.handle().view().status,paths(&n.store.handle(),"new.rs"),n.metrics.full_scans);
    assert!(
        !published,
        "real dependent batch must not validate missing child paths"
    );
    assert_eq!(n.store.handle().view().version, 1);
    assert!(n.watch.state.reasons.contains(&Signal::GenerationRace));
    native_settle(&mut n, "new/new.rs");
    assert_eq!(
        paths(&n.store.handle(), "child.rs"),
        [PathBuf::from("new/new-subtree/child.rs")]
    );
    assert_eq!(n.metrics.full_scans, 2);
    assert_eq!(n.watch.watches(), 4);
    fs::write(f.root.join("new/new-subtree/later.rs"), "x").unwrap();
    native_settle(&mut n, "new/new-subtree/later.rs");
    assert_eq!(
        n.metrics.full_scans, 2,
        "post-recovery events should use the incremental path"
    );
}
#[test]
#[cfg(target_os = "linux")]
fn native_child_rename_before_parent_rename_preserves_the_file() {
    let f = initial();
    let mut n = loci_experiment::incremental::Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    fs::rename(
        f.root.join("old/nested/seed.rs"),
        f.root.join("old/nested/renamed.rs"),
    )
    .unwrap();
    fs::rename(f.root.join("old"), f.root.join("new")).unwrap();
    std::thread::sleep(Duration::from_millis(270));
    assert!(!n.tick().unwrap());
    native_settle(&mut n, "new/nested/renamed.rs");
    assert!(paths(&n.store.handle(), "seed.rs").is_empty());
    assert_eq!(n.metrics.full_scans, 2);
    assert_eq!(n.watch.watches(), 3);
}

#[test]
fn portable_reused_rename_source_does_not_substitute_replacement_contents() {
    let f = Fixture::new();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    fs::create_dir(f.root.join("old")).unwrap();
    fs::write(f.root.join("old/a.rs"), "x").unwrap();
    p.enqueue(Change::Refresh("old".into()));
    fs::rename(f.root.join("old"), f.root.join("new")).unwrap();
    p.enqueue(Change::Rename {
        from: "old".into(),
        to: "new".into(),
    });
    fs::create_dir(f.root.join("old")).unwrap();
    fs::write(f.root.join("old/b.rs"), "x").unwrap();
    p.enqueue(Change::Refresh("old".into()));
    assert!(!p.tick(Duration::from_secs(1)).unwrap());
    assert_eq!(p.store.handle().view().version, 1);
    assert!(p.tick(Duration::from_secs(2)).unwrap());
    assert_eq!(
        paths(&p.store.handle(), "ext:rs"),
        [PathBuf::from("new/a.rs"), PathBuf::from("old/b.rs")]
    );
    assert_eq!(p.metrics.full_scans, 2);
}

#[test]
fn portable_refreshed_ancestor_cannot_supply_future_replacement_subtree() {
    let f = Fixture::new();
    let mut p = Portable::new(&f.root, Limits::default()).unwrap();
    assert!(p.tick(Duration::ZERO).unwrap());
    fs::create_dir_all(f.root.join("parent/old")).unwrap();
    fs::write(f.root.join("parent/old/a.rs"), "x").unwrap();
    p.enqueue(Change::Refresh("parent".into()));
    fs::rename(f.root.join("parent/old"), f.root.join("new")).unwrap();
    p.enqueue(Change::Rename {
        from: "parent/old".into(),
        to: "new".into(),
    });
    fs::create_dir(f.root.join("parent/old")).unwrap();
    fs::write(f.root.join("parent/old/b.rs"), "x").unwrap();
    p.enqueue(Change::Refresh("parent/old".into()));
    assert!(!p.tick(Duration::from_secs(1)).unwrap());
    assert_eq!(p.store.handle().view().version, 1);
    assert!(p.tick(Duration::from_secs(2)).unwrap());
    assert_eq!(
        paths(&p.store.handle(), "ext:rs"),
        [PathBuf::from("new/a.rs"), PathBuf::from("parent/old/b.rs")]
    );
    assert_eq!(p.metrics.full_scans, 2);
}

#[test]
#[cfg(target_os = "linux")]
fn native_atomic_save_file_batch_keeps_incremental_fast_path() {
    let f = Fixture::new();
    let mut n = loci_experiment::incremental::Native::new(&f.root, Limits::default()).unwrap();
    assert!(n.tick().unwrap());
    fs::write(f.root.join("temporary"), "x").unwrap();
    fs::rename(f.root.join("temporary"), f.root.join("saved.rs")).unwrap();
    std::thread::sleep(Duration::from_millis(270));
    assert!(n.tick().unwrap());
    assert_eq!(
        paths(&n.store.handle(), "saved.rs"),
        [PathBuf::from("saved.rs")]
    );
    assert_eq!(
        n.metrics.full_scans, 1,
        "ordinary file publication must not trigger root correction"
    );
}
