//! Single-owner v0.1 lifecycle: install source, reconcile, publish, save, stop.
use crate::events::{Change, EventBatch, EventLimits, EventSource, Loss, SourceState};
use crate::incremental::{self, Metrics};
use crate::live::{self, Gate, Store};
use crate::storage::Snapshot;
use crate::watch::{self, Limits, Recovery, Signal};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Arc;
use std::time::Instant;

pub use crate::live::Status;

#[derive(Clone, Debug)]
pub struct View {
    pub version: u64,
    pub status: Status,
    pub leases: usize,
}
impl From<live::View> for View {
    fn from(view: live::View) -> Self {
        Self {
            version: view.version,
            status: view.status,
            leases: view.leases,
        }
    }
}
#[derive(Clone)]
pub struct QueryHandle {
    root: Arc<PathBuf>,
    handle: live::QueryHandle,
}
pub struct QueryLease {
    root: Arc<PathBuf>,
    lease: live::QueryLease,
}
pub struct QueryResult {
    pub version: u64,
    pub started: View,
    pub finished: View,
    pub validated_at_start_and_finish: bool,
    pub paths: Vec<PathBuf>,
    pub matches: usize,
    pub cancelled: bool,
    pub complete: bool,
}
impl QueryHandle {
    pub fn view(&self) -> View {
        self.handle.view().into()
    }
    pub fn lease(&self) -> io::Result<QueryLease> {
        Ok(QueryLease {
            root: self.root.clone(),
            lease: self.handle.lease()?,
        })
    }
}
impl QueryLease {
    pub fn search(
        &self,
        raw: &str,
        first50: bool,
        cancel: &AtomicBool,
        progress: &AtomicUsize,
    ) -> io::Result<QueryResult> {
        let out = self.lease.search(raw, first50, cancel, progress)?;
        Ok(QueryResult {
            version: out.version,
            started: out.started.into(),
            finished: out.finished.into(),
            validated_at_start_and_finish: out.validated_at_start_and_finish,
            paths: out.paths.into_iter().map(|p| self.root.join(p)).collect(),
            matches: out.matches,
            cancelled: out.cancelled,
            complete: out.complete,
        })
    }
}

pub struct Engine {
    root: Arc<PathBuf>,
    database: Option<PathBuf>,
    identity: Option<RootIdentity>,
    source: Option<Source>,
    state: Recovery,
    store: Store,
    pending: Vec<Change>,
    correction: bool,
    stopped: bool,
    gate: Gate,
    epoch: Instant,
    last_audit: Instant,
    metrics: Metrics,
}

