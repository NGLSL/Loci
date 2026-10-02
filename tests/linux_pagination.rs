#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::Engine;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize};

#[test]
fn full_snapshot_pages_match_independent_directory_oracle_beyond_fifty() {
    let fixture = Fixture::new();
    for n in 0..137 {
        fs::write(fixture.root.join(format!("item-{n:03}.txt")), b"x").unwrap();
    }
    fs::write(fixture.root.join("unmatched.rs"), b"x").unwrap();
    let mut expected: Vec<_> = fs::read_dir(&fixture.root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().unwrap() == "txt")
        .collect();
    expected.sort();
    let engine = Engine::open(&fixture.root, None).unwrap();
    let lease = engine.query().lease().unwrap();
    let mut cursor = None;
    let mut paths = vec![];
    loop {
        let page = lease
            .page(
                "ext:txt",
                cursor.as_ref(),
                23,
                &AtomicBool::new(false),
                &AtomicUsize::new(0),
            )
            .unwrap();
        assert!(page.paths.len() <= 23);
        assert!(page.validated_at_start_and_finish);
        paths.extend(page.paths);
        if page.complete {
            assert!(page.next.is_none());
            break;
        }
        cursor = page.next;
        assert!(cursor.is_some());
    }
    paths.sort();
    assert_eq!(paths, expected);
    assert_eq!(
        lease
            .search(
                "ext:txt",
                true,
                &AtomicBool::new(false),
                &AtomicUsize::new(0)
            )
            .unwrap()
            .paths
            .len(),
        50
    );
}

#[test]
fn pinned_pages_stay_on_old_version_and_reject_cross_query_or_version_cursor() {
    use std::time::{Duration, Instant};
    let fixture = Fixture::new();
    for n in 0..71 {
        fs::write(fixture.root.join(format!("old-{n:03}.txt")), b"x").unwrap();
    }
    let mut expected: Vec<_> = fs::read_dir(&fixture.root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    expected.sort();
    let mut engine = Engine::open(&fixture.root, None).unwrap();
    let old = engine.query().lease().unwrap();
    let first = old
        .page("", None, 13, &AtomicBool::new(false), &AtomicUsize::new(0))
        .unwrap();
    let cursor = first.next.unwrap();
    fs::remove_file(fixture.root.join("old-040.txt")).unwrap();
    fs::write(fixture.root.join("new.txt"), b"x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while engine.view().version == first.version {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline, "new snapshot not published");
        std::thread::sleep(Duration::from_millis(20));
    }
    let new = engine.query().lease().unwrap();
    assert!(new
        .page(
            "",
            Some(&cursor),
            13,
            &AtomicBool::new(false),
            &AtomicUsize::new(0)
        )
        .is_err());
    assert!(old
        .page(
            "old",
            Some(&cursor),
            13,
            &AtomicBool::new(false),
            &AtomicUsize::new(0)
        )
        .is_err());
    let mut paths = first.paths;
    let mut next = Some(cursor);
    loop {
        let page = old
            .page(
                "",
                next.as_ref(),
                13,
                &AtomicBool::new(false),
                &AtomicUsize::new(0),
            )
            .unwrap();
        assert_eq!(page.version, first.version);
        assert!(!page.validated_at_start_and_finish);
        paths.extend(page.paths);
        if page.complete {
            break;
        }
        next = page.next;
    }
    paths.sort();
    assert_eq!(paths, expected);
}

#[test]
fn cancelled_page_resumes_and_reader_budget_is_released() {
    use loci_experiment::engine::MAX_PAGE_SIZE;
    let fixture = Fixture::new();
    fs::write(fixture.root.join("only.txt"), b"x").unwrap();
    let engine = Engine::open(&fixture.root, None).unwrap();
    let handle = engine.query();
    let lease = handle.lease().unwrap();
    let page = lease
        .page("", None, 1, &AtomicBool::new(true), &AtomicUsize::new(0))
        .unwrap();
    assert!(page.cancelled && !page.complete && page.paths.is_empty());
    let resumed = lease
        .page(
            "",
            page.next.as_ref(),
            1,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        )
        .unwrap();
    assert_eq!(resumed.paths, [fixture.root.join("only.txt")]);
    assert!(resumed.complete);
    for size in [0, MAX_PAGE_SIZE + 1] {
        assert!(lease
            .page(
                "",
                None,
                size,
                &AtomicBool::new(false),
                &AtomicUsize::new(0)
            )
            .is_err());
    }
    let readers: Vec<_> = (0..7).map(|_| handle.lease().unwrap()).collect();
    assert!(handle.lease().is_err());
    drop(readers);
    drop(lease);
    assert_eq!(handle.view().leases, 0);
    assert!(handle.lease().is_ok());
}

#[test]
fn cli_exports_all_pages_with_exact_paths() {
    use std::process::Command;
    let fixture = Fixture::new();
    for n in 0..121 {
        fs::write(fixture.root.join(format!("result-{n:03}.txt")), b"x").unwrap();
    }
    let mut expected: Vec<_> = fs::read_dir(&fixture.root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    expected.sort();
    let output = Command::new(env!("CARGO_BIN_EXE_loci-experiment"))
        .args(["engine", "query"])
        .arg(&fixture.root)
        .arg(fixture.base.join("index.loci"))
        .args(["", "--all", "--page-size", "17", "--null"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    use std::os::unix::ffi::OsStrExt;
    let mut got: Vec<_> = output
        .stdout
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .collect();
    got.sort();
    assert_eq!(
        got,
        expected
            .iter()
            .map(|p| p.as_os_str().as_bytes())
            .collect::<Vec<_>>()
    );
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(
        diagnostic.contains("page=8") && diagnostic.contains("complete=true"),
        "{diagnostic}"
    );
}
