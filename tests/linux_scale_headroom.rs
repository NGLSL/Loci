#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, Status};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize};

#[test]
fn default_scale_has_headroom_for_an_add_above_the_million_reference() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed.txt"), b"").unwrap();
    let mut options = EngineOptions::scale();
    assert!(
        options.limits.entries >= 1_250_000,
        "default must admit real1M inventory plus ordinary churn"
    );
    // Public configured capacity support can be proved on a bounded fixture;
    // the separate opt-in1M driver proves the actual million-to-million+1 add.
    options.limits.entries = 1_000_001;
    let database = fixture.base.join("index.loci");
    let mut engine = Engine::open_with_options(&fixture.root, Some(&database), options).unwrap();
    engine.save().unwrap();
    engine.stop().unwrap();
    drop(engine);
    let bytes = fs::read(&database).unwrap();
    assert!(bytes.starts_with(b"LOCISCL1"));
    let mut reopened =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    let lease = reopened.query().lease().unwrap();
    let page = lease
        .page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(page.paths, [fixture.root.join("seed.txt")]);
    assert!(page.complete);
    assert_eq!(reopened.view().status, Status::Pending);
    drop(lease);
    reopened.stop().unwrap();
}
#[test]
fn scale_capacity_ceiling_is_explicit_and_bounded_default_is_preserved() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed.txt"), b"").unwrap();
    for limit in [1_249_999, 1_250_000] {
        let mut options = EngineOptions::scale();
        options.limits.entries = limit;
        let mut engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
        engine.stop().unwrap();
    }
    let mut options = EngineOptions::scale();
    options.limits.entries = 1_250_001;
    let error = Engine::open_with_options(&fixture.root, None, options)
        .err()
        .expect("above explicit ceiling rejected");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    let bounded = EngineOptions::default();
    assert_eq!(bounded.limits.entries, 4096);
    assert_eq!(bounded.limits.directories, 128);
}
