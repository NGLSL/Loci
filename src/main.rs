//! Disposable filename/path index experiment. No Kite or third-party code.
#[cfg(test)]
use loci_experiment::index::normalize;
use loci_experiment::index::{Index, Query};
#[cfg(test)]
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;
const CASES: &[&str] = &[
    "invoice_00001234",
    "ab",
    "q",
    "报告",
    "报",
    "项目",
    "src/module_042",
    "ext:rs",
    "report",
    "no_such_filename",
    "ext:pdf report",
    "doc report",
];

// Deterministic corpus: 4096 parents, mixed UTF-8 names, sequential unique IDs.
// Deliberately groups some common names: block selectivity is a measured tradeoff.
fn synthetic(i: usize) -> String {
    let parent = format!(
        "/home/test/项目/workspace_{:03}/src/module_{:03}",
        (i / 4096) % 64,
        (i / 64) % 64
    );
    let name = match i % 10 {
        0 => format!("report_{i:08}.pdf"),
        1 => format!("报告_{i:08}.docx"),
        2 => format!("image_{i:08}.png"),
        3 => format!("source_{i:08}.rs"),
        4 => format!("invoice_{i:08}.txt"),
        5 => format!("ab_notes_{i:08}.md"),
        6 => format!("backup_{i:08}.zip"),
        7 => format!("日志_{i:08}.log"),
        8 => format!("video_{i:08}.mp4"),
        _ => format!("readme_{i:08}.md"),
    };
    format!("{parent}/{name}")
}

#[derive(Default)]
struct Memory {
    working: u64,
    private: u64,
    peak_working: u64,
    peak_private: u64,
}
#[cfg(windows)]
fn memory() -> Memory {
    #[repr(C)]
    struct Counters {
        cb: u32,
        faults: u32,
        peak_ws: usize,
        ws: usize,
        peak_paged: usize,
        paged: usize,
        peak_nonpaged: usize,
        nonpaged: usize,
        pagefile: usize,
        peak_pagefile: usize,
        private: usize,
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
    }
    let mut c: Counters = unsafe { std::mem::zeroed() };
    c.cb = std::mem::size_of::<Counters>() as u32;
    if unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut c,
            std::mem::size_of::<Counters>() as u32,
        )
    } == 0
    {
        panic!("memory counters unavailable");
    }
    Memory {
        working: c.ws as u64,
        private: c.private as u64,
        peak_working: c.peak_ws as u64,
        peak_private: c.peak_pagefile as u64,
    }
}
#[cfg(not(windows))]
fn memory() -> Memory {
    // Portable build fallback; VmRSS/VmHWM only, private commit is unavailable.
    let s = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let read = |key| {
        s.lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    Memory {
        working: read("VmRSS:"),
        peak_working: read("VmHWM:"),
        ..Memory::default()
    }
}
fn print_memory(tag: &str) {
    let m = memory();
    println!(
        "memory,{tag},{},{},{},{}",
        m.working, m.private, m.peak_working, m.peak_private
    );
}
fn cancel_bench(index: &Index) {
    for _ in 0..15 {
        let cancel = AtomicBool::new(false);
        let progress = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                index.search(&Query::parse("项目"), true, usize::MAX, &cancel, &progress)
            });
            while progress.load(Ordering::Acquire) < 1024 && !worker.is_finished() {
                std::thread::yield_now();
            }
            let issued = Instant::now();
            cancel.store(true, Ordering::Release);
            let out = worker.join().unwrap();
            println!(
                "cancel,{:.3},{},{},{}",
                issued.elapsed().as_secs_f64() * 1e6,
                out.checked,
                out.cancelled,
                progress.load(Ordering::Acquire)
            );
            assert!(out.cancelled && out.checked < index.records.len());
        });
    }
}
fn benchmark(index: &Index) {
    let cancel = AtomicBool::new(false);
    let progress = AtomicUsize::new(0);
    println!("kind,records,query,algorithm,mode,iteration,microseconds,matches,checked,checksum,verified");
    for raw in CASES {
        let q = Query::parse(raw);
        let oracle = index.search(&q, false, usize::MAX, &cancel, &progress);
        let out = index.search(&q, true, usize::MAX, &cancel, &progress);
        assert_eq!(
            (out.matches, out.checksum, &out.ids),
            (oracle.matches, oracle.checksum, &oracle.ids),
            "{raw}"
        );
        for indexed in [false, true] {
            for mode in ["first50", "complete"] {
                let limit = if mode == "first50" { 50 } else { usize::MAX };
                for iteration in 0..31 {
                    let start = Instant::now();
                    let out =
                        std::hint::black_box(index.search(&q, indexed, limit, &cancel, &progress));
                    println!(
                        "hot,{},{raw},{},{mode},{iteration},{:.3},{},{},{},{}",
                        index.records.len(),
                        if indexed { "block_trigram" } else { "scan" },
                        start.elapsed().as_secs_f64() * 1e6,
                        out.matches,
                        out.checked,
                        out.checksum,
                        out.verified
                    );
                }
            }
        }
    }
    print_memory("after_hot");
    cancel_bench(index);
}

