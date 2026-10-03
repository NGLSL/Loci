//! Windows file index, legacy directory engine, and local query service.
#[cfg(not(windows))]
compile_error!("Loci only supports Windows targets");

pub mod watch;

#[cfg(test)]
mod watch_tests;

pub mod index;
pub mod live;
mod signatures;

pub mod incremental;
mod partitioned;

/// Stable event-source seam under v0.1 development.
pub mod events;

pub mod engine;

/// Versioned, bounded root inventories for the v0.1 engine.
pub mod storage;

pub mod windows_events;

/// Full-volume NTFS inventory and persistent USN synchronization.
pub mod ntfs;

/// Local Windows service and its read-only query protocol.
pub mod service;
