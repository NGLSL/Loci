//! Bounded incremental filename/path inventory transactions; no Windows event backend.
pub mod events;
use crate::live::{Gate, Store};
use crate::watch::{self, Inventory, Kind, Limits, Recovery, Signal};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Refresh(PathBuf),
    Remove(PathBuf),
    Rename { from: PathBuf, to: PathBuf },
}
#[derive(Clone, Debug)]
pub(crate) enum Topology {
    Add(PathBuf),
    Remove(PathBuf),
    Rename { from: PathBuf, to: PathBuf },
}
#[derive(Default, Clone, Debug)]
pub struct Metrics {
    pub full_scans: usize,
    pub subtree_scans: usize,
    pub scanned_entries: usize,
    pub metadata_calls: usize,
    pub transactions: usize,
    pub changed_paths: usize,
}
pub(crate) fn validate_limits(limits: Limits) -> io::Result<()> {
    if limits.entries == 0
        || limits.entries > 4096
        || limits.directories == 0
        || limits.directories > 128
        || limits.queue == 0
        || limits.queue > 256
        || limits.depth > 16
        || limits.retries == 0
        || limits.retries > 4
    {
        return Err(io::Error::other(
            "incremental prototype limits exceed fixed budgets",
        ));
    }
    Ok(())
}
fn valid(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(io::Error::other("unsafe relative event path"));
    }
    if path.as_os_str().len() > 4096 {
        return Err(io::Error::other("event path byte budget"));
    }
    Ok(())
}
fn inspect(root: &Path, path: &Path, metrics: &mut Metrics) -> io::Result<Option<Kind>> {
    valid(path)?;
    let mut current = root.to_path_buf();
    let mut result = None;
    let count = path.components().count();
    for (i, part) in path.components().enumerate() {
        current.push(part.as_os_str());
        metrics.metadata_calls += 1;
        let meta = match fs::symlink_metadata(&current) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if meta.file_type().is_symlink() {
            return Ok(None);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if meta.file_attributes() & 0x400 != 0 {
                return Ok(None);
            }
        }
        if meta.is_dir() {
            if watch::skip(&current) {
                return Ok(None);
            }
            result = Some(Kind::Directory);
        } else if meta.is_file() && i + 1 == count {
            result = Some(Kind::File);
        } else {
            return Ok(None);
        }
    }
    Ok(result)
}
fn remove(
    inv: &mut Inventory,
    path: &Path,
    root: &Path,
    topology: &mut impl FnMut(Topology) -> io::Result<()>,
) -> io::Result<()> {
    let keys: Vec<_> = inv
        .entries
        .keys()
        .filter(|p| p.starts_with(path))
        .cloned()
        .collect();
    for key in keys {
        inv.entries.remove(&key);
    }
    topology(Topology::Remove(root.join(path)))?;
    Ok(())
}
fn budget(inv: &Inventory, limits: Limits) -> io::Result<()> {
    if inv.entries.len() > limits.entries.min(4096) {
        return Err(io::Error::other("incremental entry budget exhausted"));
    }
    let directories = 1 + inv
        .entries
        .values()
        .filter(|k| **k == Kind::Directory)
        .count();
    if directories > limits.directories {
        return Err(io::Error::other("incremental directory budget exhausted"));
    }
    let mut bytes = 0usize;
    for (path, kind) in &inv.entries {
        valid(path)?;
        let depth = path.components().count() - usize::from(*kind == Kind::File);
        if depth > limits.depth {
            return Err(io::Error::other("incremental depth budget exhausted"));
        }
        bytes = bytes.saturating_add(path.as_os_str().len());
        if bytes > 1024 * 1024 {
            return Err(io::Error::other("incremental input byte budget exhausted"));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if inv.entries.get(parent) != Some(&Kind::Directory) {
                return Err(io::Error::other(
                    "incremental parent inventory is unreliable",
                ));
            }
        }
    }
    Ok(())
}
fn refresh(
    root: &Path,
    path: &Path,
    inv: &mut Inventory,
    limits: Limits,
    metrics: &mut Metrics,
    topology: &mut impl FnMut(Topology) -> io::Result<()>,
) -> io::Result<()> {
    match inspect(root, path, metrics)? {
        None => remove(inv, path, root, topology)?,
        Some(Kind::File) => {
            if inv.entries.get(path) == Some(&Kind::Directory) {
                remove(inv, path, root, topology)?;
            }
            inv.entries.insert(path.to_path_buf(), Kind::File);
        }
        Some(Kind::Directory) => {
            if inv.entries.get(path) == Some(&Kind::Directory) {
                return Ok(());
            }
            remove(inv, path, root, topology)?;
            let dirs = 1 + inv
                .entries
                .values()
                .filter(|k| **k == Kind::Directory)
                .count();
            let depth = path.components().count();
            let entries = limits
                .entries
                .min(4096)
                .checked_sub(inv.entries.len() + 1)
                .ok_or_else(|| io::Error::other("subtree entry budget exhausted"))?;
            let directories = limits
                .directories
                .checked_sub(dirs)
                .ok_or_else(|| io::Error::other("subtree directory budget exhausted"))?;
            let depth = limits
                .depth
                .checked_sub(depth)
                .ok_or_else(|| io::Error::other("subtree depth budget exhausted"))?;
            metrics.subtree_scans += 1;
            let scoped = watch::scan(
                &root.join(path),
                Limits {
                    entries,
                    directories,
                    depth,
                    ..limits
                },
                |dir| topology(Topology::Add(dir.to_path_buf())),
            );
            metrics.scanned_entries += scoped.examined;
            if !scoped.complete {
                return Err(io::Error::other("incomplete incremental subtree scan"));
            }
            inv.entries.insert(path.to_path_buf(), Kind::Directory);
            for (child, kind) in scoped.entries {
                inv.entries.insert(path.join(child), kind);
            }
        }
    }
    Ok(())
}
pub(crate) fn apply(
    root: &Path,
    previous: &Inventory,
    changes: &[Change],
    limits: Limits,
    metrics: &mut Metrics,
    mut topology: impl FnMut(Topology) -> io::Result<()>,
) -> io::Result<Inventory> {
    if changes.len() > limits.queue {
        return Err(io::Error::other("incremental event queue budget exhausted"));
    }
    let mut inv = previous.clone();
    inv.examined = 0;
    for change in changes {
        match change {
            Change::Remove(path) => {
                valid(path)?;
                remove(&mut inv, path, root, &mut topology)?;
            }
            Change::Refresh(path) => refresh(root, path, &mut inv, limits, metrics, &mut topology)?,
            Change::Rename { from, to } => {
                valid(from)?;
                valid(to)?;
                if from == to {
                    continue;
                }
                if from.starts_with(to) || to.starts_with(from) {
                    return Err(io::Error::other("overlapping rename paths"));
                }
                let target = inspect(root, to, metrics)?;
                let old = inv.entries.get(from).copied();
                if target.is_none() {
                    remove(&mut inv, from, root, &mut topology)?;
                    continue;
                }
                if old.is_none() {
                    refresh(root, to, &mut inv, limits, metrics, &mut topology)?;
                    continue;
                }
                if old != target {
                    return Err(io::Error::other(
                        "rename kind mismatch; correction required",
                    ));
                }
                remove(&mut inv, to, root, &mut topology)?;
                let moved: Vec<_> = inv
                    .entries
                    .iter()
                    .filter(|(p, _)| p.starts_with(from))
                    .map(|(p, k)| (p.clone(), *k))
                    .collect();
                for (path, kind) in moved {
                    inv.entries.remove(&path);
                    let tail = path.strip_prefix(from).map_err(io::Error::other)?;
                    inv.entries.insert(to.join(tail), kind);
                }
                topology(Topology::Rename {
                    from: root.join(from),
                    to: root.join(to),
                })?;
            }
        }
        budget(&inv, limits)?;
    }
    budget(&inv, limits)?;
    metrics.transactions += 1;
    metrics.changed_paths += previous
        .entries
        .keys()
        .chain(inv.entries.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|p| previous.entries.get(*p) != inv.entries.get(*p))
        .count();
    inv.directories = 1 + inv
        .entries
        .values()
        .filter(|k| **k == Kind::Directory)
        .count();
    inv.complete = true;
    inv.errors.clear();
    Ok(inv)
}
pub struct Portable {
    pub root: PathBuf,
    pub limits: Limits,
    pub state: Recovery,
    pub store: Store,
    pub gate: Gate,
    pub metrics: Metrics,
    pending: Vec<Change>,
    correction: bool,
}
impl Portable {
    pub fn new(root: &Path, limits: Limits) -> io::Result<Self> {
        validate_limits(limits)?;
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(io::Error::other("root must be a directory"));
        }
        Ok(Self {
            root,
            limits,
            state: Recovery::new(limits.queue),
            store: Store::new(),
            gate: Gate::default(),
            metrics: Metrics::default(),
            pending: vec![],
            correction: true,
        })
    }
    pub fn enqueue(&mut self, change: Change) {
        if self.pending.len() >= self.limits.queue {
            self.invalidate(Signal::UserOverflow);
            return;
        }
        self.pending.push(change);
        self.state.signal(Signal::Change);
        self.store.observe(&self.state);
        if self.state.reasons.contains(&Signal::UserOverflow) {
            self.correction = true;
        }
    }
    pub fn invalidate(&mut self, signal: Signal) {
        self.pending.clear();
        self.correction = true;
        self.state.signal(signal);
        self.store.observe(&self.state);
    }
    pub fn restore(&mut self, inventory: Inventory) {
        self.state = Recovery::restored(inventory, self.limits.queue);
        self.pending.clear();
        self.correction = true;
        self.store.observe(&self.state);
    }
    pub fn tick(&mut self, now: Duration) -> io::Result<bool> {
        self.tick_with_hook(now, |_| {})
    }
    pub fn tick_with_hook(
        &mut self,
        now: Duration,
        mut after_apply: impl FnMut(&mut Recovery),
    ) -> io::Result<bool> {
        self.store.observe(&self.state);
        if !self.state.dirty && !self.store.needs_update() {
            return Ok(true);
        }
        if !self.gate.ready(now) {
            return Ok(false);
        }
        let attempt = (|| {
            if self.correction {
                self.pending.clear();
                let mut ok = false;
                for _ in 0..self.limits.retries {
                    let ticket = self.state.ticket();
                    self.metrics.full_scans += 1;
                    let next = watch::scan(&self.root, self.limits, |_| Ok(()));
                    self.metrics.scanned_entries += next.examined;
                    after_apply(&mut self.state);
                    if self.state.publish(ticket, next) {
                        ok = true;
                        break;
                    }
                    if self.state.reasons.contains(&Signal::ScanIncomplete) {
                        break;
                    }
                }
                if !ok {
                    self.state.signal(Signal::RetryLimit);
                    self.store.observe(&self.state);
                    return Ok(false);
                }
                self.correction = false;
            } else {
                let ticket = self.state.ticket();
                let changes = std::mem::take(&mut self.pending);
                let next = apply(
                    &self.root,
                    &self.state.inventory,
                    &changes,
                    self.limits,
                    &mut self.metrics,
                    |edit| {
                        match edit {
                            Topology::Add(path) | Topology::Remove(path) => drop(path),
                            Topology::Rename { from, to } => drop((from, to)),
                        }
                        Ok(())
                    },
                )?;
                after_apply(&mut self.state);
                if !self.state.publish(ticket, next) {
                    self.correction = true;
                    self.store.observe(&self.state);
                    return Ok(false);
                }
            }
            if !self.store.stage_delta(&self.state)? {
                return Ok(false);
            }
            Ok(self.store.commit(&self.state))
        })();
        match attempt {
            Ok(ok) => {
                self.gate.completed(now, ok);
                Ok(ok)
            }
            Err(error) => {
                self.correction = true;
                self.state.signal(Signal::ScanIncomplete);
                self.store.observe(&self.state);
                self.store.fail(&error);
                self.gate.completed(now, false);
                Err(error)
            }
        }
    }
}
#[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
mod native;
#[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
pub use native::Native;
