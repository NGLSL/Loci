//! Linux watch uses pollable input and cancellable output; no detached stdin reader.
use super::*;
use loci_experiment::engine::MonitorOwner;
use std::ffi::{c_int, c_short, c_void};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}
unsafe extern "C" {
    fn poll(fds: *mut PollFd, count: usize, timeout: c_int) -> c_int;
    fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
    fn write(fd: c_int, buffer: *const c_void, count: usize) -> isize;
    fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    fn signal(number: c_int, handler: usize) -> usize;
}
static INTERRUPTED: AtomicBool = AtomicBool::new(false);
extern "C" fn interrupt(_: c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}
pub(super) struct Signals {
    old_int: usize,
    old_term: usize,
}
impl Signals {
    pub(super) fn install() -> io::Result<Self> {
        INTERRUPTED.store(false, Ordering::Relaxed);
        let old_int = unsafe { signal(2, interrupt as *const () as usize) };
        if old_int == usize::MAX {
            return Err(io::Error::last_os_error());
        }
        let old_term = unsafe { signal(15, interrupt as *const () as usize) };
        if old_term == usize::MAX {
            unsafe {
                signal(2, old_int);
            }
            return Err(io::Error::last_os_error());
        }
        Ok(Self { old_int, old_term })
    }
}
impl Drop for Signals {
    fn drop(&mut self) {
        unsafe {
            signal(2, self.old_int);
            signal(15, self.old_term);
        }
    }
}
struct Nonblocking {
    fd: c_int,
    flags: c_int,
}
impl Nonblocking {
    fn new(fd: c_int) -> io::Result<Self> {
        let flags = unsafe { fcntl(fd, 3) };
        if flags < 0 || unsafe { fcntl(fd, 4, flags | 2048) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd, flags })
    }
}
impl Drop for Nonblocking {
    fn drop(&mut self) {
        unsafe {
            fcntl(self.fd, 4, self.flags);
        }
    }
}
fn ready(fd: c_int, events: c_short) -> io::Result<bool> {
    let mut item = PollFd {
        fd,
        events,
        revents: 0,
    };
    let result = unsafe { poll(&mut item, 1, 20) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(error);
    }
    Ok(result > 0)
}
struct Output {
    cancel: Arc<AtomicBool>,
}
impl Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            if self.cancel.load(Ordering::Acquire) || INTERRUPTED.load(Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "query output cancelled",
                ));
            }
            let count = unsafe { write(1, bytes.as_ptr().cast(), bytes.len()) };
            if count >= 0 {
                return Ok(count as usize);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                ready(1, 4)?;
            } else if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Task {
    cancel: Arc<AtomicBool>,
    worker: JoinHandle<io::Result<()>>,
    name: &'static str,
}
impl Task {
    fn finish(self) {
        report(
            self.name,
            self.worker
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("query worker panicked"))),
        );
    }
    fn stop(self) {
        self.cancel.store(true, Ordering::Release);
        self.finish();
    }
}
fn report(name: &str, result: io::Result<()>) {
    eprintln!("engine,command={name},ok={}", result.is_ok());
    if let Err(error) = result {
        eprintln!("engine,command={name},error={error}");
    }
}
fn spawn_task(handle: QueryHandle, raw: String, nul: bool, export: bool) -> io::Result<Task> {
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let name = if export { "export" } else { "query" };
    let worker = std::thread::Builder::new().name("loci-cli-query".into()).spawn(move || {
        let lease = handle.lease()?;
        let progress = AtomicUsize::new(0);
        let mut output = Output { cancel: worker_cancel.clone() };
        if !export {
            let result = lease.search(&raw, true, &worker_cancel, &progress)?;
            eprintln!("engine,version={},state={:?},complete={},validated={},matches={},paths_returned={}", result.version, result.finished.status, result.complete, result.validated_at_start_and_finish, result.matches, result.paths.len());
            write_paths(&mut output, result.paths, nul)?;
            return if result.cancelled || !result.validated_at_start_and_finish { Err(io::Error::new(io::ErrorKind::WouldBlock, "query snapshot pending or cancelled")) } else { Ok(()) };
        }
        let mut cursor = None;
        loop {
            let page = lease.page(&raw, cursor.as_ref(), DEFAULT_PAGE_SIZE, &worker_cancel, &progress)?;
            write_paths(&mut output, page.paths, nul)?;
            if page.cancelled || !page.validated_at_start_and_finish { return Err(io::Error::new(io::ErrorKind::WouldBlock, "export snapshot pending or cancelled")); }
            if page.complete { return output.flush(); }
            cursor = page.next;
        }
    })?;
    eprintln!("engine,command={name},started=true");
    Ok(Task {
        cancel,
        worker,
        name,
    })
}
fn owner_progress(startup: &mut StartupTiming, owner: &MonitorOwner) {
    startup.observe_progress(owner.view().status, owner.metrics().scanned_entries);
}

