//! Bounded recursive Win32 notifications for an explicitly selected root.
//! Uses ReadDirectoryChangesW and CancelIoEx completion semantics:
//! https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-readdirectorychangesw
//! https://learn.microsoft.com/windows/win32/api/ioapiset/nf-ioapiset-cancelioex
use crate::events::{
    Change, EventBatch, EventLimits, EventSource, Loss, SourceState, MAX_READS_PER_POLL,
    MAX_SOURCES,
};
use std::{
    ffi::{c_void, OsString},
    io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, Instant},
};

type Handle = *mut c_void;
const INVALID_HANDLE: Handle = -1isize as Handle;
const ERROR_IO_INCOMPLETE: i32 = 996;
const ERROR_IO_PENDING: i32 = 997;
const ERROR_OPERATION_ABORTED: i32 = 995;
const ERROR_NOTIFY_ENUM_DIR: i32 = 1022;
const ERROR_NOT_FOUND: i32 = 1168;
const FILE_LIST_DIRECTORY: u32 = 0x1;
const FILE_SHARE_READ_WRITE_DELETE: u32 = 0x1 | 0x2 | 0x4;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;
const FILE_FLAG_OVERLAPPED: u32 = 0x40000000;
const NOTIFY_NAMES_AND_ATTRIBUTES: u32 = 0x1 | 0x2 | 0x4;
const FILE_ACTION_ADDED: usize = 1;
const FILE_ACTION_REMOVED: usize = 2;
const FILE_ACTION_MODIFIED: usize = 3;
const FILE_ACTION_RENAMED_OLD_NAME: usize = 4;
const FILE_ACTION_RENAMED_NEW_NAME: usize = 5;
const RENAME_TTL: Duration = Duration::from_millis(100);
static SOURCES: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: Handle,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        creation: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn CreateEventW(
        security: *const c_void,
        manual_reset: i32,
        initial: i32,
        name: *const u16,
    ) -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn ResetEvent(event: Handle) -> i32;
    fn ReadDirectoryChangesW(
        directory: Handle,
        buffer: *mut c_void,
        length: u32,
        subtree: i32,
        filter: u32,
        returned: *mut u32,
        overlapped: *mut Overlapped,
        completion: *const c_void,
    ) -> i32;
    fn GetOverlappedResult(
        directory: Handle,
        overlapped: *mut Overlapped,
        transferred: *mut u32,
        wait: i32,
    ) -> i32;
    fn CancelIoEx(directory: Handle, overlapped: *mut Overlapped) -> i32;
}

struct Reservation;
impl Reservation {
    fn acquire() -> io::Result<Self> {
        SOURCES
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_SOURCES).then_some(n + 1)
            })
            .map(|_| Self)
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "Windows source limit reached"))
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        SOURCES.fetch_sub(1, Ordering::AcqRel);
    }
}
struct OwnedHandle(Handle);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

// Box is converted into raw ownership before any address is given to Win32.
// Moving this owner moves pointers only, with no Box/reference retag of live
// kernel storage. Reconstruct Box solely after Request confirms completion.
struct IoStorage {
    overlapped: *mut Overlapped,
    buffer: *mut [u32],
}
impl IoStorage {
    fn new(buffer_bytes: usize, event: Handle) -> Self {
        let overlapped = Box::new(Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event,
        });
        let buffer = vec![0u32; buffer_bytes / 4].into_boxed_slice();
        Self {
            overlapped: Box::into_raw(overlapped),
            buffer: Box::into_raw(buffer),
        }
    }
    fn buffer_bytes(&self) -> usize {
        self.buffer.len() * 4
    }
}
impl Drop for IoStorage {
    fn drop(&mut self) {
        // Request::drop has confirmed no kernel use; no slices remain alive.
        unsafe {
            drop(Box::from_raw(self.overlapped));
            drop(Box::from_raw(self.buffer));
        }
    }
}

