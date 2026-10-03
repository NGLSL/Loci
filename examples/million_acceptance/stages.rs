//! Native stage orchestration. These routines never poll the Engine themselves.
use super::*;

fn metric(status: &Json, key: &str) -> io::Result<u64> {
    status.get("metrics")?.get(key)?.number()
}
fn epoch(status: &Json) -> io::Result<u64> {
    status.get("resources")?.get("inventory_epoch")?.number()
}
fn wait_advance(worker: &mut Worker, before: &Json, compaction: bool) -> io::Result<Json> {
    let start = Instant::now();
    loop {
        let after = worker.status()?;
        let advanced = if compaction {
            metric(&after, "compactions")? > metric(before, "compactions")?
                && epoch(&after)? > epoch(before)?
                && !after
                    .get("resources")?
                    .get("compaction_in_progress")?
                    .boolean()?
        } else {
            metric(&after, "full_scans")? > metric(before, "full_scans")?
                && epoch(&after)? > epoch(before)?
        };
        if advanced
            && after.get("status")?.text()? == "Validated"
            && after.get("gaps")?.array()?.is_empty()
        {
            return Ok(after);
        }
        if after.get("status")?.text()? == "Failed" {
            return Err(io::Error::other(format!("stage failed:{}", after.encode())));
        }
        if start.elapsed() > Duration::from_secs(3600) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "publication stage deadline",
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
}
fn settle_resources(
    worker: &mut Worker,
    options: &Options,
    log: &mut Log,
    stage: &str,
) -> io::Result<()> {
    let before = worker.status()?;
    let first = process::sample(worker.child.id())?;
    let requested = if options.smoke {
        1
    } else if stage == "correction-2" {
        10
    } else {
        5
    };
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(requested) {
        thread::sleep(Duration::from_millis(100));
    }
    let after = worker.status()?;
    let last = process::sample(worker.child.id())?;
    let quiet_single_epoch = before.get("status")?.text()? == "Validated"
        && after.get("status")?.text()? == "Validated"
        && before.get("version")?.number()? == after.get("version")?.number()?
        && after.get("leases")?.number()? == 0
        && !after
            .get("resources")?
            .get("compaction_in_progress")?
            .boolean()?;
    log.record(
        "maintenance-settle",
        Json::object([
            ("stage", s(stage)),
            ("actual_ns", n(started.elapsed().as_nanos())),
            ("requested_seconds", n(requested)),
            (
                "cpu_percent_one_core",
                Json::Number(
                    (100.0
                        * last
                            .get("cpu_ticks")?
                            .number()?
                            .saturating_sub(first.get("cpu_ticks")?.number()?)
                            as f64
                        / last.get("clock_ticks_per_second")?.number()? as f64
                        / started.elapsed().as_secs_f64())
                    .to_string(),
                ),
            ),
            ("before", before),
            ("after", after),
            ("first_resources", first),
            (
                "rss_numeric_pass",
                b(last.get("vmrss_bytes")?.number()? <= 200 * 1024 * 1024),
            ),
            (
                "hwm_numeric_pass",
                b(last.get("vmhwm_bytes")?.number()? <= 512 * 1024 * 1024),
            ),
            ("quiet_single_epoch", b(quiet_single_epoch)),
            ("last_resources", last),
            ("acceptance", b(false)),
        ]),
    )?;
    if std::env::var("LOCI_DEVELOPMENT_TRIM").as_deref() == Ok("1") {
        let first = process::sample(worker.child.id())?;
        let probe = worker.request("DEVELOPMENT_NATIVE_TRIM", &[], Duration::from_secs(10))?;
        let last = process::sample(worker.child.id())?;
        log.record(
            "development-native-trim",
            Json::object([
                ("stage", s(stage)),
                ("probe", probe),
                ("before_resources", first),
                ("after_resources", last),
                ("acceptance", b(false)),
            ]),
        )?;
    }
    if !quiet_single_epoch {
        return Err(invalid(
            "maintenance settle did not preserve unleased validated epoch",
        ));
    }
    Ok(())
}
pub(super) fn compaction(worker: &mut Worker, options: &Options, log: &mut Log) -> io::Result<()> {
    let before = worker.status()?;
    let start = Instant::now();
    worker.operation("COMPACT", &[])?;
    let after = wait_advance(worker, &before, true)?;
    if metric(&before, "full_scans")? != metric(&after, "full_scans")? {
        return Err(invalid("compaction performed a root filesystem scan"));
    }
    log.record(
        "compaction",
        Json::object([
            ("elapsed_ns", n(start.elapsed().as_nanos())),
            ("before", before),
            ("after", after),
            ("waited_for_publication", b(true)),
            ("resources", process::sample(worker.child.id())?),
        ]),
    )?;
    settle_resources(worker, options, log, "compaction")?;
    correctness(worker, options, "compaction", log)?;
    Ok(())
}
pub(super) fn corrections(worker: &mut Worker, options: &Options, log: &mut Log) -> io::Result<()> {
    for trial in 0..3 {
        let before = worker.status()?;
        let start = Instant::now();
        worker.operation("REBUILD", &[])?;
        let after = wait_advance(worker, &before, false)?;
        log.record(
            "full-correction",
            Json::object([
                ("trial", n(trial as u64)),
                ("resources", process::sample(worker.child.id())?),
                ("elapsed_ns", n(start.elapsed().as_nanos())),
                ("before", before),
                ("after", after),
                ("cache_state", s("warm-or-unspecified-filesystem-cache")),
            ]),
        )?;
        settle_resources(worker, options, log, &format!("correction-{trial}"))?;
        correctness(worker, options, &format!("correction-{trial}"), log)?;
    }
    Ok(())
}
pub(super) fn cancellations(worker: &mut Worker, log: &mut Log) -> io::Result<()> {
    let before = worker.status()?;
    let version = before.get("version")?.number()?;
    let admitted = worker.request("SORT_START", &[String::new()], Duration::from_secs(10))?;
    let id = admitted.get("job_id")?.number()?;
    worker.request("CANCEL", &[id.to_string()], Duration::from_secs(10))?;
    let start = Instant::now();
    let outcome = loop {
        let state = worker.request("OP_STATE", &[id.to_string()], Duration::from_secs(5))?;
        if !matches!(state.get("state")?.text()?, "Pending" | "Running") {
            break state;
        }
        if start.elapsed() > Duration::from_secs(30) {
            return Err(invalid("query cancellation deadline"));
        }
        thread::sleep(Duration::from_millis(2));
    };
    worker.request("OP_DROP", &[id.to_string()], Duration::from_secs(10))?;
    log.record(
        "query-cancel",
        Json::object([
            ("elapsed_ns", n(start.elapsed().as_nanos())),
            ("outcome", outcome.clone()),
            (
                "cancel_observed",
                b(outcome.get("state")?.text()? == "Cancelled"),
            ),
            (
                "completion_race",
                b(outcome.get("state")?.text()? == "Complete"),
            ),
        ]),
    )?;
    // Monitor command acknowledgement is not cancellation completion evidence.
    worker.operation("REBUILD", &[])?;
    worker.request("CANCEL_CORRECTION", &[], Duration::from_secs(10))?;
    let start = Instant::now();
    let after = loop {
        let view = worker.status()?;
        let cancelled = view
            .get("gaps")?
            .array()?
            .iter()
            .any(|g| g.get("kind").and_then(Json::text).ok() == Some("Cancelled"));
        if cancelled {
            break view;
        }
        if start.elapsed() > Duration::from_secs(30) {
            return Err(invalid("correction cancellation evidence deadline"));
        }
        thread::sleep(Duration::from_millis(10));
    };
    if after.get("status")?.text()? != "Pending" || after.get("version")?.number()? != version {
        return Err(invalid("cancelled correction lost the saved publication"));
    }
    let retained = worker.query("")?;
    if retained.get("version")?.number()? != version || retained.get("validated")?.boolean()? {
        return Err(invalid(
            "cancelled correction snapshot incorrectly validated",
        ));
    }
    log.record(
        "correction-cancel",
        Json::object([
            ("elapsed_ns", n(start.elapsed().as_nanos())),
            ("before", before),
            ("after", after),
            ("retained_query", retained),
        ]),
    )
}
fn wait_presence(
    worker: &mut Worker,
    old: &Path,
    old_want: bool,
    new: &Path,
    new_want: bool,
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let name = |p: &Path| {
        p.file_name()
            .and_then(|x| x.to_str())
            .map(str::to_owned)
            .ok_or_else(|| invalid("subtree root must have a UTF8 name"))
    };
    visible(worker, &name(old)?, old, old_want, deadline)?;
    visible(worker, &name(new)?, new, new_want, deadline)?;
    Ok(())
}
struct RenameGuard {
    original: PathBuf,
    renamed: PathBuf,
    active: bool,
}
impl RenameGuard {
    fn restore(&mut self) -> io::Result<()> {
        if self.active {
            fs::rename(&self.renamed, &self.original)?;
            self.active = false;
        }
        Ok(())
    }
}
impl Drop for RenameGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::rename(&self.renamed, &self.original);
        }
    }
}
pub(super) fn subtree(
    worker: &mut Worker,
    options: &Options,
    log: &mut Log,
    dense: bool,
) -> io::Result<()> {
    let original = if dense {
        options.root.join("dense_tree")
    } else {
        let mut dirs = fs::read_dir(&options.root)?.collect::<io::Result<Vec<_>>>()?;
        dirs.sort_by_key(|d| d.file_name());
        dirs.into_iter()
            .find(|d| fs::symlink_metadata(d.path()).is_ok_and(|m| m.is_dir()))
            .ok_or_else(|| invalid("subtree fixture has no root directory"))?
            .path()
    };
    let renamed = options
        .root
        .join(format!("__loci_owned_subtree_{}", std::process::id()));
    if fs::symlink_metadata(&renamed).is_ok() {
        return Err(invalid("subtree destination exists"));
    }
    let before = worker.status()?;
    let held = worker.request("HOLD_LEASE", &[], Duration::from_secs(10))?;
    let lease = held.get("lease_id")?.number()?;
    let mut guard = RenameGuard {
        original: original.clone(),
        renamed: renamed.clone(),
        active: false,
    };
    let start = Instant::now();
    fs::rename(&original, &renamed)?;
    guard.active = true;
    wait_presence(
        worker,
        &original,
        false,
        &renamed,
        true,
        Duration::from_secs(30),
    )?;
    let after = worker.status()?;
    log.record(
        "subtree-rename",
        Json::object([
            ("dense_fixture", b(dense)),
            ("old_hex", s(hex(original.as_os_str().as_bytes()))),
            ("new_hex", s(hex(renamed.as_os_str().as_bytes()))),
            ("visibility_ns", n(start.elapsed().as_nanos())),
            ("before", before.clone()),
            ("after", after.clone()),
            ("resources", process::sample(worker.child.id())?),
        ]),
    )?;
    if metric(&before, "full_scans")? != metric(&after, "full_scans")? {
        return Err(invalid("trusted subtree rename triggered a full root scan"));
    }
    correctness(worker, options, "subtree-renamed", log)?;
    let export = worker.operation(
        "LEASE_EXPORT",
        &[
            lease.to_string(),
            hex(b"old-subtree-lease.nul"),
            String::new(),
        ],
    )?;
    let old = PathBuf::from(std::ffi::OsString::from_vec(unhex(
        export.get("path_hex")?.text()?,
    )?));
    let kinds = PathBuf::from(std::ffi::OsString::from_vec(unhex(
        export.get("kinds_path_hex")?.text()?,
    )?));
    let sorted = options.output.join("old-subtree-lease-sorted.nul");
    let sorted_kinds = options.output.join("old-subtree-lease-sorted.kinds");
    let budget = crate::budget::Budget::with_checkpoint(
        &options.output,
        options
            .output_budget_bytes
            .saturating_sub(320 * 1024 * 1024),
        &options.database,
    )?;
    external_sort(&old, &sorted, &options.output, true, &budget)?;
    external_sort(&kinds, &sorted_kinds, &options.output, false, &budget)?;
    equal_files(&sorted, &log.retained_initial[0])?;
    equal_files(&sorted_kinds, &log.retained_initial[1])?;
    if export.get("version")?.number()? != before.get("version")?.number()? {
        return Err(invalid("old lease version changed on subtree rename"));
    }
    log.record(
        "subtree-old-lease",
        Json::object([
            ("version", export.get("version")?.clone()),
            ("count", export.get("count")?.clone()),
            ("sha256", s(file_digest(&sorted)?)),
            ("kinds_sha256", s(file_digest(&sorted_kinds)?)),
            ("full_byte_set_equal", b(true)),
            ("full_kind_set_equal", b(true)),
        ]),
    )?;
    for path in [&old, &kinds, &sorted, &sorted_kinds] {
        remove_owned_artifact(&options.output, path)?;
    }
    worker.request(
        "RELEASE_LEASE",
        &[lease.to_string()],
        Duration::from_secs(10),
    )?;
    let start = Instant::now();
    guard.restore()?;
    wait_presence(
        worker,
        &renamed,
        false,
        &original,
        true,
        Duration::from_secs(30),
    )?;
    log.record(
        "subtree-rename-restored",
        Json::object([
            ("visibility_ns", n(start.elapsed().as_nanos())),
            ("status", worker.status()?),
        ]),
    )?;
    correctness(worker, options, "subtree-restored", log)?;
    Ok(())
}
