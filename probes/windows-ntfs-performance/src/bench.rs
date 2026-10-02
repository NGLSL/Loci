//! Same-fixture performance comparison for the Windows namespace prototype.
//!
//! This module deliberately measures two independent layers:
//!
//! * `scan_baseline` walks the directory tree with `read_dir` and opens every
//!   entry through the Stage A identity helper.
//! * `scan_optimized` asks the batched directory provider for the complete
//!   `(EntryId, attributes)` set and only recurses into ordinary directories.
//!
//! The query comparison starts from the same in-memory inventory.  The
//! baseline re-materializes `store::paths` for every query and collects every
//! match before taking 50 results.  `query::QueryIndex` is built once and
//! searched repeatedly.  Neither query path touches the filesystem.
//!
//! This is a measurement harness, not a product backend.  In particular, the
//! optimized scan is a batched directory enumeration experiment; it does not
//! claim to be whole-volume MFT enumeration or an Everything-equivalent
//! index.  The caller supplies fresh engineering fixture and storage
//! directories.  We leave both in place so the run can be inspected after the
//! process exits.

use crate::{
    acceptance,
    enumerate::{self, DirectoryListing},
    model::EntryId,
    query::QueryIndex,
    store, win,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    hint::black_box,
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const DIRECTORY: u32 = 0x10;
const REPARSE: u32 = 0x400;
const QUERY_LIMIT: usize = 50;
const SCAN_SAMPLES: usize = 3;
const BASELINE_QUERY_SAMPLES: usize = 32;
const OPTIMIZED_QUERY_SAMPLES: usize = 128;
const RUN_BUDGET: Duration = Duration::from_secs(60);

type Inventory = BTreeMap<EntryId, u32>;

#[derive(Debug)]
struct ScanResult {
    entries: Inventory,
    identity_opens: usize,
    directory_batches: usize,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct SearchResult {
    total: usize,
    paths: Vec<Vec<u16>>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn data_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn ordinary_directory(attributes: u32) -> bool {
    attributes & DIRECTORY != 0 && attributes & REPARSE == 0
}

fn check_count(count: usize) -> io::Result<()> {
    if matches!(count, 1000 | 10000) {
        Ok(())
    } else {
        Err(invalid("performance probe accepts only 1000 or 10000"))
    }
}

fn check_budget(deadline: Instant) -> io::Result<()> {
    if Instant::now() > deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "performance probe exceeded its 60 second sample budget",
        ))
    } else {
        Ok(())
    }
}

/// Verify that both caller-provided roots are ordinary descendants of this
/// probe's engineering run.  Pins are held by the caller for the entire run;
/// this function only performs the non-mutating path check.
fn confined_paths(root: &Path, storage_dir: &Path) -> io::Result<(PathBuf, PathBuf)> {
    if !root.is_absolute() || !storage_dir.is_absolute() {
        return Err(invalid("performance roots must be absolute paths"));
    }
    if root
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
        || storage_dir
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(invalid(
            "performance roots must not contain parent components",
        ));
    }

    let run =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-performance/run");
    let _run_pin = win::DirectoryPins::hold(&run)?;
    let run = fs::canonicalize(run)?;
    let root_pin = win::DirectoryPins::hold(root)?;
    let storage_pin = win::DirectoryPins::hold(storage_dir)?;
    let canonical_root = fs::canonicalize(root)?;
    let canonical_storage = fs::canonicalize(storage_dir)?;

    if canonical_root == run
        || canonical_storage == run
        || !canonical_root.starts_with(&run)
        || !canonical_storage.starts_with(&run)
        || canonical_root.starts_with(&canonical_storage)
        || canonical_storage.starts_with(&canonical_root)
    {
        return Err(invalid(
            "fixture and storage must be disjoint descendants of the engineering run",
        ));
    }

    // The pins are intentionally dropped here only after the same literal
    // ancestor checks used by the long-lived run.  The actual run acquires
    // fresh pins below, keeping their lifetime obvious at the call site.
    drop(root_pin);
    drop(storage_pin);
    Ok((canonical_root, canonical_storage))
}

