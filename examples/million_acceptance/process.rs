use crate::protocol::{b, invalid, n, s, Json};
use std::ffi::CString;
use std::fs;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::Instant;

unsafe extern "C" {
    fn getuid() -> u32;
    fn sysconf(name: i32) -> i64;
    fn poll(fds: *mut PollFd, count: usize, timeout: i32) -> i32;
    fn statvfs(path: *const std::ffi::c_char, out: *mut StatVfs) -> i32;
    fn statfs(path: *const std::ffi::c_char, out: *mut StatFs) -> i32;
}
#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}
#[repr(C)]
#[derive(Default)]
struct StatVfs {
    bsize: u64,
    frsize: u64,
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    favail: u64,
    fsid: u64,
    flag: u64,
    namemax: u64,
    spare: [i32; 6],
}
#[repr(C)]
#[derive(Default)]
struct StatFs {
    kind: i64,
    bsize: i64,
    blocks: u64,
    bfree: u64,
    bavail: u64,
    files: u64,
    ffree: u64,
    fsid: [i32; 2],
    namelen: i64,
    frsize: i64,
    flags: i64,
    spare: [i64; 4],
}
pub fn uid() -> u32 {
    unsafe { getuid() }
}
pub fn hz() -> io::Result<u64> {
    let v = unsafe { sysconf(2) };
    if v <= 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(v as u64)
    }
}
pub fn owned_directory(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.uid() != uid() || fs::canonicalize(path)? != path {
        return Err(invalid("owned resolved directory required"));
    }
    Ok(())
}
pub fn preflight(path: &Path) -> io::Result<Json> {
    let c =
        CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("NUL filesystem path"))?;
    let mut v = StatVfs::default();
    let mut f = StatFs::default();
    if unsafe { statvfs(c.as_ptr(), &mut v) } != 0 || unsafe { statfs(c.as_ptr(), &mut f) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let available = v
        .bavail
        .checked_mul(v.frsize)
        .ok_or_else(|| invalid("filesystem byte overflow"))?;
    let total = v
        .blocks
        .checked_mul(v.frsize)
        .ok_or_else(|| invalid("filesystem byte overflow"))?;
    let dynamic = f.kind == 0x9123683e && v.files == 0 && v.ffree == 0 && v.favail == 0;
    if available < 64 * 1024 * 1024 || available < total * 15 / 100 {
        return Err(invalid("filesystem lacks15%/64MiB reserve"));
    }
    if !dynamic && v.favail < 2048 {
        return Err(invalid("insufficient remaining fixed inode reserve"));
    }
    Ok(Json::object([
        ("filesystem_magic", s(format!("{:x}", f.kind))),
        ("available_bytes", n(available)),
        ("total_bytes", n(total)),
        (
            "free_inodes",
            if dynamic { Json::Null } else { n(v.favail) },
        ),
        ("dynamic_inode_counter", b(dynamic)),
        ("metadata_capacity_verified", b(!dynamic)),
        (
            "metadata_note",
            s(if dynamic {
                "Btrfs dynamic inode counters; creation requires physical metadata DUP pilot and remaining-unallocated checks"
            } else {
                "fixed inode counts available"
            }),
        ),
    ]))
}
pub fn identity(pid: u32) -> io::Result<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, tail) = stat
        .rsplit_once(')')
        .ok_or_else(|| invalid("procstat format"))?;
    let values: Vec<_> = tail.split_whitespace().collect();
    if values.len() < 20 || values[0] == "Z" {
        return Err(invalid("process exited or invalidprocstat"));
    }
    values[19].parse().map_err(|_| invalid("procstat identity"))
}
pub fn sample(pid: u32) -> io::Result<Json> {
    let root = std::path::PathBuf::from(format!("/proc/{pid}"));
    let stat = fs::read_to_string(root.join("stat"))?;
    let (_, tail) = stat
        .rsplit_once(')')
        .ok_or_else(|| invalid("procstat format"))?;
    let values: Vec<_> = tail.split_whitespace().collect();
    if values.len() < 20 || values[0] == "Z" {
        return Err(invalid("process exited or invalidprocstat"));
    }
    let integer = |i: usize| {
        values[i]
            .parse::<u64>()
            .map_err(|_| invalid("procstat number"))
    };
    let mut fields = std::collections::BTreeMap::from([
        ("pid".into(), n(pid)),
        ("process_start_ticks".into(), n(integer(19)?)),
        ("cpu_ticks".into(), n(integer(11)? + integer(12)?)),
        ("clock_ticks_per_second".into(), n(hz()?)),
    ]);
    for line in fs::read_to_string(root.join("status"))?.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let name = match key {
            "VmRSS" => "vmrss_bytes",
            "VmHWM" => "vmhwm_bytes",
            "VmSize" => "vmsize_bytes",
            _ => continue,
        };
        let kb = rest
            .split_whitespace()
            .next()
            .ok_or_else(|| invalid("memory format"))?
            .parse::<u64>()
            .map_err(|_| invalid("memory integer"))?;
        fields.insert(
            name.into(),
            n(kb.checked_mul(1024)
                .ok_or_else(|| invalid("memory overflow"))?),
        );
    }
    let details = (|| -> io::Result<(usize, usize, usize)> {
        let fd_paths = fs::read_dir(root.join("fd"))?
            .map(|r| r.map(|e| e.path()))
            .collect::<io::Result<Vec<_>>>()?;
        let fd_count = fd_paths.iter().filter(|p| fs::read_link(p).is_ok()).count();
        let mut watches = 0;
        let mut inotify = 0;
        for info in fs::read_dir(root.join("fdinfo"))? {
            let info = info?;
            let text = match fs::read_to_string(info.path()) {
                Ok(v) => v,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let count = text
                .lines()
                .filter(|x| x.starts_with("inotify wd:"))
                .count();
            watches += count;
            inotify += usize::from(count > 0);
        }
        Ok((fd_count, watches, inotify))
    })();
    match details {
        Ok((fd, w, ino)) => {
            fields.insert("fdinfo_available".into(), b(true));
            fields.insert("fd_count".into(), n(fd));
            fields.insert("kernel_watch_count".into(), n(w));
            fields.insert("inotify_fds_with_watches".into(), n(ino));
        }
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            fields.insert("fdinfo_available".into(), b(false));
            fields.insert("fd_count".into(), Json::Null);
            fields.insert("kernel_watch_count".into(), Json::Null);
        }
        Err(e) => return Err(e),
    }
    fields.insert("kernel_slab_bytes".into(), Json::Null);
    fields.insert(
        "kernel_slab_reason".into(),
        s("unavailable; not inferred from processRSS or watchcount"),
    );
    Ok(Json::Object(fields))
}
pub struct DeadlineRead<'a, T: Read + AsRawFd> {
    pub stream: &'a mut T,
    pub deadline: Instant,
}
impl<T: Read + AsRawFd> Read for DeadlineRead<'_, T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "worker IPC deadline"))?;
            let timeout = remaining.as_millis().min(i32::MAX as u128).max(1) as i32;
            let mut fd = PollFd {
                fd: self.stream.as_raw_fd(),
                events: 1,
                revents: 0,
            };
            let result = unsafe { poll(&mut fd, 1, timeout) };
            if result == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "worker IPC deadline",
                ));
            }
            if result < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            return self.stream.read(buf);
        }
    }
}
