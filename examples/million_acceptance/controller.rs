#[path = "stages.rs"]
mod stages;
use crate::process::{self, DeadlineRead};
use crate::protocol::{b, frame_read, frame_write, hex, invalid, n, s, unhex, Json};
use crate::Options;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAX_OUTPUT_BYTES: u64 = 512 * 1024 * 1024;
struct Worker {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    next: u64,
    identity: u64,
    hello: Json,
}
impl Worker {
    fn launch(options: &Options, label: &str) -> io::Result<Self> {
        let stderr = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(options.output.join(format!("worker-{label}.stderr.log")))?;
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--worker", "--root"])
            .arg(&options.root)
            .arg("--database")
            .arg(&options.database)
            .arg("--output")
            .arg(&options.output)
            .arg("--sha")
            .arg(&options.sha)
            .arg("--output-budget-mib")
            .arg((options.output_budget_bytes / (1024 * 1024)).to_string());
        if options.smoke {
            command.arg("--smoke");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| invalid("workerstdin missing"))?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| invalid("workerstdout missing"))?;
        let hello = (|| -> io::Result<Json> {
            let bytes = frame_read(&mut DeadlineRead {
                stream: &mut output,
                deadline: Instant::now() + Duration::from_secs(300),
            })?
            .ok_or_else(|| invalid("worker died beforeHELLO"))?;
            let hello = Json::parse(&bytes)?;
            if hello.get("id")?.number()? != 0
                || hello.get("op")?.text()? != "HELLO"
                || !hello.get("ok")?.boolean()?
                || hello.get("pid")?.number()? != child.id() as u64
                || hello.get("sha")?.text()? != options.sha.as_str()
            {
                return Err(invalid("workerHELLO mismatch"));
            }
            Ok(hello)
        })();
        match hello {
            Ok(hello) => {
                let identity = hello.get("process_start_ticks")?.number()?;
                Ok(Self {
                    child,
                    input,
                    output,
                    next: 1,
                    identity,
                    hello,
                })
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }
    fn request(&mut self, op: &str, args: &[String], timeout: Duration) -> io::Result<Json> {
        if process::identity(self.child.id())? != self.identity {
            return Err(invalid("workerPID identity changed"));
        }
        let id = self.next;
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| invalid("requestID exhausted"))?;
        let mut request = format!("{id}\t{op}");
        for a in args {
            if !a.is_ascii() || a.contains('\t') || a.contains('\n') {
                return Err(invalid("invalidrequestargument"));
            }
            request.push('\t');
            request.push_str(a);
        }
        frame_write(&mut self.input, request.as_bytes())?;
        let bytes = frame_read(&mut DeadlineRead {
            stream: &mut self.output,
            deadline: Instant::now() + timeout,
        })?
        .ok_or_else(|| invalid("workerEOF"))?;
        let reply = Json::parse(&bytes)?;
        if reply.get("id")?.number()? != id
            || reply.get("op")?.text()? != op
            || reply.get("pid")?.number()? != self.child.id() as u64
        {
            return Err(invalid("workerreply mismatch"));
        }
        if !reply.get("ok")?.boolean()? {
            return Err(io::Error::other(reply.get("error")?.text()?.to_owned()));
        }
        Ok(reply)
    }
    fn status(&mut self) -> io::Result<Json> {
        self.request("STATUS", &[], Duration::from_secs(5))
    }
    fn query(&mut self, query: &str) -> io::Result<Json> {
        self.request("QUERY50", &[hex(query.as_bytes())], Duration::from_secs(30))
    }
    fn wait_validated(&mut self, timeout: Duration) -> io::Result<Json> {
        let deadline = Instant::now() + timeout;
        let mut last_print = Instant::now();
        loop {
            let reply = self.status()?;
            let status = reply.get("status")?.text()?;
            if status == "Validated" && reply.get("gaps")?.array()?.is_empty() {
                return Ok(reply);
            }
            if matches!(status, "Failed" | "Stopped") {
                return Err(io::Error::other(format!(
                    "owner coverage: {}",
                    reply.encode()
                )));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "coverage deadline"));
            }
            if last_print.elapsed() >= Duration::from_secs(30) {
                eprintln!("awaiting coverage: {}", reply.encode());
                last_print = Instant::now();
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    fn wait_job(&mut self, id: u64, timeout: Duration) -> io::Result<Json> {
        let deadline = Instant::now() + timeout;
        loop {
            let reply = self.request("OP_STATE", &[id.to_string()], Duration::from_secs(5))?;
            match reply.get("state")?.text()? {
                "Complete" => return Ok(reply),
                "Failed" | "Cancelled" => {
                    return Err(io::Error::other(format!("operation:{}", reply.encode())))
                }
                "Pending" | "Running" => (),
                _ => return Err(invalid("operation state")),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "operationdeadline"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn operation(&mut self, op: &str, args: &[String]) -> io::Result<Json> {
        let admitted = self.request(op, args, Duration::from_secs(10))?;
        let id = admitted.get("job_id")?.number()?;
        let result = self.wait_job(id, Duration::from_secs(3600));
        let dropped = self.request("OP_DROP", &[id.to_string()], Duration::from_secs(60));
        match result {
            Ok(v) => {
                dropped?;
                Ok(v)
            }
            Err(e) => {
                let _ = dropped;
                Err(e)
            }
        }
    }
    fn stop(&mut self) -> io::Result<Json> {
        let reply = self.request(
            "STOP",
            &["10000".into(), "1".into()],
            Duration::from_secs(15),
        )?;
        let post = process::sample(self.child.id())?;
        if !reply.get("joined")?.boolean()? {
            return Err(invalid("owner did notjoin"));
        }
        self.request("QUIT", &[], Duration::from_secs(5))?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(exit) = self.child.try_wait()? {
                if !exit.success() {
                    return Err(invalid("workerexit failed"));
                }
                break;
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                self.child.wait()?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "workerexitdeadline",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(Json::object([
            ("stop_reply", reply),
            ("independent_post_resources", post),
        ]))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
struct Sampler {
    stop: Arc<AtomicBool>,
    phase: Arc<Mutex<String>>,
    thread: Option<JoinHandle<io::Result<()>>>,
}
impl Sampler {
    fn start(worker: &Worker, output: &Path, sha: &str) -> io::Result<Self> {
        let mut file = BufWriter::new(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output.join("resources.jsonl"))?,
        );
        let pid = worker.child.id();
        let identity = worker.identity;
        let sha = sha.to_owned();
        let began = Instant::now();
        let stop = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(Mutex::new(String::from("build")));
        let worker_stop = stop.clone();
        let worker_phase = phase.clone();
        let thread = thread::Builder::new()
            .name("acceptance-sampler".into())
            .spawn(move || {
                let mut written = 0;
                loop {
                    let row = process::sample(pid)?;
                    if row.get("process_start_ticks")?.number()? != identity {
                        return Err(invalid("sampledPID reused"));
                    }
                    let record = Json::object([
                        ("schema", n(1u64)),
                        ("sha", s(sha.clone())),
                        ("phase", s(worker_phase.lock().unwrap().clone())),
                        ("elapsed_ns", n(began.elapsed().as_nanos())),
                        ("sample", row),
                    ]);
                    let line = record.encode();
                    written += line.len() + 1;
                    if written > 256 * 1024 * 1024 {
                        return Err(invalid("resource logbudget"));
                    }
                    writeln!(file, "{line}")?;
                    file.flush()?;
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    for _ in 0..10 {
                        if worker_stop.load(Ordering::Acquire) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                }
                Ok(())
            })?;
        Ok(Self {
            stop,
            phase,
            thread: Some(thread),
        })
    }
    fn phase(&self, phase: &str) {
        *self.phase.lock().unwrap() = phase.into();
    }
    fn finish(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        self.thread.take().map_or(Ok(()), |t| {
            t.join().map_err(|_| invalid("samplerpanicked"))?
        })
    }
}
impl Drop for Sampler {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
struct Log {
    writer: BufWriter<File>,
    written: usize,
    sha: String,
    began: Instant,
    retained_latest: Vec<PathBuf>,
    retained_initial: Vec<PathBuf>,
}
impl Log {
    fn new(output: &Path, sha: &str) -> io::Result<Self> {
        Ok(Self {
            writer: BufWriter::new(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(output.join("timings.jsonl"))?,
            ),
            written: 0,
            sha: sha.into(),
            began: Instant::now(),
            retained_latest: Vec::new(),
            retained_initial: Vec::new(),
        })
    }
    fn record(&mut self, phase: &str, value: Json) -> io::Result<()> {
        let row = Json::object([
            ("schema", n(1u64)),
            ("sha", s(self.sha.clone())),
            ("phase", s(phase)),
            ("elapsed_ns", n(self.began.elapsed().as_nanos())),
            ("row", value),
        ]);
        let line = row.encode();
        self.written += line.len() + 1;
        if self.written > 64 * 1024 * 1024 {
            return Err(invalid("timinglogbudget"));
        }
        writeln!(self.writer, "{line}")?;
        self.writer.flush()
    }
}
fn external_sort(
    source: &Path,
    target: &Path,
    output: &Path,
    zero: bool,
    budget: &crate::budget::Budget,
) -> io::Result<()> {
    budget.check(
        fs::metadata(source)?
            .len()
            .checked_add(2 * 1024 * 1024)
            .ok_or_else(|| invalid("sortbudget overflow"))?,
    )?;
    let mut command = Command::new("sort");
    if zero {
        command.arg("--zero-terminated");
    }
    let mut child = command
        .arg("--buffer-size=64M")
        .arg(format!("--temporary-directory={}", output.display()))
        .arg("--output")
        .arg(target)
        .arg(source)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if let Some(exit) = child.try_wait()? {
            if !exit.success() {
                return Err(invalid("independentsort failed"));
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "independentsortdeadline",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}
fn remove_owned_artifact(output: &Path, path: &Path) -> io::Result<()> {
    if path.parent() != Some(output) {
        return Err(invalid("artifactcleanup escapedownedroot"));
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.uid() != process::uid() {
        return Err(invalid("artifactcleanup foreign/symlink"));
    }
    fs::remove_file(path)
}
fn file_digest(path: &Path) -> io::Result<String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(invalid("artifactsha256 commandfailed"));
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| invalid("artifactsha256 output"))?;
    let digest = text
        .split_whitespace()
        .next()
        .ok_or_else(|| invalid("artifactsha256 missing"))?;
    if digest.len() != 64 || !digest.bytes().all(|x| x.is_ascii_hexdigit()) {
        return Err(invalid("artifactsha256 format"));
    }
    Ok(digest.into())
}
fn equal_files(a: &Path, b: &Path) -> io::Result<()> {
    let mut a = BufReader::new(File::open(a)?);
    let mut b = BufReader::new(File::open(b)?);
    let mut offset = 0u64;
    loop {
        let x = a.fill_buf()?;
        let y = b.fill_buf()?;
        let len = x.len().min(y.len());
        if x[..len] != y[..len] {
            return Err(io::Error::other(format!(
                "completebyteoracle mismatch offset{offset}"
            )));
        }
        if len == 0 {
            if x.is_empty() && y.is_empty() {
                return Ok(());
            }
            return Err(invalid("completebyteoracle length mismatch"));
        }
        a.consume(len);
        b.consume(len);
        offset += len as u64;
    }
}
fn oracle(
    root: &Path,
    output: &Path,
    label: &str,
    budget: &crate::budget::Budget,
) -> io::Result<(PathBuf, PathBuf, u64, u64)> {
    budget.check(2 * 1024 * 1024)?;
    let source = output.join(format!("oracle-{label}-unsorted.nul"));
    let kinds_source = output.join(format!("oracle-{label}-unsorted.kinds"));
    let mut paths = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&source)?,
    );
    let mut kinds = BufWriter::new(
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&kinds_source)?,
    );
    let root_dev = fs::symlink_metadata(root)?.dev();
    let mut todo = vec![root.to_path_buf()];
    let mut count = 0;
    let mut dirs = 0;
    let mut bytes = 0u64;
    let mut checkpoint_bytes = 0u64;
    let mut kinds_bytes = 0u64;
    while let Some(directory) = todo.pop() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            let meta = fs::symlink_metadata(&path)?;
            let kind = if meta.is_dir() {
                "D"
            } else if meta.is_file() {
                "F"
            } else if meta.file_type().is_symlink() {
                "L"
            } else {
                return Err(invalid("oracleunsupported entrykind"));
            };
            if meta.uid() != process::uid() {
                return Err(invalid("fixtureforeignowner"));
            }
            if meta.dev() != root_dev {
                return Err(invalid(
                    "oraclecross-device scope; explicitmountpolicy needed",
                ));
            }
            let raw = path.as_os_str().as_bytes();
            if raw.len() > 4096 {
                return Err(invalid("oraclepathbudget"));
            }
            bytes += raw.len() as u64 + 1;
            if bytes > MAX_OUTPUT_BYTES {
                return Err(invalid("oracleoutputbudget"));
            }
            count += 1;
            kinds_bytes += raw.len() as u64 * 2 + 3;
            if kinds_bytes > MAX_OUTPUT_BYTES {
                return Err(invalid("oraclekindfilebudget"));
            }
            if bytes + kinds_bytes - checkpoint_bytes >= 1024 * 1024 {
                budget.check(2 * 1024 * 1024)?;
                checkpoint_bytes = bytes + kinds_bytes;
            }
            paths.write_all(raw)?;
            paths.write_all(&[0])?;
            writeln!(kinds, "{}\t{}", hex(raw), kind)?;
            if meta.is_dir() {
                dirs += 1;
                todo.push(path);
            }
        }
    }
    paths.flush()?;
    kinds.flush()?;
    drop(paths);
    drop(kinds);
    let sorted = output.join(format!("oracle-{label}.nul"));
    let kinds_sorted = output.join(format!("oracle-{label}.kinds"));
    external_sort(&source, &sorted, output, true, budget)?;
    external_sort(&kinds_source, &kinds_sorted, output, false, budget)?;
    fs::remove_file(source)?;
    fs::remove_file(kinds_source)?;
    Ok((sorted, kinds_sorted, count, dirs))
}
fn correctness(
    worker: &mut Worker,
    options: &Options,
    label: &str,
    log: &mut Log,
) -> io::Result<u64> {
    worker.wait_validated(Duration::from_secs(3600))?;
    for path in log.retained_latest.drain(..) {
        remove_owned_artifact(&options.output, &path)?;
    }
    let budget = crate::budget::Budget::with_checkpoint(
        &options.output,
        options
            .output_budget_bytes
            .saturating_sub(320 * 1024 * 1024),
        &options.database,
    )?;
    budget.check(2 * 1024 * 1024)?;
    let began = Instant::now();
    let export = worker.operation(
        "EXPORT",
        &[hex(format!("export-{label}.nul").as_bytes()), String::new()],
    )?;
    if !export.get("complete")?.boolean()? || !export.get("validated_start_finish")?.boolean()? {
        return Err(invalid(
            "fresh full export did not retain validated stablecut",
        ));
    }
    let export_path = PathBuf::from(std::ffi::OsString::from_vec(unhex(
        export.get("path_hex")?.text()?,
    )?));
    let export_kinds = PathBuf::from(std::ffi::OsString::from_vec(unhex(
        export.get("kinds_path_hex")?.text()?,
    )?));
    let export_sorted = options.output.join(format!("export-{label}-sorted.nul"));
    let export_kinds_sorted = options.output.join(format!("export-{label}-sorted.kinds"));
    external_sort(&export_path, &export_sorted, &options.output, true, &budget)?;
    external_sort(
        &export_kinds,
        &export_kinds_sorted,
        &options.output,
        false,
        &budget,
    )?;
    let (oracle, oracle_kinds, count, dirs) =
        oracle(&options.root, &options.output, label, &budget)?;
    equal_files(&export_sorted, &oracle)?;
    equal_files(&export_kinds_sorted, &oracle_kinds)?;
    if count != export.get("count")?.number()? {
        return Err(invalid("oracle/export multiplicity mismatch"));
    }
    let final_status = worker.status()?;
    if final_status.get("status")?.text()? != "Validated"
        || final_status.get("version")?.number()? != export.get("version")?.number()?
    {
        return Err(invalid("oracle stablecut changed during comparison"));
    }
    let canonical = vec![
        export_sorted.clone(),
        export_kinds_sorted.clone(),
        oracle.clone(),
        oracle_kinds.clone(),
    ];
    let mut hashes = Vec::new();
    for path in &canonical {
        hashes.push(Json::object([
            (
                "name",
                s(path
                    .file_name()
                    .ok_or_else(|| invalid("artifactbasename"))?
                    .to_string_lossy()
                    .into_owned()),
            ),
            ("bytes", n(fs::metadata(path)?.len())),
            ("sha256", s(file_digest(path)?)),
        ]));
    }
    log.record(
        "correctness-artifacts",
        Json::object([
            ("stage", s(label)),
            ("files", Json::Array(hashes)),
            ("aggregate", budget.check(0)?),
        ]),
    )?;
    remove_owned_artifact(&options.output, &export_path)?;
    remove_owned_artifact(&options.output, &export_kinds)?;
    if label == "initial" {
        log.retained_initial = canonical;
    } else {
        log.retained_latest = canonical;
    }
    log.record(
        "correctness",
        Json::object([
            ("stage", s(label)),
            ("count", n(count)),
            ("directories", n(dirs)),
            ("elapsed_ns", n(began.elapsed().as_nanos())),
            ("full_byte_set_equal", b(true)),
            ("full_kind_set_equal", b(true)),
            ("version", export.get("version")?.clone()),
        ]),
    )?;
    Ok(count)
}
fn percentile(values: &[u64], percent: usize) -> u64 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted[(sorted.len() * percent).div_ceil(100) - 1]
}
fn query_bench(
    worker: &mut Worker,
    queries: &[String],
    options: &Options,
    log: &mut Log,
) -> io::Result<()> {
    for q in queries {
        for repetition in 0..5 {
            let result = worker.query(q)?;
            log.record(
                "query-warmup",
                Json::object([
                    ("query", s(q.clone())),
                    ("repetition", n(repetition as u64)),
                    ("result", result),
                ]),
            )?;
        }
    }
    let passes = if options.smoke { 1 } else { 2 };
    for pass in 0..passes {
        let mut timings = vec![Vec::new(); queries.len()];
        for repetition in 0..options.repetitions {
            for step in 0..queries.len() {
                let index = (step + repetition * 7 + pass * 11) % queries.len();
                let q = &queries[index];
                let start = Instant::now();
                let result = worker.query(q);
                let roundtrip = start.elapsed().as_nanos();
                match result {
                    Ok(result) => {
                        let elapsed = result.get("total_public_ns")?.number()?;
                        let reply_summary = Json::object([
                            ("total_public_ns", n(elapsed)),
                            ("page_only_ns", result.get("page_only_ns")?.clone()),
                            ("version", result.get("version")?.clone()),
                            ("returned_paths", n(result.get("paths_hex")?.array()?.len())),
                            ("complete", result.get("complete")?.clone()),
                            ("validated", result.get("validated")?.clone()),
                            ("progress", result.get("progress")?.clone()),
                        ]);
                        timings[index].push(elapsed);
                        log.record(
                            "query",
                            Json::object([
                                ("query_id", n(index)),
                                ("query", s(q.clone())),
                                ("pass", n(pass as u64)),
                                ("repetition", n(repetition)),
                                ("roundtrip_ns", n(roundtrip)),
                                ("success", b(true)),
                                ("result", reply_summary),
                            ]),
                        )?;
                    }
                    Err(e) => {
                        log.record(
                            "query",
                            Json::object([
                                ("query_id", n(index)),
                                ("query", s(q.clone())),
                                ("pass", n(pass as u64)),
                                ("repetition", n(repetition)),
                                ("success", b(false)),
                                ("error", s(e.to_string())),
                            ]),
                        )?;
                        return Err(e);
                    }
                }
            }
        }
        for (index, values) in timings.iter().enumerate() {
            let p95 = percentile(values, 95);
            log.record(
                "query-summary",
                Json::object([
                    ("query", s(queries[index].clone())),
                    ("pass", n(pass as u64)),
                    ("samples", n(values.len())),
                    ("p50_ns", n(percentile(values, 50))),
                    ("p95_ns", n(p95)),
                    ("p99_ns", n(percentile(values, 99))),
                    ("limit_ns", n(50_000_000u64)),
                    ("numeric_pass", b(p95 <= 50_000_000)),
                    ("acceptance", b(false)),
                ]),
            )?;
        }
    }
    Ok(())
}
fn select_regular(root: &Path) -> io::Result<PathBuf> {
    let mut todo = vec![root.to_path_buf()];
    while let Some(dir) = todo.pop() {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let m = fs::symlink_metadata(&path)?;
            if m.uid() != process::uid() {
                return Err(invalid("foreignfixtureowner"));
            }
            if m.is_dir() {
                todo.push(path);
            } else if m.is_file()
                && m.len() == 0
                && path.file_name().is_some_and(|x| x.to_str().is_some())
            {
                return Ok(path);
            }
        }
    }
    Err(invalid("fixture has no ownedzero-byte UTF8 regularfile"))
}
fn visible(
    worker: &mut Worker,
    query: &str,
    path: &Path,
    want: bool,
    deadline: Instant,
) -> io::Result<Json> {
    let target = hex(path.as_os_str().as_bytes());
    loop {
        let result = worker.query(query)?;
        let found = result
            .get("paths_hex")?
            .array()?
            .iter()
            .any(|p| p.text().is_ok_and(|v| v == target.as_str()));
        if found == want
            && result.get("validated")?.boolean()?
            && !result.get("cancelled")?.boolean()?
        {
            return Ok(result);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "filesystemmutation visibility deadline",
            ));
        }
        thread::sleep(Duration::from_millis(2));
    }
}
struct RestoreFile {
    original: PathBuf,
    temporary: PathBuf,
    mode: u32,
    active: bool,
}
impl RestoreFile {
    fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        if !self.original.exists() {
            if self.temporary.exists() {
                fs::rename(&self.temporary, &self.original)?;
            } else {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(self.mode)
                    .custom_flags(0x20000)
                    .open(&self.original)?;
            }
        } else if self.temporary.exists() {
            let m = fs::symlink_metadata(&self.temporary)?;
            if !m.is_file() || m.len() != 0 || m.uid() != process::uid() {
                return Err(invalid("temporaryfixtureentry changed"));
            }
            fs::remove_file(&self.temporary)?;
        }
        self.active = false;
        Ok(())
    }
}
impl Drop for RestoreFile {
    fn drop(&mut self) {
        if let Err(e) = self.restore() {
            eprintln!("ownedfixture restore failed: {e}");
        }
    }
}
fn event_bench(
    worker: &mut Worker,
    options: &Options,
    log: &mut Log,
    baseline: u64,
) -> io::Result<()> {
    let original = select_regular(&options.root)?;
    let meta = fs::symlink_metadata(&original)?;
    let basename = original.file_name().unwrap().to_str().unwrap().to_owned();
    let parent = original.parent().unwrap();
    for class in ["add", "delete", "file-rename"] {
        let mut samples = Vec::new();
        for index in 0..options.repetitions + 20 {
            let warmup = index < 20;
            let temporary = parent.join(format!(
                "loci_acceptance_{}_{}_{index:08}.tmp",
                std::process::id(),
                class
            ));
            if temporary.exists() {
                return Err(invalid("eventtemporaryentry exists"));
            }
            let mut restore = RestoreFile {
                original: original.clone(),
                temporary: temporary.clone(),
                mode: meta.mode() & 0o777,
                active: true,
            };
            visible(
                worker,
                &basename,
                &original,
                true,
                Instant::now() + Duration::from_secs(10),
            )?;
            let began = Instant::now();
            match class {
                "add" => {
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .custom_flags(0x20000)
                        .open(&temporary)?;
                }
                "delete" => fs::remove_file(&original)?,
                "file-rename" => fs::rename(&original, &temporary)?,
                _ => unreachable!(),
            }
            let syscall_ns = began.elapsed().as_nanos();
            let target_query = temporary.file_name().unwrap().to_str().unwrap().to_owned();
            let observation = match class {
                "add" => visible(
                    worker,
                    &target_query,
                    &temporary,
                    true,
                    Instant::now() + Duration::from_secs(10),
                ),
                "delete" => visible(
                    worker,
                    &basename,
                    &original,
                    false,
                    Instant::now() + Duration::from_secs(10),
                ),
                "file-rename" => {
                    let result = visible(
                        worker,
                        &target_query,
                        &temporary,
                        true,
                        Instant::now() + Duration::from_secs(10),
                    );
                    match result {
                        Ok(result) => {
                            visible(
                                worker,
                                &basename,
                                &original,
                                false,
                                Instant::now() + Duration::from_secs(10),
                            )?;
                            Ok(result)
                        }
                        Err(e) => Err(e),
                    }
                }
                _ => unreachable!(),
            };
            let elapsed = began.elapsed().as_nanos();
            match observation {
                Ok(result) => {
                    if !warmup {
                        samples.push(
                            u64::try_from(elapsed).map_err(|_| invalid("eventtimer overflow"))?,
                        );
                    }
                    log.record(
                        "event",
                        Json::object([
                            ("class", s(class)),
                            ("warmup", b(warmup)),
                            ("repetition", n(index.saturating_sub(20))),
                            ("syscall_ns", n(syscall_ns)),
                            ("mutation_to_visible_ns", n(elapsed)),
                            (
                                "relative_path_hex",
                                s(hex(temporary
                                    .strip_prefix(&options.root)
                                    .unwrap()
                                    .as_os_str()
                                    .as_bytes())),
                            ),
                            ("observation_cadence_ms", n(2u64)),
                            ("baseline_entries", n(baseline)),
                            ("success", b(true)),
                            ("result", result),
                        ]),
                    )?;
                }
                Err(e) => {
                    log.record(
                        "event",
                        Json::object([
                            ("class", s(class)),
                            ("warmup", b(warmup)),
                            ("repetition", n(index.saturating_sub(20))),
                            ("syscall_ns", n(syscall_ns)),
                            ("mutation_to_visible_ns", n(elapsed)),
                            ("success", b(false)),
                            ("error", s(e.to_string())),
                        ]),
                    )?;
                    return Err(e);
                }
            }
            restore.restore()?;
            visible(
                worker,
                &basename,
                &original,
                true,
                Instant::now() + Duration::from_secs(10),
            )?;
            if class != "delete" {
                visible(
                    worker,
                    &target_query,
                    &temporary,
                    false,
                    Instant::now() + Duration::from_secs(10),
                )?;
            }
            worker.wait_validated(Duration::from_secs(10))?;
        }
        let p95 = percentile(&samples, 95);
        log.record(
            "event-summary",
            Json::object([
                ("class", s(class)),
                ("samples", n(samples.len())),
                ("p50_ns", n(percentile(&samples, 50))),
                ("p95_ns", n(p95)),
                ("p99_ns", n(percentile(&samples, 99))),
                ("limit_ns", n(500_000_000u64)),
                ("numeric_pass", b(p95 <= 500_000_000)),
                ("acceptance", b(false)),
            ]),
        )?;
        correctness(worker, options, &format!("event-{class}"), log)?;
    }
    Ok(())
}
fn idle(worker: &mut Worker, options: &Options, log: &mut Log) -> io::Result<()> {
    worker.wait_validated(Duration::from_secs(3600))?;
    let status_begin = worker.status()?;
    let first = process::sample(worker.child.id())?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(options.idle_seconds) {
        thread::sleep(Duration::from_millis(100));
    }
    let last = process::sample(worker.child.id())?;
    let elapsed = start.elapsed();
    let status_end = worker.status()?;
    let ticks = last
        .get("cpu_ticks")?
        .number()?
        .checked_sub(first.get("cpu_ticks")?.number()?)
        .ok_or_else(|| invalid("CPUcounter decreased"))?;
    let cpu = 100.0 * ticks as f64
        / first.get("clock_ticks_per_second")?.number()? as f64
        / elapsed.as_secs_f64();
    let rss = last.get("vmrss_bytes")?.number()?;
    let hwm = last.get("vmhwm_bytes")?.number()?;
    log.record(
        "idle-summary",
        Json::object([
            ("actual_ns", n(elapsed.as_nanos())),
            ("requested_seconds", n(options.idle_seconds)),
            ("cpu_percent_one_core", Json::Number(format!("{cpu:.9}"))),
            ("cpu_numeric_pass", b(cpu <= 1.0)),
            ("rss_numeric_pass", b(rss <= 200 * 1024 * 1024)),
            ("hwm_numeric_pass", b(hwm <= 512 * 1024 * 1024)),
            ("first_sample", first),
            ("last_sample", last),
            ("status_begin", status_begin),
            ("status_end", status_end),
            ("acceptance", b(false)),
        ]),
    )
}
fn restart_bench(options: &Options, queries: &[String], log: &mut Log) -> io::Result<()> {
    let count = if options.smoke {
        options.repetitions
    } else {
        options.repetitions.max(200)
    };
    let mut times = Vec::new();
    for index in 0..count + 5 {
        let start = Instant::now();
        let mut worker = Worker::launch(options, &format!("restart-{index:04}"))?;
        let stale = worker.hello.get("searchable_stale_ns")?.clone();
        let loaded = worker.hello.get("loaded_status")?.text()?.to_owned();
        if loaded != "Pending" || matches!(stale, Json::Null) {
            return Err(invalid(
                "checkpoint was not searchable stale beforeproductionowner",
            ));
        }
        let first_query = worker.query(
            queries
                .first()
                .ok_or_else(|| invalid("restartquerysuite empty"))?,
        )?;
        let external_ns = start.elapsed().as_nanos();
        let value = stale.number()?;
        if index >= 5 {
            times.push(value);
        }
        log.record(
            "restart-stale",
            Json::object([
                ("sample", n(index)),
                ("warmup", b(index < 5)),
                ("worker_public_open_search_ns", stale),
                ("spawn_to_first_reply_ns", n(external_ns)),
                ("hello", worker.hello.clone()),
                ("first_query", first_query),
                ("resources", process::sample(worker.child.id())?),
                (
                    "cache_state",
                    s("cold-process,warm-or-unspecified-filesystem-cache"),
                ),
            ]),
        )?;
        if index < 3 {
            let correction_start = Instant::now();
            let status = worker.wait_validated(Duration::from_secs(3600))?;
            log.record(
                "restart-current",
                Json::object([
                    ("sample", n(index)),
                    ("full_current_ns", n(correction_start.elapsed().as_nanos())),
                    ("status", status),
                ]),
            )?;
            correctness(&mut worker, options, &format!("restart-{index:04}"), log)?;
        }
        log.record("restart-stop", worker.stop()?)?;
    }
    let p95 = percentile(&times, 95);
    log.record(
        "restart-summary",
        Json::object([
            ("samples", n(times.len())),
            ("p50_ns", n(percentile(&times, 50))),
            ("p95_ns", n(p95)),
            ("p99_ns", n(percentile(&times, 99))),
            ("numeric_pass", b(p95 <= 2_000_000_000)),
            ("limit_ns", n(2_000_000_000u64)),
            ("full_correction_separate", b(true)),
            ("genuine_storage_cold_verified", b(false)),
            ("acceptance", b(false)),
        ]),
    )
}
fn fixture_copy(options: &Options, log: &mut Log) -> io::Result<()> {
    let input = options
        .fixture_input
        .as_ref()
        .ok_or_else(|| invalid("fixture-input required forfixture phase"))?;
    if !input.is_absolute() {
        return Err(invalid("fixtureinput absolute"));
    }
    let preflight = process::preflight(
        options
            .root
            .parent()
            .ok_or_else(|| invalid("fixtureparent"))?,
    )?;
    if preflight.get("dynamic_inode_counter")?.boolean()? {
        return Err(invalid("Btrfs fixture creation needs physical metadata DUP pilot/unallocated-budget integration; dynamic inode counters are not exhaustion"));
    }
    if options.root.exists() {
        return Err(invalid(
            "fixture destination must notexist;reuse existingroots directly",
        ));
    }
    fs::create_dir(&options.root)?;
    let mut todo = vec![(input.clone(), options.root.clone())];
    let mut count = 0;
    let start = Instant::now();
    while let Some((source, dest)) = todo.pop() {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let meta = fs::symlink_metadata(entry.path())?;
            let target = dest.join(entry.file_name());
            if meta.is_dir() {
                fs::create_dir(&target)?;
                todo.push((entry.path(), target));
            } else if meta.is_file() && meta.len() == 0 {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(0x20000)
                    .open(&target)?;
            } else {
                return Err(invalid(
                    "fixturecopy onlyreal dirs/zero-bytefiles;nonregularsource denied",
                ));
            }
            count += 1;
            if count % 10000 == 0 {
                process::preflight(&options.root)?;
                eprintln!("copied actualentries{count}");
            }
        }
    }
    log.record(
        "fixture-copy",
        Json::object([
            ("entries", n(count)),
            ("elapsed_ns", n(start.elapsed().as_nanos())),
            ("preflight", preflight),
            ("engine_acceptance", b(false)),
        ]),
    )
}
pub fn run(options: Options) -> io::Result<()> {
    if !matches!(
        options.phase.as_str(),
        "all"
            | "smoke"
            | "correctness"
            | "queries"
            | "events"
            | "restart"
            | "idle"
            | "fixture"
            | "directory-heavy"
            | "stages"
    ) {
        return Err(invalid("unknownphase"));
    }
    if !options.output.exists() {
        fs::create_dir_all(&options.output)?;
    }
    process::owned_directory(&options.output)?;
    let aggregate = crate::budget::Budget::with_checkpoint(
        &options.output,
        options
            .output_budget_bytes
            .saturating_sub(320 * 1024 * 1024),
        &options.database,
    )?;
    aggregate.check(2 * 1024 * 1024)?;
    let marker = options.output.join("driver-run-owner.json");
    let mut owned = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)?;
    let manifest = Json::object([
        ("schema", n(1u64)),
        ("sha", s(options.sha.clone())),
        ("controller_pid", n(std::process::id())),
        ("binary_sha256", s(file_digest(&std::env::current_exe()?)?)),
        (
            "binary_path_hex",
            s(hex(std::env::current_exe()?.as_os_str().as_bytes())),
        ),
        (
            "query_file_sha256",
            options
                .queries
                .as_ref()
                .map_or(Ok(Json::Null), |p| file_digest(p).map(s))?,
        ),
        ("idle_seconds", n(options.idle_seconds)),
        ("environment", process::environment()?),
        ("root_hex", s(hex(options.root.as_os_str().as_bytes()))),
        (
            "database_hex",
            s(hex(options.database.as_os_str().as_bytes())),
        ),
        ("phase", s(options.phase.clone())),
        ("smoke", b(options.smoke)),
        ("repetitions", n(options.repetitions)),
        ("output_budget_bytes", n(options.output_budget_bytes)),
        ("reference_hardware_verified", b(false)),
        ("engine_acceptance", b(false)),
    ]);
    writeln!(owned, "{}", manifest.encode())?;
    let mut log = Log::new(&options.output, &options.sha)?;
    log.record("manifest", manifest)?;
    if options.phase == "fixture" {
        return fixture_copy(&options, &mut log);
    }
    process::owned_directory(&options.root)?;
    log.record("preflight", process::preflight(&options.root)?)?;
    let queries = if let Some(file) = &options.queries {
        let text = fs::read_to_string(file)?;
        if text.len() > 256 * 1024 {
            return Err(invalid("queryinputbound"));
        }
        text.lines().map(str::to_owned).collect::<Vec<_>>()
    } else {
        vec![
            "a".into(),
            "report".into(),
            "ii".into(),
            "ia".into(),
            "rr".into(),
            "ext:rs".into(),
            "报告".into(),
            "no_such_file_zzz".into(),
        ]
    };
    if queries.is_empty() || queries.len() > 128 || queries.iter().any(|q| q.len() > 4096) {
        return Err(invalid("querysuite bound"));
    }
    if !options.smoke && queries.len() < 39 {
        return Err(invalid(
            "acceptance requires39+diversequeries preserving original32/shortnegative cases",
        ));
    }
    let build = Instant::now();
    let mut worker = Worker::launch(&options, "primary")?;
    log.record("worker-hello", worker.hello.clone())?;
    let mut sampler = Sampler::start(&worker, &options.output, &options.sha)?;
    let validated = worker.wait_validated(Duration::from_secs(3600))?;
    log.record(
        "build",
        Json::object([
            ("elapsed_ns", n(build.elapsed().as_nanos())),
            ("status", validated),
            ("resources", process::sample(worker.child.id())?),
        ]),
    )?;
    if options.phase == "smoke" {
        sampler.phase("million-smoke");
        for q in &queries {
            log.record(
                "smoke-query",
                Json::object([
                    ("query", s(q.clone())),
                    ("result", worker.query(q)?),
                    ("resources", process::sample(worker.child.id())?),
                ]),
            )?;
        }
        log.record("smoke-status", worker.status()?)?;
        sampler.finish()?;
        log.record("stop", worker.stop()?)?;
        return Ok(());
    }
    let initial = correctness(&mut worker, &options, "initial", &mut log)?;
    if options.phase == "directory-heavy" && !options.smoke && initial != 30_501 {
        return Err(invalid("directory-heavy fixture requires30,501entries"));
    }
    if !options.smoke && options.phase != "directory-heavy" && initial != 1_000_000 {
        return Err(invalid("million baseline must beexact1,000,000realentries"));
    }
    if matches!(options.phase.as_str(), "all" | "queries") {
        sampler.phase("query");
        query_bench(&mut worker, &queries, &options, &mut log)?;
    }
    if matches!(options.phase.as_str(), "all" | "events") {
        sampler.phase("event");
        event_bench(&mut worker, &options, &mut log, initial)?;
    }
    if matches!(options.phase.as_str(), "all" | "stages" | "directory-heavy") {
        sampler.phase("subtree");
        stages::subtree(
            &mut worker,
            &options,
            &mut log,
            options.phase == "directory-heavy",
        )?;
        sampler.phase("compaction");
        stages::compaction(&mut worker, &options, &mut log)?;
        sampler.phase("cancellation");
        stages::cancellations(&mut worker, &mut log)?;
        sampler.phase("correction");
        stages::corrections(&mut worker, &options, &mut log)?;
    }
    if matches!(options.phase.as_str(), "all" | "idle") {
        sampler.phase("quiet");
        idle(&mut worker, &options, &mut log)?;
    }
    sampler.phase("save");
    let save_reps = if options.phase == "all" {
        options.repetitions
    } else {
        1
    };
    let mut save_times = Vec::new();
    for index in 0..save_reps {
        let start = Instant::now();
        let saved = worker.operation("SAVE", &[])?;
        save_times.push(start.elapsed().as_nanos() as u64);
        log.record(
            "save",
            Json::object([
                ("repetition", n(index)),
                ("elapsed_ns", n(start.elapsed().as_nanos())),
                ("result", saved),
            ]),
        )?;
    }
    log.record(
        "save-summary",
        Json::object([
            ("samples", n(save_times.len())),
            ("p50_ns", n(percentile(&save_times, 50))),
            ("p95_ns", n(percentile(&save_times, 95))),
            ("p99_ns", n(percentile(&save_times, 99))),
            ("cache_state", s("warm-or-unspecified-filesystem-cache")),
            ("acceptance", b(false)),
        ]),
    )?;
    sampler.finish()?;
    log.record("stop", worker.stop()?)?;
    if matches!(options.phase.as_str(), "all" | "restart") {
        restart_bench(&options, &queries, &mut log)?;
    }
    let mut final_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(options.output.join("completed.json"))?;
    writeln!(final_file,"{}",Json::object([("sha",s(options.sha)),("measurement_completed",b(true)),("reference_hardware_verified",b(false)),("engine_acceptance",b(false)),("note",s("raw measurements only; unresolved stage/hardware/filesystem gates must be reviewed"))]).encode())?;
    Ok(())
}
