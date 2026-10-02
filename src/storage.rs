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
        atomic_save(path, &self.encode()?)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn save_in_directory(
        &self,
        parent: &fs::File,
        name: &std::ffi::OsStr,
    ) -> io::Result<()> {
        linux_atomic_save(parent, name, &self.encode()?)
    }
    fn encode(&self) -> io::Result<Vec<u8>> {
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
        Ok(bytes)
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

#[cfg(not(target_os = "linux"))]
struct Temporary(Option<PathBuf>);
#[cfg(not(target_os = "linux"))]
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
#[cfg(not(target_os = "linux"))]
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
#[cfg(all(not(windows), not(target_os = "linux")))]
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(target_os = "linux")]
fn atomic_save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    validate_destination(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| invalid("snapshot destination requires filename"))?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(0x10000 | 0x20000) // O_DIRECTORY | O_NOFOLLOW
        .open(parent)?;
    linux_atomic_save(&directory, name, bytes)
}

#[cfg(target_os = "linux")]
fn linux_atomic_save(parent: &fs::File, name: &std::ffi::OsStr, bytes: &[u8]) -> io::Result<()> {
    use std::ffi::{c_char, c_int, c_uint, CString};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    #[link(name = "c")]
    unsafe extern "C" {
        fn openat(directory: c_int, name: *const c_char, flags: c_int, ...) -> c_int;
        fn renameat(
            from_directory: c_int,
            from: *const c_char,
            to_directory: c_int,
            to: *const c_char,
        ) -> c_int;
        fn unlinkat(directory: c_int, name: *const c_char, flags: c_int) -> c_int;
    }
    struct TemporaryAt<'a> {
        parent: &'a fs::File,
        name: Option<CString>,
    }
    impl Drop for TemporaryAt<'_> {
        fn drop(&mut self) {
            if let Some(name) = &self.name {
                // Same held directory as creation; never follow a replaced path.
                unsafe { unlinkat(self.parent.as_raw_fd(), name.as_ptr(), 0) };
            }
        }
    }
    let path = Path::new(name);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(invalid("snapshot destination requires a single filename"));
    }
    let destination = CString::new(name.as_bytes()).map_err(|_| invalid("NUL in filename"))?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut created = None;
    for _ in 0..16 {
        let mut temporary = name.to_os_string();
        temporary.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let temporary =
            CString::new(temporary.as_bytes()).map_err(|_| invalid("NUL in temporary filename"))?;
        // x86_64 Linux O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC. create_new
        // semantics ensure a foreign temporary is never followed or removed.
        let fd = unsafe {
            openat(
                parent.as_raw_fd(),
                temporary.as_ptr(),
                0x1 | 0x40 | 0x80 | 0x80000,
                0o600 as c_uint,
            )
        };
        if fd >= 0 {
            // We own the new descriptor; File closes it on every error path.
            let file = unsafe { fs::File::from_raw_fd(fd) };
            created = Some((
                TemporaryAt {
                    parent,
                    name: Some(temporary),
                },
                file,
            ));
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
    }
    let (mut temporary, mut file) = created.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "snapshot temporary filename collision budget",
        )
    })?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    let from = temporary
        .name
        .as_ref()
        .ok_or_else(|| invalid("missing snapshot temporary"))?;
    if unsafe {
        renameat(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            destination.as_ptr(),
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    temporary.name = None;
    parent.sync_all()
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

#[cfg(all(test, target_os = "linux"))]
mod linux_save_tests {
    use super::*;
    use std::ffi::OsStr;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
            let base = parent.join(format!(
                "loci-bound-save-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&base).unwrap();
            Self(base)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let base = fs::canonicalize(&self.0).unwrap();
            assert_eq!(
                base.parent(),
                Some(fs::canonicalize(std::env::temp_dir()).unwrap().as_path())
            );
            assert!(base
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("loci-bound-save-"));
            fs::remove_dir_all(base).unwrap();
        }
    }

    // Storage-level evidence for the directory-relative writer used by Engine.
    // Redirect after selecting the handle, before creation/replacement/cleanup.
    // It does not claim to reproduce every concurrent filesystem relocation.
    #[test]
    fn selected_directory_binds_save_and_failure_cleanup_after_path_redirection() {
        for fail_replace in [false, true] {
            let f = Fixture::new();
            let root = f.0.join("data");
            let inside = root.join("inside");
            let outside = f.0.join("outside");
            let original = f.0.join("original");
            fs::create_dir_all(&inside).unwrap();
            fs::create_dir(&outside).unwrap();
            let inventory = Inventory {
                entries: [(PathBuf::from("new.txt"), Kind::File)].into(),
                complete: true,
                ..Inventory::default()
            };
            let snapshot = Snapshot::new(&root, inventory).unwrap();
            if fail_replace {
                fs::create_dir(outside.join("state.loci")).unwrap();
            } else {
                fs::write(outside.join("state.loci"), b"old bytes").unwrap();
            }
            let parent = fs::File::open(&outside).unwrap();
            fs::rename(&outside, &original).unwrap();
            std::os::unix::fs::symlink(&inside, &outside).unwrap();
            let result = snapshot.save_in_directory(&parent, OsStr::new("state.loci"));
            assert_eq!(fs::read_dir(&inside).unwrap().count(), 0);
            assert_eq!(fs::read_dir(&original).unwrap().count(), 1);
            if fail_replace {
                assert!(result.is_err());
                assert!(original.join("state.loci").is_dir());
            } else {
                result.unwrap();
                assert_eq!(
                    Snapshot::load(&original.join("state.loci"), &root)
                        .unwrap()
                        .inventory
                        .entries,
                    snapshot.inventory.entries
                );
            }
        }
    }
}
