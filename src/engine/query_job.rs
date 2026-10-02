//! Independent bounded query workers; immediate pages never wait for these jobs.
#[cfg(target_os = "linux")]
use super::LeaseBackend;
use super::{QueryHandle, QueryLease, QueryPage, Status};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub const MAX_QUERY_WORKERS: usize = 2;
// Filter/lowercase/path-comparison scratch and fixed worker/result state.
#[cfg(target_os = "linux")]
const JOB_SCRATCH_BYTES: usize = 512 * 1024;
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
#[cfg(target_os = "linux")]
type JobMemory = super::scale::memory::Reservation;
#[cfg(not(target_os = "linux"))]
type JobMemory = ();
struct Sorted {
    _lease: QueryLease,
    _ids: Vec<u32>,
    _memory: Option<JobMemory>,
}
struct ResultState {
    state: QueryJobState,
    count: Option<usize>,
    sorted: Option<Sorted>,
}
/// A cooperative worker pinned to one snapshot. Drop cancels and joins it.
/// An exact count becomes available only after Complete. Sorted results retain
/// their lease until this job is dropped; ordinary pages retain entry-ID order.
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
    /// Offset-based pages of a completed raw-byte lexical sort. These have no
    /// entry-ID cursor; increment offset by the number of returned paths.
    pub fn page(&self, offset: usize, size: usize) -> io::Result<QueryPage> {
        let state = self.state.lock().unwrap();
        let sorted = state.sorted.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::WouldBlock, "sorted result is not complete")
        })?;
        #[cfg(target_os = "linux")]
        if let LeaseBackend::Scale(lease) = &sorted._lease.lease {
            let mut page = lease.sorted_page(&sorted._ids, offset, size)?;
            for path in &mut page.paths {
                *path = sorted._lease.root.join(&*path);
            }
            return Ok(page);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (sorted, offset, size);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "sorted jobs require Linux scale mode",
        ))
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
        self.start_job(raw, false)
    }
    pub fn start_sort(&self, raw: &str) -> io::Result<QueryJob> {
        self.start_job(raw, true)
    }
    fn start_job(&self, raw: &str, sort: bool) -> io::Result<QueryJob> {
        let lease = self.lease()?;
        let version = lease
            .page(raw, None, 1, &AtomicBool::new(true), &AtomicUsize::new(0))?
            .version;
        if sort {
            #[cfg(target_os = "linux")]
            let supported = matches!(lease.lease, LeaseBackend::Scale(_));
            #[cfg(not(target_os = "linux"))]
            let supported = false;
            if !supported {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "sorted jobs require Linux scale mode",
                ));
            }
        }
        if self.view().status == Status::Stopped {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "monitor stopped; use a direct snapshot lease",
            ));
        }
        let permit = Permit::acquire()?;
        #[cfg(target_os = "linux")]
        let memory = match &lease.lease {
            LeaseBackend::Scale(scale) => Some(JobMemory::acquire(
                JOB_SCRATCH_BYTES + if sort { scale.sort_budget() } else { 0 },
            )?),
            _ => None,
        };
        #[cfg(not(target_os = "linux"))]
        let memory = None;
        let state = Arc::new(Mutex::new(ResultState {
            state: QueryJobState::Pending,
            count: None,
            sorted: None,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicUsize::new(0));
        let shared = state.clone();
        let cancelled = cancel.clone();
        let visited = progress.clone();
        let raw = raw.to_owned();
        let handle = self.clone();
        let worker = std::thread::Builder::new()
            .name(
                if sort {
                    "loci-query-sort"
                } else {
                    "loci-query-count"
                }
                .into(),
            )
            .spawn(move || {
                let _permit = permit;
                shared.lock().unwrap().state = QueryJobState::Running;
                let result = if sort {
                    sorted(&lease, &raw, &cancelled, &visited)
                        .map(|ids| ids.map(|ids| (ids.len(), Some(ids))))
                } else {
                    count(&lease, &handle, &raw, &cancelled, &visited)
                        .map(|count| count.map(|count| (count, None)))
                };
                let mut shared = shared.lock().unwrap();
                match result {
                    Ok(Some((count, ids)))
                        if !cancelled.load(Ordering::Acquire)
                            && handle.view().status != Status::Stopped =>
                    {
                        shared.count = Some(count);
                        shared.state = QueryJobState::Complete;
                        if let Some(ids) = ids {
                            shared.sorted = Some(Sorted {
                                _lease: lease,
                                _ids: ids,
                                _memory: memory,
                            });
                        }
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
    #[cfg(target_os = "linux")]
    if let LeaseBackend::Scale(scale) = &lease.lease {
        let mut count = 0;
        return scale
            .visit_matches(raw, cancel, progress, |_| {
                count += 1;
                Ok(())
            })
            .map(|complete| complete.then_some(count));
    }
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
fn sorted(
    lease: &QueryLease,
    raw: &str,
    cancel: &AtomicBool,
    progress: &AtomicUsize,
) -> io::Result<Option<Vec<u32>>> {
    #[cfg(target_os = "linux")]
    if let LeaseBackend::Scale(scale) = &lease.lease {
        use std::os::unix::ffi::OsStrExt;
        let budget = scale.sort_budget();
        let mut ids = Vec::new();
        if !scale.visit_matches(raw, cancel, progress, |id| {
            if ids.len() == ids.capacity() {
                let target = (ids.capacity() * 2).max(256);
                if target.saturating_mul(8) > budget {
                    return Err(io::Error::other("sort ID/scratch byte budget exhausted"));
                }
                ids.reserve_exact(target - ids.len());
            }
            ids.push(id);
            Ok(())
        })? {
            return Ok(None);
        }
        let stopped = || cancel.load(Ordering::Acquire) || scale.stopped();
        let compare = |left: &u32, right: &u32| {
            scale
                .path(*left)
                .as_os_str()
                .as_bytes()
                .cmp(scale.path(*right).as_os_str().as_bytes())
        };
        // Every comparison is consistent. Cancellation occurs between fixed-size
        // chunks and during merge, never by changing a sort comparator's order.
        for chunk in ids.chunks_mut(1024) {
            if stopped() {
                return Ok(None);
            }
            chunk.sort_unstable_by(compare);
        }
        let mut scratch = vec![0; ids.len()];
        let mut width = 1024;
        while width < ids.len() {
            for start in (0..ids.len()).step_by(width * 2) {
                let middle = (start + width).min(ids.len());
                let end = (middle + width).min(ids.len());
                let (mut left, mut right) = (start, middle);
                for out in start..end {
                    if out % 256 == 0 && stopped() {
                        return Ok(None);
                    }
                    if right >= end || (left < middle && compare(&ids[left], &ids[right]).is_le()) {
                        scratch[out] = ids[left];
                        left += 1;
                    } else {
                        scratch[out] = ids[right];
                        right += 1;
                    }
                }
            }
            std::mem::swap(&mut ids, &mut scratch);
            width *= 2;
        }
        return Ok((!stopped()).then_some(ids));
    }
    let _ = (lease, raw, cancel, progress);
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "sorted jobs require Linux scale mode",
    ))
}
