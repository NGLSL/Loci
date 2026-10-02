//! Private Linux event source for the v0.1 engine. Watches precede enumeration.
use crate::events::{EventBatch, EventLimits, EventSource, Loss, SourceState};
use crate::incremental::{self, Change, Topology};
use crate::linux_inotify::{Session, PROCESS_WATCH_CAP};
use crate::watch::{self, RawEvent, Signal};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub(crate) struct LinuxEvents {
    root: PathBuf,
    limits: EventLimits,
    session: Option<Session>,
    stopped: bool,
    watch_limit: usize,
    scale: bool,
    draining_loss: bool,
    logical: BTreeMap<i32, PathBuf>,
    retired: BTreeSet<i32>,
    consumed_ignored: BTreeSet<i32>,
    pending: Vec<RawEvent>,
    pending_since: Option<Instant>,
}

impl LinuxEvents {
    pub(crate) fn recovery_ready(&self) -> bool {
        !self.scale || !self.draining_loss
    }
    pub(crate) fn resources(&self) -> crate::engine::Resources {
        let (process_watches, process_inotify_fds) = crate::linux_inotify::process_usage();
        crate::engine::Resources {
            observed: true,
            session_watches: self.session.as_ref().map_or(0, Session::count),
            session_watch_limit: self.watch_limit,
            process_watches,
            process_watch_limit: crate::linux_inotify::PROCESS_TOTAL_WATCH_CAP,
            inotify_fds: usize::from(self.session.is_some()),
            process_inotify_fds,
            process_fd_limit: crate::linux_inotify::PROCESS_SESSION_CAP,
            queued_events: self.pending.len(),
            queue_limit: self.limits.max_events(),
            queued_event_bytes: self.pending.iter().map(|event| 16 + event.name.len()).sum(),
            queue_byte_limit: self.limits.max_events().saturating_mul(16 + 4096),
            event_buffer_bytes: self.limits.buffer_bytes(),
            ..crate::engine::Resources::default()
        }
    }
    pub(crate) fn open(root: &Path, limits: EventLimits) -> io::Result<Self> {
        Self::open_budgeted(root, limits, PROCESS_WATCH_CAP, false)
    }
    pub(crate) fn open_scale(
        root: &Path,
        limits: EventLimits,
        watch_limit: usize,
    ) -> io::Result<Self> {
        Self::open_budgeted(root, limits, watch_limit, true)
    }
    fn open_budgeted(
        root: &Path,
        limits: EventLimits,
        watch_limit: usize,
        scale: bool,
    ) -> io::Result<Self> {
        let mut session = if scale {
            Session::new_scale(watch_limit)?
        } else {
            Session::new(watch_limit)?
        };
        session.add(root)?;
        let logical = session.paths().clone();
        Ok(Self {
            root: root.to_path_buf(),
            limits,
            session: Some(session),
            stopped: false,
            watch_limit,
            scale,
            draining_loss: false,
            logical,
            retired: BTreeSet::new(),
            consumed_ignored: BTreeSet::new(),
            pending: vec![],
            pending_since: None,
        })
    }
    pub(crate) fn before_directory(&mut self, path: &Path) -> io::Result<()> {
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| io::Error::other("Linux source stopped"))?;
        session.add(path)?;
        let wd = session
            .descriptor_for(path)
            .ok_or_else(|| io::Error::other("watch registration missing"))?;
        if self.retired.contains(&wd) {
            return Err(io::Error::other(
                "ambiguous reused logical watch descriptor",
            ));
        }
        self.logical.entry(wd).or_insert_with(|| path.to_path_buf());
        Ok(())
    }
    pub(crate) fn begin_reconcile(&mut self) -> io::Result<()> {
        if self.stopped {
            return Err(io::Error::other("Linux source stopped"));
        }
        // Release both process budgets before allocating the replacement.
        self.session.take();
        self.draining_loss = false;
        self.pending.clear();
        self.pending_since = None;
        self.logical.clear();
        self.retired.clear();
        self.consumed_ignored.clear();
        let mut next = if self.scale {
            Session::new_scale(self.watch_limit)?
        } else {
            Session::new(self.watch_limit)?
        };
        next.add(&self.root)?;
        self.logical = next.paths().clone();
        self.session = Some(next);
        Ok(())
    }
    pub(crate) fn topology(&mut self, edit: Topology) -> io::Result<()> {
        let root = &self.root;
        if let Topology::Add(path) = edit {
            return self.before_directory(&root.join(path));
        }
        let session = self
            .session
            .as_mut()
            .ok_or_else(|| io::Error::other("Linux source stopped"))?;
        let previous_ids: Vec<_> = session.paths().keys().copied().collect();
        let result = match edit {
            Topology::Add(_) => unreachable!(),
            Topology::Remove(path) => session.remove_prefix(&root.join(path)),
            Topology::Rename { from, to } => {
                let from = root.join(from);
                if session.paths().values().any(|p| p.starts_with(&from)) {
                    session.remove_prefix(&root.join(&to))?;
                    session.rename_prefix(&from, &root.join(to));
                }
                Ok(())
            }
        };
        // Engine can retire a source inode instead of renaming it when its
        // destination is excluded. Remove the exact physical descriptors even
        // when poll already moved their logical paths to that destination.
        for wd in previous_ids {
            if !session.paths().contains_key(&wd) {
                self.logical.remove(&wd);
                self.retired.insert(wd);
            }
        }
        let acknowledged: Vec<_> = self
            .consumed_ignored
            .iter()
            .copied()
            .filter(|wd| session.expected_ignored.contains(wd))
            .collect();
        session.acknowledge_ignored(&acknowledged);
        for wd in acknowledged {
            self.consumed_ignored.remove(&wd);
            self.retired.remove(&wd);
        }
        result
    }
    fn lost(&mut self, losses: BTreeSet<Loss>) -> EventBatch {
        self.pending.clear();
        self.pending_since = None;
        EventBatch {
            losses,
            ..EventBatch::default()
        }
    }
    fn retire_logical(&mut self, prefix: &Path) {
        let ids: Vec<_> = self
            .logical
            .iter()
            .filter(|(_, path)| path.starts_with(prefix))
            .map(|(wd, _)| *wd)
            .collect();
        for wd in ids {
            self.logical.remove(&wd);
            self.retired.insert(wd);
        }
    }
}

