//! Shared conservative admission for bounded scale allocations. Reservations
//! describe capacity, not RSS, malloc metadata, or kernel watch/slab bytes.
use crate::engine::EngineOptions;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
        dirs.checked_mul(512),
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
