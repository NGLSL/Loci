//! Documented, read-only Windows calls. No journal create/delete and no elevation.
use crate::model::{decode_records, Journal, Record};
use std::{
    collections::BTreeSet,
    ffi::c_void,
    io, mem,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr,
    time::{Duration, Instant},
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
#[repr(C)]
struct MemoryInfo {
    cb: u32,
    faults: u32,
    peak: usize,
    working: usize,
    paged_peak: usize,
    paged: usize,
    nonpaged_peak: usize,
    nonpaged: usize,
    pagefile: usize,
    pagefile_peak: usize,
    private: usize,
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
    fn GetFileInformationByHandle(h: RawHandle, info: *mut FileInfo) -> i32;
    fn FindFirstFileNameW(
        name: *const u16,
        flags: u32,
        length: *mut u32,
        output: *mut u16,
    ) -> RawHandle;
    fn FindNextFileNameW(h: RawHandle, length: *mut u32, output: *mut u16) -> i32;
    fn FindClose(h: RawHandle) -> i32;
    fn GetCurrentProcess() -> RawHandle;
    fn GetProcessHandleCount(h: RawHandle, count: *mut u32) -> i32;
    fn GetModuleHandleW(name: *const u16) -> RawHandle;
    fn GetVolumeNameForVolumeMountPointW(root: *const u16, name: *mut u16, length: u32) -> i32;
}
#[link(name = "psapi")]
unsafe extern "system" {
    fn GetProcessMemoryInfo(h: RawHandle, info: *mut MemoryInfo, size: u32) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: RawHandle, access: u32, token: *mut RawHandle) -> i32;
    fn GetTokenInformation(
        token: RawHandle,
        class: u32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}
pub fn diagnostics() -> io::Result<()> {
    let runtime = wide(Path::new("envbox-runtime64.dll"));
    let injected = !unsafe { GetModuleHandleW(runtime.as_ptr()) }.is_null();
    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = Handle(token);
    let mut elevated = 0u32;
    let mut len = 0;
    if unsafe { GetTokenInformation(token.0, 20, (&mut elevated as *mut u32).cast(), 4, &mut len) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    println!("process_envbox_module_loaded={injected} process_token_elevated={}; observations refer to this process token and API surface",elevated!=0);
    Ok(())
}

struct Handle(RawHandle);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}
fn open(path: &Path, access: u32, flags: u32) -> io::Result<Handle> {
    let p = wide(path);
    let h = unsafe {
        CreateFileW(
            p.as_ptr(),
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
pub struct Identity {
    pub volume_serial: u64,
    pub object: u128,
    pub links: u32,
    pub attributes: u32,
}
pub fn identity(path: &Path) -> io::Result<Identity> {
    // OPEN_REPARSE_POINT prevents following a selected entry across the boundary.
    let h = open(path, 0, 0x02000000 | 0x00200000)?;
    let mut info: FileInfo = unsafe { mem::zeroed() };
    if unsafe { GetFileInformationByHandle(h.0, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Identity {
        volume_serial: info.serial as u64,
        object: ((info.index_high as u128) << 32) | info.index_low as u128,
        links: info.links,
        attributes: info.attributes,
    })
}
pub struct Metrics {
    pub handles: u32,
    pub working_set: u64,
    pub private_usage: u64,
    pub peak_working_set: u64,
}
pub fn metrics() -> io::Result<Metrics> {
    let mut count = 0;
    let mut info: MemoryInfo = unsafe { mem::zeroed() };
    info.cb = mem::size_of::<MemoryInfo>() as u32;
    if unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut info, info.cb) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Metrics {
        handles: count,
        working_set: info.working as u64,
        private_usage: info.private as u64,
        peak_working_set: info.peak as u64,
    })
}
pub fn hardlink_names(path: &Path) -> io::Result<BTreeSet<Vec<u16>>> {
    struct Find(RawHandle);
    impl Drop for Find {
        fn drop(&mut self) {
            unsafe {
                FindClose(self.0);
            }
        }
    }
    let p = wide(path);
    let mut buffer = vec![0u16; 32768];
    let mut len = buffer.len() as u32;
    let h = unsafe { FindFirstFileNameW(p.as_ptr(), 0, &mut len, buffer.as_mut_ptr()) };
    if h == INVALID {
        return Err(io::Error::last_os_error());
    }
    let h = Find(h);
    let mut out = BTreeSet::new();
    loop {
        let end = buffer
            .iter()
            .position(|&n| n == 0)
            .ok_or_else(|| io::Error::other("hardlink name lacks terminator"))?;
        if !out.insert(buffer[..end].to_vec()) {
            return Err(io::Error::other("duplicate hardlink enumeration"));
        }
        if out.len() > 1024 {
            return Err(io::Error::other("hardlink budget exceeded; incomplete"));
        }
        buffer.fill(0);
        len = buffer.len() as u32;
        if unsafe { FindNextFileNameW(h.0, &mut len, buffer.as_mut_ptr()) } == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(38) {
                break;
            }
            return Err(e);
        }
    }
    Ok(out)
}
pub struct Volume {
    handle: Handle,
    pub serial: u64,
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
    let path = wide(Path::new(&format!("{letter}:\\")));
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
    pub fn open(spec: &str, access: u32) -> io::Result<Self> {
        let (letter, serial, _, fs) = volume_metadata(spec)?;
        if fs != "NTFS" {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "non-NTFS: directory scanning + native notifications required",
            ));
        }
        Ok(Self {
            handle: open(Path::new(&format!(r"\\.\{letter}:")), access, 0x40000000)?,
            serial,
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
        let (min_major, max_major) = if b.len() >= 60 {
            (
                u16::from_le_bytes(b[56..58].try_into().unwrap()),
                u16::from_le_bytes(b[58..60].try_into().unwrap()),
            )
        } else {
            (2, 2)
        };
        let j = Journal {
            id: u64_at(0),
            first: u64_at(8) as i64,
            next: u64_at(16) as i64,
            lowest: u64_at(24) as i64,
            min_major,
            max_major,
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
        input.extend(u32::MAX.to_le_bytes());
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
}
fn outcome(label: &str, result: io::Result<impl std::fmt::Debug>) -> bool {
    match result {
        Ok(value) => {
            println!("{label}: OK {value:?}");
            true
        }
        Err(e) => {
            println!(
                "{label}: REFUSED kind={:?} os_code={:?} detail={e}",
                e.kind(),
                e.raw_os_error()
            );
            false
        }
    }
}
pub fn capabilities(spec: &str) -> io::Result<()> {
    diagnostics()?;
    let _ = metrics()?;
    let before = metrics()?;
    let now = Instant::now();
    let (letter, serial, flags, fs) = volume_metadata(spec)?;
    println!("volume={letter}: serial={serial:#x} filesystem={fs} flags={flags:#x}; no names collected; no journal mutation");
    let mount = wide(Path::new(&format!("{letter}:\\")));
    let mut guid = [0u16; 128];
    if unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), guid.as_mut_ptr(), guid.len() as u32)
    } != 0
    {
        let len = guid.iter().position(|&c| c == 0).unwrap_or(guid.len());
        println!("volume_guid={}", String::from_utf16_lossy(&guid[..len]));
    } else {
        outcome(
            "GetVolumeNameForVolumeMountPointW",
            Err::<(), _>(io::Error::last_os_error()),
        );
    }
    if fs != "NTFS" {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "not NTFS"));
    }
    let mut complete = false;
    for (label, access) in [("zero_access", 0), ("generic_read", 0x80000000)] {
        let v = match Volume::open(spec, access) {
            Ok(v) => {
                println!("CreateFileW({label}): OK");
                v
            }
            Err(e) => {
                outcome(&format!("CreateFileW({label})"), Err::<(), _>(e));
                continue;
            }
        };
        let journal = match v.query() {
            Ok(j) => {
                println!(
                    "QUERY({label}): OK id={:#x} first={} next={} lowest={} supported_major={}..{}",
                    j.id, j.first, j.next, j.lowest, j.min_major, j.max_major
                );
                Some(j)
            }
            Err(e) => {
                outcome(&format!("QUERY({label})"), Err::<(), _>(e));
                None
            }
        };
        // EOF sentinel is a permissions/API probe, never a volume enumeration.
        let enum_ok = match v.enumerate(u64::MAX, i64::MAX) {
            Err(e) if e.raw_os_error() == Some(38) => {
                println!("ENUM({label}): EOF sentinel accepted, os_code=38; enumeration correctness NOT tested");
                true
            }
            other => outcome(&format!("ENUM({label})"), other.map(|(_, r)| r.len())),
        };
        let (id, start) = journal.as_ref().map(|j| (j.id, j.next)).unwrap_or((0, 0));
        let read_ok = outcome(
            &format!(
                "READ({label}, {}, nonblocking)",
                if journal.is_some() {
                    "NextUsn"
                } else {
                    "invalid ID=0 sentinel: query unavailable"
                }
            ),
            v.read(id, start).map(|(c, r)| (c, r.len())),
        );
        // Check error paths repeatedly within this process, so lazy loader work
        // is separated from a repeatable per-operation handle leak.
        let baseline = metrics()?.handles;
        for _ in 0..20 {
            let _ = v.query();
            let _ = v.enumerate(u64::MAX, i64::MAX);
            let _ = v.read(id, start);
        }
        let final_handles = metrics()?.handles;
        println!("FSCTL_lifecycle({label}) repeated_cycles=20 handles_before={baseline} handles_after={final_handles}");
        if baseline != final_handles {
            return Err(io::Error::other(
                "FSCTL handle count did not return to baseline",
            ));
        }
        complete |= journal.is_some() && enum_ok && read_ok;
    }
    let after = metrics()?;
    println!("elapsed_ms={} handles_before={} handles_after={} total_working_set_bytes={} private_commit_bytes={} peak_total_working_set_bytes={}; kernel_bytes=unmeasured private_working_set=unmeasured",now.elapsed().as_millis(),before.handles,after.handles,after.working_set,after.private_usage,after.peak_working_set);
    if !complete {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,"documented privileged NTFS route unavailable in current token; see per-API OS errors, do not publish an empty complete inventory"));
    }
    Ok(())
}
