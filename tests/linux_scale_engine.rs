#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, QueryHandle, Status};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Mutex;
use std::time::{Duration, Instant};

static NATIVE: Mutex<()> = Mutex::new(());

fn all(handle: &QueryHandle) -> Vec<PathBuf> {
    let lease = handle.lease().unwrap();
    let mut cursor = None;
    let mut paths = Vec::new();
    loop {
        let page = lease
            .page(
                "",
                cursor.as_ref(),
                1,
                &AtomicBool::new(false),
                &AtomicUsize::new(0),
            )
            .unwrap();
        paths.extend(page.paths);
        if page.complete {
            break;
        }
        cursor = page.next;
    }
    paths.sort();
    paths
}

fn settle(engine: &mut Engine, expected: &[PathBuf]) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        engine.poll().unwrap();
        if engine.view().status == Status::Validated && all(&engine.query()) == expected {
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
fn explicit_scale_mode_keeps_each_hard_link_searchable_after_one_is_deleted() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.root.join("first.txt"), b"same object").unwrap();
    fs::hard_link(
        fixture.root.join("first.txt"),
        fixture.root.join("second.txt"),
    )
    .unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    assert_eq!(
        all(&engine.query()),
        [
            fixture.root.join("first.txt"),
            fixture.root.join("second.txt")
        ]
    );
    fs::remove_file(fixture.root.join("first.txt")).unwrap();
    settle(&mut engine, &[fixture.root.join("second.txt")]);
    let legacy = Engine::open(&fixture.root, None).unwrap();
    assert_eq!(all(&legacy.query()), [fixture.root.join("second.txt")]);
}

#[test]
fn directory_rename_preserves_old_lease_paths_and_observes_later_children() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("old/deep")).unwrap();
    fs::write(fixture.root.join("old/deep/kept.rs"), b"x").unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let old = engine.query().lease().unwrap();
    let scans = engine.metrics().full_scans;
    fs::rename(fixture.root.join("old"), fixture.root.join("中文 renamed")).unwrap();
    settle(
        &mut engine,
        &[
            fixture.root.join("中文 renamed"),
            fixture.root.join("中文 renamed/deep"),
            fixture.root.join("中文 renamed/deep/kept.rs"),
        ],
    );
    let previous = old
        .page(
            "old ext:rs",
            None,
            50,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap();
    assert_eq!(previous.paths, [fixture.root.join("old/deep/kept.rs")]);
    assert!(!previous.validated_at_start_and_finish);
    assert_eq!(
        engine.metrics().full_scans,
        scans,
        "standalone rename should retain entry relationships"
    );
    assert_eq!(
        engine.metrics().last_touched_entries,
        1,
        "rename changes the directory relationship, not descendant paths"
    );
    drop(old);
    fs::write(fixture.root.join("中文 renamed/deep/later.txt"), b"x").unwrap();
    settle(
        &mut engine,
        &[
            fixture.root.join("中文 renamed"),
            fixture.root.join("中文 renamed/deep"),
            fixture.root.join("中文 renamed/deep/kept.rs"),
            fixture.root.join("中文 renamed/deep/later.txt"),
        ],
    );
}

#[test]
fn scale_mode_never_silently_reuses_or_overwrites_a_legacy_database() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed.txt"), b"x").unwrap();
    let database = fixture.base.join("legacy.loci");
    {
        let mut legacy = Engine::open(&fixture.root, Some(&database)).unwrap();
        legacy.save().unwrap();
    }
    let bytes = fs::read(&database).unwrap();
    let error = Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale())
        .err()
        .expect("scale mode must reject an existing unsupported checkpoint");
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    assert_eq!(fs::read(&database).unwrap(), bytes);
    let future = fixture.base.join("scale.loci");
    let mut scale =
        Engine::open_with_options(&fixture.root, Some(&future), EngineOptions::scale()).unwrap();
    assert_eq!(
        scale.save().unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
    assert!(!future.exists());
}

#[test]
fn unobserved_directory_replacing_an_entry_is_corrected_before_validation() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("active")).unwrap();
    // The bounded control exclusions are retained until scope configuration is
    // introduced. This inode has no recursively observed source entry.
    fs::create_dir(fixture.root.join("target")).unwrap();
    fs::write(fixture.root.join("target/incoming.txt"), b"x").unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let scans = engine.metrics().full_scans;
    fs::rename(fixture.root.join("target"), fixture.root.join("active")).unwrap();
    settle(
        &mut engine,
        &[
            fixture.root.join("active"),
            fixture.root.join("active/incoming.txt"),
        ],
    );
    assert!(
        engine.metrics().full_scans > scans,
        "unknown source identity requires correction"
    );
    fs::write(fixture.root.join("active/later.txt"), b"x").unwrap();
    settle(
        &mut engine,
        &[
            fixture.root.join("active"),
            fixture.root.join("active/incoming.txt"),
            fixture.root.join("active/later.txt"),
        ],
    );
}

#[test]
fn replaced_root_fails_the_source_and_retains_old_snapshot_paths() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.root.join("old.txt"), b"x").unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let handle = engine.query();
    fs::rename(&fixture.root, fixture.base.join("original-root")).unwrap();
    fs::create_dir(&fixture.root).unwrap();
    fs::write(fixture.root.join("different.txt"), b"x").unwrap();
    assert!(engine.poll().is_err());
    assert!(matches!(handle.view().status, Status::Failed(_)));
    assert_eq!(all(&handle), [fixture.root.join("old.txt")]);
    engine.stop().unwrap();
    assert_eq!(handle.view().status, Status::Stopped);
}
