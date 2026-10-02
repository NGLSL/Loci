#![cfg(target_os = "linux")]
mod common;
use common::Fixture;
use loci_experiment::engine::{Engine, EngineOptions, QueryJobState};
use std::fs;
use std::time::{Duration, Instant};

#[test]
fn exact_count_is_separate_from_the_immediate_first_page() {
    let fixture = Fixture::new();
    for number in 0..120 {
        fs::write(fixture.root.join(format!("report_{number:03}.txt")), b"").unwrap();
    }
    let engine = Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let handle = engine.query();
    let page = handle
        .lease()
        .unwrap()
        .page("report", None, 50, &Default::default(), &Default::default())
        .unwrap();
    assert_eq!(page.paths.len(), 50);
    assert!(!page.complete);
    let job = handle.start_count("report").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(job.state(), QueryJobState::Pending | QueryJobState::Running) {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(job.state(), QueryJobState::Complete);
    assert_eq!(job.count(), Some(120));
    assert_eq!(job.version(), page.version);
}

#[test]
fn optional_sort_has_explicit_completion_and_bounded_pages() {
    let fixture = Fixture::new();
    for name in ["z.txt", "A.txt", "报告.txt", "b.txt"] {
        fs::write(fixture.root.join(name), b"").unwrap();
    }
    let engine = Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let job = engine.query().start_sort("ext:txt").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(job.state(), QueryJobState::Pending | QueryJobState::Running) {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(job.state(), QueryJobState::Complete);
    let first = job.page(0, 2).unwrap();
    let second = job.page(2, 2).unwrap();
    assert_eq!(
        first.paths,
        [fixture.root.join("A.txt"), fixture.root.join("b.txt")]
    );
    assert_eq!(
        second.paths,
        [fixture.root.join("z.txt"), fixture.root.join("报告.txt")]
    );
    assert!(!first.complete);
    assert!(second.complete);
    assert_eq!(job.count(), Some(4));
}

fn await_job(job: &loci_experiment::engine::QueryJob) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(job.state(), QueryJobState::Pending | QueryJobState::Running) {
        assert!(Instant::now() < deadline, "{:?}", job.state());
        std::thread::yield_now();
    }
}
fn oracle_match(raw: &[u8], query: &str) -> bool {
    // Independent valid-run oracle, including lowercasing that expands Unicode.
    let mut valid_runs = Vec::new();
    let mut rest = raw;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(run) => {
                valid_runs.push(run.to_lowercase());
                break;
            }
            Err(error) => {
                valid_runs.push(
                    std::str::from_utf8(&rest[..error.valid_up_to()])
                        .unwrap()
                        .to_lowercase(),
                );
                rest = &rest[error.valid_up_to()
                    + error
                        .error_len()
                        .unwrap_or(rest.len() - error.valid_up_to())..];
            }
        }
    }
    query.split_whitespace().all(|token| {
        let token = token.to_lowercase();
        if let Some(extension) = token.strip_prefix("ext:") {
            let name = raw.rsplit(|b| *b == b'/').next().unwrap();
            name.iter().rposition(|b| *b == b'.').is_some_and(|dot| {
                std::str::from_utf8(&name[dot + 1..])
                    .is_ok_and(|ext| ext.to_lowercase() == extension)
            })
        } else {
            valid_runs.iter().any(|run| run.contains(&token))
        }
    })
}
fn native_paths(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    let mut todo = vec![root.to_owned()];
    while let Some(directory) = todo.pop() {
        for child in fs::read_dir(directory).unwrap() {
            let child = child.unwrap();
            if child.file_type().unwrap().is_dir() {
                todo.push(child.path());
            }
            paths.push(child.path());
        }
    }
    paths.sort();
    paths
}
fn complete_paths(
    handle: &loci_experiment::engine::QueryHandle,
    query: &str,
) -> Vec<std::path::PathBuf> {
    let lease = handle.lease().unwrap();
    let mut cursor = None;
    let mut paths = Vec::new();
    loop {
        let page = lease
            .page(
                query,
                cursor.as_ref(),
                7,
                &Default::default(),
                &Default::default(),
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
fn filters_preserve_raw_unicode_ancestor_slash_and_extension_semantics() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let fixture = Fixture::new();
    for parent in ["Pre", "报告", "ΟΣ", "İ", "ancestor_only", "empty"] {
        fs::create_dir(fixture.root.join(parent)).unwrap();
    }
    for (parent, name) in [
        ("Pre", "ABc.TXT"),
        ("Pre", "École.pdf"),
        ("报告", "报告_季度.docx"),
        ("ΟΣ", "ΣΟΣ.md"),
        ("İ", "İmage.txt"),
        ("ancestor_only", "plain.bin"),
    ] {
        fs::write(fixture.root.join(parent).join(name), b"").unwrap();
    }
    for name in [b"ab\xffcd.TXT".as_slice(), b"ab\xff.txt", b"bad.tx\xfft"] {
        fs::write(
            fixture
                .root
                .join("Pre")
                .join(std::ffi::OsString::from_vec(name.to_vec())),
            b"",
        )
        .unwrap();
    }
    let engine = Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let paths = native_paths(&fixture.root);
    for query in [
        "",
        "a",
        "ab",
        "abc",
        "cd",
        "abcd",
        "ab cd",
        "pre/ab",
        "/pre",
        "pre/",
        "报告",
        "报告/报告",
        "ext:txt",
        "ext:t",
        "ext:pdf",
        "ancestor_only",
        "ancestor_only/plain",
        "plain ext:bin",
        "σ",
        "ος/σος",
        "i̇",
        "i̇/i̇m",
        "不存在",
    ] {
        let expected: Vec<_> = paths
            .iter()
            .filter(|path| {
                let mut relative = vec![b'/'];
                relative.extend_from_slice(
                    path.strip_prefix(&fixture.root)
                        .unwrap()
                        .as_os_str()
                        .as_bytes(),
                );
                oracle_match(&relative, query)
            })
            .cloned()
            .collect();
        assert_eq!(complete_paths(&engine.query(), query), expected, "{query}");
        let job = engine.query().start_count(query).unwrap();
        await_job(&job);
        assert_eq!(job.state(), QueryJobState::Complete, "{query}");
        assert_eq!(job.count(), Some(expected.len()), "{query}");
    }
}
#[test]
fn every_ascii_pair_remains_lossless_across_native_names_and_ancestry() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    let directory = fixture.root.join("SHARED_PREFIX");
    fs::create_dir(&directory).unwrap();
    for first in b'A'..=b'Z' {
        for second in b'A'..=b'Z' {
            let name = format!("{}{}.TXT", char::from(first), char::from(second));
            fs::write(directory.join(name), b"").unwrap();
        }
    }
    let engine = Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let paths = native_paths(&fixture.root);
    for first in b'a'..=b'z' {
        for second in b'a'..=b'z' {
            let query = format!("{}{}", char::from(first), char::from(second));
            let expected: Vec<_> = paths
                .iter()
                .filter(|path| {
                    oracle_match(
                        path.strip_prefix(&fixture.root)
                            .unwrap()
                            .as_os_str()
                            .as_bytes(),
                        &query,
                    )
                })
                .cloned()
                .collect();
            assert_eq!(complete_paths(&engine.query(), &query), expected, "{query}");
        }
    }
}
#[test]
fn directory_move_keeps_old_filters_and_invalidates_new_ancestry() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join("old/sub")).unwrap();
    fs::write(fixture.root.join("old/sub/REPORT.txt"), b"").unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let old = engine.query().lease().unwrap();
    let version = engine.view().version;
    fs::rename(fixture.root.join("old"), fixture.root.join("new")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.view().version <= version {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    let old_page = old
        .page(
            "old/sub/report",
            None,
            50,
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(old_page.paths, [fixture.root.join("old/sub/REPORT.txt")]);
    assert!(!old_page.validated_at_start_and_finish);
    assert!(complete_paths(&engine.query(), "old").is_empty());
    assert_eq!(
        complete_paths(&engine.query(), "new/sub/report"),
        [fixture.root.join("new/sub/REPORT.txt")]
    );
}
#[test]
fn sort_cancellation_and_budget_failure_do_not_publish_partial_count() {
    let fixture = Fixture::new();
    for number in 0..4000 {
        fs::write(fixture.root.join(format!("report_{number:05}.txt")), b"").unwrap();
    }
    let mut options = EngineOptions::scale();
    options.scale_budgets.max_sort_bytes = 64;
    let engine = Engine::open_with_options(&fixture.root, None, options).unwrap();
    let job = engine.query().start_sort("report").unwrap();
    await_job(&job);
    assert!(matches!(job.state(), QueryJobState::Failed(_)));
    assert_eq!(job.count(), None);
    assert!(job.page(0, 50).is_err());
    drop(job);
    drop(engine);
    let engine = Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let job = engine.query().start_sort("report").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while job.progress() == 0
        && matches!(job.state(), QueryJobState::Pending | QueryJobState::Running)
    {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(job.state(), QueryJobState::Running);
    job.cancel();
    await_job(&job);
    assert_eq!(job.state(), QueryJobState::Cancelled);
    assert_eq!(job.count(), None);
    let page = engine
        .query()
        .lease()
        .unwrap()
        .page("report", None, 50, &Default::default(), &Default::default())
        .unwrap();
    assert_eq!(page.paths.len(), 50);
}

#[test]
fn completed_sort_pins_its_cut_and_release_unblocks_publication() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("seed.txt"), b"").unwrap();
    let mut engine =
        Engine::open_with_options(&fixture.root, None, EngineOptions::scale()).unwrap();
    let job = engine.query().start_sort("ext:txt").unwrap();
    await_job(&job);
    let version = job.version();
    fs::write(fixture.root.join("second.txt"), b"").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.view().version == version {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    let old_page = job.page(0, 50).unwrap();
    assert_eq!(old_page.paths, [fixture.root.join("seed.txt")]);
    assert!(!old_page.validated_at_start_and_finish);
    fs::write(fixture.root.join("third.txt"), b"").unwrap();
    while engine.view().status != loci_experiment::engine::Status::ReadersPinned {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    drop(job);
    while engine.view().version <= version + 1 {
        engine.poll().unwrap();
        assert!(Instant::now() < deadline);
    }
    assert_eq!(complete_paths(&engine.query(), "ext:txt").len(), 3);
    let lease = engine.query().lease().unwrap();
    engine.stop().unwrap();
    assert!(engine.query().start_count("").is_err());
    let page = lease
        .page("", None, 50, &Default::default(), &Default::default())
        .unwrap();
    assert_eq!(page.paths.len(), 3);
    assert_eq!(
        page.finished.status,
        loci_experiment::engine::Status::Stopped
    );
}
