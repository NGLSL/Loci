//! Platform event-source seam for the bounded v0.1 engine.
//! Paths retain original spelling and OS representation; they are root-relative.
use std::collections::BTreeSet;
use std::io;

pub use crate::incremental::Change;

pub const MAX_SOURCES: usize = 8;
pub const MAX_READS_PER_POLL: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventLimits {
    max_events: usize,
    buffer_bytes: usize,
}
impl Default for EventLimits {
    fn default() -> Self {
        Self {
            max_events: 256,
            buffer_bytes: 64 * 1024,
        }
    }
}
impl EventLimits {
    /// Bounds are fixed for the current prototype; callers cannot raise them.
    pub fn new(max_events: usize, buffer_bytes: usize) -> io::Result<Self> {
        if !(1..=256).contains(&max_events)
            || !(4096..=64 * 1024).contains(&buffer_bytes)
            || !buffer_bytes.is_multiple_of(4)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "event-source budget outside supported bounds",
            ));
        }
        Ok(Self {
            max_events,
            buffer_bytes,
        })
    }
    pub fn max_events(self) -> usize {
        self.max_events
    }
    pub fn buffer_bytes(self) -> usize {
        self.buffer_bytes
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Loss {
    KernelOverflow,
    UserOverflow,
    UnpairedRename,
    InvalidEvent,
    WatchLost,
    BackendRestart,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceState {
    #[default]
    Watching,
    Stopped,
}
/// If any loss is present, discard changes and reconcile the selected root.
/// Empty Watching means no currently available event, not a filesystem snapshot.
#[derive(Clone, Debug, Default)]
pub struct EventBatch {
    pub changes: Vec<Change>,
    pub losses: BTreeSet<Loss>,
    pub state: SourceState,
}

/// A single polling owner consumes events; the source may move between threads.
/// Implementations must release resources on stop and Drop. They need not be Sync.
pub trait EventSource: Send {
    /// Never waits for a new filesystem event. Drain bounded available work only.
    fn poll(&mut self) -> io::Result<EventBatch>;
    /// Idempotent. Cancel and finish pending I/O safely before releasing buffers.
    /// Future polls return an empty Stopped batch.
    fn stop(&mut self) -> io::Result<()>;
}
