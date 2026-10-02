//! Bounded, root-bound v1 snapshots. Historical experiment formats require rebuilding.
use crate::watch::{Inventory, Kind};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"LOCISNP1";
const VERSION: u32 = 1;
const PATH_LIMIT: usize = 4096;
const INPUT_LIMIT: usize = 1024 * 1024;
const ENTRY_LIMIT: usize = 4096;
const HEADER: usize = 20;
const MAX_BYTES: usize = HEADER + 4 + PATH_LIMIT + 4 + INPUT_LIMIT + ENTRY_LIMIT * 5 + 8;
const PLATFORM: u32 = if cfg!(windows) {
    1
} else if cfg!(unix) {
    2
} else {
    3
};

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub root: PathBuf,
    pub inventory: Inventory,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn root_path(root: &Path) -> io::Result<PathBuf> {
    let root = fs::canonicalize(root)?;
    if !root.is_dir() {
        return Err(invalid("snapshot root must be a directory"));
    }
    text_path(&root)?;
    Ok(root)
}
fn text_path(path: &Path) -> io::Result<&str> {
    let text = path
        .to_str()
        .ok_or_else(|| invalid("snapshot requires UTF-8 paths"))?;
    if text.is_empty() || text.len() > PATH_LIMIT || text.contains('\0') {
        return Err(invalid("snapshot path budget or invalid path"));
    }
    Ok(text)
}
fn validate(inventory: &Inventory) -> io::Result<()> {
    if !inventory.complete || !inventory.errors.is_empty() {
        return Err(invalid("cannot save incomplete inventory"));
    }
    if inventory.entries.len() > ENTRY_LIMIT {
        return Err(invalid("snapshot entry budget"));
    }
    let mut input = 0usize;
    let mut directories = 1usize;
    for (path, kind) in &inventory.entries {
        let text = text_path(path)?;
        input = input
            .checked_add(text.len())
            .ok_or_else(|| invalid("snapshot input budget"))?;
        if input > INPUT_LIMIT {
            return Err(invalid("snapshot input budget"));
        }
        if !path.components().all(|c| matches!(c, Component::Normal(_)))
            || text
                .split(|c| c == '/' || (cfg!(windows) && c == '\\'))
                .any(|s| s.is_empty() || s == "." || s == "..")
            || (cfg!(windows) && text.contains(':'))
        {
            return Err(invalid("unsafe snapshot relative path"));
        }
        let depth = path.components().count();
        if depth > if *kind == Kind::Directory { 16 } else { 17 } {
            return Err(invalid("snapshot depth budget"));
        }
        if *kind == Kind::Directory {
            directories += 1;
            if crate::watch::skip(path) {
                return Err(invalid("excluded snapshot directory"));
            }
        }
        let mut parent = path.parent();
        while let Some(p) = parent.filter(|p| !p.as_os_str().is_empty()) {
            if inventory.entries.get(p) != Some(&Kind::Directory) {
                return Err(invalid(
                    "snapshot missing directory parent or conflicting kind",
                ));
            }
            parent = p.parent();
        }
    }
    if directories > 128 {
        return Err(invalid("snapshot directory budget"));
    }
    Ok(())
}
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}
fn put_path(bytes: &mut Vec<u8>, path: &Path) -> io::Result<()> {
    let text = text_path(path)?;
    bytes.extend_from_slice(&(text.len() as u32).to_le_bytes());
    bytes.extend_from_slice(text.as_bytes());
    Ok(())
}
impl Snapshot {
    pub fn new(root: &Path, inventory: Inventory) -> io::Result<Self> {
        validate(&inventory)?;
        Ok(Self {
            root: root_path(root)?,
            inventory,
        })
    }
    pub fn save(&self, path: &Path) -> io::Result<()> {
        validate(&self.inventory)?;
        if root_path(&self.root)? != self.root {
            return Err(invalid("snapshot root changed"));
        }
        let mut payload = Vec::new();
        put_path(&mut payload, &self.root)?;
        payload.extend_from_slice(&(self.inventory.entries.len() as u32).to_le_bytes());
        for (relative, kind) in &self.inventory.entries {
            payload.push(if *kind == Kind::File { 0 } else { 1 });
            put_path(&mut payload, relative)?;
        }
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&PLATFORM.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&payload);
        bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
        atomic_save(path, &bytes)
    }
    pub fn load(path: &Path, root: &Path) -> io::Result<Self> {
        validate_destination(path)?;
        // Reject nonregular paths before open as well as through the opened
        // handle. The handle check closes replacement races after this check.
        if !fs::metadata(path)?.is_file() {
            return Err(invalid("snapshot input must be a regular file"));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(all(
            target_os = "linux",
            any(
                target_arch = "x86",
                target_arch = "x86_64",
                target_arch = "aarch64",
                target_arch = "arm",
                target_arch = "riscv64"
            )
        ))]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Linux O_NONBLOCK on the supported asm-generic/x86 profiles. A
            // concurrently substituted FIFO must not block before metadata.
            options.custom_flags(0x800);
        }
        let file = options.open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("snapshot input must be a regular file"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > MAX_BYTES {
            return Err(invalid("snapshot byte budget"));
        }
        if !bytes.starts_with(MAGIC) {
            return Err(invalid("unsupported snapshot format; rebuild the index"));
        }
        if bytes.len() < HEADER + 8 {
            return Err(invalid("truncated snapshot"));
        }
        let mut pos = 8;
        if number(&bytes, &mut pos)? != VERSION {
            return Err(invalid("unsupported snapshot version; rebuild the index"));
        }
        if number(&bytes, &mut pos)? != PLATFORM {
            return Err(invalid("snapshot platform mismatch; rebuild the index"));
        }
        let length = number(&bytes, &mut pos)? as usize;
        if length > MAX_BYTES - HEADER - 8 || length.checked_add(HEADER + 8) != Some(bytes.len()) {
            return Err(invalid("invalid snapshot payload length"));
        }
        let body = &bytes[..bytes.len() - 8];
        let stored = u64::from_le_bytes(
            bytes[bytes.len() - 8..]
                .try_into()
                .map_err(|_| invalid("truncated checksum"))?,
        );
        if stored != checksum(body) {
            return Err(invalid("snapshot checksum mismatch"));
        }
        let stored_root = get_path(body, &mut pos)?;
        let root = root_path(root)?;
        if stored_root != root {
            return Err(invalid("snapshot root mismatch; rebuild the index"));
        }
        let count = number(body, &mut pos)? as usize;
        if count > ENTRY_LIMIT {
            return Err(invalid("snapshot entry budget"));
        }
        let mut inventory = Inventory {
            complete: true,
            ..Inventory::default()
        };
        let mut input = 0usize;
        for _ in 0..count {
            let kind = match body.get(pos) {
                Some(0) => Kind::File,
                Some(1) => Kind::Directory,
                _ => return Err(invalid("invalid snapshot kind")),
            };
            pos += 1;
            let relative = get_path(body, &mut pos)?;
            input += text_path(&relative)?.len();
            if input > INPUT_LIMIT {
                return Err(invalid("snapshot input budget"));
            }
            if inventory.entries.insert(relative, kind).is_some() {
                return Err(invalid("duplicate snapshot path"));
            }
        }
        if pos != body.len() {
            return Err(invalid("snapshot trailing bytes"));
        }
        validate(&inventory)?;
        inventory.directories = 1 + inventory
            .entries
            .values()
            .filter(|k| **k == Kind::Directory)
            .count();
        inventory.examined = inventory.entries.len();
        Ok(Self { root, inventory })
    }
}
fn number(bytes: &[u8], pos: &mut usize) -> io::Result<u32> {
    let end = pos
        .checked_add(4)
        .ok_or_else(|| invalid("snapshot offset overflow"))?;
    let data = bytes
        .get(*pos..end)
        .ok_or_else(|| invalid("truncated snapshot number"))?;
    *pos = end;
    Ok(u32::from_le_bytes(
        data.try_into()
            .map_err(|_| invalid("truncated snapshot number"))?,
    ))
}
fn get_path(bytes: &[u8], pos: &mut usize) -> io::Result<PathBuf> {
    let length = number(bytes, pos)? as usize;
    if length == 0 || length > PATH_LIMIT {
        return Err(invalid("snapshot path budget"));
    }
    let end = pos
        .checked_add(length)
        .ok_or_else(|| invalid("snapshot path overflow"))?;
    let data = bytes
        .get(*pos..end)
        .ok_or_else(|| invalid("truncated snapshot path"))?;
    *pos = end;
    let text = std::str::from_utf8(data).map_err(|_| invalid("snapshot requires UTF-8 paths"))?;
    Ok(PathBuf::from(text))
}

