//! Parent/name entry inventory behind the public Engine facade. Initial small
//! native mode; persistence and increased scale budgets are separate milestones.
mod inventory;
pub(super) mod query;
use super::{EngineOptions, Source, Status};
use crate::events::{Change, EventSource, SourceState};
use crate::incremental::{Metrics, Topology};
use inventory::{EntryId, Inventory, Kind};
use std::fs::{self, Metadata, ReadDir};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

struct Directory {
    id: EntryId,
    path: PathBuf,
    depth: usize,
}
struct Scan {
    inventory: Inventory,
    todo: Vec<Directory>,
    current: Option<(Directory, ReadDir)>,
    generation: u64,
}
pub(super) struct Runtime {
    root: PathBuf,
    source: Source,
    options: EngineOptions,
    inventory: Inventory,
    scan: Option<Scan>,
    pending: Vec<Change>,
    correction: bool,
    stopped: bool,
    generation: u64,
    pub store: query::Store,
    pub metrics: Metrics,
}
impl Runtime {
    pub fn new(root: &Path, source: Source, options: EngineOptions) -> io::Result<Self> {
        crate::incremental::validate_limits(options.limits)?;
        if options.scan_batch == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scan batch must be positive",
            ));
        }
        Ok(Self {
            root: root.to_path_buf(),
            source,
            options,
            inventory: Inventory::new(0),
            scan: None,
            pending: vec![],
            correction: true,
            stopped: false,
            generation: 1,
            store: query::Store::new(),
            metrics: Metrics::default(),
        })
    }
    pub fn fail(&mut self, error: &io::Error) {
        self.store.status(Status::Failed(
            error.to_string().chars().take(256).collect(),
        ));
        self.correction = true;
        self.scan = None;
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.stopped = true;
        self.pending.clear();
        self.scan = None;
        self.store.status(Status::Stopped);
        self.source.stop()
    }
    fn capture(&mut self) -> io::Result<()> {
        let batch = self.source.poll()?;
        if batch.state == SourceState::Stopped {
            return self.stop();
        }
        if !batch.losses.is_empty() {
            self.pending.clear();
            self.scan = None;
            self.correction = true;
            self.generation += 1;
            self.store.status(Status::Pending);
            return Ok(());
        }
        for change in &batch.changes {
            match change {
                Change::Refresh(path) | Change::Remove(path) => crate::incremental::valid(path)?,
                Change::Rename { from, to } => {
                    crate::incremental::valid(from)?;
                    crate::incremental::valid(to)?;
                }
            }
        }
        if !batch.changes.is_empty() {
            self.generation += 1;
            self.store.status(Status::Pending);
            if self.pending.len().saturating_add(batch.changes.len()) > self.options.limits.queue {
                self.pending.clear();
                self.scan = None;
                self.correction = true;
            } else {
                self.pending.extend(batch.changes);
            }
        }
        Ok(())
    }
    pub fn poll(&mut self) -> io::Result<bool> {
        if self.stopped {
            return Ok(false);
        }
        self.capture()?;
        if self.stopped {
            return Ok(false);
        }
        if !self.correction && self.pending.is_empty() {
            return Ok(self.store.handle.view().status == Status::Validated);
        }
        if !self.store.ready() {
            return Ok(false);
        }
        if self.correction {
            return self.correct();
        }
        let generation = self.generation;
        self.inventory.reset_work();
        let changes = std::mem::take(&mut self.pending);
        if crate::incremental::requires_reconcile(&self.root, &changes, &mut self.metrics)? {
            self.correction = true;
            return Ok(false);
        }
        for change in changes {
            if let Err(error) = self.apply(change) {
                self.fail(&error);
                return Ok(false);
            }
        }
        self.capture()?;
        if self.stopped {
            return Ok(false);
        }
        if generation != self.generation {
            self.correction = true;
            return Ok(false);
        }
        self.metrics.transactions += 1;
        self.metrics.last_touched_entries = self.inventory.touched;
        self.metrics.last_copied_entries = self.inventory.copied_entries;
        self.metrics.last_copied_segments = self.inventory.copied_segments;
        Ok(self.store.publish(self.inventory.data.clone()))
    }
    fn correct(&mut self) -> io::Result<bool> {
        if self.scan.is_none() {
            self.source.begin_reconcile()?;
            self.pending.clear();
            self.metrics.full_scans += 1;
            self.scan = Some(Scan {
                inventory: Inventory::new(self.inventory.data.epoch + 1),
                todo: vec![Directory {
                    id: 0,
                    path: self.root.clone(),
                    depth: 0,
                }],
                current: None,
                generation: self.generation,
            });
        }
        let mut scan = self.scan.take().unwrap();
        let mut processed = 0;
        while processed < self.options.scan_batch {
            if scan.current.is_none() {
                let Some(directory) = scan.todo.pop() else {
                    break;
                };
                if directory.depth > self.options.limits.depth {
                    return Err(io::Error::other("scale directory depth exhausted"));
                }
                self.source.before_directory(&directory.path)?;
                let listing = fs::read_dir(&directory.path)?;
                scan.current = Some((directory, listing));
            }
            let (directory, listing) = scan.current.as_mut().unwrap();
            let Some(child) = listing.next() else {
                scan.current = None;
                continue;
            };
            let child = child?;
            processed += 1;
            self.metrics.scanned_entries += 1;
            if crate::watch::skip(&child.path()) {
                continue;
            }
            let metadata = fs::symlink_metadata(child.path())?;
            self.metrics.metadata_calls += 1;
            let Some(kind) = kind(&metadata) else {
                continue;
            };
            let name = child.file_name();
            if name.to_str().is_none() {
                return Err(io::Error::other(
                    "scale text mode requires UTF-8 names at this stage",
                ));
            }
            let id = scan.inventory.insert(
                directory.id,
                name.as_bytes(),
                kind,
                metadata.dev(),
                metadata.ino(),
            )?;
            if scan.inventory.entries > self.options.limits.entries
                || scan.inventory.directories > self.options.limits.directories
            {
                return Err(io::Error::other("scale inventory budget exhausted"));
            }
            if kind == Kind::Directory {
                scan.todo.push(Directory {
                    id,
                    path: child.path(),
                    depth: directory.depth + 1,
                });
            }
        }
        self.capture()?;
        if self.stopped {
            return Ok(false);
        }
        if scan.generation != self.generation {
            self.scan = None;
            self.store.status(Status::Pending);
            return Ok(false);
        }
        if scan.current.is_some() || !scan.todo.is_empty() {
            self.scan = Some(scan);
            return Ok(false);
        }
        self.inventory = scan.inventory;
        self.correction = false;
        Ok(self.store.publish(self.inventory.data.clone()))
    }
    fn inspect(&mut self, relative: &Path) -> io::Result<Option<(Kind, Metadata)>> {
        let mut path = self.root.clone();
        let mut result = None;
        for component in relative.components() {
            path.push(component.as_os_str());
            self.metrics.metadata_calls += 1;
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(error) => return Err(error),
            };
            let Some(kind) = kind(&metadata) else {
                return Ok(None);
            };
            result = Some((kind, metadata));
        }
        Ok(result)
    }
    fn remove(&mut self, path: &Path) -> io::Result<()> {
        if let Some(id) = self.inventory.find(path) {
            if self.inventory.data.entry(id).kind == Kind::Directory {
                self.source.topology(Topology::Remove(path.to_path_buf()))?;
            }
            self.inventory.remove(id);
            self.metrics.changed_paths += 1;
        }
        Ok(())
    }
    fn refresh(&mut self, path: &Path) -> io::Result<()> {
        if crate::watch::skip(path) {
            return self.remove(path);
        }
        let Some((kind, metadata)) = self.inspect(path)? else {
            return self.remove(path);
        };
        if let Some(id) = self.inventory.find(path) {
            let old = self.inventory.data.entry(id);
            if old.dev != metadata.dev() || old.ino != metadata.ino() || old.kind != kind {
                return Err(io::Error::other(
                    "object identity changed; correction required",
                ));
            }
            return Ok(());
        }
        if kind == Kind::Directory {
            // New/moved-in subtrees need watch-before-enumeration. The initial
            // small mode obtains that through correction rather than guessing.
            return Err(io::Error::other(
                "new directory requires watched correction",
            ));
        }
        let parent = self
            .inventory
            .find(path.parent().unwrap_or(Path::new("")))
            .ok_or_else(|| io::Error::other("unknown parent; correction required"))?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("entry name missing"))?;
        if name.to_str().is_none() {
            return Err(io::Error::other(
                "scale text mode requires UTF-8 names at this stage",
            ));
        }
        if self.inventory.entries >= self.options.limits.entries {
            return Err(io::Error::other("scale entry budget exhausted"));
        }
        self.inventory.insert(
            parent,
            name.as_bytes(),
            kind,
            metadata.dev(),
            metadata.ino(),
        )?;
        self.metrics.changed_paths += 1;
        Ok(())
    }
    fn apply(&mut self, change: Change) -> io::Result<()> {
        match change {
            Change::Remove(path) => self.remove(&path),
            Change::Refresh(path) => self.refresh(&path),
            Change::Rename { from, to } => {
                if from == to {
                    return Ok(());
                }
                if from.starts_with(&to) || to.starts_with(&from) {
                    return Err(io::Error::other("overlapping rename; correction required"));
                }
                let Some((kind, metadata)) = self.inspect(&to)? else {
                    return self.remove(&from);
                };
                let Some(id) = self.inventory.find(&from) else {
                    self.remove(&to)?;
                    return self.refresh(&to);
                };
                let old = self.inventory.data.entry(id);
                if old.kind != kind || old.dev != metadata.dev() || old.ino != metadata.ino() {
                    return Err(io::Error::other(
                        "ambiguous rename identity; correction required",
                    ));
                }
                if crate::watch::skip(&to) {
                    return self.remove(&from);
                }
                let parent = self
                    .inventory
                    .find(to.parent().unwrap_or(Path::new("")))
                    .ok_or_else(|| {
                        io::Error::other("unknown rename parent; correction required")
                    })?;
                let name = to
                    .file_name()
                    .ok_or_else(|| io::Error::other("rename name missing"))?;
                self.remove(&to)?;
                self.inventory.rename(id, parent, name.as_bytes())?;
                if kind == Kind::Directory {
                    self.source.topology(Topology::Rename { from, to })?;
                }
                self.metrics.changed_paths += 1;
                Ok(())
            }
        }
    }
}
fn kind(metadata: &Metadata) -> Option<Kind> {
    if metadata.is_dir() {
        Some(Kind::Directory)
    } else if metadata.is_file() {
        Some(Kind::File)
    } else {
        None
    }
}