impl EventSource for LinuxEvents {
    fn poll(&mut self) -> io::Result<EventBatch> {
        let Some(session) = self.session.as_mut() else {
            if !self.stopped {
                return Err(io::Error::other(
                    "Linux event session unavailable; reopen the engine",
                ));
            }
            return Ok(EventBatch {
                state: SourceState::Stopped,
                ..EventBatch::default()
            });
        };
        let captured = match session.capture_with_buffer(
            self.limits.max_events().saturating_sub(self.pending.len()),
            self.limits.buffer_bytes(),
        ) {
            Ok(captured) => captured,
            Err(error) => {
                self.pending.clear();
                self.pending_since = None;
                return Err(error);
            }
        };
        if self.scale {
            self.draining_loss = !captured.drained && (captured.truncated || self.draining_loss);
        }
        let mut losses = BTreeSet::new();
        if captured.kernel_overflow {
            losses.insert(Loss::KernelOverflow);
        }
        if captured.truncated {
            losses.insert(Loss::UserOverflow);
        }
        if !losses.is_empty() {
            return Ok(self.lost(losses));
        }
        if self
            .pending_since
            .is_some_and(|since| since.elapsed() >= Duration::from_millis(100))
        {
            return Ok(self.lost(BTreeSet::from([Loss::UnpairedRename])));
        }
        self.pending.extend(captured.events);
        // Never use the old translator's guessed remove/refresh for an unpaired
        // rename. Preserve the complete ordered batch until its cookie is paired.
        let mut pairs = BTreeMap::<u32, (bool, bool)>::new();
        for event in &self.pending {
            if event.mask & watch::IN_Q_OVERFLOW != 0 {
                losses.insert(Loss::KernelOverflow);
            }
            if event.mask & watch::IN_UNMOUNT != 0 {
                losses.insert(Loss::WatchLost);
            }
            if self
                .logical
                .get(&event.wd)
                .is_some_and(|path| path == &self.root)
                && event.mask
                    & (watch::IN_IGNORED
                        | incremental::events::DELETE_SELF
                        | incremental::events::MOVE_SELF)
                    != 0
            {
                losses.insert(Loss::WatchLost);
            }
            let bits = event.mask & (incremental::events::FROM | incremental::events::TO);
            if bits == 0 {
                continue;
            }
            if event.cookie == 0 || bits == (incremental::events::FROM | incremental::events::TO) {
                losses.insert(Loss::InvalidEvent);
                continue;
            }
            let pair = pairs.entry(event.cookie).or_default();
            let slot = if bits == incremental::events::FROM {
                &mut pair.0
            } else {
                &mut pair.1
            };
            if *slot {
                losses.insert(Loss::InvalidEvent);
            }
            *slot = true;
        }
        if !losses.is_empty() {
            return Ok(self.lost(losses));
        }
        if pairs.values().any(|pair| *pair == (false, true)) {
            return Ok(self.lost(BTreeSet::from([Loss::UnpairedRename])));
        }
        if pairs.values().any(|pair| *pair == (true, false)) {
            let since = self.pending_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= Duration::from_millis(100) {
                return Ok(self.lost(BTreeSet::from([Loss::UnpairedRename])));
            }
            return Ok(EventBatch {
                losses: BTreeSet::from([Loss::UnpairedRename]),
                ..EventBatch::default()
            });
        }
        let translated = {
            let session = self.session.as_ref().unwrap();
            let expected: BTreeSet<_> = session
                .expected_ignored
                .union(&self.retired)
                .copied()
                .collect();
            incremental::events::translate(&self.root, &self.logical, &expected, &self.pending)
        };
        self.pending.clear();
        self.pending_since = None;
        let translated = match translated {
            Ok(translated) => translated,
            Err(signal) => {
                let loss = match signal {
                    Signal::KernelOverflow => Loss::KernelOverflow,
                    // Root retirement/unmount was detected above. A stale
                    // child descriptor or child self-event needs a fresh
                    // watch topology, not permanent engine failure.
                    Signal::WatchLost | Signal::UnknownWatch => Loss::InvalidEvent,
                    _ => Loss::InvalidEvent,
                };
                return Ok(self.lost(BTreeSet::from([loss])));
            }
        };
        // wd paths must describe the next poll even while publication is held by
        // a query lease. This updates logical topology without inspecting disk.
        for change in &translated.changes {
            match change {
                Change::Remove(path) => self.retire_logical(&self.root.join(path)),
                Change::Rename { from, to } => {
                    let from = self.root.join(from);
                    let to = self.root.join(to);
                    self.retire_logical(&to);
                    for path in self.logical.values_mut() {
                        if let Ok(tail) = path.strip_prefix(&from) {
                            *path = if tail.as_os_str().is_empty() {
                                to.clone()
                            } else {
                                to.join(tail)
                            };
                        }
                    }
                }
                Change::Refresh(_) => {}
            }
        }
        self.consumed_ignored
            .extend(translated.ignored.iter().copied());
        if self.retired.union(&self.consumed_ignored).count() > self.watch_limit {
            // A long publication gap must not grow retirement metadata without
            // bound. Engine correction discards this topology and its fd.
            self.retired.clear();
            self.consumed_ignored.clear();
            return Ok(self.lost(BTreeSet::from([Loss::InvalidEvent])));
        }
        let session = self.session.as_mut().unwrap();
        let acknowledged: Vec<_> = translated
            .ignored
            .iter()
            .copied()
            .filter(|wd| session.expected_ignored.contains(wd))
            .collect();
        session.acknowledge_ignored(&acknowledged);
        for wd in acknowledged {
            self.consumed_ignored.remove(&wd);
            self.retired.remove(&wd);
        }
        Ok(EventBatch {
            changes: translated.changes,
            ..EventBatch::default()
        })
    }
    fn stop(&mut self) -> io::Result<()> {
        self.stopped = true;
        self.session.take();
        self.pending.clear();
        self.pending_since = None;
        self.logical.clear();
        self.retired.clear();
        self.consumed_ignored.clear();
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);
    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "loci-linux-source-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed),
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            // Only these explicitly created empty directories are removed.
            let _ = std::fs::remove_dir(self.0.join("child"));
            let _ = std::fs::remove_dir(self.0.join("target"));
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    #[test]
    fn failed_reconcile_is_not_a_user_stop() {
        let root = Root::new();
        let mut source = LinuxEvents::open(&root.0, EventLimits::default()).unwrap();
        std::fs::remove_dir(&root.0).unwrap();
        let error = source.begin_reconcile().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(2)); // native add_watch ENOENT
        assert!(source.poll().is_err());
        source.stop().unwrap();
        assert_eq!(source.poll().unwrap().state, SourceState::Stopped);
    }

    #[test]
    fn retiring_renamed_inode_removes_its_advanced_logical_descriptor() {
        let root = Root::new();
        let child = root.0.join("child");
        std::fs::create_dir(&child).unwrap();
        let mut source = LinuxEvents::open(&root.0, EventLimits::default()).unwrap();
        source.before_directory(&child).unwrap();
        std::fs::rename(&child, root.0.join("target")).unwrap();
        let batch = source.poll().unwrap();
        assert!(batch.losses.is_empty());
        assert!(batch.changes.contains(&Change::Rename {
            from: "child".into(),
            to: "target".into(),
        }));
        assert!(source
            .logical
            .values()
            .any(|path| path == &root.0.join("target")));
        // Engine excludes target and retires the original physical prefix.
        source.topology(Topology::Remove("child".into())).unwrap();
        assert_eq!(source.logical.len(), 1);
        assert_eq!(source.session.as_ref().unwrap().count(), 1);
        let batch = source.poll().unwrap();
        assert!(batch.losses.is_empty());
        assert!(source.retired.is_empty());
        assert!(source.consumed_ignored.is_empty());
        source.stop().unwrap();
    }
}
