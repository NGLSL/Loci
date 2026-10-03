#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, QueryHandle, Status};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

fn paths(handle: &QueryHandle) -> Vec<PathBuf> {
    let page = handle
        .lease()
        .unwrap()
        .page(
            "",
            None,
            1024,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap();
    assert!(page.complete);
    page.paths
}
fn fail(engine: &mut Engine) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let _ = engine.poll();
        if matches!(engine.view().status, Status::Failed(_)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no explicit failure: {:?}",
            engine.view()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn exhausted_name_arena_preserves_published_paths_and_reports_usage() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("a"), b"").unwrap();
    let mut options = EngineOptions::scale();
    options.scale_budgets.max_name_bytes = 3;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    let handle = engine.query();
    let version = engine.view().version;
    fs::rename(fixture.root.join("a"), fixture.root.join("longer")).unwrap();
    fail(&mut engine);
    assert_eq!(engine.view().version, version);
    assert_eq!(paths(&handle), [fixture.root.join("a")]);
    assert!(engine.view().resources.inventory_name_bytes <= 3);
}
#[test]
fn retained_byte_credit_rejects_a_local_copy_before_publication() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed.txt"), b"").unwrap();
    let baseline = Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let bytes = baseline.view().resources.snapshot_bytes;
    drop(baseline);
    let mut options = EngineOptions::scale();
    options.scale_budgets.max_snapshot_bytes = bytes + 1024;
    options.scale_budgets.max_retained_bytes = bytes + 2048;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    // A small snapshot uses one fixed-capacity segment. The next local COW
    // would retain two segments, exceeding this deliberately narrow budget.
    assert_eq!(engine.view().status, Status::Validated);
    let handle = engine.query();
    let version = engine.view().version;
    fs::write(fixture.root.join("new.txt"), b"").unwrap();
    fail(&mut engine);
    assert_eq!(engine.view().version, version);
    assert_eq!(paths(&handle), [fixture.root.join("seed.txt")]);
    assert!(engine.view().resources.retained_snapshot_bytes <= bytes + 2048);
}
#[test]
fn configured_lease_limit_is_released_when_a_reader_drops() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("a.txt"), b"").unwrap();
    let mut options = EngineOptions::scale();
    options.scale_budgets.max_leases = 1;
    let engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    let handle = engine.query();
    let lease = handle.lease().unwrap();
    assert!(handle.lease().is_err());
    drop(lease);
    assert!(handle.lease().is_ok());
}

#[test]
fn cli_waits_for_batched_scale_build_before_exporting_all_rows() {
    let fixture = Fixture::new();
    for number in 0..80 {
        fs::write(fixture.root.join(format!("file_{number:03}.txt")), b"").unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "query"])
        .arg(&fixture.root)
        .arg(fixture.base.join("not_saved.loci"))
        .args([
            "",
            "--scale",
            "--all",
            "--null",
            "--scan-batch",
            "1",
            "--entries",
            "1000",
            "--directories",
            "100",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let paths: Vec<_> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    assert_eq!(paths.len(), 80);
    assert!(!fixture.base.join("not_saved.loci").exists());
}

#[test]
fn deleted_slots_are_reclaimed_before_another_live_entry_exhausts_the_budget() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("old"), b"").unwrap();
    let mut options = EngineOptions::scale();
    options.scale_budgets.max_slots = 2;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    let previous = engine.view().version;
    fs::remove_file(fixture.root.join("old")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.view().version == previous {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    let version = engine.view().version;
    assert!(paths(&engine.query()).is_empty());
    fs::write(fixture.root.join("new"), b"").unwrap();
    while engine.view().version <= version || engine.view().status != Status::Validated {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    assert_eq!(paths(&engine.query()), [fixture.root.join("new")]);
    assert_eq!(engine.metrics().full_scans, 1);
    assert_eq!(engine.view().resources.inventory_slots, 2);
}

#[test]
fn directory_move_beyond_depth_preserves_old_query_and_reopenable_checkpoint() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("a")).unwrap();
    fs::write(fixture.root.join("a/file"), b"").unwrap();
    fs::create_dir(fixture.root.join("b")).unwrap();
    let database = fixture.base.join("state.loci");
    let mut options = EngineOptions::scale();
    options.limits.depth = 1;
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), options.clone()).unwrap();
    assert_eq!(engine.view().status, Status::Validated);
    let handle = engine.query();
    let old = paths(&handle);
    engine.save().unwrap();
    let saved = fs::read(&database).unwrap();
    fs::rename(fixture.root.join("a"), fixture.root.join("b/a")).unwrap();
    fail(&mut engine);
    assert_eq!(paths(&handle), old);
    assert_eq!(
        engine.view().coverage_gaps[0].path,
        fixture.root.join("b/a")
    );
    assert!(
        !handle
            .lease()
            .unwrap()
            .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap()
            .validated_at_start_and_finish
    );
    if engine.save().is_err() {
        assert_eq!(fs::read(&database).unwrap(), saved);
    }
    engine.stop().unwrap();
    let reopened = Engine::open_with_options(&fixture.root, Some(&database), options).unwrap();
    assert_eq!(paths(&reopened.query()), old);
}