struct Request {
    directory: OwnedHandle,
    event: OwnedHandle,
    storage: IoStorage,
    pending: bool,
    _reservation: Reservation,
}
// Win32 directory/event handles are process-wide. One owner accesses this
// request, and buffer/OVERLAPPED storage stays allocated until completion.
unsafe impl Send for Request {}
impl Request {
    fn arm(&mut self) -> io::Result<()> {
        debug_assert!(!self.pending);
        unsafe {
            if ResetEvent(self.event.0) == 0 {
                return Err(io::Error::last_os_error());
            }
            self.storage.overlapped.write(Overlapped {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                event: self.event.0,
            });
            // Name, directory-name and attribute notifications only; no content.
            let ok = ReadDirectoryChangesW(
                self.directory.0,
                self.storage.buffer as *mut c_void,
                self.storage.buffer_bytes() as u32,
                1,
                NOTIFY_NAMES_AND_ATTRIBUTES,
                ptr::null_mut(),
                self.storage.overlapped,
                ptr::null(),
            );
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_IO_PENDING) {
                    return Err(error);
                }
            }
        }
        self.pending = true;
        Ok(())
    }
    fn complete(&mut self, wait: bool) -> io::Result<Option<usize>> {
        let mut bytes = 0;
        let ok = unsafe {
            GetOverlappedResult(
                self.directory.0,
                self.storage.overlapped,
                &mut bytes,
                i32::from(wait),
            )
        };
        if ok != 0 {
            self.pending = false;
            return Ok(Some(bytes as usize));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE) {
            return Ok(None);
        }
        self.pending = false;
        Err(error)
    }
    fn cancel(&mut self) -> io::Result<()> {
        if !self.pending {
            return Ok(());
        }
        let cancel_error = if unsafe { CancelIoEx(self.directory.0, self.storage.overlapped) } == 0
        {
            let error = io::Error::last_os_error();
            (error.raw_os_error() != Some(ERROR_NOT_FOUND)).then_some(error)
        } else {
            None
        };
        // Cancellation is only a request. Even ERROR_NOT_FOUND can race normal
        // completion; confirm completion before closing handles or freeing data.
        let completion = self.complete(true);
        if let Some(error) = cancel_error {
            return Err(error);
        }
        match completion {
            Ok(Some(_)) => Ok(()),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(ERROR_OPERATION_ABORTED | ERROR_NOTIFY_ENUM_DIR)
                ) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
            Ok(None) => Err(io::Error::new(
                io::ErrorKind::Other,
                "I/O remained pending after completion wait",
            )),
        }
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        let _ = self.cancel();
        // complete(true) confirms terminal completion for our valid private
        // handles. Do not release backing storage on an incomplete result.
        if self.pending {
            std::process::abort();
        }
    }
}

