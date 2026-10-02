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
    source: Option<Box<dyn EventSource>>,
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
impl Engine {
    /// Native Windows source. Other platforms explicitly await their v0.1 adapter.
    pub fn open(root: &Path, database: Option<&Path>) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let root = fs::canonicalize(root)?;
            let identity = RootIdentity::open(&root)?;
            let source = crate::windows_events::WindowsEvents::open(&root, EventLimits::default())?;
            Self::start(root, database, identity, Box::new(source))
        }
        #[cfg(not(windows))]
        {
            let _ = (root, database);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "v0.1 native Engine adapter currently requires Windows",
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
        Self::start(root, database, identity, Box::new(source))
    }
    fn start(
        root: PathBuf,
        database: Option<&Path>,
        identity: RootIdentity,
        source: Box<dyn EventSource>,
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
        if batch.state == SourceState::Stopped {
            self.stop()?;
            return Ok(());
        }
        if !batch.losses.is_empty() {
            if batch.losses.contains(&Loss::WatchLost) {
                if let Some(mut source) = self.source.take() {
                    let _ = source.stop();
                }
                return Err(io::Error::other("event watch lost; reopen the engine"));
            }
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
                let ticket = self.state.ticket();
                self.check_root()?;
                self.metrics.full_scans += 1;
                let inventory = watch::scan(&self.root, Limits::default(), |_| Ok(()));
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
        let next = incremental::apply(
            &self.root,
            &self.state.inventory,
            &changes,
            Limits::default(),
            &mut self.metrics,
            |_| Ok(()),
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
        #[cfg(not(windows))]
        let file = File::open(path)?;
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
