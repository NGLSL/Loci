//! Cooperative single writer. Queries retain snapshots, never this ownership.
use super::{Engine, QueryHandle, Status, View};
use crate::incremental::Metrics;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const MONITOR_POLL_INTERVAL: Duration = Duration::from_millis(20);
pub const MONITOR_COMMAND_CAPACITY: usize = 8;
pub const MONITOR_OWNER_CAPACITY: usize = 8;
static LIVE_OWNERS: AtomicUsize = AtomicUsize::new(0);
struct OwnerCredit;
impl OwnerCredit {
    fn acquire() -> io::Result<Self> {
        LIVE_OWNERS
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MONITOR_OWNER_CAPACITY).then_some(n + 1)
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "process monitor thread budget exhausted",
                )
            })?;
        Ok(Self)
    }
}
impl Drop for OwnerCredit {
    fn drop(&mut self) {
        LIVE_OWNERS.fetch_sub(1, Ordering::AcqRel);
    }
}

enum CommandKind {
    Save,
    Rebuild,
    Compact,
}
struct Command {
    kind: CommandKind,
    reply: mpsc::SyncSender<io::Result<()>>,
}
/// One admitted operation. A timeout does not cancel or imply completion.
pub struct MonitorRequest {
    receiver: mpsc::Receiver<io::Result<()>>,
}
impl MonitorRequest {
    pub fn wait(&self, timeout: Duration) -> io::Result<()> {
        self.receiver
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => io::Error::new(
                    io::ErrorKind::TimedOut,
                    "monitor operation still pending; completion is not established",
                ),
                mpsc::RecvTimeoutError::Disconnected => {
                    io::Error::other("monitor operation disconnected")
                }
            })?
    }
    pub fn try_complete(&self) -> Option<io::Result<()>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                Some(Err(io::Error::other("monitor operation disconnected")))
            }
        }
    }
}
/// Exactly one background Engine, at most eight queued commands. Drop joins;
/// a timed-out explicit stop retains ownership until a later join or Drop.
pub struct MonitorOwner {
    query: QueryHandle,
    supports_cancel: bool,
    sender: mpsc::SyncSender<Command>,
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    metrics: Arc<Mutex<Metrics>>,
    worker: Option<JoinHandle<io::Result<()>>>,
}
impl Engine {
    pub fn spawn(self) -> io::Result<MonitorOwner> {
        let credit = OwnerCredit::acquire()?;
        #[cfg(target_os = "linux")]
        let supports_cancel = self.scale.is_some();
        #[cfg(not(target_os = "linux"))]
        let supports_cancel = false;
        let query = self.query();
        let metrics = Arc::new(Mutex::new(self.metrics().clone()));
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel::<Command>(MONITOR_COMMAND_CAPACITY);
        let (worker_stop, worker_cancel, worker_metrics) =
            (stop.clone(), cancel.clone(), metrics.clone());
        let worker = thread::Builder::new().name("loci-monitor".into()).spawn(move || {
            let _credit = credit;
            let mut engine = self;
            while !worker_stop.load(Ordering::Acquire) {
                // One command per poll keeps admitted commands from starving capture.
                match receiver.recv_timeout(MONITOR_POLL_INTERVAL) {
                    Ok(command) => {
                        if worker_stop.load(Ordering::Acquire) { break; }
                        let result = match command.kind {
                            CommandKind::Compact => {
                                worker_cancel.store(false, Ordering::Release);
                                engine.request_compaction()
                            },
                            CommandKind::Save if engine.view().status != Status::Validated => Err(io::Error::new(io::ErrorKind::WouldBlock, "unsaved: monitor observation is not validated; prior checkpoint preserved")),
                            CommandKind::Save => engine.save(),
                            CommandKind::Rebuild => {
                                worker_cancel.store(false, Ordering::Release);
                                engine.request_rebuild()
                            }
                        };
                        let _ = command.reply.try_send(result);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                if worker_stop.load(Ordering::Acquire) { break; }
                // Engine publishes failures; its bounded recovery policy decides retries.
                let _ = engine.poll_with_cancel(&worker_cancel);
                *worker_metrics.lock().unwrap() = engine.metrics().clone();
            }
            // Dropping the Engine after stop also releases resources if stop reports an error.
            engine.stop()
        })?;
        Ok(MonitorOwner {
            query,
            supports_cancel,
            sender,
            stop,
            cancel,
            metrics,
            worker: Some(worker),
        })
    }
}
impl MonitorOwner {
    pub fn query(&self) -> QueryHandle {
        self.query.clone()
    }
    pub fn view(&self) -> View {
        self.query.view()
    }
    /// True only after an explicit stop joined the worker (including a reported stop error).
    pub fn is_joined(&self) -> bool {
        self.worker.is_none()
    }
    pub fn metrics(&self) -> Metrics {
        self.metrics.lock().unwrap().clone()
    }
    fn request(&self, kind: CommandKind) -> io::Result<MonitorRequest> {
        if self.stop.load(Ordering::Acquire) {
            return Err(io::Error::other("monitor is stopping or stopped"));
        }
        let (reply, receiver) = mpsc::sync_channel(1);
        self.sender
            .try_send(Command { kind, reply })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "monitor command budget exhausted",
                ),
                mpsc::TrySendError::Disconnected(_) => io::Error::other("monitor stopped"),
            })?;
        Ok(MonitorRequest { receiver })
    }
    pub fn save(&self) -> io::Result<MonitorRequest> {
        self.request(CommandKind::Save)
    }
    pub fn request_rebuild(&self) -> io::Result<MonitorRequest> {
        self.request(CommandKind::Rebuild)
    }
    pub fn request_compaction(&self) -> io::Result<MonitorRequest> {
        self.request(CommandKind::Compact)
    }
    /// Interrupt scale correction without ending monitoring. Rebuild resumes it.
    /// Acceptance is immediate; Pending with a Cancelled gap confirms completion.
    /// The bounded compatibility backend returns Unsupported and keeps monitoring.
    pub fn cancel(&self) -> io::Result<()> {
        if !self.supports_cancel {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "scan cancellation requires scale mode",
            ));
        }
        self.cancel.store(true, Ordering::Release);
        Ok(())
    }
    /// Requests stop independently of queued work. Success means the worker joined.
    /// Timeout leaves the worker owned here; resources may still be held by OS I/O.
    pub fn stop(&mut self, timeout: Duration) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        self.cancel.store(true, Ordering::Release);
        let deadline = Instant::now() + timeout;
        if let Some(worker) = &self.worker {
            while !worker.is_finished() {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "monitor still shutting down; resources may remain held until join",
                    ));
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| io::Error::other("monitor worker panicked"))?
        })
    }
}
impl Drop for MonitorOwner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.cancel.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