// Native Linux needs directory registration during scans and incremental
// transactions. These hooks stay private; the public EventSource seam is fixed.
enum Source {
    External(Box<dyn EventSource>),
    #[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
    Linux(crate::linux_events::LinuxEvents),
}
impl EventSource for Source {
    fn poll(&mut self) -> io::Result<EventBatch> {
        match self {
            Self::External(source) => source.poll(),
            #[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
            Self::Linux(source) => source.poll(),
        }
    }
    fn stop(&mut self) -> io::Result<()> {
        match self {
            Self::External(source) => source.stop(),
            #[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
            Self::Linux(source) => source.stop(),
        }
    }
}
impl Source {
    fn before_directory(&mut self, _path: &Path) -> io::Result<()> {
        match self {
            Self::External(_) => Ok(()),
            #[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
            Self::Linux(source) => source.before_directory(_path),
        }
    }
    fn topology(&mut self, _edit: incremental::Topology) -> io::Result<()> {
        match self {
            Self::External(_) => Ok(()),
            #[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
            Self::Linux(source) => source.topology(_edit),
        }
    }
    fn begin_reconcile(&mut self) -> io::Result<bool> {
        match self {
            Self::External(_) => Ok(false),
            #[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
            Self::Linux(source) => {
                source.begin_reconcile()?;
                Ok(true)
            }
        }
    }
}
impl Engine {
    /// Native Windows or x86_64 Linux source for the explicitly selected root.
    pub fn open(root: &Path, database: Option<&Path>) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let root = fs::canonicalize(root)?;
            let identity = RootIdentity::open(&root)?;
            let source = crate::windows_events::WindowsEvents::open(&root, EventLimits::default())?;
            Self::start(root, database, identity, Source::External(Box::new(source)))
        }
        #[cfg(target_os = "linux")]
        {
            let root = fs::canonicalize(root)?;
            let identity = RootIdentity::open(&root)?;
            let source = crate::linux_events::LinuxEvents::open(&root, EventLimits::default())?;
            Self::start(root, database, identity, Source::Linux(source))
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (root, database);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native Engine requires Windows or x86_64 Linux",
            ))
        }
    }
    /// External platform/test source, already installed for this canonical root.
    /// The caller must satisfy EventSource's root and bounded-poll contract.
    pub fn with_source(
        root: &Path,
        database: Option<&Path>,
        source: impl EventSource + 'static,
    ) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        let identity = RootIdentity::open(&root)?;
        Self::start(root, database, identity, Source::External(Box::new(source)))
    }
    fn start(
        root: PathBuf,
        database: Option<&Path>,
        identity: RootIdentity,
        source: Source,
    ) -> io::Result<Self> {
        let database = database
            .map(|path| database_path(path, &root))
            .transpose()?;
        let mut engine = Self {
            root: Arc::new(root),
            database,
            identity: Some(identity),
            source: Some(source),
            state: Recovery::new(EventLimits::default().max_events()),
            store: Store::new(),
            pending: vec![],
            correction: true,
            stopped: false,
            gate: Gate::default(),
            epoch: Instant::now(),
            last_audit: Instant::now(),
            metrics: Metrics::default(),
        };
        engine.check_root()?;
        if let Some(path) = &engine.database {
            match Snapshot::load(path, &engine.root) {
                Ok(snapshot) => {
                    // Seed an offline version before observing Startup/Restart.
                    // It is never exposed as Validated before root reconciliation.
                    let mut offline = Recovery::new(Limits::default().queue);
                    offline.inventory = snapshot.inventory.clone();
                    offline.dirty = false;
                    offline.generation = 0;
                    if engine.store.stage_delta(&offline)? {
                        engine.store.commit(&offline);
                    }
                    engine.state = Recovery::restored(snapshot.inventory, Limits::default().queue);
                    engine.store.observe(&engine.state);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        engine.poll()?;
        Ok(engine)
    }
    pub fn query(&self) -> QueryHandle {
        QueryHandle {
            root: self.root.clone(),
            handle: self.store.handle(),
        }
    }
    pub fn view(&self) -> View {
        self.store.handle().view().into()
    }
    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }
    fn check_root(&self) -> io::Result<()> {
        let identity = self
            .identity
            .as_ref()
            .ok_or_else(|| io::Error::other("engine is stopped"))?;
        identity.check(&self.root)
    }
    fn invalidate(&mut self, signal: Signal) {
        self.pending.clear();
        self.correction = true;
        self.state.signal(signal);
        self.store.observe(&self.state);
    }
    fn capture(&mut self) -> io::Result<()> {
        if let Err(error) = self.check_root() {
            if let Some(mut source) = self.source.take() {
                let _ = source.stop();
            }
            return Err(error);
        }
        let result = self
            .source
            .as_mut()
            .ok_or_else(|| io::Error::other("event source failed; reopen the engine"))?
            .poll();
        let batch = match result {
            Ok(batch) => batch,
            Err(error) => {
                if let Some(mut source) = self.source.take() {
                    let _ = source.stop();
                }
                return Err(error);
            }
        };
        self.consume(batch)?;
        if self.stopped {
            return Ok(());
        }
        if let Err(error) = self.check_root() {
            if let Some(mut source) = self.source.take() {
                let _ = source.stop();
            }
            return Err(error);
        }
        Ok(())
    }
    fn consume(&mut self, batch: EventBatch) -> io::Result<()> {
        // The Windows cancellation/lost-watch path can be Stopped + WatchLost.
        // Unexpected watch loss remains a failure, not a successful user stop.
        if batch.losses.contains(&Loss::WatchLost) {
            if let Some(mut source) = self.source.take() {
                let _ = source.stop();
            }
            return Err(io::Error::other("event watch lost; reopen the engine"));
        }
        if batch.state == SourceState::Stopped {
            self.stop()?;
            return Ok(());
        }
        if !batch.losses.is_empty() {
            for loss in batch.losses {
                self.invalidate(match loss {
                    Loss::KernelOverflow => Signal::KernelOverflow,
                    Loss::UserOverflow => Signal::UserOverflow,
                    Loss::WatchLost => Signal::WatchLost,
                    Loss::BackendRestart => Signal::Restart,
                    Loss::UnpairedRename | Loss::InvalidEvent => Signal::GenerationRace,
                });
            }
            return Ok(());
        }
        // Validate EVERY path before any inspection, even during startup/correction.
        for change in &batch.changes {
            match change {
                Change::Refresh(path) | Change::Remove(path) => incremental::valid(path)?,
                Change::Rename { from, to } => {
                    incremental::valid(from)?;
                    incremental::valid(to)?;
                }
            }
        }
        if self.pending.len().saturating_add(batch.changes.len()) > Limits::default().queue {
            self.invalidate(Signal::UserOverflow);
        } else if !batch.changes.is_empty() {
            self.pending.extend(batch.changes);
            self.state.signal(Signal::Change);
            self.store.observe(&self.state);
        }
        Ok(())
    }
    /// Nonblocking event drain; true only for a published, validated observation.
    /// Scan/build attempts are bounded and throttled; false means Pending/readers/Stopped.
    pub fn poll(&mut self) -> io::Result<bool> {
        if self.stopped {
            return Ok(false);
        }
        if let Err(error) = self.capture() {
            self.invalidate(Signal::WatchLost);
            self.store.fail(&error);
            if let Some(mut source) = self.source.take() {
                let _ = source.stop();
            }
            return Err(error);
        }
        if self.stopped {
            return Ok(false);
        }
        if self.last_audit.elapsed().as_secs() >= 30 {
            self.invalidate(Signal::Periodic);
        }
        if !self.state.dirty && !self.store.needs_update() {
            return Ok(true);
        }
        let now = self.epoch.elapsed();
        if !self.gate.ready(now) {
            return Ok(false);
        }
        let outcome = self.update();
        match outcome {
            Ok(success) => {
                self.gate.completed(self.epoch.elapsed(), success);
                Ok(success)
            }
            Err(error) => {
                self.correction = true;
                self.state.signal(Signal::ScanIncomplete);
                self.store.observe(&self.state);
                self.store.fail(&error);
                self.gate.completed(self.epoch.elapsed(), false);
                Err(error)
            }
        }
    }
    fn update(&mut self) -> io::Result<bool> {
        if self.correction {
            for _ in 0..Limits::default().retries {
                self.pending.clear();
                self.check_root()?;
                let source = self
                    .source
                    .as_mut()
                    .ok_or_else(|| io::Error::other("event source missing"))?;
                if source.begin_reconcile()? {
                    // Replacing an inotify fd loses a cut; only the following
                    // complete scan and final drain can validate the new source.
                    self.state.signal(Signal::Restart);
                    self.store.observe(&self.state);
                }
                let ticket = self.state.ticket();
                self.metrics.full_scans += 1;
                let inventory = watch::scan(&self.root, Limits::default(), |dir| {
                    source.before_directory(dir)
                });
                self.metrics.scanned_entries += inventory.examined;
                if !inventory.complete {
                    return Err(io::Error::other(format!(
                        "root scan incomplete: {}",
                        inventory.errors.join("; ")
                    )));
                }
                // Same constraints as persistence and incremental updates, plus UTF8.
                let inventory = Snapshot::new(&self.root, inventory)?.inventory;
                self.capture()?;
                if self.stopped {
                    return Ok(false);
                }
                if !self.state.publish(ticket, inventory) {
                    continue;
                }
                if !self.store.stage_delta(&self.state)? {
                    return Ok(false);
                }
                self.capture()?;
                if self.stopped {
                    return Ok(false);
                }
                if self.store.commit(&self.state) {
                    self.correction = false;
                    self.last_audit = Instant::now();
                    return Ok(true);
                }
            }
            self.invalidate(Signal::RetryLimit);
            return Ok(false);
        }
        let ticket = self.state.ticket();
        let changes = std::mem::take(&mut self.pending);
        if incremental::requires_reconcile(&self.root, &changes, &mut self.metrics)? {
            self.invalidate(Signal::GenerationRace);
            return Ok(false);
        }
        let source = self
            .source
            .as_mut()
            .ok_or_else(|| io::Error::other("event source missing"))?;
        let next = incremental::apply(
            &self.root,
            &self.state.inventory,
            &changes,
            Limits::default(),
            &mut self.metrics,
            |edit| source.topology(edit),
        )?;
        self.capture()?;
        if self.stopped {
            return Ok(false);
        }
        if !self.state.publish(ticket, next) {
            self.correction = true;
            self.store.observe(&self.state);
            return Ok(false);
        }
        if !self.store.stage_delta(&self.state)? {
            return Ok(false);
        }
        self.capture()?;
        if self.stopped {
            return Ok(false);
        }
        Ok(self.store.commit(&self.state))
    }
    /// Save only a currently validated observation; never persist partial inventory.
    pub fn save(&mut self) -> io::Result<()> {
        if !self.poll()? || self.view().status != Status::Validated {
            return Err(io::Error::other(
                "save requires a validated engine observation",
            ));
        }
        let path = self.database.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "no database was selected")
        })?;
        self.check_root()?;
        Snapshot::new(&self.root, self.state.inventory.clone())?.save(path)
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.stopped = true;
        self.pending.clear();
        self.store.stop();
        let result = self
            .source
            .take()
            .map_or(Ok(()), |mut source| source.stop());
        self.identity.take();
        result
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn database_path(path: &Path, root: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "database must name a file"))?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let path = fs::canonicalize(parent)?.join(name);
    if path.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "database must be outside the monitored root (including temporary saves)",
        ));
    }
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "database must be a regular file",
            ));
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "database reparse points are unsupported",
                ));
            }
        }
    }
    Ok(path)
}