// Bounded real filesystem verification. Never follows symlinks/junctions; never
// scans outside the explicitly supplied root. Errors are counted, not fatal.
fn walk(root: &Path, base: &Path, out: &mut Vec<String>, errors: &mut usize) {
    if out.len() >= 20_000 {
        return;
    }
    let entries = match fs::read_dir(root) {
        Ok(x) => x,
        Err(_) => {
            *errors += 1;
            return;
        }
    };
    for entry in entries {
        if out.len() >= 20_000 {
            break;
        }
        let entry = match entry {
            Ok(x) => x,
            Err(_) => {
                *errors += 1;
                continue;
            }
        };
        let p = entry.path();
        let meta = match fs::symlink_metadata(&p) {
            Ok(m) => m,
            Err(_) => {
                *errors += 1;
                continue;
            }
        };
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                continue;
            }
        } // reparse point
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() {
            if [
                "target",
                ".git",
                ".agents",
                ".codex",
                ".scratch",
                "artifacts",
                "node_modules",
                "resources",
                "work",
            ]
            .contains(&entry.file_name().to_string_lossy().as_ref())
            {
                continue;
            }
            walk(&p, base, out, errors);
        } else if meta.is_file() {
            if let Some(relative) = p.strip_prefix(base).ok().and_then(Path::to_str) {
                out.push(relative.replace('\\', "/"));
            } else {
                *errors += 1;
            }
        }
    }
}
// Delta state-model experiment: this is NOT a kernel watcher or WAL.
#[cfg(test)]
#[derive(Default)]
struct Overlay {
    generation: u64,
    dirty: bool,
    changes: BTreeMap<String, Option<String>>,
}
#[cfg(test)]
impl Overlay {
    fn set(&mut self, id: &str, path: Option<&str>) {
        self.changes.insert(id.to_owned(), path.map(str::to_owned));
        self.generation += 1;
    }
    fn merged(&self, base: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        let mut merged = base.clone();
        for (id, path) in &self.changes {
            if let Some(path) = path {
                merged.insert(id.clone(), path.clone());
            } else {
                merged.remove(id);
            }
        }
        merged
    }
    fn reconcile(
        &mut self,
        scanned: BTreeMap<String, String>,
        base: &mut BTreeMap<String, String>,
        expected: u64,
    ) -> bool {
        if self.generation != expected {
            return false;
        }
        *base = scanned;
        self.changes.clear();
        self.dirty = false;
        self.generation += 1;
        true
    }
}
/// Minimal bounded validation entry point; not a standalone CLI product.
#[cfg(target_os = "linux")]
fn live_check(args: &[String]) -> io::Result<()> {
    use loci_experiment::watch::Limits;
    use std::time::Duration;
    if args.len() != 4 {
        return Err(io::Error::other(
            "usage: live-check explicit-root query duration-ms incremental|rescan",
        ));
    }
    let milliseconds: u64 = args[2].parse().map_err(io::Error::other)?;
    if !(1..=10000).contains(&milliseconds) || args[1].len() > 512 {
        return Err(io::Error::other(
            "live-check duration/query budget exhausted",
        ));
    }
    let root = Path::new(&args[0]);
    let deadline = Instant::now() + Duration::from_millis(milliseconds);
    match args[3].as_str() {
        "incremental" => {
            let mut engine = loci_experiment::incremental::Native::new(root, Limits::default())?;
            loop {
                let ok = engine.tick();
                let handle = engine.store.handle();
                if let Ok(lease) = handle.lease() {
                    let result = lease.search(
                        &args[1],
                        false,
                        &AtomicBool::new(false),
                        &AtomicUsize::new(0),
                    )?;
                    println!("live_check,incremental,version={},generation={},validated={},matches={},full_scans={},subtree_scans={},rebuilt_records={}",
                        result.version,result.snapshot_generation,result.validated_at_start_and_finish,result.matches,
                        engine.metrics.full_scans,engine.metrics.subtree_scans,engine.store.rebuilt_records);
                } else {
                    println!("live_check,incremental,no_snapshot");
                }
                if let Err(error) = ok {
                    eprintln!("live_check,incremental,failed={error}");
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        "rescan" => {
            let mut engine = loci_experiment::live::Native::new(root, Limits::default())?;
            loop {
                let ok = engine.tick();
                let handle = engine.store.handle();
                if let Ok(lease) = handle.lease() {
                    let result = lease.search(
                        &args[1],
                        false,
                        &AtomicBool::new(false),
                        &AtomicUsize::new(0),
                    )?;
                    println!("live_check,rescan,version={},generation={},validated={},matches={},full_scans={},rebuilt_records={}",
                        result.version,result.snapshot_generation,result.validated_at_start_and_finish,result.matches,
                        engine.watch.full_scans,engine.store.rebuilt_records);
                } else {
                    println!("live_check,rescan,no_snapshot");
                }
                if let Err(error) = ok {
                    eprintln!("live_check,rescan,failed={error}");
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        _ => {
            return Err(io::Error::other(
                "live-check mode must be incremental or rescan",
            ))
        }
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn live_check(_args: &[String]) -> io::Result<()> {
    Err(io::Error::other("native live-check requires x86_64 Linux"))
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("live-check") => live_check(&args[2..])?,
        Some("build") => {
            let n: usize = args[2].parse().unwrap();
            assert!((1..=1_000_000).contains(&n));
            let start = Instant::now();
            let index = Index::from_paths((0..n).map(synthetic));
            let build_us = start.elapsed().as_secs_f64() * 1e6;
            print_memory("after_build");
            let start = Instant::now();
            index.save(Path::new(&args[3]))?;
            println!(
                "build,{n},{build_us:.3},{:.3},{},{},{},{}",
                start.elapsed().as_secs_f64() * 1e6,
                fs::metadata(&args[3])?.len(),
                index.parents.len(),
                index.dictionary.len(),
                index.postings.len()
            );
            print_memory("after_save");
        }
        Some("bench") => {
            let start = Instant::now();
            let index = Index::load(Path::new(&args[2]))?;
            println!("load,{:.3}", start.elapsed().as_secs_f64() * 1e6);
            print_memory("loaded");
            benchmark(&index);
        }
        Some("query") => {
            let start = Instant::now();
            let index = Index::load(Path::new(&args[2]))?;
            let load = start.elapsed().as_secs_f64() * 1e6;
            let q = Query::parse(&args[3]);
            let start = Instant::now();
            let limit = if args.get(4).is_some_and(|s| s == "complete") {
                usize::MAX
            } else {
                50
            };
            let out = index.search(
                &q,
                true,
                limit,
                &AtomicBool::new(false),
                &AtomicUsize::new(0),
            );
            println!(
                "cold,{},{},{load:.3},{:.3},{},{},{}",
                index.records.len(),
                args[3],
                start.elapsed().as_secs_f64() * 1e6,
                out.matches,
                out.checked,
                out.checksum
            );
            println!("prefilter,{},{}", out.checked, out.verified);
            print_memory("cold_after_query");
        }
        Some("scan") => {
            let root = fs::canonicalize(&args[2])?;
            let start = Instant::now();
            let mut paths = vec![];
            let mut errors = 0;
            walk(&root, &root, &mut paths, &mut errors);
            paths.sort();
            let walk_us = start.elapsed().as_secs_f64() * 1e6;
            let n = paths.len();
            let start = Instant::now();
            let index = Index::from_paths(paths.into_iter());
            println!(
                "scan,{n},{errors},{walk_us:.3},{:.3},cap=20000",
                start.elapsed().as_secs_f64() * 1e6
            );
            for raw in ["cargo", "报告", "src/ui", "ext:rs"] {
                let q = Query::parse(raw);
                let c = AtomicBool::new(false);
                let p = AtomicUsize::new(0);
                let oracle = index.search(&q, false, usize::MAX, &c, &p);
                let out = index.search(&q, true, usize::MAX, &c, &p);
                assert_eq!(
                    (out.matches, out.checksum, &out.ids),
                    (oracle.matches, oracle.checksum, &oracle.ids)
                );
                println!("real_query,{raw},{},{}", out.matches, out.checked);
            }
            print_memory("real_scan");
        }
        _ => {
            panic!("usage: build N db | bench db | query db query [complete] | scan explicit-root")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn search_paths(index: &Index, q: &str, indexed: bool) -> Vec<String> {
        index
            .search(
                &Query::parse(q),
                indexed,
                usize::MAX,
                &AtomicBool::new(false),
                &AtomicUsize::new(0),
            )
            .ids
            .iter()
            .map(|i| index.path(*i as usize))
            .collect()
    }
    #[test]
    fn chinese_short_path_extension_and_case_match_oracle() {
        let index = Index::from_paths(
            [
                "/资料/报告.PDF",
                "/src/main.rs",
                "/资料/日报.txt",
                "/src/report.pdf",
                "/src/letter.q",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        for q in [
            "报",
            "报告",
            "ext:pdf",
            "资料",
            "src/main",
            "REPORT",
            "q",
            "ext:rs",
            "资料 ext:pdf",
            "absent",
            "资料 报",
        ] {
            assert_eq!(
                search_paths(&index, q, true),
                search_paths(&index, q, false),
                "{q}"
            );
        }
        assert_eq!(
            search_paths(&index, "资料 ext:pdf", true),
            vec!["/资料/报告.PDF"]
        );
    }
    #[test]
    fn block_false_positives_and_boundary_trigrams_are_verified() {
        let index = Index::from_paths(
            ["/ab/cdef.txt", "/ab/zzz.rs", "/aa/bcd.txt"]
                .into_iter()
                .map(str::to_owned),
        );
        assert_eq!(search_paths(&index, "b/cd", true), vec!["/ab/cdef.txt"]);
        assert!(search_paths(&index, "cde ext:rs", true).is_empty());
    }
    #[test]
    fn generated_queries_match_complete_scan_across_blocks() {
        let index = Index::from_paths((0..2048).map(synthetic));
        let cancel = AtomicBool::new(false);
        let progress = AtomicUsize::new(0);
        for raw in CASES
            .iter()
            .map(|s| s.to_string())
            .chain((0..200).map(|i| format!("{:04}", (i * 193) % 2048)))
        {
            let q = Query::parse(&raw);
            let a = index.search(&q, true, usize::MAX, &cancel, &progress);
            let b = index.search(&q, false, usize::MAX, &cancel, &progress);
            assert_eq!(
                (a.matches, a.checksum, a.ids),
                (b.matches, b.checksum, b.ids),
                "{raw}"
            );
        }
    }
    #[test]
    fn pre_cancel_and_first_page_have_defined_semantics() {
        let index = Index::from_paths((0..2048).map(synthetic));
        let p = AtomicUsize::new(0);
        let out = index.search(
            &Query::parse("ab"),
            true,
            usize::MAX,
            &AtomicBool::new(true),
            &p,
        );
        assert!(out.cancelled);
        assert_eq!(out.checked, 0);
        let out = index.search(&Query::parse("项目"), true, 50, &AtomicBool::new(false), &p);
        assert_eq!(out.ids.len(), 50);
        assert_eq!(out.matches, 50); // partial count, not total
    }
    #[test]
    fn persistence_round_trip() {
        let p = std::env::temp_dir().join(format!("loci-roundtrip-{}.idx", std::process::id()));
        let a = Index::from_paths((0..120).map(synthetic));
        a.save(&p).unwrap();
        let b = Index::load(&p).unwrap();
        for q in CASES {
            assert_eq!(search_paths(&a, q, true), search_paths(&b, q, true));
        }
        fs::remove_file(p).unwrap();
    }
    #[test]
    fn add_delete_rename_and_loss_reconcile_with_generation_guard() {
        let mut base = BTreeMap::from([
            ("1".to_owned(), "/old/a".to_owned()),
            ("2".to_owned(), "/old/b".to_owned()),
        ]);
        let mut overlay = Overlay::default();
        overlay.set("3", Some("/new/c"));
        overlay.set("2", None);
        overlay.set("1", Some("/renamed/a"));
        let merged = overlay.merged(&base);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged["1"], "/renamed/a");
        overlay.dirty = true;
        let old_generation = overlay.generation;
        let mut actual = merged;
        actual.insert("4".to_owned(), "/lost/d".to_owned()); // dropped event
        overlay.set("5", Some("/racing/e"));
        assert!(!overlay.reconcile(actual.clone(), &mut base, old_generation));
        assert!(overlay.dirty);
        actual.insert("5".to_owned(), "/racing/e".to_owned());
        let current = overlay.generation;
        assert!(overlay.reconcile(actual, &mut base, current));
        assert_eq!(base.len(), 4);
        assert!(!overlay.dirty);
        assert_eq!(base["4"], "/lost/d");
    }
}

#[cfg(test)]
mod prefilter_tests {
    use super::*;
    fn compare(index: &Index, raw: &str) {
        let q = Query::parse(raw);
        let cancel = AtomicBool::new(false);
        let progress = AtomicUsize::new(0);
        let a = index.search(&q, true, usize::MAX, &cancel, &progress);
        let b = index.search(&q, false, usize::MAX, &cancel, &progress);
        assert_eq!(
            (a.matches, a.checksum, a.ids),
            (b.matches, b.checksum, b.ids),
            "{raw}"
        );
    }
    #[test]
    fn every_normalized_substring_has_no_false_negative() {
        let paths = [
            "/資料/İSTANBUL/报告.PDF",
            "/ab/ABc.txt",
            "/ß/Σ/Kelvin.md",
            "/doc/report.rs",
            "/报告/a b.rs",
            "/配合/é.txt",
        ];
        let index = Index::from_paths(paths.into_iter().map(str::to_owned));
        for path in paths {
            let path = normalize(path);
            let mut boundaries: Vec<_> = path.char_indices().map(|(i, _)| i).collect();
            boundaries.push(path.len());
            for (pos, &start) in boundaries.iter().enumerate() {
                for &end in boundaries.iter().skip(pos + 1).take(8) {
                    compare(&index, &path[start..end]);
                }
            }
        }
    }
    #[test]
    fn short_absence_skips_blocks_and_two_byte_terms_remain_exact() {
        let index = Index::from_paths((0..2048).map(synthetic));
        let out = index.search(
            &Query::parse("q"),
            true,
            usize::MAX,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        );
        assert_eq!((out.matches, out.checked, out.verified), (0, 0, 0));
        for q in [
            "ab",
            "ba",
            "zz",
            "qq",
            "a q",
            "报 ab",
            "ext:rs ab",
            "01",
            "00",
        ] {
            compare(&index, q);
        }
    }
    #[test]
    fn all_bits_collision_still_requires_exact_verification() {
        let mut index = Index::from_paths(
            ["/x/report.pdf", "/x/file.docx"]
                .into_iter()
                .map(str::to_owned),
        );
        index.record_signatures.fill(u128::MAX);
        for short in &mut index.short_signatures {
            short.bytes.fill(u64::MAX);
            short.pairs = u128::MAX;
        }
        compare(&index, "doc report");
        assert_eq!(
            index
                .search(
                    &Query::parse("doc report"),
                    true,
                    usize::MAX,
                    &AtomicBool::new(false),
                    &AtomicUsize::new(0)
                )
                .matches,
            0
        );
    }
    #[test]
    fn parent_tokens_and_extensions_do_not_require_name_only_matches() {
        let index = Index::from_paths(
            ["/doc/report.pdf", "/report/x.docx", "/doc/report.rs"]
                .into_iter()
                .map(str::to_owned),
        );
        for q in [
            "doc report",
            "doc report ext:pdf",
            "ext:docx report",
            "x ext:docx",
        ] {
            compare(&index, q);
        }
        let out = index.search(
            &Query::parse("doc report"),
            true,
            usize::MAX,
            &AtomicBool::new(false),
            &AtomicUsize::new(0),
        );
        assert_eq!(out.matches, 3);
    }
}