pub(super) fn watch(
    engine: Engine,
    _root: &Path,
    _database: &Path,
    nul: bool,
    _options: EngineOptions,
    mut startup: StartupTiming,
) -> io::Result<()> {
    let _input = Nonblocking::new(0)?;
    let _output = Nonblocking::new(1)?;
    startup.before_poll(&engine);
    let mut searchable = engine.view().version > 0;
    let mut owner = engine.spawn()?;
    let handle = owner.query();
    coverage(&handle);
    if searchable {
        eprintln!("engine,watch-ready");
    }
    let mut previous = (owner.view().version, owner.view().status);
    let mut input = Vec::<u8>::with_capacity(4096);
    let mut task: Option<Task> = None;
    let result = (|| -> io::Result<()> {
        loop {
            if INTERRUPTED.load(Ordering::Relaxed) {
                break;
            }
            if task.as_ref().is_some_and(|task| task.worker.is_finished()) {
                task.take().unwrap().finish();
            }
            let view = owner.view();
            if !searchable && view.version > 0 {
                eprintln!(
                    "engine,phase={},first_searchable_ms={},version={},state={:?}",
                    if view.status == Status::Validated {
                        "validated"
                    } else {
                        "stale"
                    },
                    startup.started.elapsed().as_millis(),
                    view.version,
                    view.status
                );
                searchable = true;
                eprintln!("engine,watch-ready");
            }
            owner_progress(&mut startup, &owner);
            if previous != (view.version, view.status.clone()) {
                eprintln!("engine,version={},state={:?}", view.version, view.status);
                coverage(&handle);
                previous = (view.version, view.status);
            }
            if !input.contains(&b'\n') {
                if !ready(0, 1)? {
                    continue;
                }
                let mut chunk = [0u8; 256];
                let count = unsafe { read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
                if count == 0 {
                    break;
                }
                if count < 0 {
                    let error = io::Error::last_os_error();
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) {
                        continue;
                    }
                    return Err(error);
                }
                input.extend_from_slice(&chunk[..count as usize]);
                if input.len() > 4096 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "watch command exceeds 4096 bytes",
                    ));
                }
            }
            let Some(end) = input.iter().position(|byte| *byte == b'\n') else {
                continue;
            };
            let line = String::from_utf8(input.drain(..=end).collect()).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "watch commands require UTF-8")
            })?;
            let line = line.trim_end_matches(['\n', '\r']);
            if line == "stop" {
                break;
            }
            let (name, raw) = line.split_once(' ').unwrap_or((line, ""));
            match name {
                "query" | "export" => {
                    if task.is_some() { report(name, Err(io::Error::new(io::ErrorKind::WouldBlock, "one foreground query/export is already active; cancel it first"))); }
                    else { task = Some(spawn_task(handle.clone(), raw.to_owned(), nul, name == "export")?); }
                }
                "status" => {
                    coverage(&handle);
                    let view = handle.view();
                    eprintln!("engine,version={},state={:?},leases={}", view.version, view.status, view.leases);
                    let metrics = owner.metrics();
                    eprintln!("engine,full_scans={},scanned_entries={},audited_directories={},scope_checks={},compactions={},compaction_attempts={},compaction_restarts={},compacted_entries={},reclaimed_slots={},reclaimed_name_bytes={}",
                        metrics.full_scans, metrics.scanned_entries, metrics.audited_directories, metrics.scope_checks,
                        metrics.compactions, metrics.compaction_attempts, metrics.compaction_restarts,
                        metrics.compacted_entries, metrics.reclaimed_slots, metrics.reclaimed_name_bytes);
                    report("status", Ok(()));
                }
                "cancel" => {
                    let result = owner.cancel();
                    if let Some(task) = task.take() { task.stop(); report("cancel", Ok(())); }
                    else { report("cancel", result); }
                }
                "save" => report("save", owner.save().and_then(|request| request.wait(Duration::from_secs(2)))),
                "compact" => report("compact", owner.request_compaction().and_then(|request| request.wait(Duration::from_secs(2)))),
                "rebuild" => {
                    startup = StartupTiming::new();
                    report("rebuild", owner.request_rebuild().and_then(|request| request.wait(Duration::from_secs(2))));
                }
                _ => report("invalid", Err(io::Error::new(io::ErrorKind::InvalidInput, "watch commands: query QUERY | export QUERY | status | rebuild | compact | cancel | save | stop"))),
            }
        }
        Ok(())
    })();
    if let Some(task) = task.take() {
        task.stop();
    }
    let saved = owner
        .save()
        .and_then(|request| request.wait(Duration::from_secs(2)));
    eprintln!(
        "engine,shutdown_saved={},unsaved={}",
        saved.is_ok(),
        saved.is_err()
    );
    if let Err(error) = &saved {
        eprintln!("engine,shutdown_save_error={error}");
    }
    let stopped = owner.stop(Duration::from_secs(2));
    eprintln!(
        "engine,version={},state={:?},joined={}",
        owner.view().version,
        owner.view().status,
        owner.is_joined()
    );
    result.and(saved).and(stopped)
}
