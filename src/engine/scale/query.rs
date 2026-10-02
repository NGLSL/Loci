use super::inventory::{Data, Kind};
use crate::engine::{QueryPage, QueryResult, Status, View, MAX_PAGE_SIZE};
use crate::index::Query;
use std::collections::HashSet;
use std::io;
use std::os::unix::ffi::OsStrExt;
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
    pub fn new(budgets: crate::engine::ScaleBudgets) -> Self {
        Self {
            handle: Handle {
                shared: Arc::new(Mutex::new(Shared {
                    snapshot: None,
                    retired: Weak::new(),
                    budgets,
                    view: View {
                        version: 0,
                        status: Status::Empty,
                        leases: 0,
                        coverage_gaps: vec![],
                        resources: crate::engine::Resources::default(),
                    },
                })),
            },
        }
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
    pub fn status(&self, status: Status) {
        self.handle.shared.lock().unwrap().view.status = status;
    }
    pub fn resources(&self, resources: crate::engine::Resources) {
        self.handle.shared.lock().unwrap().view.resources = resources;
    }
    pub fn gap(&self, gap: crate::engine::CoverageGap) {
        let mut shared = self.handle.shared.lock().unwrap();
        if shared.view.coverage_gaps.len() < 256 {
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
        shared.view.version += 1;
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
        self.shared.lock().unwrap().view.clone()
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
        let mut offset = cursor.map_or(1, |cursor| cursor.offset);
        let mut paths = Vec::with_capacity(size);
        let mut visited = 0;
        let mut cancelled = false;
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
            let path = self.snapshot.data.path(id);
            let mut raw_path = vec![b'/'];
            raw_path.extend_from_slice(path.as_os_str().as_bytes());
            if query.matches_raw(&raw_path) {
                paths.push(path);
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
