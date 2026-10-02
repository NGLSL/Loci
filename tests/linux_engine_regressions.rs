//! Independent acceptance regressions: expected red on 4cedb870.
#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, Status};
use loci_experiment::storage::Snapshot;
use loci_experiment::watch::Kind;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

fn paths(engine: &Engine) -> Vec<PathBuf> {
    let result = engine
        .query()
        .lease()
        .unwrap()
        .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert!(result.complete && result.validated_at_start_and_finish);
    result.paths
}

fn await_validated(engine: &mut Engine) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let published = engine.poll().unwrap();
        if published && engine.view().status == Status::Validated {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "did not settle: {:?}",
            engine.view()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn excluded_directory_replacement_imports_children_and_keeps_watching() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    fs::create_dir(root.join("active")).unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("target/hidden.txt"), b"x").unwrap();
    let mut engine = Engine::open(root, None).unwrap();
    assert_eq!(paths(&engine), [root.join("active")]);

    // Linux permits replacing an existing empty directory with this rename.
    // The incoming inode was excluded and has not had a recursive watch.
    fs::rename(root.join("target"), root.join("active")).unwrap();
    await_validated(&mut engine);
    let after_rename = paths(&engine);

    // Check future events as well as the contents already present at replacement.
    fs::write(root.join("active/later.txt"), b"x").unwrap();
    std::thread::sleep(Duration::from_millis(280));
    await_validated(&mut engine);
    let after_later_write = paths(&engine);
    engine.stop().unwrap();

    assert_eq!(
        (after_rename, after_later_write),
        (
            vec![root.join("active"), root.join("active/hidden.txt")],
            vec![
                root.join("active"),
                root.join("active/hidden.txt"),
                root.join("active/later.txt"),
            ],
        ),
        "a Validated snapshot must contain the admitted subtree and later child"
    );
}

#[test]
fn changed_database_parent_cannot_redirect_saves_inside_root() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    let outside = fixture.base.join("outside-data");
    let original = fixture.base.join("original-data");
    let inside = root.join("inside-data");
    fs::create_dir(&outside).unwrap();
    fs::create_dir(&inside).unwrap();
    fs::write(root.join("seed.txt"), b"seed").unwrap();
    let mut engine = Engine::open(root, Some(&outside.join("state.loci"))).unwrap();
    engine.save().unwrap();
    let before = fs::read(outside.join("state.loci")).unwrap();

    // A successful redirected save must actually persist the updated snapshot,
    // rather than merely returning Ok without touching either destination.
    fs::write(root.join("added.txt"), b"added").unwrap();
    await_validated(&mut engine);

    // Change the accepted parent between API calls; no concurrent race is needed.
    fs::rename(&outside, &original).unwrap();
    std::os::unix::fs::symlink(&inside, &outside).unwrap();
    let result = engine.save();
    let inside_entries = fs::read_dir(&inside).unwrap().count();
    engine.stop().unwrap();

    // Refusal or a write bound to the original outside directory can both be safe.
    // Merely requiring Err would overconstrain a directory-handle based fix.
    assert_eq!(
        inside_entries, 0,
        "save must not write through a redirected parent into its root: {result:?}"
    );
    if result.is_err() {
        assert_eq!(fs::read(original.join("state.loci")).unwrap(), before);
    } else {
        let saved = Snapshot::load(&original.join("state.loci"), root).unwrap();
        assert_eq!(
            saved
                .inventory
                .entries
                .get(std::path::Path::new("added.txt")),
            Some(&Kind::File),
            "a successful save must write the updated inventory to its original parent"
        );
    }
}

#[test]
fn same_database_parent_moved_inside_root_is_rejected_despite_matching_identity() {
    let fixture = Fixture::new();
    let root = &fixture.root;
    let outside = fixture.base.join("outside");
    let parent = outside.join("data");
    let moved = root.join("relocated");
    fs::create_dir_all(&parent).unwrap();
    fs::write(root.join("seed.txt"), b"seed").unwrap();
    let database = parent.join("state.loci");
    let mut engine = Engine::open(root, Some(&database)).unwrap();
    engine.save().unwrap();
    let before = fs::read(&database).unwrap();

    // Retain the same parent inode but redirect an ancestor to its new location
    // inside root. Parent identity alone is insufficient to preserve containment.
    fs::rename(&outside, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &outside).unwrap();
    await_validated(&mut engine);
    let error = engine.save().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    engine.stop().unwrap();
    assert_eq!(fs::read(moved.join("data/state.loci")).unwrap(), before);
    assert_eq!(fs::read_dir(moved.join("data")).unwrap().count(), 1);
}
