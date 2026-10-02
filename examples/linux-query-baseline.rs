//! Opt-in public Engine query measurement against an existing real fixture.
//! Usage: cargo run --release --example linux-query-baseline -- ROOT RAW_LOG
#[cfg(target_os = "linux")]
fn main() -> std::io::Result<()> {
    use loci_experiment::engine::{Engine, EngineOptions, QueryJobState, Status};
    use loci_experiment::index::Query;
    use std::fs;
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize};
    use std::time::{Duration, Instant};
    let args: Vec<_> = std::env::args_os().collect();
    if args.len() != 3 {
        return Err(std::io::Error::other(
            "usage: linux-query-baseline ROOT RAW_LOG",
        ));
    }
    let root = fs::canonicalize(&args[1])?;
    let log_path = PathBuf::from(&args[2]);
    if log_path.starts_with(&root) {
        return Err(std::io::Error::other(
            "raw log must be outside monitored root",
        ));
    }
    let mut oracle = Vec::new();
    let mut todo = vec![root.clone()];
    while let Some(dir) = todo.pop() {
        for child in fs::read_dir(dir)? {
            let child = child?;
            if child.file_type()?.is_dir() {
                todo.push(child.path());
            }
            oracle.push(child.path());
        }
    }
    oracle.sort();
    if oracle.len() != 100_000 {
        return Err(std::io::Error::other(
            "baseline requires exactly100,000 real entries",
        ));
    }
    let build = Instant::now();
    let mut engine = Engine::open_with_options(&root, None, EngineOptions::scale())?;
    while engine.view().status != Status::Validated {
        engine.poll()?;
        if build.elapsed() > Duration::from_secs(90) {
            return Err(std::io::Error::other("build did not validate"));
        }
    }
    let build_ms = build.elapsed().as_secs_f64() * 1000.;
    let handle = engine.query();
    let cases = [
        "",
        "a",
        "b",
        "c",
        "ab",
        "in",
        "re",
        "abc",
        "log",
        "pdf",
        "invoice",
        "report",
        "source",
        "报告",
        "告",
        "dir",
        "dir00000",
        "dir01999/",
        "/dir",
        "ext:txt",
        "ext:pdf",
        "ext:rs",
        "ext:docx",
        "ext:md",
        "invoice ext:txt",
        "report ext:pdf",
        "dir00000 invoice",
        "dir01999 source",
        "dir00100/ab",
        "never_present",
        "z",
        "xyz",
        "报告 ext:docx",
        "backup",
        "image",
        "video",
        "ii",
        "ia",
        "rr",
    ];
    let mut log = std::io::BufWriter::new(fs::File::create(log_path)?);
    writeln!(log, "{{\"type\":\"metadata\",\"entries\":{},\"queries\":{},\"repetitions\":200,\"build_ms\":{build_ms:.6},\"snapshot_bytes\":{},\"session_watches\":{}}}", oracle.len(), cases.len(), engine.view().resources.snapshot_bytes, engine.view().resources.session_watches)?;
    let cancel = AtomicBool::new(false);
    let progress = AtomicUsize::new(0);
    let mut all_ns = Vec::new();
    for (case, query) in cases.iter().enumerate() {
        let verifier = Query::parse(query);
        let expected: Vec<_> = oracle
            .iter()
            .filter(|path| {
                let mut relative = vec![b'/'];
                relative
                    .extend_from_slice(path.strip_prefix(&root).unwrap().as_os_str().as_bytes());
                verifier.matches_raw(&relative)
            })
            .cloned()
            .collect();
        let lease = handle.lease()?;
        let mut cursor = None;
        let mut complete = Vec::new();
        loop {
            let page = lease.page(query, cursor.as_ref(), 1024, &cancel, &progress)?;
            complete.extend(page.paths);
            if page.complete {
                break;
            }
            cursor = page.next;
        }
        complete.sort();
        assert_eq!(complete, expected, "full oracle query={query}");
        drop(lease);
        let count = handle.start_count(query)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while matches!(
            count.state(),
            QueryJobState::Pending | QueryJobState::Running
        ) {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(count.state(), QueryJobState::Complete);
        assert_eq!(count.count(), Some(expected.len()));
        drop(count);
        writeln!(log, "{{\"type\":\"case\",\"case\":{case},\"query\":{query:?},\"exact_matches\":{},\"oracle_equal\":true,\"count_equal\":true}}", expected.len())?;
        let mut timings = Vec::new();
        for repetition in 0..200 {
            let started = Instant::now();
            let lease = handle.lease()?;
            let lease_ns = started.elapsed().as_nanos();
            let page_started = Instant::now();
            let page = lease.page(query, None, 50, &cancel, &progress)?;
            let page_ns = page_started.elapsed().as_nanos();
            let elapsed_ns = started.elapsed().as_nanos();
            assert!(page.validated_at_start_and_finish && !page.cancelled);
            assert_eq!(page.paths.len(), expected.len().min(50));
            assert!(page
                .paths
                .iter()
                .all(|path| expected.binary_search(path).is_ok()));
            writeln!(log, "{{\"type\":\"query\",\"case\":{case},\"query\":{query:?},\"repetition\":{repetition},\"lease_ns\":{lease_ns},\"page_ns\":{page_ns},\"elapsed_ns\":{elapsed_ns},\"paths\":{},\"complete\":{},\"version\":{}}}", page.paths.len(), page.complete, page.version)?;
            timings.push(elapsed_ns);
            all_ns.push(elapsed_ns);
        }
        timings.sort_unstable();
        eprintln!(
            "case={case},query={query:?},matches={},p50_ms={:.6},p95_ms={:.6},p99_ms={:.6}",
            expected.len(),
            timings[99] as f64 / 1e6,
            timings[189] as f64 / 1e6,
            timings[197] as f64 / 1e6
        );
    }
    // Optional global sorting is measured outside first-page timing loops.
    let sorted = handle.start_sort("")?;
    let deadline = Instant::now() + Duration::from_secs(90);
    while matches!(
        sorted.state(),
        QueryJobState::Pending | QueryJobState::Running
    ) {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(sorted.state(), QueryJobState::Complete);
    let mut actual = Vec::new();
    let mut offset = 0;
    loop {
        let page = sorted.page(offset, 1024)?;
        offset += page.paths.len();
        actual.extend(page.paths);
        if page.complete {
            break;
        }
    }
    assert_eq!(actual, oracle);
    drop(sorted);
    let first_job = handle.start_sort("")?;
    let second_job = handle.start_sort("")?;
    assert!(
        matches!(handle.start_sort(""), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    first_job.cancel();
    second_job.cancel();
    drop(first_job);
    drop(second_job);
    writeln!(log, "{{\"type\":\"jobs\",\"sorted_oracle_equal\":true,\"third_worker_rejected\":true,\"cancelled_workers_joined\":true}}")?;
    all_ns.sort_unstable();
    eprintln!("aggregate_queries={},build_ms={build_ms:.6},p50_ms={:.6},p95_ms={:.6},p99_ms={:.6},resources={:?}", all_ns.len(), all_ns[all_ns.len()/2-1] as f64 / 1e6, all_ns[all_ns.len()*95/100-1] as f64 / 1e6, all_ns[all_ns.len()*99/100-1] as f64 / 1e6, engine.view().resources);
    log.flush()?;
    eprintln!(
        "harness_memory={:?}",
        fs::read_to_string("/proc/self/status")?
            .lines()
            .filter(|line| line.starts_with("VmRSS:") || line.starts_with("VmHWM:"))
            .collect::<Vec<_>>()
    );
    engine.stop()
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This native baseline requires Linux.");
}
