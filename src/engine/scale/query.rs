use super::inventory::Data;
use crate::engine::{QueryPage, QueryResult, Status, View, MAX_PAGE_SIZE};
use crate::index::Query;
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
    pub fn new() -> Self {
        Self {
            handle: Handle {
                shared: Arc::new(Mutex::new(Shared {
                    snapshot: None,
                    retired: Weak::new(),
                    view: View {
                        version: 0,
                        status: Status::Empty,
                        leases: 0,
                    },
                })),
            },
        }
    }
    pub fn status(&self, status: Status) {
        self.handle.shared.lock().unwrap().view.status = status;
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
    pub(super) fn publish(&self, data: Data) -> bool {
        if !self.ready() {
            return false;
        }
        let mut shared = self.handle.shared.lock().unwrap();
        shared.view.version += 1;
        let version = shared.view.version;
        if let Some(old) = shared.snapshot.take() {
            shared.retired = Arc::downgrade(&old);
        }
        shared.snapshot = Some(Arc::new(Snapshot { data, version }));
        shared.view.status = Status::Validated;
        true
    }
}
impl Handle {
    pub fn view(&self) -> View {
        self.shared.lock().unwrap().view.clone()
    }
    pub fn lease(&self) -> io::Result<Lease> {
        let mut shared = self.shared.lock().unwrap();
        if shared.view.leases >= crate::live::MAX_LEASES {
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
            let text = path.to_str().ok_or_else(|| {
                io::Error::other("scale text query requires UTF-8 names at this stage")
            })?;
            if query.matches(&format!("/{text}")) {
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
