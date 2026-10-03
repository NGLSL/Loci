//! Independent bounded query workers; immediate pages never wait for these jobs.

use super::{QueryHandle, QueryLease, Status};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub const MAX_QUERY_WORKERS: usize = 2;
// Filter/lowercase/path-comparison scratch and fixed worker/result state.

static WORKERS: AtomicUsize = AtomicUsize::new(0);
struct Permit;
impl Permit {
    fn acquire() -> io::Result<Self> {
        WORKERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |workers| {
                (workers < MAX_QUERY_WORKERS).then_some(workers + 1)
            })
            .map_err(|_| {
                io::Error::new(io::ErrorKind::WouldBlock, "query worker budget exhausted")
            })?;
        Ok(Self)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryJobState {
    Pending,
    Running,
    Complete,
    Cancelled,
    Failed(String),
}

struct ResultState {
    state: QueryJobState,
    count: Option<usize>,
}
/// A cooperative worker pinned to one snapshot. Drop cancels and joins it.
/// An exact count becomes available only after Complete.
pub struct QueryJob {
    state: Arc<Mutex<ResultState>>,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicUsize>,
    version: u64,
    worker: Option<JoinHandle<()>>,
}
impl QueryJob {
    pub fn state(&self) -> QueryJobState {
        self.state.lock().unwrap().state.clone()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    /// Number of entry slots visited during filtering, independent of match count.
    pub fn progress(&self) -> usize {
        self.progress.load(Ordering::Acquire)
    }
    pub fn count(&self) -> Option<usize> {
        self.state.lock().unwrap().count
    }
    pub fn version(&self) -> u64 {
        self.version
    }
}
impl Drop for QueryJob {
    fn drop(&mut self) {
        self.cancel();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl QueryHandle {
    pub fn start_count(&self, raw: &str) -> io::Result<QueryJob> {
        self.start_job(raw)
    }

    fn start_job(&self, raw: &str) -> io::Result<QueryJob> {
        let lease = self.lease()?;
        let version = lease
            .page(raw, None, 1, &AtomicBool::new(true), &AtomicUsize::new(0))?
            .version;

        if self.view().status == Status::Stopped {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "monitor stopped; use a direct snapshot lease",
            ));
        }
        let permit = Permit::acquire()?;

        let state = Arc::new(Mutex::new(ResultState {
            state: QueryJobState::Pending,
            count: None,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicUsize::new(0));
        let shared = state.clone();
        let cancelled = cancel.clone();
        let visited = progress.clone();
        let raw = raw.to_owned();
        let handle = self.clone();
        let worker = std::thread::Builder::new()
            .name("loci-query-count".into())
            .spawn(move || {
                let _permit = permit;
                shared.lock().unwrap().state = QueryJobState::Running;
                let result = count(&lease, &handle, &raw, &cancelled, &visited);
                let mut shared = shared.lock().unwrap();
                match result {
                    Ok(Some(count))
                        if !cancelled.load(Ordering::Acquire)
                            && handle.view().status != Status::Stopped =>
                    {
                        shared.count = Some(count);
                        shared.state = QueryJobState::Complete;
                    }
                    Ok(_) => shared.state = QueryJobState::Cancelled,
                    Err(error) => {
                        shared.state =
                            QueryJobState::Failed(error.to_string().chars().take(256).collect())
                    }
                }
            })?;
        Ok(QueryJob {
            state,
            cancel,
            progress,
            version,
            worker: Some(worker),
        })
    }
}
fn count(
    lease: &QueryLease,
    handle: &QueryHandle,
    raw: &str,
    cancel: &AtomicBool,
    progress: &AtomicUsize,
) -> io::Result<Option<usize>> {
    let mut cursor = None;
    let mut count = 0;
    loop {
        if cancel.load(Ordering::Acquire) || handle.view().status == Status::Stopped {
            return Ok(None);
        }
        let page = lease.page(raw, cursor.as_ref(), 256, cancel, &AtomicUsize::new(0))?;
        count += page.paths.len();
        progress.store(count, Ordering::Release);
        if page.cancelled {
            return Ok(None);
        }
        if page.complete {
            return Ok(Some(count));
        }
        cursor = page.next;
    }
}
