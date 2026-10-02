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
    let mut options = EngineOptions::scale();
    options.scale_budgets.max_snapshot_bytes = 50_000;
    options.scale_budgets.max_retained_bytes = 51_000;
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
    assert!(engine.view().resources.retained_snapshot_bytes <= 51_000);
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
