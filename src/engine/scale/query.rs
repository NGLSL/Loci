use super::inventory::{Data, Kind};
use crate::engine::{QueryPage, QueryResult, Status, View, MAX_PAGE_SIZE};
use crate::index::Query;
use std::collections::HashSet;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

struct Snapshot {
    data: Data,
    version: u64,
}
struct Shared {
    snapshot: Option<Arc<Snapshot>>,
    retired: Weak<Snapshot>,
    view: View,
    budgets: crate::engine::ScaleBudgets,
    _memory: Arc<super::memory::Reservation>,
}
#[derive(Clone)]
pub(crate) struct Handle {
    shared: Arc<Mutex<Shared>>,
}
pub(crate) struct Lease {
    shared: Arc<Mutex<Shared>>,
    snapshot: Arc<Snapshot>,
    started: View,
}
#[derive(Clone)]
pub(crate) struct Cursor {
    snapshot: Weak<Snapshot>,
    query: String,
    offset: usize,
}
pub(crate) struct Store {
    pub handle: Handle,
}
impl Store {
    pub fn new(
        budgets: crate::engine::ScaleBudgets,
        memory: Arc<super::memory::Reservation>,
    ) -> Self {
        Self {
            handle: Handle {
                shared: Arc::new(Mutex::new(Shared {
                    snapshot: None,
                    retired: Weak::new(),
                    budgets,
                    _memory: memory,
                    view: View {
                        version: 0,
                        status: Status::Empty,
                        leases: 0,
                        coverage_gaps: vec![],
                        resources: crate::engine::Resources::default(),
                        observed_losses: Default::default(),
                    },
                })),
            },
        }
    }
    pub(super) fn checkpoint(&self) -> io::Result<(Data, u64)> {
        let shared = self.handle.shared.lock().unwrap();
        if shared.view.status != Status::Validated || !shared.view.coverage_gaps.is_empty() {
            return Err(io::Error::other(
                "checkpoint requires validated complete coverage",
            ));
        }
        let snapshot = shared
            .snapshot
            .as_ref()
            .ok_or_else(|| io::Error::other("no checkpoint snapshot"))?;
        Ok((snapshot.data.clone(), snapshot.version))
    }
    pub(super) fn seed(&self, data: Data, version: u64) -> io::Result<()> {
        let mut shared = self.handle.shared.lock().unwrap();
        if data.allocated_bytes() > shared.budgets.max_snapshot_bytes
            || data.allocated_bytes() > shared.budgets.max_retained_bytes
        {
            return Err(io::Error::other("loaded checkpoint memory budget"));
        }
        shared.snapshot = Some(Arc::new(Snapshot { data, version }));
        shared.view.version = version;
        shared.view.status = Status::Pending;
        Ok(())
    }
    pub(super) fn retained_bytes(&self, writer: &Data, candidate: Option<&Data>) -> usize {
        let shared = self.handle.shared.lock().unwrap();
        let mut seen = HashSet::new();
        let mut bytes = writer.accounted_bytes(&mut seen);
        if let Some(candidate) = candidate {
            bytes += candidate.accounted_bytes(&mut seen);
        }
        if let Some(current) = &shared.snapshot {
            bytes += current.data.accounted_bytes(&mut seen);
        }
        if let Some(retired) = shared.retired.upgrade() {
            bytes += retired.data.accounted_bytes(&mut seen);
        }
        bytes
    }
    pub(super) fn allocation_credit(
        &self,
        writer: &Data,
        candidate: Option<&Data>,
    ) -> io::Result<usize> {
        let bytes = self.retained_bytes(writer, candidate);
        self.handle
            .shared
            .lock()
            .unwrap()
            .budgets
            .max_retained_bytes
            .checked_sub(bytes)
            .ok_or_else(|| io::Error::other("retained snapshot byte budget exhausted"))
    }
    pub fn losses(&self, losses: &std::collections::BTreeSet<crate::events::Loss>) {
        self.handle
            .shared
            .lock()
            .unwrap()
            .view
            .observed_losses
            .extend(losses);
    }
    pub fn status(&self, status: Status) {
        self.handle.shared.lock().unwrap().view.status = status;
    }
    pub fn resources(&self, resources: crate::engine::Resources) {
        self.handle.shared.lock().unwrap().view.resources = resources;
    }
    pub fn gap(&self, gap: crate::engine::CoverageGap) {
        let mut shared = self.handle.shared.lock().unwrap();
        if shared.view.coverage_gaps.len() < 256
            && !shared
                .view
                .coverage_gaps
                .iter()
                .any(|old| old.path == gap.path && old.kind == gap.kind)
        {
            shared.view.coverage_gaps.push(gap);
        }
    }
    pub fn clear_gaps(&self) {
        self.handle
            .shared
            .lock()
            .unwrap()
            .view
            .coverage_gaps
            .clear();
    }
    pub fn ready(&self) -> bool {
        let mut shared = self.handle.shared.lock().unwrap();
        if shared.retired.strong_count() > 0 {
            shared.view.status = Status::ReadersPinned;
            false
        } else {
            true
        }
    }
    pub(super) fn publish(&self, data: Data) -> io::Result<bool> {
        if !self.ready() {
            return Ok(false);
        }
        self.allocation_credit(&data, None)?;
        let mut shared = self.handle.shared.lock().unwrap();
        if data.allocated_bytes() > shared.budgets.max_snapshot_bytes {
            return Err(io::Error::other("snapshot byte budget exhausted"));
        }
        shared.view.version = shared.view.version.checked_add(1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "snapshot version exhausted; rebuild required",
            )
        })?;
        let version = shared.view.version;
        if let Some(old) = shared.snapshot.take() {
            shared.retired = Arc::downgrade(&old);
        }
        shared.snapshot = Some(Arc::new(Snapshot { data, version }));
        shared.view.status = Status::Validated;
        Ok(true)
    }
}
impl Handle {
    pub fn view(&self) -> View {
        let mut view = self.shared.lock().unwrap().view.clone();
        view.resources.process_memory_reserved_bytes = super::memory::reserved_bytes();
        view
    }
    pub fn lease(&self) -> io::Result<Lease> {
        let mut shared = self.shared.lock().unwrap();
        if shared.view.leases >= shared.budgets.max_leases {
            return Err(io::Error::other("query lease budget exhausted"));
        }
        let snapshot = shared
            .snapshot
            .clone()
            .ok_or_else(|| io::Error::other("no published scale snapshot yet"))?;
        shared.view.leases += 1;
        Ok(Lease {
            shared: self.shared.clone(),
            snapshot,
            started: shared.view.clone(),
        })
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.shared.lock().unwrap().view.leases -= 1;
    }
}
impl Lease {
    pub fn entry_kind(&self, relative: &std::path::Path) -> io::Result<crate::engine::EntryKind> {
        for id in 1..self.snapshot.data.slots {
            let entry = self.snapshot.data.entry(id as u32);
            if entry.alive && self.snapshot.data.path(id as u32) == relative {
                return Ok(match entry.kind {
                    Kind::File => crate::engine::EntryKind::File,
                    Kind::Directory => crate::engine::EntryKind::Directory,
                    Kind::Symlink => crate::engine::EntryKind::Symlink,
                });
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "entry absent from snapshot",
        ))
    }