struct Temporary(Option<PathBuf>);
impl Drop for Temporary {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}
fn validate_destination(path: &Path) -> io::Result<()> {
    if path.file_name().is_none() {
        return Err(invalid("snapshot destination requires filename"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::path::Prefix;
        if path.as_os_str().encode_wide().any(|c| c == 0) {
            return Err(invalid("NUL in snapshot destination"));
        }
        for component in path.components() {
            match component {
                Component::Normal(name) if name.encode_wide().any(|c| c == u16::from(b':')) => {
                    return Err(invalid("ADS in snapshot destination"));
                }
                Component::Prefix(prefix) if matches!(prefix.kind(), Prefix::DeviceNS(_)) => {
                    return Err(invalid("device namespace in snapshot destination"));
                }
                _ => {}
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if path.as_os_str().as_bytes().contains(&0) {
            return Err(invalid("NUL in snapshot destination"));
        }
    }
    Ok(())
}
fn atomic_save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    validate_destination(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| invalid("snapshot destination requires filename"))?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut created = None;
    for _ in 0..16 {
        let mut temp_name = name.to_os_string();
        temp_name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let temp = parent.join(temp_name);
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => {
                created = Some((Temporary(Some(temp)), file));
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    let (mut temp, mut file) = created.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "snapshot temporary filename collision budget",
        )
    })?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    replace(
        temp.0
            .as_ref()
            .ok_or_else(|| invalid("missing snapshot temporary"))?,
        path,
    )?;
    // The name no longer belongs to us after replacement. Never remove another
    // writer's file if that temporary name is reused before this guard drops.
    temp.0 = None;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
#[cfg(not(windows))]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}
#[cfg(windows)]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if from[..from.len() - 1].contains(&0) || to[..to.len() - 1].contains(&0) {
        return Err(invalid("NUL in snapshot destination"));
    }
    // Same-directory rename only; do not allow cross-volume copy/delete fallback.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
