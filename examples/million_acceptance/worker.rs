use crate::process;
use crate::protocol::{b, frame_read, frame_write, hex, invalid, n, s, unhex, Json};
use crate::Options;
use loci_experiment::engine::{
    Engine, EngineOptions, EntryKind, MonitorOwner, MonitorRequest, QueryJob, QueryJobState,
    QueryLease,
};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_EXPORT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_JOBS: usize = 12;
const MAX_HELD_LEASES: usize = 2;
enum Job {
    Monitor {
        request: MonitorRequest,
        started: Instant,
        done: Option<Result<(), String>>,
    },
    Query {
        job: QueryJob,
        started: Instant,
    },
    Export {
        cancel: Arc<AtomicBool>,
        progress: Arc<AtomicUsize>,
        thread: Option<JoinHandle<()>>,
        receiver: mpsc::Receiver<Result<Json, String>>,
        done: Option<Result<Json, String>>,
        started: Instant,
    },
}
impl Job {
    fn status(&mut self) -> Json {
        match self {
            Self::Monitor {
                request,
                started,
                done,
            } => {
                if done.is_none() {
                    if let Some(result) = request.try_complete() {
                        *done = Some(result.map_err(|e| e.to_string()));
                    }
                }
                let state = match done {
                    None => "Running",
                    Some(Ok(())) => "Complete",
                    Some(Err(_)) => "Failed",
                };
                Json::object([
                    ("state", s(state)),
                    ("elapsed_ns", n(started.elapsed().as_nanos())),
                    (
                        "error",
                        done.as_ref()
                            .and_then(|r| r.as_ref().err())
                            .map_or(Json::Null, |e| s(e.clone())),
                    ),
                ])
            }
            Self::Query { job, started } => {
                let (state, error) = match job.state() {
                    QueryJobState::Pending => ("Pending", Json::Null),
                    QueryJobState::Running => ("Running", Json::Null),
                    QueryJobState::Complete => ("Complete", Json::Null),
                    QueryJobState::Cancelled => ("Cancelled", Json::Null),
                    QueryJobState::Failed(e) => ("Failed", s(e)),
                };
                Json::object([
                    ("state", s(state)),
                    ("elapsed_ns", n(started.elapsed().as_nanos())),
                    ("version", n(job.version())),
                    ("count", job.count().map_or(Json::Null, n)),
                    ("progress", n(job.progress())),
                    ("error", error),
                ])
            }
            Self::Export {
                receiver,
                done,
                thread,
                progress,
                started,
                ..
            } => {
                if done.is_none() {
                    match receiver.try_recv() {
                        Ok(result) => *done = Some(result),
                        Err(mpsc::TryRecvError::Empty) => (),
                        Err(mpsc::TryRecvError::Disconnected) => {
                            *done = Some(Err("export disconnected".into()))
                        }
                    }
                }
                if done.is_some() {
                    if let Some(t) = thread.take() {
                        if t.join().is_err() {
                            *done = Some(Err("export panicked".into()));
                        }
                    }
                }
                match done {
                    Some(Ok(value)) => value.clone(),
                    Some(Err(error)) => Json::object([
                        ("state", s("Failed")),
                        ("error", s(error.clone())),
                        ("elapsed_ns", n(started.elapsed().as_nanos())),
                        ("progress", n(progress.load(Ordering::Acquire))),
                    ]),
                    None => Json::object([
                        ("state", s("Running")),
                        ("elapsed_ns", n(started.elapsed().as_nanos())),
                        ("progress", n(progress.load(Ordering::Acquire))),
                    ]),
                }
            }
        }
    }
    fn cancel(&self) -> io::Result<()> {
        match self {
            Self::Query { job, .. } => {
                job.cancel();
                Ok(())
            }
            Self::Export { cancel, .. } => {
                cancel.store(true, Ordering::Release);
                Ok(())
            }
            Self::Monitor { .. } => Err(invalid("cannot cancel admitted monitor command")),
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        if let Self::Export { cancel, thread, .. } = self {
            cancel.store(true, Ordering::Release);
            if let Some(t) = thread.take() {
                let _ = t.join();
            }
        }
    }
}
fn query_text(value: &str) -> io::Result<String> {
    String::from_utf8(unhex(value)?).map_err(|_| invalid("query UTF8"))
}
fn integer(value: &str) -> io::Result<u64> {
    value.parse().map_err(|_| invalid("protocol integer"))
}
fn safe_output(output: &Path, raw: &str) -> io::Result<PathBuf> {
    let bytes = unhex(raw)?;
    if bytes.is_empty()
        || bytes.len() > 128
        || bytes.contains(&b'/')
        || bytes.contains(&0)
        || bytes == b"."
        || bytes == b".."
    {
        return Err(invalid("output basename bound"));
    }
    process::owned_directory(output)?;
    Ok(output.join(std::ffi::OsString::from_vec(bytes)))
}
fn export(
    lease: Arc<QueryLease>,
    path: PathBuf,
    query: String,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicUsize>,
    budget: crate::budget::Budget,
) -> io::Result<Job> {
    let started = Instant::now();
    budget.check(2 * 1024 * 1024)?;
    let mut writer = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(0x20000)
            .open(&path)?,
    );
    let kinds_path = path.with_extension("kinds");
    let mut kinds = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(0x20000)
            .open(&kinds_path)?,
    );
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_cancel = cancel.clone();
    let worker_progress = progress.clone();
    let thread = thread::Builder::new()
        .name("acceptance-export".into())
        .spawn(move || {
            let result = (|| -> io::Result<Json> {
                let mut cursor = None;
                let mut count = 0u64;
                let mut bytes = 0u64;
                let mut validated = true;
                let mut checkpoint_bytes = 0u64;
                let mut kinds_bytes = 0u64;
                let version = loop {
                    let page = lease.page(
                        &query,
                        cursor.as_ref(),
                        256,
                        &worker_cancel,
                        &worker_progress,
                    )?;
                    validated &= page.validated_at_start_and_finish;
                    if page.cancelled || worker_cancel.load(Ordering::Acquire) {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "export cancelled",
                        ));
                    }
                    let kinds_page = page
                        .kinds
                        .as_ref()
                        .ok_or_else(|| invalid("scale typed page metadata unavailable"))?;
                    if kinds_page.len() != page.paths.len() {
                        return Err(invalid("typed page path/kind alignment"));
                    }
                    for (p, kind) in page.paths.iter().zip(kinds_page) {
                        let raw = p.as_os_str().as_bytes();
                        bytes = bytes
                            .checked_add(raw.len() as u64 + 1)
                            .ok_or_else(|| invalid("export byte overflow"))?;
                        if bytes > MAX_EXPORT_BYTES {
                            return Err(invalid("export output budget"));
                        }
                        writer.write_all(raw)?;
                        writer.write_all(&[0])?;
                        let kind = match kind {
                            EntryKind::File => "F",
                            EntryKind::Directory => "D",
                            EntryKind::Symlink => "L",
                        };
                        kinds_bytes += raw.len() as u64 * 2 + 3;
                        if kinds_bytes > MAX_EXPORT_BYTES {
                            return Err(invalid("kind sidecar filebudget"));
                        }
                        if bytes + kinds_bytes - checkpoint_bytes >= 1024 * 1024 {
                            budget.check(2 * 1024 * 1024)?;
                            checkpoint_bytes = bytes + kinds_bytes;
                        }
                        writeln!(kinds, "{}\t{}", hex(raw), kind)?;
                        count += 1;
                    }
                    if page.complete {
                        break page.version;
                    }
                    cursor = page.next;
                    if cursor.is_none() {
                        return Err(invalid("incomplete export lacks cursor"));
                    }
                };
                writer.flush()?;
                writer.get_ref().sync_all()?;
                kinds.flush()?;
                kinds.get_ref().sync_all()?;
                Ok(Json::object([
                    ("state", s("Complete")),
                    ("complete", b(true)),
                    ("validated_start_finish", b(validated)),
                    ("path_hex", s(hex(path.as_os_str().as_bytes()))),
                    ("kinds_path_hex", s(hex(kinds_path.as_os_str().as_bytes()))),
                    ("count", n(count)),
                    ("version", n(version)),
                    ("bytes", n(bytes)),
                    ("elapsed_ns", n(started.elapsed().as_nanos())),
                ]))
            })()
            .map_err(|e| e.to_string());
            let _ = sender.try_send(result);
        })?;
    Ok(Job::Export {
        cancel,
        progress,
        thread: Some(thread),
        receiver,
        done: None,
        started,
    })
}
fn status(owner: &MonitorOwner) -> Json {
    let v = owner.view();
    let r = v.resources;
    let m = owner.metrics();
    Json::object([
        ("status", s(format!("{:?}", v.status))),
        ("version", n(v.version)),
        ("leases", n(v.leases)),
        (
            "gaps",
            Json::Array(
                v.coverage_gaps
                    .iter()
                    .map(|g| {
                        Json::object([
                            ("kind", s(format!("{:?}", g.kind))),
                            ("path_hex", s(hex(g.path.as_os_str().as_bytes()))),
                            ("error", s(g.error.clone())),
                            (
                                "errno",
                                g.errno.map_or(Json::Null, |e| Json::Number(e.to_string())),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "losses",
            Json::Array(
                v.observed_losses
                    .iter()
                    .map(|l| s(format!("{l:?}")))
                    .collect(),
            ),
        ),
        (
            "resources",
            Json::object([
                ("session_watches", n(r.session_watches)),
                ("session_watch_limit", n(r.session_watch_limit)),
                ("process_watch_limit", n(r.process_watch_limit)),
                ("process_fd_limit", n(r.process_fd_limit)),
                ("queue_limit", n(r.queue_limit)),
                ("queue_byte_limit", n(r.queue_byte_limit)),
                ("event_buffer_bytes", n(r.event_buffer_bytes)),
                ("slot_limit", n(r.slot_limit)),
                ("name_byte_limit", n(r.name_byte_limit)),
                ("snapshot_byte_limit", n(r.snapshot_byte_limit)),
                ("retained_byte_limit", n(r.retained_byte_limit)),
                ("process_watches", n(r.process_watches)),
                ("inotify_fds", n(r.inotify_fds)),
                ("process_inotify_fds", n(r.process_inotify_fds)),
                ("inventory_slots", n(r.inventory_slots)),
                ("inventory_name_bytes", n(r.inventory_name_bytes)),
                ("snapshot_bytes", n(r.snapshot_bytes)),
                ("retained_snapshot_bytes", n(r.retained_snapshot_bytes)),
                ("memory_reserved_bytes", n(r.memory_reserved_bytes)),
                (
                    "process_memory_reserved_bytes",
                    n(r.process_memory_reserved_bytes),
                ),
                ("process_memory_limit", n(r.process_memory_limit)),
                ("compaction_in_progress", b(r.compaction_in_progress)),
                ("inventory_epoch", n(r.inventory_epoch)),
                ("queued_events", n(r.queued_events)),
                ("queued_event_bytes", n(r.queued_event_bytes)),
            ]),
        ),
        (
            "metrics",
            Json::object([
                ("full_scans", n(m.full_scans)),
                ("subtree_scans", n(m.subtree_scans)),
                ("scanned_entries", n(m.scanned_entries)),
                ("metadata_calls", n(m.metadata_calls)),
                ("transactions", n(m.transactions)),
                ("changed_paths", n(m.changed_paths)),
                ("audited_directories", n(m.audited_directories)),
                ("correction_attempts", n(m.correction_attempts)),
                ("compaction_attempts", n(m.compaction_attempts)),
                ("compactions", n(m.compactions)),
                ("compaction_restarts", n(m.compaction_restarts)),
                ("compacted_entries", n(m.compacted_entries)),
                ("reclaimed_slots", n(m.reclaimed_slots)),
                ("reclaimed_name_bytes", n(m.reclaimed_name_bytes)),
                ("scope_checks", n(m.scope_checks)),
                ("last_touched_entries", n(m.last_touched_entries)),
                ("last_copied_entries", n(m.last_copied_entries)),
                ("last_copied_segments", n(m.last_copied_segments)),
            ]),
        ),
    ])
}
fn query50(owner: &MonitorOwner, raw: &str) -> io::Result<Json> {
    let start = Instant::now();
    let lease = owner.query().lease()?;
    let page_start = Instant::now();
    let progress = AtomicUsize::new(0);
    let page = lease.page(raw, None, 50, &AtomicBool::new(false), &progress)?;
    let page_ns = page_start.elapsed().as_nanos();
    let total = start.elapsed().as_nanos();
    Ok(Json::object([
        ("total_public_ns", n(total)),
        ("page_only_ns", n(page_ns)),
        ("progress", n(progress.load(Ordering::Acquire))),
        ("version", n(page.version)),
        ("complete", b(page.complete)),
        ("validated", b(page.validated_at_start_and_finish)),
        ("cancelled", b(page.cancelled)),
        ("status_start", s(format!("{:?}", page.started.status))),
        ("status_finish", s(format!("{:?}", page.finished.status))),
        (
            "paths_hex",
            Json::Array(
                page.paths
                    .iter()
                    .map(|p| s(hex(p.as_os_str().as_bytes())))
                    .collect(),
            ),
        ),
    ]))
}
fn response(id: u64, op: &str, result: io::Result<Json>) -> Json {
    let mut fields = BTreeMap::from([
        ("schema".into(), n(1u64)),
        ("id".into(), n(id)),
        ("op".into(), s(op)),
        ("pid".into(), n(std::process::id())),
    ]);
    match result {
        Ok(value) => {
            fields.insert("ok".into(), b(true));
            if let Json::Object(v) = value {
                fields.extend(v);
            } else {
                fields.insert("result".into(), value);
            }
        }
        Err(e) => {
            fields.insert("ok".into(), b(false));
            fields.insert("error".into(), s(e.to_string()));
        }
    }
    Json::Object(fields)
}
fn send(output: &mut impl Write, value: &Json) -> io::Result<()> {
    let encoded = value.encode();
    if encoded.len() > crate::protocol::MAX_FRAME {
        let error = response(
            value.get("id")?.number()?,
            value.get("op")?.text()?,
            Err(invalid("response exceeds64KiB; use raw export")),
        );
        return frame_write(output, error.encode().as_bytes());
    }
    frame_write(output, encoded.as_bytes())
}
pub fn run(options: Options) -> io::Result<()> {
    process::owned_directory(&options.root)?;
    fs::create_dir_all(&options.output)?;
    process::owned_directory(&options.output)?;
    let budget = crate::budget::Budget::new(
        &options.output,
        options
            .output_budget_bytes
            .saturating_sub(320 * 1024 * 1024),
    )?;
    budget.check(2 * 1024 * 1024)?;
    let baseline = process::sample(std::process::id())?;
    let begun = Instant::now();
    let engine_options = EngineOptions::scale();
    let engine = Engine::open_with_options(
        &options.root,
        Some(&options.database),
        engine_options.clone(),
    )?;
    let open_ns = begun.elapsed().as_nanos();
    let loaded = engine.view();
    let initial = engine
        .query()
        .lease()
        .and_then(|lease| lease.page("", None, 50, &AtomicBool::new(false), &AtomicUsize::new(0)));
    let searchable = initial.as_ref().ok().map(|_| begun.elapsed().as_nanos());
    let mut owner = Some(engine.spawn()?);
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut hello = status(owner.as_ref().unwrap());
    if let Json::Object(ref mut fields) = hello {
        fields.insert("baseline_resources".into(), baseline.clone());
        fields.insert("sha".into(), s(options.sha.clone()));
        fields.insert("open_ns".into(), n(open_ns));
        fields.insert(
            "searchable_stale_ns".into(),
            searchable.map_or(Json::Null, n),
        );
        fields.insert("loaded_status".into(), s(format!("{:?}", loaded.status)));
        fields.insert("loaded_version".into(), n(loaded.version));
        fields.insert("poll_ms".into(), n(20u64));
        fields.insert("trusted_batch_extra_debounce_ms".into(), n(0u64));
        fields.insert("unpaired_rename_expiry_ms".into(), n(100u64));
        fields.insert("output_budget_bytes".into(), n(options.output_budget_bytes));
        fields.insert("live_entry_limit".into(), n(engine_options.limits.entries));
        fields.insert(
            "process_start_ticks".into(),
            baseline.get("process_start_ticks")?.clone(),
        );
    }
    send(&mut output, &response(0, "HELLO", Ok(hello)))?;
    let mut jobs: BTreeMap<u64, Job> = BTreeMap::new();
    let mut leases: BTreeMap<u64, Arc<QueryLease>> = BTreeMap::new();
    let mut next_handle = 1u64;
    let mut last_request = 0u64;
    while let Some(frame) = frame_read(&mut input)? {
        if !frame.is_ascii() {
            return Err(invalid("request body ASCII required"));
        }
        let text = std::str::from_utf8(&frame).map_err(|_| invalid("request UTF8"))?;
        let args: Vec<_> = text.split('\t').collect();
        if args.len() < 2 {
            return Err(invalid("request id and opcode required"));
        }
        let id = integer(args[0])?;
        if id == 0 || id <= last_request {
            return Err(invalid("request id must increase"));
        }
        last_request = id;
        let op = args[1];
        let arg = |index: usize| {
            args.get(index)
                .copied()
                .ok_or_else(|| invalid("opcode argument missing"))
        };
        let mut quit = false;
        let result = (|| -> io::Result<Json> {
            if op == "QUIT" {
                if owner.is_some() {
                    return Err(invalid("STOP beforeQUIT"));
                }
                quit = true;
                return Ok(Json::object([("exiting", b(true))]));
            }
            if op == "STOP" {
                let timeout = integer(arg(2)?)?;
                if timeout > 60000 {
                    return Err(invalid("stop timeout bound"));
                }
                let keep = args.get(3).is_some_and(|v| *v == "1");
                for job in jobs.values() {
                    let _ = job.cancel();
                }
                jobs.clear();
                leases.clear();
                if let Some(active) = owner.as_mut() {
                    active.stop(Duration::from_millis(timeout))?;
                }
                owner.take();
                let post = process::sample(std::process::id())?;
                quit = !keep;
                return Ok(Json::object([
                    ("stopped", b(true)),
                    ("joined", b(true)),
                    ("baseline_resources", baseline.clone()),
                    ("post_resources", post),
                ]));
            }
            let active = owner.as_ref().ok_or_else(|| invalid("owner stopped"))?;
            match op {
                "STATUS" => Ok(status(active)),
                "QUERY50" => query50(active, &query_text(arg(2)?)?),
                "HOLD_LEASE" => {
                    if leases.len() >= MAX_HELD_LEASES {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "heldlease budget",
                        ));
                    }
                    let lease = active.query().lease()?;
                    let version = lease
                        .page("", None, 1, &AtomicBool::new(false), &AtomicUsize::new(0))?
                        .version;
                    let handle = next_handle;
                    next_handle += 1;
                    leases.insert(handle, Arc::new(lease));
                    Ok(Json::object([
                        ("lease_id", n(handle)),
                        ("version", n(version)),
                    ]))
                }
                "RELEASE_LEASE" => {
                    leases
                        .remove(&integer(arg(2)?)?)
                        .ok_or_else(|| invalid("unknownlease"))?;
                    Ok(Json::object([("released", b(true))]))
                }
                "CANCEL_CORRECTION" => {
                    active.cancel()?;
                    Ok(Json::object([("requested", b(true))]))
                }
                "OP_STATE" => {
                    let handle = integer(arg(2)?)?;
                    let job = jobs.get_mut(&handle).ok_or_else(|| invalid("unknownjob"))?;
                    let mut state = job.status();
                    if let Json::Object(ref mut fields) = state {
                        fields.insert("job_id".into(), n(handle));
                    }
                    Ok(state)
                }
                "OP_DROP" => {
                    jobs.remove(&integer(arg(2)?)?)
                        .ok_or_else(|| invalid("unknownjob"))?;
                    Ok(Json::object([("released", b(true))]))
                }
                "CANCEL" => {
                    jobs.get(&integer(arg(2)?)?)
                        .ok_or_else(|| invalid("unknownjob"))?
                        .cancel()?;
                    Ok(Json::object([("requested", b(true))]))
                }
                "SORT_PAGE" => {
                    let handle = integer(arg(2)?)?;
                    let offset = integer(arg(3)?)? as usize;
                    let size = integer(arg(4)?)? as usize;
                    if size == 0 || size > 50 {
                        return Err(invalid("sortpage size1..50"));
                    }
                    let Some(Job::Query { job, .. }) = jobs.get(&handle) else {
                        return Err(invalid("not a queryjob"));
                    };
                    let page = job.page(offset, size)?;
                    Ok(Json::object([
                        ("version", n(page.version)),
                        ("complete", b(page.complete)),
                        (
                            "paths_hex",
                            Json::Array(
                                page.paths
                                    .iter()
                                    .map(|p| s(hex(p.as_os_str().as_bytes())))
                                    .collect(),
                            ),
                        ),
                    ]))
                }
                "SAVE" | "REBUILD" | "COMPACT" | "COUNT_START" | "SORT_START" | "EXPORT"
                | "LEASE_EXPORT" => {
                    if jobs.len() >= MAX_JOBS {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "protocol operation budget;OP_DROP completedjobs",
                        ));
                    }
                    if matches!(op, "EXPORT" | "LEASE_EXPORT")
                        && jobs.values().any(|j| matches!(j, Job::Export { .. }))
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "one foregroundexport budget",
                        ));
                    }
                    let started = Instant::now();
                    let job = match op {
                        "SAVE" => Job::Monitor {
                            request: active.save()?,
                            started,
                            done: None,
                        },
                        "REBUILD" => Job::Monitor {
                            request: active.request_rebuild()?,
                            started,
                            done: None,
                        },
                        "COMPACT" => Job::Monitor {
                            request: active.request_compaction()?,
                            started,
                            done: None,
                        },
                        "COUNT_START" => Job::Query {
                            job: active.query().start_count(&query_text(arg(2)?)?)?,
                            started,
                        },
                        "SORT_START" => Job::Query {
                            job: active.query().start_sort(&query_text(arg(2)?)?)?,
                            started,
                        },
                        "EXPORT" => export(
                            Arc::new(active.query().lease()?),
                            safe_output(&options.output, arg(2)?)?,
                            query_text(arg(3)?)?,
                            Arc::new(AtomicBool::new(false)),
                            Arc::new(AtomicUsize::new(0)),
                            budget.clone(),
                        )?,
                        "LEASE_EXPORT" => {
                            let lease = leases
                                .get(&integer(arg(2)?)?)
                                .ok_or_else(|| invalid("unknownlease"))?
                                .clone();
                            export(
                                lease,
                                safe_output(&options.output, arg(3)?)?,
                                query_text(arg(4)?)?,
                                Arc::new(AtomicBool::new(false)),
                                Arc::new(AtomicUsize::new(0)),
                                budget.clone(),
                            )?
                        }
                        _ => unreachable!(),
                    };
                    let handle = next_handle;
                    next_handle += 1;
                    jobs.insert(handle, job);
                    Ok(Json::object([
                        ("job_id", n(handle)),
                        ("state", s("Pending")),
                    ]))
                }
                _ => Err(invalid("unknownopcode")),
            }
        })();
        send(&mut output, &response(id, op, result))?;
        if quit {
            break;
        }
    }
    jobs.clear();
    leases.clear();
    if let Some(mut active) = owner.take() {
        active.stop(Duration::from_secs(10))?;
    }
    Ok(())
}
