//! Read-only native NTFS volume access; errors retain their Win32 code.
use super::usn::{decode_records, Journal, Record};
use std::{
    ffi::c_void,
    io, mem,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
type RawHandle = *mut c_void;
const INVALID: RawHandle = -1isize as RawHandle;
const QUERY: u32 = 0x000900f4;
const ENUM: u32 = 0x000900b3;
const READ: u32 = 0x000900bb;
const BUFFER: usize = 65536;
#[repr(C)]
struct Overlapped {
    internal: usize,
    high: usize,
    offset: u32,
    offset_high: u32,
    event: RawHandle,
}
#[repr(C)]
struct FileInfo {
    attributes: u32,
    creation: [u32; 2],
    access: [u32; 2],
    write: [u32; 2],
    serial: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
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
        template: RawHandle,
    ) -> RawHandle;
    fn CloseHandle(h: RawHandle) -> i32;
    fn DeviceIoControl(
        h: RawHandle,
        code: u32,
        input: *const c_void,
        size: u32,
        output: *mut c_void,
        output_size: u32,
        returned: *mut u32,
        ov: *mut Overlapped,
    ) -> i32;
    fn CreateEventW(
        security: *const c_void,
        manual: i32,
        initial: i32,
        name: *const u16,
    ) -> RawHandle;
    fn WaitForSingleObject(h: RawHandle, millis: u32) -> u32;
    fn GetOverlappedResult(h: RawHandle, ov: *mut Overlapped, returned: *mut u32, wait: i32)
        -> i32;
    fn CancelIoEx(h: RawHandle, ov: *mut Overlapped) -> i32;
    fn GetVolumeInformationW(
        root: *const u16,
        label: *mut u16,
        label_size: u32,
        serial: *mut u32,
        component: *mut u32,
        flags: *mut u32,
        fs: *mut u16,
        fs_size: u32,
    ) -> i32;
    fn GetFileInformationByHandle(h: RawHandle, info: *mut u32) -> i32;
    fn GetVolumePathNameW(path: *const u16, root: *mut u16, length: u32) -> i32;
    fn GetVolumeNameForVolumeMountPointW(root: *const u16, name: *mut u16, length: u32) -> i32;
    fn GetFileInformationByHandleEx(h: RawHandle, class: u32, info: *mut c_void, size: u32) -> i32;
}
struct Handle(RawHandle);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
// Windows kernel handles can move between threads; each operation has its own OVERLAPPED.
unsafe impl Send for Handle {}
fn open_wide(path: &[u16], access: u32, flags: u32) -> io::Result<Handle> {
    if path.is_empty() || path.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty path or embedded NUL",
        ));
    }
    let terminated: Vec<u16> = path.iter().copied().chain(Some(0)).collect();
    let h = unsafe {
        CreateFileW(
            terminated.as_ptr(),
            access,
            7,
            ptr::null(),
            3,
            flags,
            ptr::null_mut(),
        )
    };
    if h == INVALID {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(h))
    }
}
fn identity(h: &Handle) -> io::Result<(u64, u64, u32)> {
    let mut info: FileInfo = unsafe { mem::zeroed() };
    if unsafe { GetFileInformationByHandle(h.0, (&mut info as *mut FileInfo).cast()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        info.serial as u64,
        (info.index_high as u64) << 32 | info.index_low as u64,
        info.attributes,
    ))
}
/// Identify an ordinary directory independently of drive-letter aliases.
/// GetVolumePathNameW resolves drive-mounted volumes; no privileged volume handle is needed.
pub(super) fn directory_source(path: &Path) -> io::Result<(String, u64)> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory source requires an absolute path",
        ));
    }
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    let before = open_wide(&raw, 0, 0x02000000 | 0x00200000)?;
    let expected = identity(&before)?;
    if expected.1 == 0 || expected.2 & (0x10 | 0x400) != 0x10 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory source requires an ordinary directory",
        ));
    }
    let terminated: Vec<u16> = raw.iter().copied().chain(Some(0)).collect();
    let mut mount = vec![0u16; 32768];
    if unsafe { GetVolumePathNameW(terminated.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut guid = [0u16; 128];
    if unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let end = guid
        .iter()
        .position(|&c| c == 0)
        .ok_or_else(|| io::Error::other("unterminated volume GUID"))?;
    let source =
        String::from_utf16(&guid[..end]).map_err(|_| io::Error::other("invalid volume GUID"))?;
    let mut serial = 0;
    if unsafe {
        GetVolumeInformationW(
            guid.as_ptr(),
            ptr::null_mut(),
            0,
            &mut serial,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if serial as u64 != expected.0
        || identity(&open_wide(&raw, 0, 0x02000000 | 0x00200000)?)? != expected
    {
        return Err(io::Error::other(
            "directory source identity changed during lookup",
        ));
    }
    Ok((source, expected.1))
}
pub struct Volume {
    handle: Handle,
    pub serial: u64,
    pub guid: String,
    pub letter: char,
    pub root: u64,
}
pub fn volume_metadata(spec: &str) -> io::Result<(char, u64, u32, String)> {
    let b = spec.as_bytes();
    if b.len() != 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "explicit drive-letter volume required, e.g. D:",
        ));
    }
    let letter = (b[0] as char).to_ascii_uppercase();
    let path = format!("{letter}:\\")
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut serial = 0;
    let mut component = 0;
    let mut flags = 0;
    let mut fs = [0u16; 32];
    if unsafe {
        GetVolumeInformationW(
            path.as_ptr(),
            ptr::null_mut(),
            0,
            &mut serial,
            &mut component,
            &mut flags,
            fs.as_mut_ptr(),
            32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let n = fs.iter().position(|&c| c == 0).unwrap_or(fs.len());
    Ok((
        letter,
        serial as u64,
        flags,
        String::from_utf16(&fs[..n]).map_err(|_| io::Error::other("invalid filesystem name"))?,
    ))
}
impl Volume {
    pub fn open(spec: &str) -> io::Result<Self> {
        let (letter, serial, _, fs) = volume_metadata(spec)?;
        if fs != "NTFS" {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "NTFS volume required",
            ));
        }
        let mount: Vec<u16> = format!("{letter}:\\")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut guid = [0u16; 128];
        if unsafe {
            GetVolumeNameForVolumeMountPointW(mount.as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let length = guid
            .iter()
            .position(|&c| c == 0)
            .ok_or_else(|| io::Error::other("unterminated volume GUID"))?;
        let guid = String::from_utf16(&guid[..length])
            .map_err(|_| io::Error::other("invalid volume GUID"))?;
        let root_path: Vec<u16> = guid.encode_utf16().collect();
        let root_handle = open_wide(&root_path, 0, 0x02000000 | 0x00200000)?;
        let (root_serial, root, attrs) = identity(&root_handle)?;
        if root_serial != serial || root == 0 || attrs & (0x10 | 0x400) != 0x10 {
            return Err(io::Error::other("invalid volume root identity"));
        }
        let volume_path: Vec<u16> = guid.trim_end_matches('\\').encode_utf16().collect();
        let handle = open_wide(&volume_path, 0x80000000, 0x40000000)?;
        Ok(Self {
            handle,
            serial,
            guid,
            letter,
            root,
        })
    }
    fn ioctl(&self, code: u32, input: &[u8], deadline: Duration) -> io::Result<Vec<u8>> {
        // Buffers and OVERLAPPED stay alive until completion, including cancellation.
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let event = Handle(event);
        let mut ov = Box::new(Overlapped {
            internal: 0,
            high: 0,
            offset: 0,
            offset_high: 0,
            event: event.0,
        });
        // u64 backing guarantees alignment for USN/FSCTL buffers.
        let mut buffer = vec![0u64; BUFFER / 8];
        let mut returned = 0;
        let ok = unsafe {
            DeviceIoControl(
                self.handle.0,
                code,
                input.as_ptr().cast(),
                input.len() as u32,
                buffer.as_mut_ptr().cast(),
                BUFFER as u32,
                &mut returned,
                &mut *ov,
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(997) {
                return Err(e);
            }
            let wait = unsafe {
                WaitForSingleObject(event.0, deadline.as_millis().min(u32::MAX as u128) as u32)
            };
            if wait != 0 {
                let wait_error = if wait == 258 {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "FSCTL deadline; cancellation requested and drained",
                    )
                } else {
                    io::Error::last_os_error()
                };
                let cancel = unsafe { CancelIoEx(self.handle.0, &mut *ov) };
                let ce = (cancel == 0).then(io::Error::last_os_error);
                let completed =
                    unsafe { GetOverlappedResult(self.handle.0, &mut *ov, &mut returned, 1) };
                let completion_error = (completed == 0).then(io::Error::last_os_error);
                if let Some(e) = ce {
                    if e.raw_os_error() != Some(1168) {
                        return Err(e);
                    }
                }
                if let Some(e) = completion_error {
                    if e.raw_os_error() != Some(995) {
                        return Err(e);
                    }
                }
                return Err(wait_error);
            }
            if unsafe { GetOverlappedResult(self.handle.0, &mut *ov, &mut returned, 0) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        if returned as usize > BUFFER {
            return Err(io::Error::other("invalid FSCTL return length"));
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), returned as usize) };
        Ok(bytes.to_vec())
    }
    pub fn query(&self) -> io::Result<Journal> {
        let b = self.ioctl(QUERY, &[], Duration::from_secs(2))?;
        if b.len() < 56 {
            return Err(io::Error::other("short USN_JOURNAL_DATA"));
        }
        let u64_at = |i| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        let j = Journal {
            id: u64_at(0),
            first: u64_at(8) as i64,
            next: u64_at(16) as i64,
            lowest: u64_at(24) as i64,
        };
        if j.first < 0 || j.lowest < 0 || j.next < j.first {
            return Err(io::Error::other("invalid journal boundaries"));
        }
        Ok(j)
    }
    pub fn enumerate(&self, start: u64, high: i64) -> io::Result<(u64, Vec<Record>)> {
        let mut input = Vec::new();
        input.extend(start.to_le_bytes());
        input.extend(0i64.to_le_bytes());
        input.extend(high.to_le_bytes());
        let b = self.ioctl(ENUM, &input, Duration::from_secs(2))?;
        if b.len() < 8 {
            return Err(io::Error::other("short enumeration cursor"));
        }
        let cursor = u64::from_le_bytes(b[..8].try_into().unwrap());
        let records = decode_records(&b[8..])?;
        if cursor <= start {
            return Err(io::Error::other("MFT enumeration cursor failed to advance"));
        }
        Ok((cursor, records))
    }
    pub fn read(&self, journal_id: u64, start: i64) -> io::Result<(i64, Vec<Record>)> {
        let mut input = Vec::new();
        input.extend(start.to_le_bytes());
        input.extend(0x0011_b300u32.to_le_bytes());
        input.extend(0u32.to_le_bytes());
        input.extend(0u64.to_le_bytes());
        input.extend(0u64.to_le_bytes());
        input.extend(journal_id.to_le_bytes());
        let b = self.ioctl(READ, &input, Duration::from_secs(2))?;
        if b.len() < 8 {
            return Err(io::Error::other("short journal cursor"));
        }
        let cursor = i64::from_le_bytes(b[..8].try_into().unwrap());
        if cursor < start {
            return Err(io::Error::other("journal cursor moved backwards"));
        }
        let records = decode_records(&b[8..])?;
        Ok((cursor, records))
    }
    /// Enumerate every name in one ordinary directory. A reparse directory is never followed.
    pub fn list_directory(
        &self,
        path: &[u16],
        parent: u64,
        cancel: &AtomicBool,
    ) -> io::Result<Vec<Record>> {
        check_cancel(cancel)?;
        let before = open_wide(path, 0, 0x02000000 | 0x00200000)?;
        let expected = identity(&before)?;
        if expected.0 != self.serial
            || expected.1 != parent
            || parent == 0
            || expected.2 & (0x10 | 0x400) != 0x10
        {
            return Err(io::Error::other(
                "directory identity mismatch or reparse directory",
            ));
        }
        let handle = open_wide(path, 1, 0x02000000 | 0x00200000)?;
        if identity(&handle)? != expected {
            return Err(io::Error::other("directory changed before enumeration"));
        }
        let mut buffer = vec![0u64; BUFFER / 8];
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), BUFFER) };
        let mut class = 11;
        let mut entries = Vec::new();
        let mut name_bytes = 0usize;
        loop {
            check_cancel(cancel)?;
            bytes.fill(0);
            if unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    class,
                    bytes.as_mut_ptr().cast(),
                    BUFFER as u32,
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(18) {
                    break;
                }
                return Err(error);
            }
            class = 10;
            let batch = decode_directory_batch(bytes, parent)?;
            name_bytes = batch
                .iter()
                .try_fold(name_bytes, |n, r| n.checked_add(r.name.len() * 2))
                .ok_or_else(|| io::Error::other("directory memory budget overflow"))?;
            let count = entries
                .len()
                .checked_add(batch.len())
                .ok_or_else(|| io::Error::other("directory entry count overflow"))?;
            // Accounts for Vec spare capacity and temporary decoded batch, supporting over a million small names.
            if count
                .checked_mul(mem::size_of::<Record>() * 2)
                .and_then(|n| n.checked_add(name_bytes))
                .is_none_or(|n| n > 256 * 1024 * 1024)
            {
                return Err(io::Error::other(
                    "directory listing exceeds 256 MiB memory budget",
                ));
            }
            entries
                .try_reserve(batch.len())
                .map_err(|e| io::Error::other(e.to_string()))?;
            entries.extend(batch);
        }
        check_cancel(cancel)?;
        if identity(&handle)? != expected
            || identity(&open_wide(path, 0, 0x02000000 | 0x00200000)?)? != expected
        {
            return Err(io::Error::other("directory changed during enumeration"));
        }
        // Native iteration can repeat entries when the directory changes concurrently.
        // Sorting consumes no second name inventory and makes such an observation explicit.
        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        if entries.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err(io::Error::other("directory enumeration repeated a name"));
        }
        check_cancel(cancel)?;
        Ok(entries)
    }
}
fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "native enumeration cancelled",
        ))
    } else {
        Ok(())
    }
}
fn decode_directory_batch(bytes: &[u8], parent: u64) -> io::Result<Vec<Record>> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid FILE_ID_BOTH_DIR_INFO batch",
        )
    };
    let mut records = Vec::new();
    let mut offset = 0usize;
    loop {
        if offset % 8 != 0 || offset + 104 > bytes.len() {
            return Err(invalid());
        }
        let b = &bytes[offset..];
        let next = u32::from_le_bytes(b[..4].try_into().unwrap()) as usize;
        let attrs = u32::from_le_bytes(b[56..60].try_into().unwrap());
        let length = u32::from_le_bytes(b[60..64].try_into().unwrap()) as usize;
        let end = 104usize.checked_add(length).ok_or_else(invalid)?;
        if length == 0
            || length % 2 != 0
            || length > 510
            || end > b.len()
            || (next != 0 && (next % 8 != 0 || next < end || next > b.len()))
        {
            return Err(invalid());
        }
        let name: Vec<u16> = b[104..end]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        if name.iter().any(|&c| c == 0 || c == 47 || c == 92) {
            return Err(invalid());
        }
        if name != [46] && name != [46, 46] {
            let object = u64::from_le_bytes(b[96..104].try_into().unwrap());
            if object == 0 {
                return Err(invalid());
            }
            records.push(Record {
                object,
                parent,
                usn: 0,
                reason: 0,
                attributes: attrs,
                name,
            });
        }
        if next == 0 {
            break;
        }
        offset = offset.checked_add(next).ok_or_else(invalid)?;
    }
    Ok(records)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directory_batch_preserves_utf16_and_rejects_overlap() {
        let mut b = vec![0u8; BUFFER];
        b[60..64].copy_from_slice(&4u32.to_le_bytes());
        b[96..104].copy_from_slice(&42u64.to_le_bytes());
        b[104..106].copy_from_slice(&0xd800u16.to_le_bytes());
        b[106..108].copy_from_slice(&65u16.to_le_bytes());
        let records = decode_directory_batch(&b, 7).unwrap();
        assert_eq!(records[0].name, [0xd800, 65]);
        assert_eq!(records[0].object, 42);
        b[..4].copy_from_slice(&104u32.to_le_bytes());
        assert!(decode_directory_batch(&b, 7).is_err());
    }
    #[test]
    fn native_directory_lists_hardlinks_and_validates_identity() {
        use std::os::windows::ffi::OsStrExt;
        let dir =
            std::env::temp_dir().join(format!("loci-native-directory-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        std::fs::write(dir.join("原始.txt"), b"data").unwrap();
        std::fs::hard_link(dir.join("原始.txt"), dir.join("alias.txt")).unwrap();
        std::fs::create_dir(dir.join("child")).unwrap();
        let path: Vec<u16> = dir.as_os_str().encode_wide().collect();
        let h = open_wide(&path, 0, 0x02000000 | 0x00200000).unwrap();
        let (serial, root, _) = identity(&h).unwrap();
        // Directory namespace validation does not require an elevated volume handle.
        let volume = Volume {
            handle: h,
            serial,
            guid: String::new(),
            letter: 'C',
            root,
        };
        let entries = volume
            .list_directory(&path, root, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(entries.len(), 3);
        let original = entries
            .iter()
            .find(|r| r.name == "原始.txt".encode_utf16().collect::<Vec<_>>())
            .unwrap();
        let alias = entries
            .iter()
            .find(|r| r.name == "alias.txt".encode_utf16().collect::<Vec<_>>())
            .unwrap();
        assert_eq!(original.object, alias.object);
        assert!(entries.iter().all(|r| r.parent == root));
        assert_eq!(
            volume
                .list_directory(&path, root + 1, &AtomicBool::new(false))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        assert_eq!(
            volume
                .list_directory(&path, root, &AtomicBool::new(true))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }
    #[test]
    fn directory_source_matches_real_directory_identity() {
        let dir = std::env::temp_dir().join(format!("loci-native-source-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let raw: Vec<u16> = dir.as_os_str().encode_wide().collect();
        let handle = open_wide(&raw, 0, 0x02000000 | 0x00200000).unwrap();
        let (_, expected, _) = identity(&handle).unwrap();
        let (guid, object) = directory_source(&dir).unwrap();
        assert_eq!(object, expected);
        assert!(guid.starts_with(r"\\?\Volume{"));
        assert!(guid.ends_with(r"}\"));
        assert_eq!(
            directory_source(&dir.parent().unwrap().join(dir.file_name().unwrap())).unwrap(),
            (guid, object)
        );
        assert_eq!(
            directory_source(Path::new("relative")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        std::fs::write(dir.join("file"), b"data").unwrap();
        assert_eq!(
            directory_source(&dir.join("file")).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
    #[test]
    fn cancellation_is_explicit() {
        assert_eq!(
            check_cancel(&AtomicBool::new(true)).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }
}

#[cfg(test)]
pub(super) fn test_directory(path: &[u16]) -> io::Result<Volume> {
    let handle = open_wide(path, 0, 0x02000000 | 0x00200000)?;
    let (serial, root, _) = identity(&handle)?;
    Ok(Volume {
        handle,
        serial,
        guid: String::new(),
        letter: 'D',
        root,
    })
}