    fn validated(&self, finished: &View) -> bool {
        self.started.status == Status::Validated
            && finished.status == Status::Validated
            && self.started.version == self.snapshot.version
            && finished.version == self.snapshot.version
    }
    /// Increasing entry ID order; slots retired in this immutable snapshot are skipped.
    pub fn page(
        &self,
        raw: &str,
        cursor: Option<&Cursor>,
        size: usize,
        cancel: &AtomicBool,
        progress: &AtomicUsize,
    ) -> io::Result<QueryPage> {
        if raw.len() > 512 || !(1..=MAX_PAGE_SIZE).contains(&size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "query/page budget exceeded",
            ));
        }
        let weak = Arc::downgrade(&self.snapshot);
        if cursor
            .is_some_and(|cursor| cursor.query != raw || !Weak::ptr_eq(&cursor.snapshot, &weak))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cursor belongs to a different snapshot or query (or has expired)",
            ));
        }
        let query = Query::parse(raw);
        let (grams, short, pairs) = query.filter();
        let mut offset = cursor.map_or(1, |cursor| cursor.offset);
        let mut paths = Vec::with_capacity(size);
        let mut kinds = Vec::with_capacity(size);
        let mut visited = 0;
        let mut cancelled = false;
        let mut normalized = Vec::new();
        while offset < self.snapshot.data.slots {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let id = offset as u32;
            offset += 1;
            visited += 1;
            if !self.snapshot.data.entry(id).alive {
                continue;
            }
            if self.matches(id, &query, grams, &short, &pairs, &mut normalized) {
                paths.push(self.snapshot.data.path(id));
                kinds.push(self.kind(id));
            }
            if visited % 64 == 0 {
                progress.store(visited, Ordering::Release);
            }
            if paths.len() == size {
                break;
            }
        }
        progress.store(visited, Ordering::Release);
        let complete = offset >= self.snapshot.data.slots && !cancelled;
        let finished = self.shared.lock().unwrap().view.clone();
        Ok(QueryPage {
            version: self.snapshot.version,
            started: self.started.clone(),
            validated_at_start_and_finish: self.validated(&finished),
            finished,
            paths,
            kinds: Some(kinds),
            cancelled,
            complete,
            next: (!complete).then(|| crate::engine::PageCursor {
                cursor: crate::engine::CursorBackend::Scale(Cursor {
                    snapshot: weak,
                    query: raw.to_owned(),
                    offset,
                }),
            }),
        })
    }
    fn matches(
        &self,
        id: u32,
        query: &Query,
        grams: u128,
        short: &crate::signatures::ShortSignature,
        pairs: &crate::signatures::PairSignature,
        normalized: &mut Vec<u8>,
    ) -> bool {
        let data = &self.snapshot.data;
        let entry = data.entry(id);
        if query.unfiltered() {
            return entry.alive;
        }
        if !entry.alive || !data.search.admits(id, grams, short, pairs) {
            return false;
        }
        data.search
            .fill_path(entry.parent, data.name(id), normalized);
        query.matches_normalized(normalized, data.name(id))
    }
    fn kind(&self, id: u32) -> crate::engine::EntryKind {
        match self.snapshot.data.entry(id).kind {
            Kind::File => crate::engine::EntryKind::File,
            Kind::Directory => crate::engine::EntryKind::Directory,
            Kind::Symlink => crate::engine::EntryKind::Symlink,
        }
    }
    pub(crate) fn sort_budget(&self) -> usize {
        self.shared.lock().unwrap().budgets.max_sort_bytes
    }
    pub(crate) fn stopped(&self) -> bool {
        self.shared.lock().unwrap().view.status == Status::Stopped
    }
    pub(crate) fn path(&self, id: u32) -> std::path::PathBuf {
        self.snapshot.data.path(id)
    }
    /// Stream matching IDs without resolving any result paths. Return false on
    /// cancellation; callbacks own their bounded accumulation policy.
    pub(crate) fn visit_matches(
        &self,
        raw: &str,
        cancel: &AtomicBool,
        progress: &AtomicUsize,
        mut matched: impl FnMut(u32) -> io::Result<()>,
    ) -> io::Result<bool> {
        if raw.len() > 512 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "query budget exceeded",
            ));
        }
        let query = Query::parse(raw);
        let (grams, short, pairs) = query.filter();
        let mut normalized = Vec::new();
        for id in 1..self.snapshot.data.slots as u32 {
            if id % 64 == 0 {
                progress.store(id as usize, Ordering::Release);
                if cancel.load(Ordering::Acquire)
                    || self.shared.lock().unwrap().view.status == Status::Stopped
                {
                    return Ok(false);
                }
            }
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            if self.matches(id, &query, grams, &short, &pairs, &mut normalized) {
                matched(id)?;
            }
        }
        progress.store(
            self.snapshot.data.slots.saturating_sub(1),
            Ordering::Release,
        );
        Ok(!cancel.load(Ordering::Acquire)
            && self.shared.lock().unwrap().view.status != Status::Stopped)
    }
    pub(crate) fn sorted_page(
        &self,
        ids: &[u32],
        offset: usize,
        size: usize,
    ) -> io::Result<QueryPage> {
        if !(1..=MAX_PAGE_SIZE).contains(&size) || offset > ids.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sorted page budget/offset",
            ));
        }
        let end = offset.saturating_add(size).min(ids.len());
        let finished = self.shared.lock().unwrap().view.clone();
        Ok(QueryPage {
            version: self.snapshot.version,
            started: self.started.clone(),
            validated_at_start_and_finish: self.validated(&finished),
            finished,
            paths: ids[offset..end].iter().map(|id| self.path(*id)).collect(),
            kinds: Some(ids[offset..end].iter().map(|id| self.kind(*id)).collect()),
            cancelled: false,
            complete: end == ids.len(),
            next: None,
        })
    }
    pub fn search(
        &self,
        raw: &str,
        first50: bool,
        cancel: &AtomicBool,
        progress: &AtomicUsize,
    ) -> io::Result<QueryResult> {
        let mut cursor = None;
        let mut paths = Vec::new();
        let mut matches = 0;
        let (cancelled, complete, finished) = loop {
            let page = self.page(raw, cursor.as_ref(), 50, cancel, progress)?;
            matches += page.paths.len();
            if paths.len() < 50 {
                paths.extend(page.paths.into_iter().take(50 - paths.len()));
            }
            if page.cancelled || page.complete || first50 {
                break (page.cancelled, page.complete, page.finished);
            }
            cursor = page.next.map(|cursor| match cursor.cursor {
                crate::engine::CursorBackend::Scale(cursor) => cursor,
                _ => unreachable!(),
            });
        };
        Ok(QueryResult {
            version: self.snapshot.version,
            started: self.started.clone(),
            validated_at_start_and_finish: self.validated(&finished),
            finished,
            paths,
            matches,
            cancelled,
            complete,
        })
    }
}
