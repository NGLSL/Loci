//! Independent bounded watcher/index snapshot experiment; no Kite integration.
#[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
pub mod linux_inotify;
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
#[cfg(any(target_os = "linux", feature = "linux-ffi-check"))]
mod linux_events;
/// Versioned, bounded root inventories for the v0.1 engine.
pub mod storage;

#[cfg(windows)]
pub mod windows_events;
