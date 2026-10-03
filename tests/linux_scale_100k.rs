#![cfg(target_os = "linux")]
mod common;
#[path = "common/fixture_capacity.rs"]
mod fixture_capacity;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, QueryHandle, Status};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::time::{Duration, Instant};

fn oracle(root: &Path) -> Vec<PathBuf> {
    let mut todo = vec![root.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(directory) = todo.pop() {
        for child in fs::read_dir(directory).unwrap() {
            let child = child.unwrap();
            let path = child.path();
            if child.file_type().unwrap().is_dir() {
                todo.push(path.clone());
            }
            paths.push(path);
        }
    }
    paths.sort();
    paths
}
fn pages(handle: &QueryHandle) -> Vec<PathBuf> {
    let lease = handle.lease().unwrap();
    let mut cursor = None;
    let mut paths = Vec::new();
    loop {
        let page = lease
            .page(
                "",
                cursor.as_ref(),
                1024,
                &AtomicBool::new(false),
                &AtomicUsize::new(0),
            )
            .unwrap();
        assert!(!page.cancelled);
        paths.extend(page.paths);
        if page.complete {
            break;
        }
        cursor = page.next;
    }
    paths.sort();
    paths
}
fn settle(engine: &mut Engine, previous: u64) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while engine.view().version <= previous || engine.view().status != Status::Validated {
        engine.poll().unwrap();
        assert!(
            Instant::now() < deadline,
            "did not validate: {:?}",
            engine.view()
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
#[ignore = "creates 100,000 real entries and performs full native/oracle checks"]
fn real_hundred_thousand_entries_remain_correct_after_local_updates() {
    let fixture = Fixture::new();
    if !fixture_capacity::hundred_thousand_fixture_preflight(&fixture.base) {
        return;
    }
    let create = Instant::now();
    let families = [
        ("invoice", "txt"),
        ("report", "pdf"),
        ("source", "rs"),
        ("报告", "docx"),
        ("ab_notes", "md"),
    ];
    for directory in 0..2000 {
        let folder = fixture.root.join(format!("dir{directory:05}"));
        fs::create_dir(&folder).unwrap();
        for child in 0..49 {
            let number = directory * 49 + child;
            let (prefix, ext) = families[number % families.len()];
            fs::write(folder.join(format!("{prefix}_{number:08}.{ext}")), b"").unwrap();
        }
    }
    eprintln!(
        "real100k fixture_seconds={:.6},filesystem={}",
        create.elapsed().as_secs_f64(),
        String::from_utf8(
            Command::new("stat")
                .args(["-f", "-c", "%T"])
                .arg(&fixture.root)
                .output()
                .unwrap()
                .stdout
        )
        .unwrap()
        .trim()
    );
    let build = Instant::now();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    settle(&mut engine, 0);
    let build_seconds = build.elapsed().as_secs_f64();
    let expected = oracle(&fixture.root);
    assert_eq!(expected.len(), 100000);
    let query = Instant::now();
    assert_eq!(pages(&engine.query()), expected);
    eprintln!(
        "real100k build_seconds={build_seconds:.6},full_enumeration_seconds={:.6},resources={:?}",
        query.elapsed().as_secs_f64(),
        engine.view().resources
    );

    let scans = engine.metrics().full_scans;
    let add = fixture.root.join("dir00000/new_file.txt");
    let renamed = fixture.root.join("dir00000/renamed_file.txt");
    for action in 0..3 {
        let version = engine.view().version;
        let update = Instant::now();
        match action {
            0 => fs::write(&add, b"x").unwrap(),
            1 => fs::rename(&add, &renamed).unwrap(),
            _ => fs::remove_file(&renamed).unwrap(),
        }
        settle(&mut engine, version);
        eprintln!(
            "real100k update_action={action},visibility_seconds={:.6},work={:?}",
            update.elapsed().as_secs_f64(),
            engine.metrics()
        );
        assert_eq!(pages(&engine.query()), oracle(&fixture.root));
        assert_eq!(engine.metrics().full_scans, scans);
        assert!(engine.metrics().last_touched_entries <= 2);
        assert!(engine.metrics().last_copied_entries <= 2048);
    }
    let old = engine.query().lease().unwrap();
    let version = engine.view().version;
    fs::rename(
        fixture.root.join("dir00000"),
        fixture.root.join("renamed_directory"),
    )
    .unwrap();
    settle(&mut engine, version);
    assert_eq!(pages(&engine.query()), oracle(&fixture.root));
    assert_eq!(engine.metrics().full_scans, scans);
    assert_eq!(engine.metrics().last_touched_entries, 1);
    let before = old
        .page(
            "dir00000",
            None,
            100,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap();
    assert_eq!(before.paths.len(), 50);
    assert!(before
        .paths
        .iter()
        .all(|path| path.starts_with(fixture.root.join("dir00000"))));
    assert!(!before.validated_at_start_and_finish);
    drop(old);
    let version = engine.view().version;
    fs::write(fixture.root.join("renamed_directory/followup.txt"), b"x").unwrap();
    settle(&mut engine, version);
    assert_eq!(pages(&engine.query()), oracle(&fixture.root));
    assert_eq!(engine.metrics().full_scans, scans);
    let version = engine.view().version;
    fs::remove_dir_all(fixture.root.join("dir00100")).unwrap();
    settle(&mut engine, version);
    assert_eq!(pages(&engine.query()), oracle(&fixture.root));
    assert_eq!(engine.metrics().full_scans, scans);
    assert_eq!(engine.metrics().last_touched_entries, 50);
    assert!(engine.metrics().last_copied_entries <= 3072);
    eprintln!(
        "real100k process_memory={:?}",
        fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
            .collect::<Vec<_>>()
    );
    engine.stop().unwrap();
    assert_eq!(engine.view().resources.session_watches, 0);
}
