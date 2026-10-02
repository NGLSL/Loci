#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, Status};
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize};

#[test]
fn linux_new_engine_opens_saves_and_reconciles_offline_changes() {
    let f = Fixture::new();
    let db = f.base.join("inventory.loci");
    fs::write(f.root.join("中文 old.rs"), "x").unwrap();
    fs::write(f.root.join("deleted.txt"), "x").unwrap();
    let mut engine = Engine::open(&f.root, Some(&db)).unwrap();
    assert_eq!(engine.view().status, Status::Validated);
    engine.save().unwrap();
    engine.stop().unwrap();
    fs::rename(f.root.join("中文 old.rs"), f.root.join("中文 new.rs")).unwrap();
    fs::remove_file(f.root.join("deleted.txt")).unwrap();
    fs::write(f.root.join("added.txt"), "x").unwrap();
    let engine = Engine::open(&f.root, Some(&db)).unwrap();
    assert_eq!(engine.view().status, Status::Validated);
    let query = engine.query();
    let out = query
        .lease()
        .unwrap()
        .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    assert_eq!(
        out.paths,
        [f.root.join("added.txt"), f.root.join("中文 new.rs")]
    );
    assert!(out.complete && out.validated_at_start_and_finish);
}
