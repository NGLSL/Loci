//! Bounded single-writer snapshot bridge. Validated means "at last observed cut",
//! never a claim that the filesystem cannot change after the final event drain.
use crate::index::{Index, Query};
use crate::watch::{Inventory, Recovery, Signal};
use std::collections::BTreeSet;
use std::io;
use std::path::{Component, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

pub const MAX_LEASES: usize = 8;
const MAX_PATH_BYTES: usize = 4096;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Empty,
    Pending,
    Validated,
    Failed(String),
    ReadersPinned,
    Stopped,
}
#[derive(Clone, Debug)]
pub struct View {
    pub version: u64,
    pub observed_generation: u64,
    pub snapshot_generation: u64,
    pub status: Status,
    pub reasons: BTreeSet<Signal>,
    pub leases: usize,
}
enum LiveIndex {
    Flat { index: Index, paths: Vec<PathBuf> },
    Shards(crate::partitioned::Shards),
}
struct Snapshot {
    index: LiveIndex,
    version: u64,
    generation: u64,
}
struct Shared {
    current: Option<Arc<Snapshot>>,
    retired: Weak<Snapshot>,
    view: View,
    stopped: bool,
}
#[derive(Clone)]
pub struct QueryHandle {
    shared: Arc<Mutex<Shared>>,
}
pub struct QueryLease {
    shared: Arc<Mutex<Shared>>,
    snapshot: Arc<Snapshot>,
    pub started: View,
}
impl Drop for QueryLease {
    fn drop(&mut self) {
        self.shared.lock().unwrap().view.leases -= 1;
    }
}
pub struct QueryResult {
    pub version: u64,
    pub snapshot_generation: u64,
    pub started: View,
    pub finished: View,
    pub validated_at_start_and_finish: bool,
    pub paths: Vec<PathBuf>,
    pub matches: usize,
    pub cancelled: bool,
    /// Complete matching count; first50 can stop before visiting all records.
    pub complete: bool,
}
impl QueryHandle {
    pub fn view(&self) -> View {
        self.shared.lock().unwrap().view.clone()
    }
    pub fn lease(&self) -> io::Result<QueryLease> {
        let mut shared = self.shared.lock().unwrap();
        if shared.view.leases >= MAX_LEASES {
            return Err(io::Error::other("query lease budget exhausted"));
        }
        let snapshot = shared
            .current
            .clone()
            .ok_or_else(|| io::Error::other("no validated snapshot yet"))?;
        shared.view.leases += 1;
        Ok(QueryLease {
            shared: self.shared.clone(),
            snapshot,
            started: shared.view.clone(),
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
        if raw.len() > 512 {
            return Err(io::Error::other("query byte budget exhausted"));
        }
        let query = Query::parse(raw);
        let out = match &self.snapshot.index {
            LiveIndex::Flat { index, paths } => {
                let out = index.search(
                    &query,
                    true,
                    if first50 { 50 } else { usize::MAX },
                    cancel,
                    progress,
                );
                crate::partitioned::Found {
                    paths: out
                        .ids
                        .iter()
                        .map(|id| paths[*id as usize].clone())
                        .collect(),
                    matches: out.matches,
                    cancelled: out.cancelled,
                }
            }
            LiveIndex::Shards(shards) => shards.search(&query, first50, cancel, progress),
        };
        let finished = self.shared.lock().unwrap().view.clone();
        let validated = self.started.status == Status::Validated
            && finished.status == Status::Validated
            && self.started.version == self.snapshot.version
            && finished.version == self.snapshot.version
            && self.started.observed_generation == self.snapshot.generation
            && finished.observed_generation == self.snapshot.generation;
        Ok(QueryResult {
            version: self.snapshot.version,
            snapshot_generation: self.snapshot.generation,
            started: self.started.clone(),
            finished,
            validated_at_start_and_finish: validated,
            paths: out.paths,
            matches: out.matches,
            cancelled: out.cancelled,
            complete: !out.cancelled && (!first50 || out.matches < 50),
        })
    }
}
struct Candidate {
    index: LiveIndex,
    generation: u64,
}
pub struct Store {
    handle: QueryHandle,
    staged: Option<Candidate>,
    pub builds: usize,
    pub rebuilt_records: usize,
    pub rebuilt_partitions: usize,
    pub last_rebuilt_records: usize,
    pub last_rebuilt_partitions: usize,
}
impl Default for Store {
    fn default() -> Self {
        Self::new()
    }
}
impl Store {
    pub fn new() -> Self {
        let view = View {
            version: 0,
            observed_generation: 0,
            snapshot_generation: 0,
            status: Status::Empty,
            reasons: BTreeSet::new(),
            leases: 0,
        };
        Self {
            handle: QueryHandle {
                shared: Arc::new(Mutex::new(Shared {
                    current: None,
                    retired: Weak::new(),
                    view,
                    stopped: false,
                })),
            },
            staged: None,
            builds: 0,
            rebuilt_records: 0,
            rebuilt_partitions: 0,
            last_rebuilt_records: 0,
            last_rebuilt_partitions: 0,
        }
    }
    pub fn handle(&self) -> QueryHandle {
        self.handle.clone()
    }
    pub fn observe(&mut self, state: &Recovery) {
        let mut shared = self.handle.shared.lock().unwrap();
        if shared.stopped {
            return;
        }
        let changed = shared.view.observed_generation != state.generation;
        shared.view.observed_generation = state.generation;
        shared.view.reasons = state.reasons.clone();
        if (state.dirty || shared.view.snapshot_generation != state.generation)
            && (changed || matches!(shared.view.status, Status::Validated | Status::Empty))
        {
            shared.view.status = Status::Pending;
        }
    }
    pub fn fail(&mut self, error: impl ToString) {
        self.staged = None;
        let message: String = error.to_string().chars().take(256).collect();
        let mut shared = self.handle.shared.lock().unwrap();
        if !shared.stopped {
            shared.view.status = Status::Failed(message);
        }
    }
    /// End monitoring while preserving immutable snapshots for existing readers.
    pub fn stop(&mut self) {
        self.staged = None;
        let mut shared = self.handle.shared.lock().unwrap();
        shared.stopped = true;
        shared.view.status = Status::Stopped;
    }
    pub fn needs_update(&self) -> bool {
        self.handle.view().status != Status::Validated
    }
    fn prepare(&mut self, state: &Recovery) -> io::Result<Option<(Vec<PathBuf>, Vec<String>)>> {
        if self.handle.shared.lock().unwrap().stopped {
            return Err(io::Error::other("query store is stopped"));
        }
        self.observe(state);
        if state.dirty || !state.inventory.complete {
            return Ok(None);
        }
        if self.staged.is_some() {
            return Err(io::Error::other("one staged candidate only"));
        }
        {
            let mut shared = self.handle.shared.lock().unwrap();
            if shared.retired.strong_count() > 0 {
                shared.view.status = Status::ReadersPinned;
                return Ok(None);
            }
        }
        let paths: Vec<_> = state.inventory.entries.keys().cloned().collect();
        if paths.len() > 4096 {
            return Err(io::Error::other("live snapshot entry budget exhausted"));
        }
        let mut strings = Vec::with_capacity(paths.len());
        let mut bytes = 0usize;
        for path in &paths {
            let mut parts = vec![];
            for component in path.components() {
                let Component::Normal(part) = component else {
                    return Err(io::Error::other("unsafe relative path"));
                };
                // Reject the whole candidate; never silently omit non-UTF-8 entries.
                parts.push(part.to_str().ok_or_else(|| {
                    io::Error::other("live query index requires UTF-8 paths; snapshot rejected")
                })?);
            }
            let text = parts.join("/");
            bytes = bytes.saturating_add(text.len());
            if text.len() > MAX_PATH_BYTES || bytes > MAX_INPUT_BYTES {
                return Err(io::Error::other(
                    "live index path/input byte budget exhausted",
                ));
            }
            strings.push(text);
        }
        Ok(Some((paths, strings)))
    }
    pub fn stage(&mut self, state: &Recovery) -> io::Result<bool> {
        let Some((paths, strings)) = self.prepare(state)? else {
            return Ok(false);
        };
        self.builds += 1;
        self.last_rebuilt_records = paths.len();
        self.last_rebuilt_partitions = 1;
        self.rebuilt_records += paths.len();
        self.rebuilt_partitions += 1;
        let index = Index::from_paths(strings.into_iter());
        self.staged = Some(Candidate {
            index: LiveIndex::Flat { index, paths },
            generation: state.generation,
        });
        Ok(true)
    }
    pub fn stage_delta(&mut self, state: &Recovery) -> io::Result<bool> {
        let Some((paths, strings)) = self.prepare(state)? else {
            return Ok(false);
        };
        let current = self.handle.shared.lock().unwrap().current.clone();
        let previous = current.as_ref().and_then(|s| match &s.index {
            LiveIndex::Shards(parts) => Some(parts),
            _ => None,
        });
        let (shards, parts, records) = crate::partitioned::Shards::build(paths, strings, previous);
        self.builds += 1;
        self.last_rebuilt_records = records;
        self.last_rebuilt_partitions = parts;
        self.rebuilt_records += records;
        self.rebuilt_partitions += parts;
        self.staged = Some(Candidate {
            index: LiveIndex::Shards(shards),
            generation: state.generation,
        });
        Ok(true)
    }
    pub fn commit(&mut self, state: &Recovery) -> bool {
        self.observe(state);
        let Some(next) = self.staged.take() else {
            return false;
        };
        if state.dirty || next.generation != state.generation {
            return false;
        }
        let mut shared = self.handle.shared.lock().unwrap();
        if shared.stopped {
            return false;
        }
        if shared.retired.strong_count() > 0 {
            shared.view.status = Status::ReadersPinned;
            return false;
        }
        let version = shared
            .view
            .version
            .checked_add(1)
            .expect("snapshot version overflow");
        if let Some(old) = shared.current.take() {
            shared.retired = Arc::downgrade(&old);
        }
        shared.current = Some(Arc::new(Snapshot {
            index: next.index,
            version,
            generation: next.generation,
        }));
        shared.view.version = version;
        shared.view.snapshot_generation = next.generation;
        shared.view.observed_generation = next.generation;
        shared.view.status = Status::Validated;
        shared.view.reasons.clear();
        true
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        self.stop();
    }
}
/// Explicit caller clock makes storm throttling reproducible without sleeps.
/// One writer; minimum 250ms between attempts, exponential failure backoff to 2s.
#[derive(Default)]
pub struct Gate {
    next: Duration,
    failures: u32,
    pub attempts: usize,
}
impl Gate {
    pub fn ready(&self, now: Duration) -> bool {
        now >= self.next
    }
    pub fn completed(&mut self, now: Duration, success: bool) {
        self.attempts += 1;
        self.failures = if success {
            0
        } else {
            (self.failures + 1).min(3)
        };
        self.next = now.saturating_add(Duration::from_millis(250 << self.failures));
    }
}
/// Portable fixture driver: changes must be signalled by caller; no Windows watcher.
pub struct Portable {
    pub root: PathBuf,
    pub limits: crate::watch::Limits,
    pub state: Recovery,
    pub store: Store,
    pub gate: Gate,
    pub full_scans: usize,
    pub scanned_entries: usize,
}
impl Portable {
    pub fn new(root: &std::path::Path, limits: crate::watch::Limits) -> io::Result<Self> {
        let root = std::fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(io::Error::other("root must be directory"));
        }
        Ok(Self {
            root,
            limits,
            state: Recovery::new(limits.queue),
            store: Store::new(),
            gate: Gate::default(),
            full_scans: 0,
            scanned_entries: 0,
        })
    }
    pub fn restore(&mut self, inventory: Inventory) {
        self.state = Recovery::restored(inventory, self.limits.queue);
        self.store.observe(&self.state);
    }
    pub fn signal(&mut self, signal: Signal) {
        self.state.signal(signal);
        self.store.observe(&self.state);
    }
    pub fn tick(&mut self, now: Duration) -> io::Result<bool> {
        self.tick_with_hook(now, |_| {})
    }
    pub fn tick_with_hook(
        &mut self,
        now: Duration,
        mut after_scan: impl FnMut(&mut Recovery),
    ) -> io::Result<bool> {
        self.store.observe(&self.state);
        if !self.state.dirty && !self.store.needs_update() {
            return Ok(true);
        }
        if !self.gate.ready(now) {
            return Ok(false);
        }
        let mut success = false;
        for _ in 0..self.limits.retries {
            let ticket = self.state.ticket();
            self.full_scans += 1;
            let candidate = crate::watch::scan(&self.root, self.limits, |_| Ok(()));
            self.scanned_entries += candidate.examined;
            after_scan(&mut self.state);
            if self.state.publish(ticket, candidate) {
                success = true;
                break;
            }
            if self.state.reasons.contains(&Signal::ScanIncomplete) {
                break;
            }
        }
        if !success {
            self.state.signal(Signal::RetryLimit);
            self.store.observe(&self.state);
            self.gate.completed(now, false);
            return Ok(false);
        }
        match self.store.stage(&self.state) {
            Ok(staged) => {
                success = staged && self.store.commit(&self.state);
            }
            Err(error) => {
                self.store.fail(&error);
                self.gate.completed(now, false);
                return Err(error);
            }
        }
        self.gate.completed(now, success);
        Ok(success)
    }
}
#[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
pub struct Native {
    pub watch: crate::linux_inotify::Runtime,
    pub store: Store,
    pub gate: Gate,
    epoch: std::time::Instant,
}
#[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
impl Native {
    pub fn new(root: &std::path::Path, limits: crate::watch::Limits) -> io::Result<Self> {
        Ok(Self {
            watch: crate::linux_inotify::Runtime::new(root, limits)?,
            store: Store::new(),
            gate: Gate::default(),
            epoch: std::time::Instant::now(),
        })
    }
    pub fn restore(&mut self, inventory: Inventory) {
        self.watch.restore(inventory);
        self.store.observe(&self.watch.state);
    }
    pub fn tick(&mut self) -> io::Result<bool> {
        self.tick_with_hook(|| {})
    }
    pub fn tick_with_hook(&mut self, after_build: impl FnOnce()) -> io::Result<bool> {
        self.tick_with_hooks(|| {}, after_build)
    }
    pub fn tick_with_hooks(
        &mut self,
        after_scan: impl FnMut(),
        after_build: impl FnOnce(),
    ) -> io::Result<bool> {
        if let Err(error) = self.watch.poll() {
            self.store.observe(&self.watch.state);
            self.store.fail(&error);
            return Err(error);
        }
        self.store.observe(&self.watch.state);
        if !self.watch.state.dirty && !self.store.needs_update() {
            return Ok(true);
        }
        let now = self.epoch.elapsed();
        if !self.gate.ready(now) {
            return Ok(false);
        }
        // Dirty status is visible before any bounded scan or index allocation.
        self.watch.state.signal(Signal::Periodic);
        self.store.observe(&self.watch.state);
        let attempt = (|| {
            if !self.watch.reconcile_with_hook(after_scan)? {
                self.store.observe(&self.watch.state);
                return Ok(false);
            }
            if !self.store.stage(&self.watch.state)? {
                return Ok(false);
            }
            after_build();
            // Events during index build invalidate the candidate before atomic publication.
            self.watch.poll()?;
            Ok(self.store.commit(&self.watch.state))
        })();
        match attempt {
            Ok(success) => {
                self.gate.completed(self.epoch.elapsed(), success);
                Ok(success)
            }
            Err(error) => {
                // Reconcile/poll may have advanced the recovery generation and
                // added a failure reason before returning this error.
                self.store.observe(&self.watch.state);
                self.store.fail(&error);
                self.gate.completed(self.epoch.elapsed(), false);
                Err(error)
            }
        }
    }
}