fn baseline_scan(
    root: &Path,
    root_id: &win::Identity,
    deadline: Instant,
) -> io::Result<ScanResult> {
    let mut entries = Inventory::new();
    let mut pending = vec![(root.to_path_buf(), root_id.object)];
    let mut identity_opens = 0usize;

    while let Some((directory, parent)) = pending.pop() {
        check_budget(deadline)?;
        for item in fs::read_dir(&directory)? {
            check_budget(deadline)?;
            let item = item?;
            let path = item.path();
            // This is the baseline's defining cost: one native identity open
            // for every directory entry, including directories and reparses.
            let identity = win::identity(&path)?;
            identity_opens += 1;
            let name = item.file_name().encode_wide().collect::<Vec<_>>();
            let entry = EntryId {
                parent,
                object: identity.object,
                name,
            };
            if entries.insert(entry.clone(), identity.attributes).is_some() {
                return Err(data_error(
                    "baseline produced a duplicate parent/name entry",
                ));
            }
            if ordinary_directory(identity.attributes) {
                pending.push((path, identity.object));
            }
        }
    }

    Ok(ScanResult {
        entries,
        identity_opens,
        directory_batches: 0,
    })
}

fn insert_listing(
    directory: &Path,
    parent: u128,
    listing: DirectoryListing,
    entries: &mut Inventory,
    pending: &mut Vec<(PathBuf, u128)>,
    batches: &mut usize,
) -> io::Result<()> {
    *batches = batches
        .checked_add(listing.batches)
        .ok_or_else(|| data_error("directory batch counter overflow"))?;
    for (entry, attributes) in listing.entries {
        if entry.parent != parent {
            return Err(data_error(
                "optimized enumeration returned an entry for a different parent",
            ));
        }
        if entries.insert(entry.clone(), attributes).is_some() {
            return Err(data_error(
                "optimized enumeration produced a duplicate parent/name entry",
            ));
        }
        if ordinary_directory(attributes) {
            let child = directory.join(OsString::from_wide(&entry.name));
            pending.push((child, entry.object));
        }
    }
    Ok(())
}

fn optimized_scan(
    root: &Path,
    root_id: &win::Identity,
    began: Instant,
    deadline: Instant,
) -> io::Result<ScanResult> {
    let cancel = AtomicBool::new(false);
    let mut entries = Inventory::new();
    let mut pending = vec![(root.to_path_buf(), root_id.object)];
    let mut directory_batches = 0usize;
    while let Some((directory, parent)) = pending.pop() {
        check_budget(deadline)?;
        let listing =
            enumerate::list_directory(&directory, parent, root_id.volume_serial, &cancel, began)?;
        insert_listing(
            &directory,
            parent,
            listing,
            &mut entries,
            &mut pending,
            &mut directory_batches,
        )?;
    }
    Ok(ScanResult {
        entries,
        identity_opens: 0,
        directory_batches,
    })
}

fn sorted_percentile(samples: &[u128], numerator: usize, denominator: usize) -> u128 {
    assert!(!samples.is_empty());
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let last = ordered.len() - 1;
    let index = (last * numerator + denominator - 1) / denominator;
    ordered[index.min(last)]
}

fn print_scan_summary(label: &str, samples: &[u128], first: &ScanResult) {
    let p50 = sorted_percentile(samples, 50, 100);
    let p95 = sorted_percentile(samples, 95, 100);
    println!(
        "scan_benchmark label={label} samples={} p50_ns={p50} p95_ns={p95} entries={} identity_opens={} directory_batches={}",
        samples.len(),
        first.entries.len(),
        first.identity_opens,
        first.directory_batches
    );
}

fn contains_literal(path: &[u16], needle: &[u16]) -> bool {
    needle.is_empty() || path.windows(needle.len()).any(|window| window == needle)
}

