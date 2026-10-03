//! Independent, bounded ReadDirectoryChangesW probe. This is not USN replay.
use std::{ffi::c_void, io, os::windows::ffi::OsStrExt, path::Path, ptr};
type Handle = *mut c_void;
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
    fn CreateEventW(security: *const c_void, manual: i32, initial: i32, name: *const u16)
        -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn ResetEvent(handle: Handle) -> i32;
    fn ReadDirectoryChangesW(
        handle: Handle,
        buffer: *mut c_void,
        length: u32,
        subtree: i32,
        filter: u32,
        returned: *mut u32,
        overlapped: *mut Overlapped,
        completion: *const c_void,
    ) -> i32;
    fn GetOverlappedResult(
        handle: Handle,
        overlapped: *mut Overlapped,
        bytes: *mut u32,
        wait: i32,
    ) -> i32;
    fn CancelIoEx(handle: Handle, overlapped: *mut Overlapped) -> i32;
}
struct Owned(Handle);
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
#[derive(Debug)]
pub struct Record {
    pub action: u32,
    pub name: Vec<u16>,
}
pub struct Watch {
    directory: Owned,
    event: Owned,
    // Raw ownership keeps kernel-visible allocations at fixed addresses without
    // reborrowing live kernel-mutated storage. Reconstitute only after completion.
    overlapped: *mut Overlapped,
    buffer: *mut [u32],
    pending: bool,
    stopped: bool,
    pub records: Vec<Record>,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
impl Watch {
    pub fn open(root: &Path) -> io::Result<Self> {
        if !root.is_absolute() {
            return Err(invalid("watch root must be absolute"));
        }
        let mut wide: Vec<u16> = root.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(invalid("NUL in root"));
        }
        wide.push(0);
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                1,
                7,
                ptr::null(),
                3,
                0x02000000 | 0x40000000,
                ptr::null_mut(),
            )
        };
        if handle == -1isize as Handle {
            return Err(io::Error::last_os_error());
        }
        let directory = Owned(handle);
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let event = Owned(event);
        let overlapped = Box::into_raw(Box::new(Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: event.0,
        }));
        let buffer = Box::into_raw(vec![0u32; 16384].into_boxed_slice());
        let mut result = Self {
            directory,
            event,
            overlapped,
            buffer,
            pending: false,
            stopped: false,
            records: Vec::new(),
        };
        result.arm()?;
        Ok(result)
    }
    fn arm(&mut self) -> io::Result<()> {
        unsafe {
            if ResetEvent(self.event.0) == 0 {
                return Err(io::Error::last_os_error());
            }
            self.overlapped.write(Overlapped {
                internal: 0,
                internal_high: 0,
                offset: 0,
                offset_high: 0,
                event: self.event.0,
            });
            if ReadDirectoryChangesW(
                self.directory.0,
                self.buffer as *mut c_void,
                65536,
                1,
                1 | 2 | 4,
                ptr::null_mut(),
                self.overlapped,
                ptr::null(),
            ) == 0
            {
                let e = io::Error::last_os_error();
                if e.raw_os_error() != Some(997) {
                    return Err(e);
                }
            }
        }
        self.pending = true;
        Ok(())
    }
    fn complete(&mut self, wait: bool) -> io::Result<Option<usize>> {
        let mut bytes = 0;
        if unsafe {
            GetOverlappedResult(
                self.directory.0,
                self.overlapped,
                &mut bytes,
                i32::from(wait),
            )
        } != 0
        {
            self.pending = false;
            return Ok(Some(bytes as usize));
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() == Some(996) {
            return Ok(None);
        }
        self.pending = false;
        Err(e)
    }
    pub fn drain(&mut self) -> io::Result<usize> {
        self.records.clear();
        if self.stopped {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "watch stopped"));
        }
        let Some(length) = self.complete(false)? else {
            return Ok(0);
        };
        if length == 0 || length > 65536 {
            self.stopped = true;
            return Err(invalid(
                "notification overflow or invalid length; rescan required",
            ));
        }
        let decoded = {
            let bytes = unsafe { std::slice::from_raw_parts(self.buffer as *const u8, length) };
            decode(bytes)
        };
        match decoded {
            Ok(records) => {
                self.records = records;
                self.arm()?;
                Ok(self.records.len())
            }
            Err(e) => {
                self.stopped = true;
                Err(e)
            }
        }
    }
    pub fn stop(&mut self) -> io::Result<()> {
        self.stopped = true;
        if !self.pending {
            return Ok(());
        }
        let cancel = if unsafe { CancelIoEx(self.directory.0, self.overlapped) } == 0 {
            let e = io::Error::last_os_error();
            (e.raw_os_error() != Some(1168)).then_some(e)
        } else {
            None
        };
        let completed = self.complete(true);
        if let Some(e) = cancel {
            return Err(e);
        }
        match completed {
            Ok(Some(_)) => Ok(()),
            Err(e) if matches!(e.raw_os_error(), Some(995 | 1022)) => Ok(()),
            Err(e) => Err(e),
            Ok(None) => Err(invalid("cancel completion remained pending")),
        }
    }
}
impl Drop for Watch {
    fn drop(&mut self) {
        let _ = self.stop();
        if self.pending {
            std::process::abort();
        }
        unsafe {
            drop(Box::from_raw(self.overlapped));
            drop(Box::from_raw(self.buffer));
        }
    }
}
fn decode(bytes: &[u8]) -> io::Result<Vec<Record>> {
    let mut records = Vec::new();
    let mut at = 0;
    loop {
        if records.len() >= 256 {
            return Err(invalid("256 record batch limit exceeded; rescan required"));
        }
        let h = bytes
            .get(at..at + 12)
            .ok_or_else(|| invalid("truncated notification header"))?;
        let get = |n| u32::from_le_bytes(h[n..n + 4].try_into().unwrap());
        let next = get(0) as usize;
        let action = get(4);
        let length = get(8) as usize;
        if length == 0 || length % 2 != 0 || !(1..=5).contains(&action) {
            return Err(invalid("invalid action or UTF16 length"));
        }
        let end = at
            .checked_add(12 + length)
            .ok_or_else(|| invalid("length overflow"))?;
        let name = bytes
            .get(at + 12..end)
            .ok_or_else(|| invalid("truncated UTF16 name"))?
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>();
        if name.contains(&0) {
            return Err(invalid("NUL in notification name"));
        }
        records.push(Record { action, name });
        if next == 0 {
            if bytes.len() - end > 3 {
                return Err(invalid("invalid final padding"));
            }
            break;
        }
        if next % 4 != 0
            || next < 12 + length
            || at.checked_add(next).is_none_or(|n| n >= bytes.len())
        {
            return Err(invalid("invalid next notification offset"));
        }
        at += next;
    }
    Ok(records)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn raw_surrogate_is_retained() {
        let mut b = vec![];
        for v in [0u32, 1, 2] {
            b.extend(v.to_le_bytes());
        }
        b.extend(0xd800u16.to_le_bytes());
        assert_eq!(decode(&b).unwrap()[0].name, [0xd800]);
    }
    #[test]
    fn malformed_is_error_not_empty() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[0; 12]).is_err());
    }
    #[test]
    fn over_capacity_is_error_and_does_not_publish_partial_batch() {
        let mut bytes = Vec::new();
        for i in 0..257 {
            for value in [if i == 256 { 0u32 } else { 16 }, 1, 2] {
                bytes.extend(value.to_le_bytes());
            }
            bytes.extend((b'a' as u16).to_le_bytes());
            bytes.extend([0, 0]);
        }
        assert!(decode(&bytes).is_err());
    }
}
