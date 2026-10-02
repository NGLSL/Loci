//! Separate raw-entry checkpoint; input stays private until graph validation.
use super::inventory::{Data, Inventory, Kind};
use crate::engine::EngineOptions;
use std::ffi::{c_char, c_int, CString, OsStr};
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
const MAGIC: &[u8; 8] = b"LOCISCL1";
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}
fn put(bytes: &mut Vec<u8>, raw: &[u8]) -> io::Result<()> {
    let length = u32::try_from(raw.len()).map_err(|_| invalid("checkpoint field length"))?;
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(raw);
    Ok(())
}
fn scope(options: &EngineOptions) -> io::Result<Vec<u8>> {
    let mut exclusions = options.exclusions.clone();
    exclusions.sort();
    exclusions.dedup();
    if exclusions.len() > 65536 {
        return Err(invalid("checkpoint exclusion budget"));
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&1u32.to_le_bytes()); // Explicit no-mount-traversal policy.
    bytes.extend_from_slice(&(exclusions.len() as u32).to_le_bytes());
    for path in exclusions {
        put(&mut bytes, path.as_os_str().as_bytes())?;
        if bytes.len() > 1024 * 1024 {
            return Err(invalid("checkpoint scope byte budget"));
        }
    }
    Ok(bytes)
}
fn max_bytes(options: &EngineOptions) -> io::Result<usize> {
    options
        .scale_budgets
        .max_slots
        .checked_mul(30)
        .and_then(|bytes| bytes.checked_add(options.scale_budgets.max_name_bytes))
        .and_then(|bytes| bytes.checked_add(1024 * 1024 + 8192))
        .filter(|bytes| *bytes <= 512 * 1024 * 1024)
        .ok_or_else(|| invalid("checkpoint total byte budget"))
}
pub(super) fn encode(
    root: &Path,
    source: (u64, u64),
    mount: u64,
    options: &EngineOptions,
    data: &Data,
    version: u64,
) -> io::Result<Vec<u8>> {
    if data.epoch == u64::MAX || version == 0 || version == u64::MAX {
        return Err(invalid(
            "checkpoint publication epoch/version exhausted; rebuild required",
        ));
    }
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u64.to_le_bytes()); // Final coherent encoded length.
    put(&mut bytes, root.as_os_str().as_bytes())?;
    bytes.extend_from_slice(&source.0.to_le_bytes());
    bytes.extend_from_slice(&source.1.to_le_bytes());
    bytes.extend_from_slice(&mount.to_le_bytes());
    bytes.extend_from_slice(&data.epoch.to_le_bytes());
    bytes.extend_from_slice(&version.to_le_bytes());
    put(&mut bytes, &scope(options)?)?;
    bytes.extend_from_slice(&(data.slots as u64).to_le_bytes());
    for id in 0..data.slots as u32 {
        let entry = data.entry(id);
        bytes.extend_from_slice(&entry.parent.to_le_bytes());
        bytes.push(match entry.kind {
            Kind::File => 0,
            Kind::Directory => 1,
            Kind::Symlink => 2,
        });
        bytes.push(u8::from(entry.alive));
        bytes.extend_from_slice(&entry.dev.to_le_bytes());
        bytes.extend_from_slice(&entry.ino.to_le_bytes());
        put(&mut bytes, data.name(id))?;
    }
    let final_length = bytes
        .len()
        .checked_add(8)
        .ok_or_else(|| invalid("checkpoint length overflow"))?;
    if final_length > max_bytes(options)? {
        return Err(invalid("checkpoint byte budget"));
    }
    bytes[12..20].copy_from_slice(&(final_length as u64).to_le_bytes());
    let sum = checksum(&bytes);
    bytes.extend_from_slice(&sum.to_le_bytes());
    Ok(bytes)
}
unsafe extern "C" {
    fn openat(directory: c_int, name: *const c_char, flags: c_int, ...) -> c_int;
    fn flock(fd: c_int, operation: c_int) -> c_int;
}
pub(crate) struct WriterLock {
    _file: File,
}
impl WriterLock {
    pub fn acquire(parent: &File, name: &OsStr) -> io::Result<Self> {
        let mut raw = name.as_bytes().to_vec();
        raw.extend_from_slice(b".lock");
        let name = CString::new(raw).map_err(|_| invalid("NUL in lock filename"))?;
        // O_RDWR|O_CREAT|O_NOFOLLOW|O_CLOEXEC|O_NONBLOCK.
        let fd = unsafe {
            openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                2 | 0x40 | 0x20000 | 0x80000 | 0x800,
                0o600u32,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if !file.metadata()?.is_file() {
            return Err(invalid("writer lock must be regular"));
        }
        if unsafe { flock(fd, 2 | 4) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { _file: file })
    }
}
struct Input<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Input<'a> {
    fn raw(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| invalid("checkpoint length overflow"))?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| invalid("truncated checkpoint"))?;
        self.position = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.raw(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.raw(8)?.try_into().unwrap()))
    }
    fn field(&mut self, limit: usize) -> io::Result<&'a [u8]> {
        let length = self.u32()? as usize;
        if length > limit {
            return Err(invalid("checkpoint field budget"));
        }
        self.raw(length)
    }
}
pub(super) fn load(
    parent: &File,
    name: &OsStr,
    root: &Path,
    source: (u64, u64),
    mount: u64,
    options: &EngineOptions,
) -> io::Result<Option<(Inventory, u64)>> {
    let name = CString::new(name.as_bytes()).map_err(|_| invalid("NUL in checkpoint filename"))?;
    let fd = unsafe { openat(parent.as_raw_fd(), name.as_ptr(), 0x20000 | 0x80000 | 0x800) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(error)
        };
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    let maximum = max_bytes(options)?;
    if !metadata.is_file() || metadata.len() > maximum as u64 {
        return Err(invalid("checkpoint regular-file/byte budget"));
    }
    let mut bytes = Vec::new();
    file.take(maximum as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(invalid("checkpoint byte budget"));
    }
    if !bytes.starts_with(MAGIC) {
        return Err(invalid(
            "unsupported scale checkpoint format; rebuild required",
        ));
    }
    if bytes.len() < 28 {
        return Err(invalid("truncated scale checkpoint"));
    }
    let body = &bytes[..bytes.len() - 8];
    if checksum(body) != u64::from_le_bytes(bytes[bytes.len() - 8..].try_into().unwrap()) {
        return Err(invalid("scale checkpoint checksum mismatch"));
    }
    let mut input = Input {
        bytes: body,
        position: 8,
    };
    if input.u32()? != 1 {
        return Err(invalid(
            "unsupported scale checkpoint version; rebuild required",
        ));
    }
    if input.u64()? != bytes.len() as u64 {
        return Err(invalid("scale checkpoint encoded length"));
    }
    if input.field(4096)? != root.as_os_str().as_bytes() {
        return Err(invalid("scale checkpoint root mismatch; rebuild required"));
    }
    if (input.u64()?, input.u64()?) != source {
        return Err(invalid(
            "scale checkpoint source identity mismatch; rebuild required",
        ));
    }
    if input.u64()? != mount {
        return Err(invalid(
            "scale checkpoint mount identity mismatch; rebuild required",
        ));
    }
    let epoch = input.u64()?;
    let version = input.u64()?;
    if version == 0 || version == u64::MAX || epoch == u64::MAX {
        return Err(invalid("checkpoint publication version"));
    }
    if input.field(1024 * 1024)? != scope(options)? {
        return Err(invalid("scale checkpoint scope mismatch; rebuild required"));
    }
    let slots = usize::try_from(input.u64()?).map_err(|_| invalid("checkpoint slot count"))?;
    if slots == 0
        || slots > options.scale_budgets.max_slots
        || slots > u32::MAX as usize
        || slots > (body.len() - input.position) / 26
    {
        return Err(invalid("checkpoint slot budget"));
    }
    let mut inventory = Inventory::empty(epoch, options.scale_budgets);
    let mut alive = Vec::with_capacity(slots);
    for id in 0..slots {
        let parent = input.u32()?;
        let kind = match input.raw(1)?[0] {
            0 => Kind::File,
            1 => Kind::Directory,
            2 => Kind::Symlink,
            _ => return Err(invalid("checkpoint entry kind")),
        };
        let live = match input.raw(1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(invalid("checkpoint entry alive flag")),
        };
        let dev = input.u64()?;
        let ino = input.u64()?;
        let name = input.field(255)?;
        if parent as usize >= slots
            || (id == 0 && (parent != 0 || !live || kind != Kind::Directory || !name.is_empty()))
            || (id != 0
                && (name.is_empty()
                    || name == b"."
                    || name == b".."
                    || name.contains(&0)
                    || name.contains(&b'/')))
        {
            return Err(invalid("checkpoint entry identity or basename"));
        }
        inventory.insert_restored(parent, name, kind, dev, ino, live)?;
        alive.push(live);
    }
    if input.position != body.len() {
        return Err(invalid("checkpoint trailing records"));
    }
    let mut colors = vec![0u8; slots];
    colors[0] = 2;
    let mut depths = vec![0usize; slots];
    for id in 1..slots {
        let entry = inventory.data.entry(id as u32);
        if inventory.data.entry(entry.parent).kind != Kind::Directory
            || (alive[id] && !alive[entry.parent as usize])
        {
            return Err(invalid("checkpoint missing live directory parent"));
        }
        if colors[id] == 2 {
            continue;
        }
        let mut chain = Vec::new();
        let mut cursor = id;
        while colors[cursor] == 0 {
            colors[cursor] = 1;
            chain.push(cursor);
            cursor = inventory.data.entry(cursor as u32).parent as usize;
        }
        if colors[cursor] == 1 {
            return Err(invalid("checkpoint parent cycle"));
        }
        let mut depth = depths[cursor];
        for member in chain.into_iter().rev() {
            depth += 1;
            if depth > options.limits.depth + 1 {
                return Err(invalid("checkpoint depth budget"));
            }
            depths[member] = depth;
            colors[member] = 2;
        }
    }
    if inventory.entries > options.limits.entries
        || inventory.directories > options.limits.directories
    {
        return Err(invalid("checkpoint live inventory budget"));
    }
    inventory.validate_restored_lookup()?;
    inventory.finish_derived(&std::sync::atomic::AtomicBool::new(false))?;
    inventory.reset_work();
    Ok(Some((inventory, version)))
}