/// The baseline query intentionally retains the old full-path materialization
/// cost for every invocation.  `limit` applies only after all matches have
/// been collected, while `total` remains complete.
fn baseline_search(
    root: u128,
    entries: &Inventory,
    needle: &[u16],
    limit: usize,
) -> io::Result<SearchResult> {
    let paths = store::paths(root, entries)?;
    let matches: Vec<Vec<u16>> = paths
        .into_iter()
        .filter(|path| contains_literal(path, needle))
        .collect();
    let total = matches.len();
    let paths = matches.into_iter().take(limit).collect();
    Ok(SearchResult { total, paths })
}

fn optimized_search(index: &QueryIndex, needle: &[u16], limit: usize) -> io::Result<SearchResult> {
    let (total, paths) = index.search(needle, limit);
    Ok(SearchResult { total, paths })
}

fn compare_result(label: &str, expected: &SearchResult, actual: &SearchResult) -> io::Result<()> {
    if expected != actual {
        return Err(data_error(format!(
            "query result mismatch probe={label} baseline_total={} optimized_total={} baseline_returned={} optimized_returned={}",
            expected.total,
            actual.total,
            expected.paths.len(),
            actual.paths.len()
        )));
    }
    Ok(())
}

fn query_probes() -> Vec<(&'static str, Vec<u16>)> {
    vec![
        ("empty", Vec::new()),
        ("short", "file".encode_utf16().collect()),
        (
            "path",
            "bucket-001\\file-00000.txt".encode_utf16().collect(),
        ),
        ("literal", "unicode-中文-😀.txt".encode_utf16().collect()),
        (
            "nonexistent",
            "__loci_performance_probe_missing__"
                .encode_utf16()
                .collect(),
        ),
        ("raw_utf16_d800", vec![0xd800]),
    ]
}

fn run_queries(
    root_object: u128,
    entries: &Inventory,
    paths: &BTreeSet<Vec<u16>>,
    deadline: Instant,
) -> io::Result<()> {
    let derived_path_utf16_payload_bytes = paths.iter().try_fold(0usize, |total, path| {
        total
            .checked_add(path.len().saturating_mul(2))
            .ok_or_else(|| data_error("derived path payload byte count overflow"))
    })?;
    let metrics_before = win::metrics()?;
    let build_start = Instant::now();
    let index = QueryIndex::build(root_object, entries)?;
    let build_ns = build_start.elapsed().as_nanos();
    let metrics_after = win::metrics()?;
    black_box(&index);
    let working_set_delta = metrics_after.working_set as i128 - metrics_before.working_set as i128;
    let private_commit_delta =
        metrics_after.private_usage as i128 - metrics_before.private_usage as i128;
    let handles_delta = metrics_after.handles as i128 - metrics_before.handles as i128;
    println!(
        "query_index_build paths={} derived_path_utf16_payload_bytes={} elapsed_ns={} elapsed_us={} metrics_scope=baseline_and_optimized_inventory_retained metrics_before_working_set_bytes={} metrics_after_working_set_bytes={} working_set_delta_bytes={working_set_delta} metrics_before_private_commit_bytes={} metrics_after_private_commit_bytes={} private_commit_delta_bytes={private_commit_delta} metrics_before_handles={} metrics_after_handles={} handles_delta={handles_delta} private_heap=unmeasured kernel_bytes=unmeasured",
        paths.len(),
        derived_path_utf16_payload_bytes,
        build_ns,
        build_ns / 1_000,
        metrics_before.working_set,
        metrics_after.working_set,
        metrics_before.private_usage,
        metrics_after.private_usage,
        metrics_before.handles,
        metrics_after.handles,
    );

    for (label, needle) in query_probes() {
        check_budget(deadline)?;
        // One untimed correctness pass also establishes the complete count
        // and first-50 result before the warm samples begin.
        let baseline = baseline_search(root_object, entries, &needle, QUERY_LIMIT)?;
        let optimized = optimized_search(&index, &needle, QUERY_LIMIT)?;
        compare_result(label, &baseline, &optimized)?;

        let mut baseline_samples = Vec::with_capacity(BASELINE_QUERY_SAMPLES);
        let mut optimized_samples = Vec::with_capacity(OPTIMIZED_QUERY_SAMPLES);
        let mut baseline_total = baseline.total;
        let mut optimized_total = optimized.total;

        // Alternate while both sample budgets remain.  The optimized side has
        // four times as many samples because it is expected to be the cheap
        // hot path; the total work is still bounded and visible in output.
        let mut baseline_done = 0usize;
        let mut optimized_done = 0usize;
        while baseline_done < BASELINE_QUERY_SAMPLES || optimized_done < OPTIMIZED_QUERY_SAMPLES {
            check_budget(deadline)?;
            if baseline_done < BASELINE_QUERY_SAMPLES {
                let start = Instant::now();
                let result = baseline_search(root_object, entries, &needle, QUERY_LIMIT)?;
                baseline_samples.push(start.elapsed().as_nanos());
                baseline_total = result.total;
                compare_result(label, &baseline, &result)?;
                black_box(&result);
                baseline_done += 1;
            }
            if optimized_done < OPTIMIZED_QUERY_SAMPLES {
                let start = Instant::now();
                let result = optimized_search(&index, &needle, QUERY_LIMIT)?;
                optimized_samples.push(start.elapsed().as_nanos());
                optimized_total = result.total;
                compare_result(label, &baseline, &result)?;
                black_box(&result);
                optimized_done += 1;
            }
        }

        let baseline_p50 = sorted_percentile(&baseline_samples, 50, 100);
        let baseline_p95 = sorted_percentile(&baseline_samples, 95, 100);
        let optimized_p50 = sorted_percentile(&optimized_samples, 50, 100);
        let optimized_p95 = sorted_percentile(&optimized_samples, 95, 100);
        let ratio = if optimized_p50 == 0 {
            f64::INFINITY
        } else {
            baseline_p50 as f64 / optimized_p50 as f64
        };
        println!(
            "query_benchmark probe={label} baseline_samples={} optimized_samples={} baseline_p50_ns={} baseline_p95_ns={} optimized_p50_ns={} optimized_p95_ns={} baseline_total={} optimized_total={} returned_limit={} baseline_to_optimized_p50_ratio={ratio:.2}",
            baseline_samples.len(),
            optimized_samples.len(),
            baseline_p50,
            baseline_p95,
            optimized_p50,
            optimized_p95,
            baseline_total,
            optimized_total,
            QUERY_LIMIT,
        );
    }
    Ok(())
}