// Retain the selected object's handle so identity cannot be recycled during ownership.
struct RootIdentity {
    _file: File,
    id: (u64, u64),
}
impl RootIdentity {
    fn open(path: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "root must remain the selected directory",
            ));
        }
        #[cfg(windows)]
        let file = {
            use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(io::Error::other("root became a reparse point"));
            }
            fs::OpenOptions::new()
                .access_mode(0)
                .custom_flags(0x02000000 | 0x00200000)
                .open(path)?
        };
        #[cfg(target_os = "linux")]
        let file = open_linux_root(path)?;
        #[cfg(not(any(windows, target_os = "linux")))]
        let file = File::open(path)?;
        if !file.metadata()?.is_dir() {
            return Err(io::Error::other("opened root is not a directory"));
        }
        let id = file_identity(&file)?;
        Ok(Self { _file: file, id })
    }
    fn check(&self, path: &Path) -> io::Result<()> {
        let current = Self::open(path)?;
        if current.id != self.id {
            return Err(io::Error::other(
                "selected root identity changed; reopen explicitly",
            ));
        }
        Ok(())
    }
}
#[cfg(target_os = "linux")]
fn open_linux_root(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    // x86_64 Linux O_DIRECTORY | O_NOFOLLOW: a replacement FIFO must not
    // block open, and a replacement symlink must not retarget root ownership.
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x10000 | 0x20000)
        .open(path)
}
#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    // BY_HANDLE_FILE_INFORMATION consists of 13 DWORDs (three FILETIME pairs).
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(handle: *mut c_void, information: *mut u32) -> i32;
    }
    let mut information = [0u32; 13];
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        information[7] as u64,
        ((information[11] as u64) << 32) | information[12] as u64,
    ))
}
#[cfg(unix)]
fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}
#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File) -> io::Result<(u64, u64)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "root identity unavailable on this platform",
    ))
}

