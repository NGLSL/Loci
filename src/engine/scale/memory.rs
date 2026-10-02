//! Shared conservative admission for bounded scale allocations. Reservations
//! describe capacity, not RSS, malloc metadata, or kernel watch/slab bytes.
use crate::engine::EngineOptions;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

const LARGE_RELEASE: usize = 16 * 1024 * 1024;
#[cfg(target_env = "gnu")]
static RECLAIM_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(target_env = "gnu")]
static RECLAIM_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Declare this field last, so its destructor runs after the owned allocations
/// it describes. Arm only for allocations actually owned by the dropping value.
#[derive(Clone, Default)]
pub(super) struct ReclaimOnDrop {
    released_bytes: usize,
}
impl ReclaimOnDrop {
    pub(super) fn new(released_bytes: usize) -> Self {
        Self { released_bytes }
    }
    pub(super) fn arm(&mut self, released_bytes: usize) {
        self.released_bytes = released_bytes;
    }
}
impl Drop for ReclaimOnDrop {
    fn drop(&mut self) {
        if self.released_bytes >= LARGE_RELEASE {
            #[cfg(target_env = "gnu")]
            RECLAIM_REQUESTED.store(true, Ordering::Release);
        }
    }
}
/// Called outside shared snapshot locks. Quiet polls only check an atomic;
/// allocator maintenance occurs once after a real large allocation release.
pub(super) fn reclaim_released() -> bool {
    #[cfg(target_env = "gnu")]
    {
        if !RECLAIM_REQUESTED.load(Ordering::Acquire)
            || RECLAIM_RUNNING
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
        {
            return false;
        }
        let requested = RECLAIM_REQUESTED.swap(false, Ordering::AcqRel);
        if requested {
            unsafe extern "C" {
                fn malloc_trim(pad: usize) -> std::ffi::c_int;
            }
            // GNU libc returns only unused allocator pages. It neither moves
            // nor invalidates live buffers, including immutable leased data.
            unsafe {
                malloc_trim(0);
            }
        }
        RECLAIM_RUNNING.store(false, Ordering::Release);
        return requested;
    }
    #[cfg(not(target_env = "gnu"))]
    false
}

pub const PROCESS_MEMORY_LIMIT: usize = 4 * 1024 * 1024 * 1024;
static RESERVED: AtomicUsize = AtomicUsize::new(0);
pub(crate) fn reserved_bytes() -> usize {
    RESERVED.load(Ordering::Acquire)
}
pub(crate) struct Reservation {
    bytes: usize,
}
impl Reservation {
    pub(crate) fn acquire(bytes: usize) -> io::Result<Self> {
        RESERVED
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= PROCESS_MEMORY_LIMIT)
            })
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "process scale memory admission exhausted",
                )
            })?;
        Ok(Self { bytes })
    }
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        RESERVED.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
pub(crate) fn owner(options: &EngineOptions, native: bool) -> io::Result<Arc<Reservation>> {
    let slots = options.scale_budgets.max_slots;
    let entries = options.limits.entries;
    let dirs = options.limits.directories;
    // Two writers: active plus correction/compaction. Each lookup entry has
    // 192 bytes of conservative HashMap/control/collision/adjacency capacity;
    // 16 bytes per physical slot includes geometric position-vector growth.
    // Native paths occur in physical/reverse/logical tables, each <=4096 bytes.
    // DFS compaction uses depth-sized frames; scanning may retain directory paths.
    let bounds = [
        Some(options.scale_budgets.max_retained_bytes),
        slots.checked_mul(32),
        entries.checked_mul(384),
        // Two writer adjacency maps, including geometric bucket capacity and
        // the inline64 child IDs; large child-vector overflow also joins the
        // per-entry writer bound above.
        dirs.checked_mul(2048),
        dirs.checked_mul(4096 + 128),
        if native {
            options.watch_limit.checked_mul(3 * (4096 + 256))
        } else {
            Some(0)
        },
        options.scale_budgets.max_queue_bytes.checked_mul(2),
        options
            .event_limits
            .max_events()
            .checked_mul(2 * (4096 + 128)),
        Some(options.event_limits.buffer_bytes().saturating_mul(2)),
        // Mount input+decoded map, exclusion copies, segment table copies,
        // accounting hash sets, status/error/path/page scratch, bounded commands.
        Some(160 * 1024 * 1024),
        slots.checked_mul(16),
        // Additional 128 bits in each scale-only 256-bit pair signature. Actual
        // candidate/retired filter capacities also join max_retained_bytes above.
        slots.checked_mul(16),
    ];
    let bytes = bounds
        .into_iter()
        .try_fold(0usize, |sum, bound| sum.checked_add(bound?))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "scale memory admission overflow",
            )
        })?;
    Ok(Arc::new(Reservation::acquire(bytes)?))
}
