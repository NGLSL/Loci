#![cfg(target_os = "linux")]
mod common;
#[path = "common/fixture_capacity.rs"]
mod fixture_capacity;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, QueryHandle, Status};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Mutex;
use std::time::{Duration, Instant};
static NATIVE: Mutex<()> = Mutex::new(());
fn settle(engine: &mut Engine) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while engine.view().status != Status::Validated {
        engine.poll().unwrap();
        if engine.view().status == Status::Validated {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", engine.view());
    }
}
fn all(handle: &QueryHandle) -> Vec<PathBuf> {
    let lease = handle.lease().unwrap();
    let mut cursor = None;
    let mut paths = Vec::new();
    loop {
        let page = lease
            .page(
                "",
                cursor.as_ref(),
                256,
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
#[test]
fn scale_checkpoint_reopens_all_real_raw_names_links_and_more_than_legacy_capacity() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::symlink;
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    let database = fixture.base.join("scale.loci");
    let mut expected = Vec::new();
    for n in 0..5000 {
        let path = fixture.root.join(format!("real-{n:04}.txt"));
        fs::write(&path, "x").unwrap();
        expected.push(path);
    }
    let raw = fixture
        .root
        .join(OsString::from_vec(b"raw-\xff.txt".to_vec()));
    fs::write(&raw, "x").unwrap();
    expected.push(raw);
    let hardlink = fixture.root.join("hardlink.txt");
    fs::hard_link(&expected[0], &hardlink).unwrap();
    expected.push(hardlink);
    let link = fixture.root.join("link");
    symlink("real-0000.txt", &link).unwrap();
    expected.push(link);
    expected.sort();
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    settle(&mut engine);
    assert_eq!(all(&engine.query()), expected);
    engine.save().unwrap();
    engine.stop().unwrap();
    let mut reopened =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    assert_eq!(all(&reopened.query()), expected);
    settle(&mut reopened);
    assert_eq!(all(&reopened.query()), expected);
    assert_eq!(
        reopened
            .query()
            .lease()
            .unwrap()
            .entry_kind(&fixture.root.join("link"))
            .unwrap(),
        loci_experiment::engine::EntryKind::Symlink
    );
    reopened.stop().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "query"])
        .arg(&fixture.root)
        .arg(&database)
        .arg("")
        .args(["--scale", "--all", "--null"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut expected_bytes = Vec::new();
    for path in &expected {
        use std::os::unix::ffi::OsStrExt;
        expected_bytes.extend_from_slice(path.as_os_str().as_bytes());
        expected_bytes.push(0);
    }
    // Public default scale order is immutable entry ID, so compare independent byte sets.
    let mut actual: Vec<_> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    let mut oracle: Vec<_> = expected_bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    actual.sort();
    oracle.sort();
    assert_eq!(actual, oracle);
}

#[test]
fn competing_scale_writer_is_rejected_and_stop_drop_release_its_lock() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed.txt"), "x").unwrap();
    let database = fixture.base.join("scale.loci");
    let mut first =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    first.save().unwrap();
    let old = fs::read(&database).unwrap();
    let error = Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale())
        .err()
        .unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "build"])
        .arg(&fixture.root)
        .arg(&database)
        .arg("--scale")
        .output()
        .unwrap();
    assert!(!child.status.success());
    assert_eq!(fs::read(&database).unwrap(), old);
    first.stop().unwrap();
    let second =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    drop(second);
    assert!(
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).is_ok()
    );
}
fn reseal(bytes: &mut [u8]) {
    let end = bytes.len() - 8;
    let sum = bytes[..end]
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    bytes[end..].copy_from_slice(&sum.to_le_bytes());
}
fn records_start(bytes: &[u8]) -> usize {
    let root_length = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) as usize;
    let scope_length_at = 24 + root_length + 5 * 8;
    let scope_length = u32::from_le_bytes(
        bytes[scope_length_at..scope_length_at + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    scope_length_at + 4 + scope_length + 8
}
#[test]
fn damaged_and_hostile_scale_checkpoints_fail_bounded_without_overwriting_the_file() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    fs::create_dir(fixture.root.join("dir")).unwrap();
    fs::write(fixture.root.join("dir/file.txt"), "x").unwrap();
    let database = fixture.base.join("scale.loci");
    {
        let mut engine =
            Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale())
                .unwrap();
        engine.save().unwrap();
    }
    let valid = fs::read(&database).unwrap();
    let mut cases = vec![valid[..12].to_vec()];
    let mut checksum = valid.clone();
    let last = checksum.len() - 1;
    checksum[last] ^= 1;
    cases.push(checksum);
    let mut version = valid.clone();
    version[8..12].copy_from_slice(&99u32.to_le_bytes());
    reseal(&mut version);
    cases.push(version);
    let mut length = valid.clone();
    length[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    reseal(&mut length);
    cases.push(length);
    let records = records_start(&valid);
    let mut cycle = valid.clone();
    cycle[records + 26..records + 30].copy_from_slice(&1u32.to_le_bytes());
    reseal(&mut cycle);
    cases.push(cycle);
    let mut parent = valid.clone();
    parent[records + 26..records + 30].copy_from_slice(&u32::MAX.to_le_bytes());
    reseal(&mut parent);
    cases.push(parent);
    for bytes in cases {
        fs::write(&database, &bytes).unwrap();
        let started = Instant::now();
        let error =
            Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale())
                .err()
                .expect("hostile store must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(fs::read(&database).unwrap(), bytes);
    }
    fs::write(&database, &valid).unwrap();
    let mut different_scope = EngineOptions::scale();
    different_scope.exclusions.push(PathBuf::from("dir"));
    let error = Engine::open_with_options(&fixture.root, Some(&database), different_scope)
        .err()
        .unwrap();
    assert!(error.to_string().contains("scope mismatch"));
    assert_eq!(fs::read(&database).unwrap(), valid);
}