#[cfg(all(test, target_os = "linux"))]
mod linux_kernel_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    struct Fixture {
        parent: PathBuf,
        base: PathBuf,
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            static SEQUENCE: AtomicU64 = AtomicU64::new(0);
            let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
            let base = parent.join(format!(
                "loci-engine-kernel-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&base).unwrap();
            let root = base.join("data");
            fs::create_dir(&root).unwrap();
            Self { parent, base, root }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let target = fs::canonicalize(&self.base).unwrap();
            assert_eq!(target.parent(), Some(self.parent.as_path()));
            assert!(target
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("loci-engine-kernel-"));
            fs::remove_dir_all(target).unwrap();
        }
    }
    fn paths(engine: &Engine) -> Vec<PathBuf> {
        engine
            .query()
            .lease()
            .unwrap()
            .search("", false, &AtomicBool::new(false), &AtomicUsize::new(0))
            .unwrap()
            .paths
    }

    #[test]
    fn linux_engine_real_kernel_overflow_preserves_then_reconciles_snapshot() {
        let f = Fixture::new();
        fs::write(f.root.join("kept.txt"), b"kept").unwrap();
        let baseline = crate::linux_inotify::process_usage();
        let mut engine = Engine::open(&f.root, None).unwrap();
        assert_eq!(engine.view().status, Status::Validated);
        assert_eq!(paths(&engine), [f.root.join("kept.txt")]);
        let scans = engine.metrics().full_scans;
        let capacity: usize = fs::read_to_string("/proc/sys/fs/inotify/max_queued_events")
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert!(
            (1..=131072).contains(&capacity),
            "unbounded test kernel queue: {capacity}"
        );
        // Only the correction deadline is controlled. The fd, filesystem events,
        // queue overflow record and subsequent correction are all native.
        // Retain the overflowing fd across bounded eight-read drains until the
        // actual kernel overflow record behind the ordinary events is observed.
        engine
            .gate
            .completed(engine.epoch.elapsed() + Duration::from_secs(60), true);
        let transient = f.root.join("transient.txt");
        for _ in 0..(capacity / 2 + 1024) {
            fs::write(&transient, b"transient").unwrap();
            fs::remove_file(&transient).unwrap();
        }
        fs::write(f.root.join("survivor.txt"), b"survivor").unwrap();
        for _ in 0..32 {
            assert!(!engine.poll().unwrap());
            if engine.state.reasons.contains(&Signal::KernelOverflow) {
                break;
            }
        }
        assert!(
            engine.state.reasons.contains(&Signal::KernelOverflow),
            "native IN_Q_OVERFLOW was not observed"
        );
        assert_eq!(engine.view().status, Status::Pending);
        assert_eq!(paths(&engine), [f.root.join("kept.txt")]);
        assert_eq!(engine.metrics().full_scans, scans);
        eprintln!("new Linux Engine observed actual IN_Q_OVERFLOW; max_queued_events={capacity}");
        engine.gate = Gate::default();
        let deadline = Instant::now() + Duration::from_secs(8);
        while !engine.poll().unwrap() {
            assert!(
                Instant::now() < deadline,
                "overflow correction did not validate"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(engine.view().status, Status::Validated);
        assert_eq!(
            paths(&engine),
            [f.root.join("kept.txt"), f.root.join("survivor.txt")]
        );
        assert!(engine.metrics().full_scans > scans);
        engine.stop().unwrap();
        engine.stop().unwrap();
        assert_eq!(crate::linux_inotify::process_usage(), baseline);
    }

    #[test]
    fn linux_root_open_rejects_fifo_and_symlink_without_blocking() {
        let f = Fixture::new();
        let fifo = f.base.join("replacement-fifo");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        let started = Instant::now();
        assert!(open_linux_root(&fifo).is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        let link = f.base.join("replacement-symlink");
        symlink(&f.root, &link).unwrap();
        assert!(open_linux_root(&link).is_err());
        assert!(open_linux_root(&f.root)
            .unwrap()
            .metadata()
            .unwrap()
            .is_dir());
    }
}
