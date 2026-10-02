//! Parent/name entry inventory behind the public Engine facade. Initial small
//! native mode; persistence and increased scale budgets are separate milestones.
pub(super) mod checkpoint;
mod inventory;
pub(super) mod query;
mod scope;
use super::{CoverageGap, CoverageGapKind, EngineOptions, Source, Status};
use crate::events::{Change, EventSource, Loss, SourceState};
use crate::incremental::{Metrics, Topology};
use inventory::{EntryId, Inventory, Kind};
use std::fs::{self, Metadata, ReadDir};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

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
    scope: scope::Scope,
    inventory: Inventory,
    scan: Option<Scan>,
    pending: Vec<Change>,
    pending_bytes: usize,
    correction: bool,
    stopped: bool,
    generation: u64,
    retry_after: Option<Instant>,
    failures: usize,
    cancelled: bool,
    audit_cursor: Option<EntryId>,
    last_audit: Instant,
    drain_polls: usize,
    pub store: query::Store,
    pub metrics: Metrics,
}
impl Runtime {
    pub fn new(root: &Path, source: Source, options: EngineOptions) -> io::Result<Self> {
        if options.limits.entries == 0
            || options.limits.entries > 1_000_000
            || options.limits.directories == 0
            || options.limits.directories > 65536
            || options.limits.depth == 0
            || options.limits.depth > 256
            || options.limits.queue == 0
            || options.limits.queue > 65536
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scale limits exceed supported bounds",
            ));
        }
        if options.scan_batch == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scan batch must be positive",
            ));
        }
        for exclusion in &options.exclusions {
            crate::incremental::valid(exclusion)?;
        }
        if options.scale_budgets.max_slots < 2
            || options.scale_budgets.max_slots > u32::MAX as usize
            || options.scale_budgets.max_name_bytes == 0
            || options.scale_budgets.max_snapshot_bytes == 0
            || options.scale_budgets.max_retained_bytes < options.scale_budgets.max_snapshot_bytes
            || options.scale_budgets.max_queue_bytes == 0
            || !(1..=crate::live::MAX_LEASES).contains(&options.scale_budgets.max_leases)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid scale memory/query budgets",
            ));
        }
        if !(1..=16).contains(&options.recovery.retry_limit)
            || !(1..=1024).contains(&options.recovery.audit_batch)
            || !(1..=4096).contains(&options.recovery.max_drain_polls)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid recovery/audit budget",
            ));
        }
        let budgets = options.scale_budgets;
        Ok(Self {
            root: root.to_path_buf(),
            scope: scope::Scope::read(root)?,
            source,
            options,
            inventory: Inventory::empty(0, budgets),
            scan: None,
            pending: vec![],
            pending_bytes: 0,
            correction: true,
            stopped: false,
            generation: 1,
            retry_after: None,
            failures: 0,
            cancelled: false,
            audit_cursor: None,
            last_audit: Instant::now(),
            drain_polls: 0,
            store: query::Store::new(budgets),
            metrics: Metrics::default(),
        })
    }
    pub fn restore_checkpoint(
        &mut self,
        parent: &std::fs::File,
        name: &std::ffi::OsStr,
        source: (u64, u64),
    ) -> io::Result<bool> {
        let Some((inventory, version)) = checkpoint::load(
            parent,
            name,
            &self.root,
            source,
            self.scope.root_mount,
            &self.options,
        )?
        else {
            return Ok(false);
        };
        self.store.seed(inventory.data.clone(), version)?;
        self.inventory = inventory;
        self.update_resources();
        Ok(true)
    }
    pub fn encode_checkpoint(&self, source: (u64, u64)) -> io::Result<Vec<u8>> {
        let (data, version) = self.store.checkpoint()?;
        checkpoint::encode(
            &self.root,
            source,
            self.scope.root_mount,
            &self.options,
            &data,
            version,
        )
    }
    pub fn request_rebuild(&mut self) -> io::Result<()> {
        if self.stopped {
            return Err(io::Error::other(
                "source stopped; reopen selected root explicitly",
            ));
        }
        self.cancelled = false;
        self.failures = 0;
        self.retry_after = None;
        self.drain_polls = 0;
        self.scan = None;
        self.clear_pending();
        self.correction = true;
        self.generation += 1;
        self.store.status(Status::Pending);
        Ok(())
    }
    fn cancel_recovery(&mut self) {
        self.cancelled = true;
        self.scan = None;
        self.clear_pending();
        self.correction = true;
        self.store.gap(CoverageGap {
            path: self.root.clone(),
            kind: CoverageGapKind::Cancelled,
            error: "correction cancelled; request rebuild to resume".into(),
            errno: None,
        });
        self.store.status(Status::Pending);
        self.update_resources();
    }
    fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_bytes = 0;
    }
    fn change_bytes(change: &Change) -> usize {
        std::mem::size_of::<Change>()
            + match change {
                Change::Refresh(path) | Change::Remove(path) => path.as_os_str().as_bytes().len(),
                Change::Rename { from, to } => {
                    from.as_os_str().as_bytes().len() + to.as_os_str().as_bytes().len()
                }
            }
    }
    fn update_resources(&self) {
        let mut resources = self.source.resources();
        resources.queued_events += self.pending.len();
        resources.queue_limit += self.options.limits.queue;
        resources.queued_event_bytes += self.pending_bytes;
        resources.queue_byte_limit += self.options.scale_budgets.max_queue_bytes;
        let inventory = self
            .scan
            .as_ref()
            .map_or(&self.inventory, |scan| &scan.inventory);
        resources.inventory_slots = inventory.data.slots;
        resources.inventory_name_bytes = inventory.name_bytes;
        resources.snapshot_bytes = inventory.data.allocated_bytes();
        resources.retained_snapshot_bytes = self.store.retained_bytes(
            &self.inventory.data,
            self.scan.as_ref().map(|scan| &scan.inventory.data),
        );
        resources.slot_limit = self.options.scale_budgets.max_slots;
        resources.name_byte_limit = self.options.scale_budgets.max_name_bytes;
        resources.snapshot_byte_limit = self.options.scale_budgets.max_snapshot_bytes;
        resources.retained_byte_limit = self.options.scale_budgets.max_retained_bytes;
        self.store.resources(resources);
    }
    pub fn check_selected_mount(&self, selected: &std::fs::File) -> io::Result<()> {
        if scope::mount_id(selected)? != self.scope.root_mount {
            return Err(io::Error::other(
                "selected root mount changed during source installation",
            ));
        }
        Ok(())
    }
    fn update_scope(&mut self) -> io::Result<()> {
        let scope = scope::Scope::read(&self.root).map_err(|error| {
            self.store.gap(CoverageGap {
                path: self.root.clone(),
                kind: if error.kind() == io::ErrorKind::PermissionDenied {
                    CoverageGapKind::Permission
                } else {
                    CoverageGapKind::ScopeUnknown
                },
                error: error.to_string(),
                errno: error.raw_os_error(),
            });
            error
        })?;
        if scope.root_mount != self.scope.root_mount {
            self.store.gap(CoverageGap {
                path: self.root.clone(),
                kind: CoverageGapKind::MountChanged,
                error: "selected root mount identity changed; reopen explicitly".into(),
                errno: None,
            });
            let _ = self.source.stop();
            self.stopped = true;
            return Err(io::Error::other(
                "selected root mount identity changed; reopen explicitly",
            ));
        }
        if scope != self.scope {
            for path in self.scope.changed_boundaries(&scope) {
                self.store.gap(CoverageGap {
                    path,
                    kind: CoverageGapKind::MountChanged,
                    error: "nested mount scope changed; correcting source".into(),
                    errno: None,
                });
            }
            self.scope = scope;
            self.generation += 1;
            self.correction = true;
            self.scan = None;
            self.clear_pending();
            self.store.status(Status::Pending);
        }
        Ok(())
    }
    fn scoped_error(&self, path: &Path, error: io::Error) -> io::Error {
        let kind = if error.raw_os_error() == Some(28) {
            CoverageGapKind::KernelWatchLimit
        } else if error.kind() == io::ErrorKind::PermissionDenied {
            CoverageGapKind::Permission
        } else if error.to_string().contains("budget") {
            CoverageGapKind::WatchBudget
        } else {
            CoverageGapKind::Scan
        };
        self.store.gap(CoverageGap {
            path: path.to_path_buf(),
            kind,
            error: error.to_string(),
            errno: error.raw_os_error(),
        });
        error
    }
    pub fn fail(&mut self, error: &io::Error) {
        if self.store.handle.view().coverage_gaps.is_empty() {
            self.store.gap(CoverageGap {
                path: self.root.clone(),
                kind: if error.kind() == io::ErrorKind::PermissionDenied {
                    CoverageGapKind::Permission
                } else if error.to_string().contains("identity changed") {
                    CoverageGapKind::SourceIdentity
                } else {
                    CoverageGapKind::Source
                },
                error: error.to_string(),
                errno: error.raw_os_error(),
            });
        }
        if error.to_string().contains("selected root identity changed") {
            self.store.gap(CoverageGap {
                path: self.root.clone(),
                kind: CoverageGapKind::SourceIdentity,
                error: error.to_string(),
                errno: error.raw_os_error(),
            });
        }
        self.update_resources();
        self.failures += 1;
        if self.failures >= self.options.recovery.retry_limit {
            self.store.gap(CoverageGap {
                path: self.root.clone(),
                kind: CoverageGapKind::RetryLimit,
                error: "finite automatic recovery attempts exhausted; request rebuild to retry"
                    .into(),
                errno: None,
            });
        }
        self.retry_after = Some(Instant::now() + self.options.recovery.retry_delay);
        self.store.status(Status::Failed(
            error.to_string().chars().take(256).collect(),
        ));
        self.correction = true;
        self.scan = None;
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.stopped = true;
        self.clear_pending();
        self.scan = None;
        self.store.status(Status::Stopped);
        let result = self.source.stop();
        self.update_resources();
        result
    }
    fn excluded(&self, relative: &Path) -> bool {
        self.options
            .exclusions
            .iter()
            .any(|excluded| relative.starts_with(excluded))
    }
    fn record_losses(&self, losses: &std::collections::BTreeSet<Loss>) {
        self.store.losses(losses);
        for loss in losses {
            let kind = match loss {
                Loss::KernelOverflow => CoverageGapKind::KernelOverflow,
                Loss::UserOverflow => CoverageGapKind::UserOverflow,
                Loss::WatchLost => CoverageGapKind::WatchLost,
                Loss::InvalidEvent => CoverageGapKind::UnknownWatch,
                _ => CoverageGapKind::Source,
            };
            self.store.gap(CoverageGap {
                path: self.root.clone(),
                kind,
                error: format!("observed event loss: {loss:?}"),
                errno: None,
            });
        }
    }
    fn capture(&mut self) -> io::Result<()> {
        self.update_scope()?;
        let batch = self.source.poll()?;
        self.update_resources();
        if batch.state == SourceState::Stopped {
            if !batch.losses.is_empty() {
                self.record_losses(&batch.losses);
                self.stop()?;
                return Err(io::Error::other(
                    "event source stopped after observation loss",
                ));
            }
            return self.stop();
        }
        if !batch.losses.is_empty() {
            self.record_losses(&batch.losses);
            self.clear_pending();
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
            let bytes = batch.changes.iter().map(Self::change_bytes).sum::<usize>();
            if self.pending.len().saturating_add(batch.changes.len()) > self.options.limits.queue
                || self.pending_bytes.saturating_add(bytes)
                    > self.options.scale_budgets.max_queue_bytes
            {
                self.record_losses(&[Loss::UserOverflow].into());
                self.clear_pending();
                self.scan = None;
                self.correction = true;
            } else {
                self.pending_bytes += bytes;
                self.pending.extend(batch.changes);
            }
        }
        self.update_resources();
        Ok(())
    }
    pub fn poll_with_cancel(&mut self, cancel: &AtomicBool) -> io::Result<bool> {
        if self.failures >= self.options.recovery.retry_limit
            || self.retry_after.is_some_and(|after| Instant::now() < after)
        {
            return Ok(false);
        }
        if self.stopped {
            return Ok(false);
        }
        self.capture()?;
        if self.stopped {
            return Ok(false);
        }
        if !self.correction && self.pending.is_empty() {
            self.audit()?;
            if self.correction {
                return Ok(false);
            }
            return Ok(self.store.handle.view().status == Status::Validated);
        }
        if self.correction && cancel.load(Ordering::Relaxed) {
            self.cancel_recovery();
            return Ok(false);
        }
        if !self.store.ready() {
            return Ok(false);
        }
        if self.correction {
            if cancel.load(Ordering::Relaxed) {
                self.cancel_recovery();
            }
            if self.cancelled {
                return Ok(false);
            }
            if !self.source.recovery_ready() {
                self.drain_polls += 1;
                if self.drain_polls >= self.options.recovery.max_drain_polls {
                    let error = io::Error::other(
                        "bounded loss drain exhausted; request rebuild after source becomes quiet",
                    );
                    self.failures = self.options.recovery.retry_limit - 1;
                    self.fail(&error);
                }
                return Ok(false);
            }
            self.drain_polls = 0;
            return self.correct(cancel);
        }
        let generation = self.generation;
        self.inventory.reset_work();
        self.inventory
            .set_allocation_credit(self.store.allocation_credit(&self.inventory.data, None)?);
        let changes = std::mem::take(&mut self.pending);
        self.pending_bytes = 0;
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
        if self.correction {
            return Ok(false);
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
        let published = self.store.publish(self.inventory.data.clone())?;
        self.update_resources();
        Ok(published)
    }
    fn correct(&mut self, cancel: &AtomicBool) -> io::Result<bool> {
        if self.scan.is_none() {
            self.source.begin_reconcile()?;
            self.clear_pending();
            self.metrics.full_scans += 1;
            self.metrics.correction_attempts += 1;
            self.scan = Some(Scan {
                inventory: Inventory::new(
                    self.inventory.data.epoch.checked_add(1).ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "source epoch exhausted; rebuild required",
                        )
                    })?,
                    self.options.scale_budgets,
                )?,
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
        scan.inventory.set_allocation_credit(
            self.store
                .allocation_credit(&self.inventory.data, Some(&scan.inventory.data))?,
        );
        let mut processed = 0;
        while processed < self.options.scan_batch {
            if cancel.load(Ordering::Relaxed) {
                self.cancel_recovery();
                return Ok(false);
            }
            if scan.current.is_none() {
                let Some(directory) = scan.todo.pop() else {
                    break;
                };
                if directory.depth > self.options.limits.depth {
                    return Err(io::Error::other("scale directory depth exhausted"));
                }
                processed += 1;
                self.source
                    .before_directory(&directory.path)
                    .map_err(|error| self.scoped_error(&directory.path, error))?;
                self.update_resources();
                let listing = fs::read_dir(&directory.path)
                    .map_err(|error| self.scoped_error(&directory.path, error))?;
                scan.current = Some((directory, listing));
            }
            let (directory, listing) = scan.current.as_mut().unwrap();
            let Some(child) = listing.next() else {
                scan.current = None;
                continue;
            };
            let child = child.map_err(|error| self.scoped_error(&directory.path, error))?;
            processed += 1;
            self.metrics.scanned_entries += 1;
            if self.excluded(child.path().strip_prefix(&self.root).unwrap()) {
                continue;
            }
            let metadata = fs::symlink_metadata(child.path())
                .map_err(|error| self.scoped_error(&child.path(), error))?;
            self.metrics.metadata_calls += 1;
            let Some(kind) = kind(&metadata) else {
                continue;
            };
            let name = child.file_name();
            if scan.inventory.entries >= self.options.limits.entries {
                return Err(io::Error::other("scale live entry budget exhausted"));
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
            if kind == Kind::Directory && !self.scope.boundary(&child.path()) {
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
            self.fail(&io::Error::other(
                "correction candidate invalidated by concurrent observation; retry bounded",
            ));
            return Ok(false);
        }
        if scan.current.is_some() || !scan.todo.is_empty() {
            self.scan = Some(scan);
            self.update_resources();
            return Ok(false);
        }
        self.inventory = scan.inventory;
        self.audit_cursor = None;
        self.last_audit = Instant::now();
        self.correction = false;
        self.failures = 0;
        self.retry_after = None;
        self.store.clear_gaps();
        let published = self.store.publish(self.inventory.data.clone())?;
        self.update_resources();
        Ok(published)
    }
    fn audit(&mut self) -> io::Result<()> {
        if self.last_audit.elapsed() < self.options.recovery.audit_interval {
            return Ok(());
        }
        self.last_audit = Instant::now();
        use std::ops::Bound::{Excluded, Unbounded};
        let range = (self.audit_cursor.map_or(Unbounded, Excluded), Unbounded);
        let ids: Vec<_> = self
            .inventory
            .directory_ids
            .range(range)
            .take(self.options.recovery.audit_batch)
            .copied()
            .collect();
        if ids.is_empty() {
            self.audit_cursor = None;
            return Ok(());
        }
        for id in ids {
            self.audit_cursor = Some(id);
            let path = self.root.join(self.inventory.data.path(id));
            if self.scope.boundary(&path) {
                continue;
            }
            self.metrics.audited_directories += 1;
            self.metrics.metadata_calls += 1;
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| self.scoped_error(&path, error))?;
            let old = self.inventory.data.entry(id);
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || (id != 0 && (old.dev != metadata.dev() || old.ino != metadata.ino()))
            {
                self.store.gap(CoverageGap {
                    path,
                    kind: CoverageGapKind::UnknownWatch,
                    error: "audited directory identity changed; correcting source".into(),
                    errno: None,
                });
                self.correction = true;
                self.generation += 1;
                self.store.status(Status::Pending);
                return Ok(());
            }
            drop(fs::read_dir(&path).map_err(|error| self.scoped_error(&path, error))?);
        }
        Ok(())
    }
    fn inspect(&mut self, relative: &Path) -> io::Result<Option<(Kind, Metadata)>> {
        if self.excluded(relative) {
            return Ok(None);
        }
        let mut path = self.root.clone();
        let mut result = None;
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
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
            if components.peek().is_some()
                && (kind != Kind::Directory || self.scope.boundary(&path))
            {
                return Ok(None);
            }
            result = Some((kind, metadata));
        }
        Ok(result)
    }
    fn remove(&mut self, path: &Path) -> io::Result<()> {
        if let Some(id) = self.inventory.find(path) {
            if self.inventory.data.entry(id).kind == Kind::Directory {
                self.source.topology(Topology::Remove(path.to_path_buf()))?;
            }
            self.inventory.remove(id)?;
            self.metrics.changed_paths += 1;
        }
        Ok(())
    }
    fn refresh(&mut self, path: &Path) -> io::Result<()> {
        if self.excluded(path) {
            return self.remove(path);
        }
        let Some((kind, metadata)) = self.inspect(path)? else {
            return self.remove(path);
        };
        if kind == Kind::Directory && !self.scope.boundary(&self.root.join(path)) {
            // IN_ATTRIB preserves object identity, but a revoked listing permission
            // invalidates coverage immediately instead of waiting for the audit cycle.
            drop(
                fs::read_dir(self.root.join(path))
                    .map_err(|error| self.scoped_error(&self.root.join(path), error))?,
            );
        }
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
            self.correction = true;
            self.store.status(Status::Pending);
            return Ok(());
        }
        let parent = self
            .inventory
            .find(path.parent().unwrap_or(Path::new("")))
            .ok_or_else(|| io::Error::other("unknown parent; correction required"))?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("entry name missing"))?;
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
                if self.excluded(&to) {
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
    } else if metadata.file_type().is_symlink() {
        Some(Kind::Symlink)
    } else {
        None
    }
}