#[test]
fn parent_redirection_or_in_root_relocation_preserves_old_scale_checkpoint() {
    use std::os::unix::fs::symlink;
    let _guard = NATIVE.lock().unwrap();
    for same_directory in [false, true] {
        let fixture = Fixture::new();
        let parent = fixture.base.join("state");
        fs::create_dir(&parent).unwrap();
        let database = parent.join("scale.loci");
        fs::write(fixture.root.join("seed.txt"), "x").unwrap();
        let mut engine =
            Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale())
                .unwrap();
        engine.save().unwrap();
        let old = fs::read(&database).unwrap();
        let moved = if same_directory {
            fixture.root.join("state")
        } else {
            fixture.base.join("old-state")
        };
        fs::rename(&parent, &moved).unwrap();
        if !same_directory {
            fs::create_dir(fixture.root.join("redirect")).unwrap();
            symlink(fixture.root.join("redirect"), &parent).unwrap();
        }
        fs::write(fixture.root.join("added.txt"), "x").unwrap();
        assert!(engine.save().is_err());
        assert_eq!(fs::read(moved.join("scale.loci")).unwrap(), old);
        if !same_directory {
            assert!(fs::read_dir(fixture.root.join("redirect"))
                .unwrap()
                .next()
                .is_none());
        }
    }
}

#[test]
fn killed_monitor_releases_writer_lock_and_checkpoint_reopens_in_a_fresh_process() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    let database = fixture.base.join("scale.loci");
    fs::write(fixture.root.join("seed.txt"), "x").unwrap();
    {
        let mut engine =
            Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale())
                .unwrap();
        engine.save().unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "watch"])
        .arg(&fixture.root)
        .arg(&database)
        .arg("--scale")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    loop {
        let line = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        if line.contains("watch-ready") {
            break;
        }
    }
    fs::write(fixture.root.join("offline.txt"), "x").unwrap();
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    reader.join().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "query"])
        .arg(&fixture.root)
        .arg(&database)
        .arg("")
        .args(["--scale", "--all", "--null"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    use std::os::unix::ffi::OsStrExt;
    let mut rows: Vec<_> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|row| !row.is_empty())
        .collect();
    rows.sort();
    assert_eq!(
        rows,
        [
            fixture.root.join("offline.txt").as_os_str().as_bytes(),
            fixture.root.join("seed.txt").as_os_str().as_bytes()
        ]
    );
}

#[test]
fn a_legacy_owner_cannot_downgrade_a_scale_database_created_after_it_opened() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    let database = fixture.base.join("scale.loci");
    fs::write(fixture.root.join("seed.txt"), "x").unwrap();
    let mut legacy = Engine::open(&fixture.root, Some(&database)).unwrap();
    {
        let mut scale =
            Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale())
                .unwrap();
        scale.save().unwrap();
        assert_eq!(
            legacy.save().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
    let valid = fs::read(&database).unwrap();
    assert_eq!(
        legacy.save().unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read(&database).unwrap(), valid);
}

