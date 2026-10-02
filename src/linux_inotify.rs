//! x86_64 Linux inotify experiment. `linux-ffi-check` on Windows only type-checks
//! portable Rust; it does not link libc or validate ABI/kernel behavior.
use crate::watch::{self, Inventory, Limits, Recovery, Signal};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_char, c_int, c_void, CString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
pub const PROCESS_WATCH_CAP: usize = 128;
pub const PROCESS_SESSION_CAP: usize = 8;
static LIVE_WATCHES: AtomicUsize = AtomicUsize::new(0);
static LIVE_SESSIONS: AtomicUsize = AtomicUsize::new(0);
fn reserve(counter: &AtomicUsize, cap: usize) -> io::Result<()> {
    counter
        .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < cap).then_some(n + 1)
        })
        .map(|_| ())
        .map_err(|_| io::Error::other("process-wide inotify budget exhausted"))
}
pub fn process_usage() -> (usize, usize) {
    (
        LIVE_WATCHES.load(Ordering::Acquire),
        LIVE_SESSIONS.load(Ordering::Acquire),
    )
}
#[cfg(all(target_os = "linux", not(target_arch = "x86_64")))]
compile_error!("This inotify FFI experiment is limited to x86_64 Linux");
#[cfg_attr(target_os = "linux", link(name = "c"))]
unsafe extern "C" {
    fn inotify_init1(flags: c_int) -> c_int;
    fn inotify_add_watch(fd: c_int, path: *const c_char, mask: u32) -> c_int;
    fn inotify_rm_watch(fd: c_int, wd: c_int) -> c_int;
    fn read(fd: c_int, buffer: *mut c_void, count: usize) -> isize;
    fn close(fd: c_int) -> c_int;
}
pub(crate) struct Captured {
    pub events: Vec<watch::RawEvent>,
    pub truncated: bool,
    pub kernel_overflow: bool,
}
pub struct Session {
    fd: c_int,
    watches: BTreeMap<i32, PathBuf>,
    limit: usize,
    pub(crate) expected_ignored: BTreeSet<i32>,
}
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            close(self.fd);
        }
        LIVE_WATCHES.fetch_sub(self.watches.len(), Ordering::AcqRel);
        LIVE_SESSIONS.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Session {
    pub fn new(limit: usize) -> io::Result<Self> {
        // x86_64 Linux O_NONBLOCK | O_CLOEXEC. No changes to global kernel limits.
        reserve(&LIVE_SESSIONS, PROCESS_SESSION_CAP)?;
        let fd = unsafe { inotify_init1(0x800 | 0x80000) };
        if fd < 0 {
            let error = io::Error::last_os_error();
            LIVE_SESSIONS.fetch_sub(1, Ordering::AcqRel);
            return Err(error);
        }
        Ok(Self {
            fd,
            watches: BTreeMap::new(),
            limit,
            expected_ignored: BTreeSet::new(),
        })
    }
    pub fn add(&mut self, path: &Path) -> io::Result<()> {
        if self.watches.values().any(|p| p == path) {
            return Ok(());
        }
        if self.watches.len() >= self.limit {
            return Err(io::Error::other("watch budget exhausted"));
        }
        #[cfg(target_os = "linux")]
        let bytes = {
            use std::os::unix::ffi::OsStrExt;
            path.as_os_str().as_bytes().to_vec()
        };
        #[cfg(not(target_os = "linux"))]
        let bytes = path
            .to_str()
            .ok_or_else(|| io::Error::other("type-check fallback"))?
            .as_bytes()
            .to_vec();
        let path_c = CString::new(bytes).map_err(io::Error::other)?;
        // CREATE, DELETE, MOVED_FROM/TO, ATTRIB, DELETE_SELF, MOVE_SELF,
        // ONLYDIR and DONT_FOLLOW. Do not subscribe OPEN/ACCESS from our scans.
        let mask = 0x100 | 0x200 | 0x40 | 0x80 | 0x4 | 0x400 | 0x800 | 0x01000000 | 0x02000000;
        reserve(&LIVE_WATCHES, PROCESS_WATCH_CAP)?;
        let wd = unsafe { inotify_add_watch(self.fd, path_c.as_ptr(), mask) };
        if wd < 0 {
            let error = io::Error::last_os_error();
            LIVE_WATCHES.fetch_sub(1, Ordering::AcqRel);
            return Err(error);
        }
        if self.expected_ignored.contains(&wd) {
            LIVE_WATCHES.fetch_sub(1, Ordering::AcqRel);
            return Err(io::Error::other("ambiguous reused watch descriptor"));
        }
        if self
            .watches
            .get(&wd)
            .is_some_and(|previous| previous != path)
        {
            LIVE_WATCHES.fetch_sub(1, Ordering::AcqRel);
            return Err(io::Error::other("aliased watch descriptor"));
        }
        if self.watches.insert(wd, path.to_path_buf()).is_some() {
            LIVE_WATCHES.fetch_sub(1, Ordering::AcqRel);
        }
        Ok(())
    }
    pub(crate) fn paths(&self) -> &BTreeMap<i32, PathBuf> {
        &self.watches
    }
    pub(crate) fn remove_prefix(&mut self, prefix: &Path) -> io::Result<()> {
        let ids: Vec<_> = self
            .watches
            .iter()
            .filter(|(_, p)| p.starts_with(prefix))
            .map(|(wd, _)| *wd)
            .collect();
        for wd in ids {
            let rc = unsafe { inotify_rm_watch(self.fd, wd) };
            if rc < 0 && io::Error::last_os_error().raw_os_error() != Some(22) {
                return Err(io::Error::last_os_error());
            }
            self.watches.remove(&wd);
            LIVE_WATCHES.fetch_sub(1, Ordering::AcqRel);
            self.expected_ignored.insert(wd);
        }
        if self.expected_ignored.len() > PROCESS_WATCH_CAP {
            return Err(io::Error::other("retired descriptor budget exhausted"));
        }
        Ok(())
    }
    pub(crate) fn rename_prefix(&mut self, from: &Path, to: &Path) {
        for path in self.watches.values_mut() {
            if let Ok(tail) = path.strip_prefix(from) {
                *path = to.join(tail);
            }
        }
    }
    pub(crate) fn acknowledge_ignored(&mut self, ids: &[i32]) {
        for wd in ids {
            self.expected_ignored.remove(wd);
        }
    }
    pub(crate) fn capture(&mut self, capacity: usize) -> io::Result<Captured> {
        let mut captured = Captured {
            events: vec![],
            truncated: false,
            kernel_overflow: false,
        };
        for _ in 0..8 {
            let mut buffer = [0u8; 65536];
            let n = unsafe { read(self.fd, buffer.as_mut_ptr().cast(), buffer.len()) };
            if n < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    return Ok(captured);
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if n == 0 {
                return Err(io::Error::other("inotify EOF"));
            }
            let batch = watch::decode_events(
                &buffer[..n as usize],
                capacity.saturating_sub(captured.events.len()),
            )?;
            captured.kernel_overflow |= batch.kernel_overflow;
            captured.truncated |= batch.dropped;
            captured.events.extend(batch.events);
        }
        captured.truncated = true;
        Ok(captured)
    }
    pub fn count(&self) -> usize {
        self.watches.len()
    }
    pub fn pump(&mut self, state: &mut Recovery) -> io::Result<usize> {
        let known: BTreeSet<_> = self.watches.keys().copied().collect();
        let mut events = 0;
        // Limit draining even when another process continuously changes files.
        for _ in 0..8 {
            let mut buffer = [0u8; 65536];
            let n = unsafe { read(self.fd, buffer.as_mut_ptr().cast(), buffer.len()) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::WouldBlock {
                    return Ok(events);
                }
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                state.signal(Signal::WatchLost);
                return Err(e);
            }
            if n == 0 {
                state.signal(Signal::WatchLost);
                return Err(io::Error::other("inotify EOF"));
            }
            let batch = match watch::decode_events(&buffer[..n as usize], 256) {
                Ok(x) => x,
                Err(e) => {
                    state.signal(Signal::WatchLost);
                    return Err(e);
                }
            };
            events += batch.events.len();
            let dropped = batch.dropped;
            watch::accept_batch(batch, &known, state);
            if dropped {
                state.signal(Signal::UserOverflow);
            }
        }
        state.signal(Signal::UserOverflow);
        Ok(events)
    }
}
pub struct Runtime {
    pub root: PathBuf,
    pub limits: Limits,
    pub state: Recovery,
    pub events_observed: usize,
    pub full_scans: usize,
    pub scanned_entries: usize,
    pub(crate) session: Option<Session>,
    last_check: Instant,
}
impl Runtime {
    pub fn new(root: &Path, limits: Limits) -> io::Result<Self> {
        let root = std::fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(io::Error::other("root must be a directory"));
        }
        Ok(Self {
            root,
            limits,
            state: Recovery::new(limits.queue),
            events_observed: 0,
            full_scans: 0,
            scanned_entries: 0,
            session: None,
            last_check: Instant::now(),
        })
    }
    pub fn restore(&mut self, inventory: Inventory) {
        self.state = Recovery::restored(inventory, self.limits.queue);
    }
    pub fn watches(&self) -> usize {
        self.session.as_ref().map_or(0, Session::count)
    }
    pub fn reconcile(&mut self) -> io::Result<bool> {
        self.reconcile_with_hook(|| {})
    }
    pub fn reconcile_with_hook(&mut self, mut after_scan: impl FnMut()) -> io::Result<bool> {
        self.state.signal(Signal::Periodic);
        self.session.take(); // Drop old fd/watches BEFORE allocating replacement.
        for _ in 0..self.limits.retries {
            let ticket = self.state.ticket();
            let mut next = match Session::new(self.limits.directories) {
                Ok(x) => x,
                Err(e) => {
                    self.state.signal(Signal::WatchLost);
                    return Err(e);
                }
            };
            self.full_scans += 1;
            let candidate = watch::scan(&self.root, self.limits, |path| next.add(path));
            self.scanned_entries += candidate.examined;
            if !candidate.complete {
                self.state.publish(ticket, candidate);
                return Ok(false);
            }
            after_scan(); // Linux fixture injects a race between scan and queue drain.
            self.events_observed += next.pump(&mut self.state)?;
            if self.state.publish(ticket, candidate) {
                self.session = Some(next); // The old session was already dropped before scanning.
                self.last_check = Instant::now();
                return Ok(true);
            }
        }
        self.state.signal(Signal::RetryLimit);
        Ok(false)
    }
    pub fn poll(&mut self) -> io::Result<()> {
        if let Some(session) = &mut self.session {
            match session.pump(&mut self.state) {
                Ok(events) => self.events_observed += events,
                Err(error) => {
                    self.session.take(); // The next bounded retry can replace a failed fd.
                    return Err(error);
                }
            }
        }
        if self.last_check.elapsed() >= Duration::from_secs(2) {
            self.state.signal(Signal::Periodic);
            self.last_check = Instant::now();
        }
        Ok(())
    }
    pub fn tick(&mut self) -> io::Result<bool> {
        self.poll()?;
        if self.state.dirty {
            self.reconcile()
        } else {
            Ok(true)
        }
    }
}