#[test]
fn directory_move_cannot_publish_descendants_beyond_checkpoint_path_bytes() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    let mut leaf = fixture.root.join("a");
    fs::create_dir(&leaf).unwrap();
    for number in 0..15 {
        leaf.push(format!("{number:02}{}", "d".repeat(238)));
        fs::create_dir(&leaf).unwrap();
    }
    leaf.push("f".repeat(230));
    assert!(leaf.as_os_str().as_bytes().len() <= 4096);
    fs::write(&leaf, b"").unwrap();
    let destination_parent = fixture.root.join("p".repeat(245));
    fs::create_dir(&destination_parent).unwrap();
    let mut options = EngineOptions::scale();
    options.limits.depth = 32;
    let database = fixture.base.join("state.loci");
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), options.clone()).unwrap();
    let old = paths(&engine.query());
    engine.save().unwrap();
    fs::rename(fixture.root.join("a"), destination_parent.join("a")).unwrap();
    fail(&mut engine);
    assert_eq!(paths(&engine.query()), old);
    assert!(engine.view().coverage_gaps[0]
        .path
        .starts_with(destination_parent.join("a")));
    assert!(
        engine.view().coverage_gaps[0]
            .path
            .as_os_str()
            .as_bytes()
            .len()
            > 4096
    );
    engine.stop().unwrap();
    let reopened = Engine::open_with_options(&fixture.root, Some(&database), options).unwrap();
    assert_eq!(paths(&reopened.query()), old);
}

#[test]
fn reliable_new_directory_is_bounded_local_work_and_preserves_old_lease() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed"), b"").unwrap();
    let mut options = EngineOptions::scale();
    options.scan_batch = 1;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    while engine.view().status != Status::Validated {
        engine.poll().unwrap();
    }
    let lease = engine.query().lease().unwrap();
    let source = fixture.root.join("incoming");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("nested/new"), b"").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        engine.poll().unwrap();
        if engine.view().status == Status::Validated && paths(&engine.query()).len() == 4 {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", engine.view());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        engine.metrics().full_scans,
        1,
        "reliable known-parent mkdir/move-in must stay local"
    );
    let mut actual = paths(&engine.query());
    actual.sort();
    assert_eq!(
        actual,
        [
            fixture.root.join("incoming"),
            fixture.root.join("incoming/nested"),
            fixture.root.join("incoming/nested/new"),
            fixture.root.join("seed")
        ]
    );
    assert_eq!(
        lease
            .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap()
            .paths,
        [fixture.root.join("seed")]
    );
    drop(lease);
    let version = engine.view().version;
    fs::write(fixture.root.join("incoming/nested/later"), b"").unwrap();
    while engine.view().version == version || engine.view().status != Status::Validated {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    assert_eq!(paths(&engine.query()).len(), 5);
    assert_eq!(engine.metrics().full_scans, 1);
}

#[test]
fn cancelling_a_batched_new_directory_preserves_the_last_published_cut() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed"), b"").unwrap();
    let mut options = EngineOptions::scale();
    options.scan_batch = 1;
    let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    while engine.view().status != Status::Validated {
        engine.poll().unwrap();
    }
    fs::create_dir(fixture.root.join("new")).unwrap();
    for number in 0..50 {
        fs::write(fixture.root.join(format!("new/file-{number}")), b"").unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.view().status == Status::Validated {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    engine.poll_with_cancel(&AtomicBool::new(true)).unwrap();
    assert_eq!(paths(&engine.query()), [fixture.root.join("seed")]);
    assert_eq!(engine.view().status, Status::Pending);
    assert!(engine
        .view()
        .coverage_gaps
        .iter()
        .any(|gap| gap.kind == loci_experiment::engine::CoverageGapKind::Cancelled));
    engine.request_rebuild().unwrap();
    while engine.view().status != Status::Validated {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    assert_eq!(paths(&engine.query()).len(), 52);
}

#[test]
fn directory_move_with_deleted_long_child_can_save_and_reopen_its_live_paths() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    let mut leaf = fixture.root.join("a");
    fs::create_dir(&leaf).unwrap();
    for number in 0..15 {
        leaf.push(format!("{number:02}{}", "d".repeat(238)));
        fs::create_dir(&leaf).unwrap();
    }
    let deleted = leaf.join("f".repeat(230));
    fs::write(&deleted, b"").unwrap();
    let destination = fixture.root.join("p".repeat(245));
    fs::create_dir(&destination).unwrap();
    let mut options = EngineOptions::scale();
    options.limits.depth = 32;
    let database = fixture.base.join("state.loci");
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), options.clone()).unwrap();
    fs::remove_file(deleted).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while paths(&engine.query()).len() != 17 || engine.view().status != Status::Validated {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    fs::rename(fixture.root.join("a"), destination.join("a")).unwrap();
    while paths(&engine.query())
        .iter()
        .any(|path| path.starts_with(fixture.root.join("a")))
        || engine.view().status != Status::Validated
    {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    let expected = paths(&engine.query());
    assert!(expected
        .iter()
        .all(|path| path.as_os_str().as_bytes().len() <= 4096));
    engine.save().unwrap();
    engine.stop().unwrap();
    let reopened = Engine::open_with_options(&fixture.root, Some(&database), options).unwrap();
    let mut actual = paths(&reopened.query());
    actual.sort();
    let mut expected = expected;
    expected.sort();
    assert_eq!(actual, expected);
}