#[test]
fn failed_atomic_save_preserves_old_checkpoint_and_cleans_owned_temporaries() {
    use std::os::unix::fs::PermissionsExt;
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    let parent = fixture.base.join("state");
    fs::create_dir(&parent).unwrap();
    let database = parent.join("scale.loci");
    fs::write(fixture.root.join("seed.txt"), "x").unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    engine.save().unwrap();
    let old = fs::read(&database).unwrap();
    fs::write(fixture.root.join("new.txt"), "x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while all(&engine.query()).len() != 2 || engine.view().status != Status::Validated {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    struct Restore(PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let restore = Restore(parent.clone());
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(
        engine.save().unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(fs::read(&database).unwrap(), old);
    drop(restore);
    let names: Vec<_> = fs::read_dir(&parent)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        names.len(),
        2,
        "only checkpoint and stable writer lock remain: {:?}",
        names
    );
    engine.save().unwrap();
    assert_ne!(fs::read(&database).unwrap(), old);
}

#[test]
#[ignore = "opt-in: creates 100,000 real entries across 2,000 owned directories; check disk/inode budget"]
fn one_hundred_thousand_entry_checkpoint_saves_reopens_and_exports_full_set() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    if !fixture_capacity::hundred_thousand_fixture_preflight(&fixture.base) {
        return;
    }
    let database = fixture.base.join("scale.loci");
    let mut expected = Vec::with_capacity(100_000);
    for directory in 0..2000 {
        let parent = fixture.root.join(format!("dir-{directory:04}"));
        fs::create_dir(&parent).unwrap();
        expected.push(parent.clone());
        for n in 0..49 {
            let path = parent.join(format!("real-{n:04}.txt"));
            fs::write(&path, "").unwrap();
            expected.push(path);
        }
    }
    expected.sort();
    let mut options = EngineOptions::scale();
    options.scan_batch = 4096;
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), options.clone()).unwrap();
    settle(&mut engine);
    assert_eq!(all(&engine.query()), expected);
    engine.save().unwrap();
    engine.stop().unwrap();
    let mut reopened = Engine::open_with_options(&fixture.root, Some(&database), options).unwrap();
    assert_eq!(all(&reopened.query()), expected);
    settle(&mut reopened);
    assert_eq!(all(&reopened.query()), expected);
    reopened.stop().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "query"])
        .arg(&fixture.root)
        .arg(&database)
        .arg("")
        .args(["--scale", "--all", "--null", "--scan-batch", "4096"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    use std::os::unix::ffi::OsStrExt;
    let mut paths: Vec<_> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .collect();
    paths.sort();
    let expected: Vec<_> = expected
        .iter()
        .map(|path| path.as_os_str().as_bytes())
        .collect();
    assert_eq!(paths, expected);
}

#[test]
fn deleted_slots_and_renamed_directory_parents_survive_checkpoint_and_later_updates() {
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    let database = fixture.base.join("scale.loci");
    fs::create_dir(fixture.root.join("a")).unwrap();
    fs::create_dir(fixture.root.join("b")).unwrap();
    fs::write(fixture.root.join("a/kept.txt"), "x").unwrap();
    fs::write(fixture.root.join("a/deleted.txt"), "x").unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    fs::remove_file(fixture.root.join("a/deleted.txt")).unwrap();
    fs::rename(fixture.root.join("a"), fixture.root.join("b/renamed")).unwrap();
    let mut expected = vec![
        fixture.root.join("b"),
        fixture.root.join("b/renamed"),
        fixture.root.join("b/renamed/kept.txt"),
    ];
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        engine.poll().unwrap();
        if engine.view().status == Status::Validated && all(&engine.query()) == expected {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    engine.save().unwrap();
    engine.stop().unwrap();
    let mut reopened =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    assert_eq!(all(&reopened.query()), expected);
    settle(&mut reopened);
    fs::write(fixture.root.join("b/renamed/later.txt"), "x").unwrap();
    expected.push(fixture.root.join("b/renamed/later.txt"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        reopened.poll().unwrap();
        if reopened.view().status == Status::Validated && all(&reopened.query()) == expected {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    reopened.save().unwrap();
}

#[test]
fn simulated_external_source_uses_the_same_scale_checkpoint_format_and_lock() {
    use loci_experiment::events::{EventBatch, EventSource};
    struct Quiet;
    impl EventSource for Quiet {
        fn poll(&mut self) -> std::io::Result<EventBatch> {
            Ok(EventBatch::default())
        }
        fn stop(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let _guard = NATIVE.lock().unwrap();
    let fixture = Fixture::new();
    let database = fixture.base.join("scale.loci");
    fs::write(fixture.root.join("seed.txt"), "x").unwrap();
    let mut external = Engine::with_source_and_options(
        &fixture.root,
        Some(&database),
        Quiet,
        EngineOptions::scale(),
    )
    .unwrap();
    external.save().unwrap();
    external.stop().unwrap();
    let reopened =
        Engine::open_with_options(&fixture.root, Some(&database), EngineOptions::scale()).unwrap();
    assert_eq!(all(&reopened.query()), [fixture.root.join("seed.txt")]);
}
