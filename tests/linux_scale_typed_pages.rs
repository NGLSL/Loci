#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, EntryKind, QueryLease, Status};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

fn typed(lease: &QueryLease) -> BTreeMap<std::path::PathBuf, EntryKind> {
    let mut out = BTreeMap::new();
    let mut cursor = None;
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
        let kinds = page.kinds.expect("scale pages must retain snapshot kinds");
        assert_eq!(page.paths.len(), kinds.len());
        for (path, kind) in page.paths.into_iter().zip(kinds) {
            assert!(out.insert(path, kind).is_none());
        }
        if page.complete {
            break;
        }
        cursor = page.next;
    }
    out
}
fn await_count(engine: &mut Engine, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        engine.poll().unwrap();
        if engine.view().status == Status::Validated
            && typed(&engine.query().lease().unwrap()).len() == count
        {
            return;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
fn raw_typed_pages_and_sorted_pages_follow_their_leased_snapshot() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("old")).unwrap();
    fs::write(fixture.root.join("old/报告.txt"), b"").unwrap();
    let raw = fixture
        .root
        .join("old")
        .join(std::ffi::OsString::from_vec(b"raw_\xff.txt".to_vec()));
    fs::write(&raw, b"").unwrap();
    fs::hard_link(&raw, fixture.root.join("hardlink.txt")).unwrap();
    symlink("old/报告.txt", fixture.root.join("link")).unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let old = engine.query().lease().unwrap();
    let initial = typed(&old);
    assert_eq!(initial[&raw], EntryKind::File);
    assert_eq!(initial[&fixture.root.join("hardlink.txt")], EntryKind::File);
    assert_eq!(initial[&fixture.root.join("old")], EntryKind::Directory);
    assert_eq!(initial[&fixture.root.join("link")], EntryKind::Symlink);
    let job = engine.query().start_sort("").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while job.count().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let page = job.page(0, 50).unwrap();
    assert_eq!(page.paths.len(), page.kinds.as_ref().unwrap().len());
    let sorted: BTreeMap<_, _> = page.paths.into_iter().zip(page.kinds.unwrap()).collect();
    assert_eq!(sorted, initial);
    drop(job);
    fs::rename(fixture.root.join("old"), fixture.root.join("new")).unwrap();
    fs::remove_file(fixture.root.join("link")).unwrap();
    fs::write(fixture.root.join("link"), b"").unwrap();
    await_count(&mut engine, 5);
    // Native event delivery may already have the old count; require the new kind/path.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        engine.poll().unwrap();
        let current = typed(&engine.query().lease().unwrap());
        if current.get(&fixture.root.join("new")) == Some(&EntryKind::Directory)
            && current.get(&fixture.root.join("link")) == Some(&EntryKind::File)
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(typed(&old), initial);
}
#[test]
fn typed_first_page_keeps_bounded_query_work_and_compatibility_pages_have_no_kinds() {
    let fixture = Fixture::new();
    for i in 0..2000 {
        fs::write(fixture.root.join(format!("item_{i:08}.txt")), b"").unwrap();
    }
    let engine = Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let before = engine.metrics().clone();
    let progress = AtomicUsize::new(0);
    let page = engine
        .query()
        .lease()
        .unwrap()
        .page("", None, 10, &AtomicBool::new(false), &progress)
        .unwrap();
    assert_eq!(page.paths.len(), 10);
    assert_eq!(page.kinds.unwrap(), vec![EntryKind::File; 10]);
    assert!(
        progress.load(std::sync::atomic::Ordering::Acquire) <= 64,
        "typed metadata must use matched IDs, without a second slot search"
    );
    assert_eq!(engine.metrics().full_scans, before.full_scans);
    assert_eq!(engine.metrics().metadata_calls, before.metadata_calls);
    drop(engine);
    let bounded = Engine::open(&fixture.root, None).unwrap();
    let page = bounded
        .query()
        .lease()
        .unwrap()
        .page("", None, 10, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert!(
        page.kinds.is_none(),
        "bounded snapshot lacks retained types"
    );
}