/// A single-owner, movable native source. Drop cancels outstanding I/O.
pub struct WindowsEvents {
    request: Option<Request>,
    limits: EventLimits,
    old_name: Option<(PathBuf, Instant)>,
}
impl WindowsEvents {
    pub fn open(root: &Path, limits: EventLimits) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "root must be canonical and absolute",
            ));
        }
        let mut name: Vec<u16> = root.as_os_str().encode_wide().collect();
        if name.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "root contains NUL",
            ));
        }
        name.push(0);
        let reservation = Reservation::acquire()?;
        let directory = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ_WRITE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                ptr::null_mut(),
            )
        };
        if directory == INVALID_HANDLE {
            return Err(io::Error::last_os_error());
        }
        let directory = OwnedHandle(directory);
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let event = OwnedHandle(event);
        let mut request = Request {
            storage: IoStorage::new(limits.buffer_bytes(), event.0),
            directory,
            event,
            pending: false,
            _reservation: reservation,
        };
        request.arm()?;
        Ok(Self {
            request: Some(request),
            limits,
            old_name: None,
        })
    }
    fn arm_or_loss(&mut self, batch: &mut EventBatch) -> io::Result<bool> {
        match self.request.as_mut().unwrap().arm() {
            Ok(()) => Ok(true),
            Err(error) if error.raw_os_error() == Some(ERROR_NOTIFY_ENUM_DIR) => {
                batch.losses.insert(Loss::KernelOverflow);
                Self::lose_rename(&mut self.old_name, batch);
                Ok(false)
            }
            Err(error) => {
                let _ = self.stop();
                Err(error)
            }
        }
    }
    fn lose_rename(old_name: &mut Option<(PathBuf, Instant)>, batch: &mut EventBatch) {
        if old_name.take().is_some() {
            batch.losses.insert(Loss::UnpairedRename);
        }
    }
    fn decode(
        limits: EventLimits,
        old_name: &mut Option<(PathBuf, Instant)>,
        bytes: &[u8],
        batch: &mut EventBatch,
    ) {
        let mut offset = 0;
        let mut records = 0;
        loop {
            if records >= limits.max_events() {
                batch.losses.insert(Loss::UserOverflow);
                break;
            }
            records += 1;
            let Some(header) = bytes.get(offset..offset + 12) else {
                batch.losses.insert(Loss::InvalidEvent);
                break;
            };
            let read = |n| u32::from_le_bytes(header[n..n + 4].try_into().unwrap()) as usize;
            let next = read(0);
            let action = read(4);
            let length = read(8);
            if length == 0 || length > 4096 || length % 2 != 0 {
                batch.losses.insert(Loss::InvalidEvent);
                break;
            }
            let end = offset + 12 + length;
            if end > bytes.len()
                || (next != 0
                    && (next % 4 != 0
                        || next < 12 + length
                        || offset.checked_add(next).is_none_or(|n| n >= bytes.len())))
            {
                batch.losses.insert(Loss::InvalidEvent);
                break;
            }
            let wide: Vec<u16> = bytes[offset + 12..end]
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            let path = PathBuf::from(OsString::from_wide(&wide));
            if wide.contains(&0)
                || wide.contains(&(b':' as u16))
                || !path.components().all(|c| matches!(c, Component::Normal(_)))
                || wide
                    .split(|c| *c == b'\\' as u16 || *c == b'/' as u16)
                    .any(|c| c.is_empty() || c == [b'.' as u16] || c == [b'.' as u16, b'.' as u16])
            {
                batch.losses.insert(Loss::InvalidEvent);
                break;
            }
            if action != FILE_ACTION_RENAMED_NEW_NAME {
                Self::lose_rename(old_name, batch);
            }
            let change = match action {
                FILE_ACTION_ADDED | FILE_ACTION_MODIFIED => Some(Change::Refresh(path)),
                FILE_ACTION_REMOVED => Some(Change::Remove(path)),
                FILE_ACTION_RENAMED_OLD_NAME => {
                    *old_name = Some((path, Instant::now()));
                    None
                }
                FILE_ACTION_RENAMED_NEW_NAME => match old_name.take() {
                    Some((from, at)) if at.elapsed() < RENAME_TTL => {
                        Some(Change::Rename { from, to: path })
                    }
                    _ => {
                        batch.losses.insert(Loss::UnpairedRename);
                        None
                    }
                },
                _ => {
                    batch.losses.insert(Loss::InvalidEvent);
                    None
                }
            };
            if let Some(change) = change {
                batch.changes.push(change);
                if batch.changes.len() >= limits.max_events() {
                    batch.losses.insert(Loss::UserOverflow);
                    break;
                }
            }
            if next == 0 {
                break;
            }
            offset += next;
        }
        if !batch.losses.is_empty() {
            Self::lose_rename(old_name, batch);
        }
    }
}
impl EventSource for WindowsEvents {
    fn poll(&mut self) -> io::Result<EventBatch> {
        let mut batch = EventBatch::default();
        if self.request.is_none() {
            batch.state = SourceState::Stopped;
            return Ok(batch);
        }
        if self
            .old_name
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= RENAME_TTL)
        {
            Self::lose_rename(&mut self.old_name, &mut batch);
        }
        if !self.request.as_ref().unwrap().pending && !self.arm_or_loss(&mut batch)? {
            return Ok(batch);
        }
        for read in 0..MAX_READS_PER_POLL {
            let completion = self.request.as_mut().unwrap().complete(false);
            match completion {
                Ok(None) => break,
                Ok(Some(0)) => {
                    batch.losses.insert(Loss::KernelOverflow);
                    Self::lose_rename(&mut self.old_name, &mut batch);
                }
                Ok(Some(length)) => {
                    let request = self.request.as_ref().unwrap();
                    if length > request.storage.buffer_bytes() {
                        batch.losses.insert(Loss::InvalidEvent);
                        Self::lose_rename(&mut self.old_name, &mut batch);
                    } else {
                        // I/O is complete and is not rearmed until decoding ends.
                        let bytes = unsafe {
                            std::slice::from_raw_parts(request.storage.buffer as *const u8, length)
                        };
                        Self::decode(self.limits, &mut self.old_name, bytes, &mut batch);
                    }
                }
                Err(error) if error.raw_os_error() == Some(ERROR_NOTIFY_ENUM_DIR) => {
                    batch.losses.insert(Loss::KernelOverflow);
                    Self::lose_rename(&mut self.old_name, &mut batch);
                }
                Err(error) if error.raw_os_error() == Some(ERROR_OPERATION_ABORTED) => {
                    batch.losses.insert(Loss::WatchLost);
                    Self::lose_rename(&mut self.old_name, &mut batch);
                    self.request.take();
                    batch.state = SourceState::Stopped;
                    return Ok(batch);
                }
                Err(error) => {
                    let _ = self.stop();
                    return Err(error);
                }
            }
            let budget_reached =
                read + 1 == MAX_READS_PER_POLL || batch.changes.len() >= self.limits.max_events();
            if budget_reached {
                batch.losses.insert(Loss::UserOverflow);
                Self::lose_rename(&mut self.old_name, &mut batch);
            }
            // Mark the read budget even if rearming also reports kernel loss.
            if !self.arm_or_loss(&mut batch)? || budget_reached {
                break;
            }
        }
        Ok(batch)
    }
    fn stop(&mut self) -> io::Result<()> {
        self.old_name = None;
        if let Some(mut request) = self.request.take() {
            request.cancel()
        } else {
            Ok(())
        }
    }
}