/// Run the bounded same-fixture baseline/optimized scan and query comparison.
///
/// The caller must pass fresh, existing, disjoint ordinary directories under
/// `.scratch/windows-ntfs-performance/run`.  Only `root` is populated by the
/// Stage B seed helper.  The function intentionally leaves both directories
/// and all stdout evidence in place for review.
pub fn run(root: &Path, storage_dir: &Path, count: usize) -> io::Result<()> {
    check_count(count)?;
    let (canonical_root, _canonical_storage) = confined_paths(root, storage_dir)?;

    // Keep the pins alive over seed, every filesystem walk, and every query
    // phase.  The optimized query itself never uses these handles; they guard
    // the static namespace used to produce the measurement.
    let _root_pin = win::DirectoryPins::hold(root)?;
    let _storage_pin = win::DirectoryPins::hold(storage_dir)?;
    acceptance::seed(&canonical_root, count)?;

    let root_id = win::identity(&canonical_root)?;
    let oracle = acceptance::oracle(&canonical_root)?;
    let benchmark_started = Instant::now();
    let deadline = benchmark_started + RUN_BUDGET;

    let mut baseline_samples = Vec::with_capacity(SCAN_SAMPLES);
    let mut optimized_samples = Vec::with_capacity(SCAN_SAMPLES);
    let mut baseline_first: Option<ScanResult> = None;
    let mut optimized_first: Option<ScanResult> = None;
    let mut canonical_entries: Option<Inventory> = None;
    let mut canonical_paths: Option<BTreeSet<Vec<u16>>> = None;

    for round in 0..SCAN_SAMPLES {
        check_budget(deadline)?;
        let baseline_first_order = round % 2 == 0;
        if baseline_first_order {
            let start = Instant::now();
            let baseline = baseline_scan(&canonical_root, &root_id, deadline)?;
            baseline_samples.push(start.elapsed().as_nanos());
            if baseline_first.is_none() {
                baseline_first = Some(baseline);
            } else {
                let expected = baseline_first.as_ref().unwrap();
                if expected.entries != baseline.entries {
                    return Err(data_error("baseline scan changed on a static fixture"));
                }
            }

            let start = Instant::now();
            let optimized = optimized_scan(&canonical_root, &root_id, start, deadline)?;
            optimized_samples.push(start.elapsed().as_nanos());
            if optimized_first.is_none() {
                optimized_first = Some(optimized);
            } else {
                let expected = optimized_first.as_ref().unwrap();
                if expected.entries != optimized.entries {
                    return Err(data_error("optimized scan changed on a static fixture"));
                }
            }
        } else {
            let start = Instant::now();
            let optimized = optimized_scan(&canonical_root, &root_id, start, deadline)?;
            optimized_samples.push(start.elapsed().as_nanos());
            if optimized_first.is_none() {
                optimized_first = Some(optimized);
            } else {
                let expected = optimized_first.as_ref().unwrap();
                if expected.entries != optimized.entries {
                    return Err(data_error("optimized scan changed on a static fixture"));
                }
            }

            let start = Instant::now();
            let baseline = baseline_scan(&canonical_root, &root_id, deadline)?;
            baseline_samples.push(start.elapsed().as_nanos());
            if baseline_first.is_none() {
                baseline_first = Some(baseline);
            } else {
                let expected = baseline_first.as_ref().unwrap();
                if expected.entries != baseline.entries {
                    return Err(data_error("baseline scan changed on a static fixture"));
                }
            }
        }

        if canonical_entries.is_none() {
            let baseline = baseline_first.as_ref().unwrap();
            let paths = store::paths(root_id.object, &baseline.entries)?;
            if paths != oracle {
                return Err(data_error(
                    "baseline inventory differs from independent complete path oracle",
                ));
            }
            canonical_entries = Some(baseline.entries.clone());
            canonical_paths = Some(paths);
        }
    }

    let baseline = baseline_first
        .as_ref()
        .ok_or_else(|| data_error("baseline produced no samples"))?;
    let optimized = optimized_first
        .as_ref()
        .ok_or_else(|| data_error("optimized produced no samples"))?;
    if baseline.entries != optimized.entries {
        return Err(data_error(
            "baseline and optimized complete EntryId/attribute maps differ",
        ));
    }
    let optimized_paths = store::paths(root_id.object, &optimized.entries)?;
    if optimized_paths != oracle {
        return Err(data_error(
            "optimized inventory differs from independent complete path oracle",
        ));
    }

    print_scan_summary("baseline", &baseline_samples, baseline);
    print_scan_summary("optimized", &optimized_samples, optimized);
    let baseline_p50 = sorted_percentile(&baseline_samples, 50, 100);
    let optimized_p50 = sorted_percentile(&optimized_samples, 50, 100);
    let scan_ratio = if optimized_p50 == 0 {
        f64::INFINITY
    } else {
        baseline_p50 as f64 / optimized_p50 as f64
    };
    println!(
        "scan_comparison entries={} complete_path_oracle={} entry_attributes_equal=true baseline_identity_opens={} optimized_identity_opens=0 baseline_directory_batches=0 optimized_directory_batches={} baseline_to_optimized_p50_ratio={scan_ratio:.2}",
        baseline.entries.len(),
        oracle.len(),
        baseline.identity_opens,
        optimized.directory_batches,
    );

    let entries = canonical_entries
        .as_ref()
        .ok_or_else(|| data_error("missing canonical inventory"))?;
    let paths = canonical_paths
        .as_ref()
        .ok_or_else(|| data_error("missing canonical paths"))?;
    run_queries(root_id.object, entries, paths, deadline)?;

    let elapsed = benchmark_started.elapsed().as_millis();
    println!(
        "performance_complete=true fixture_files={count} static_fixture=true complete_path_oracle=true usn=false mft_volume_enumeration=false elapsed_ms={elapsed} sample_budget_seconds=60 evidence_preserved=true"
    );
    Ok(())
}
