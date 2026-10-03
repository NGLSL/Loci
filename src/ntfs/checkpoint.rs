//! Streaming checkpoint; inventory, source identity and USN cursor commit together.
use super::{graph::Graph, invalid, Limits};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: &[u8; 8] = b"LOCNTFS2";
const FNV: u64 = 0xcbf29ce484222325;
fn hash(h: &mut u64, data: &[u8]) {
    for b in data {
        *h = (*h ^ u64::from(*b)).wrapping_mul(0x100000001b3);
    }
}

pub(super) struct Saved {
    pub graph: Graph,
    pub guid: String,
    pub serial: u64,
    pub journal: u64,
    pub cursor: i64,
    pub version: u64,
    pub storage_root: u64,
    pub storage_parents: std::collections::HashMap<u64, u64>,
}
struct Output<W: Write> {
    file: W,
    checksum: u64,
}
impl<W: Write> Output<W> {
    fn put(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.write_all(data)?;
        hash(&mut self.checksum, data);
        Ok(())
    }
    fn u64(&mut self, n: u64) -> io::Result<()> {
        self.put(&n.to_le_bytes())
    }
}
struct Input<R: Read> {
    file: R,
    checksum: u64,
}
impl<R: Read> Input<R> {
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut b = [0; N];
        self.file.read_exact(&mut b)?;
        hash(&mut self.checksum, &b);
        Ok(b)
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
}
struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
pub(super) fn save(path: &Path, saved: &Saved) -> io::Result<()> {
    save_cancel(path, saved, &std::sync::atomic::AtomicBool::new(false))
}
pub(super) fn save_cancel(
    path: &Path,
    saved: &Saved,
    cancel: &std::sync::atomic::AtomicBool,
) -> io::Result<()> {
    super::cancelled(cancel)?;
    saved.graph.validate()?;
    if saved.guid.len() > 128 || saved.cursor < 0 {
        return Err(invalid("invalid NTFS checkpoint identity"));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| invalid("checkpoint requires filename"))?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut tempname = name.to_os_string();
    tempname.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = Temporary(parent.join(tempname));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp.0)?;
    let mut output = Output {
        file: BufWriter::with_capacity(64 * 1024, file),
        checksum: FNV,
    };
    output.put(MAGIC)?;
    output.u64(saved.serial)?;
    output.u64(saved.graph.root)?;
    output.u64(saved.journal)?;
    output.u64(saved.cursor as u64)?;
    output.u64(saved.version)?;
    output.u64(saved.storage_root)?;
    output.u64(saved.storage_parents.len() as u64)?;
    for (object, parent) in &saved.storage_parents {
        output.u64(*object)?;
        output.u64(*parent)?;
    }
    output.u64(saved.graph.live as u64)?;
    output.put(&(saved.guid.len() as u16).to_le_bytes())?;
    output.put(saved.guid.as_bytes())?;
    for (number, (_, node)) in saved.graph.nodes().filter(|(_, n)| n.alive).enumerate() {
        if number % 1024 == 0 {
            super::cancelled(cancel)?;
        }
        output.u64(node.object)?;
        output.u64(node.parent)?;
        output.put(&node.attributes.to_le_bytes())?;
        let name = saved.graph.name(node);
        output.put(&(name.len() as u16).to_le_bytes())?;
        for unit in name {
            output.put(&unit.to_le_bytes())?;
        }
    }
    output.file.write_all(&output.checksum.to_le_bytes())?;
    output.file.flush()?;
    output.file.get_ref().sync_all()?;
    drop(output);
    super::cancelled(cancel)?;
    replace(&temp.0, path)
}
#[cfg(test)]
pub(super) fn load(path: &Path, limits: Limits) -> io::Result<Saved> {
    load_cancel(path, limits, &std::sync::atomic::AtomicBool::new(false))
}
pub(super) fn load_cancel(
    path: &Path,
    limits: Limits,
    cancel: &std::sync::atomic::AtomicBool,
) -> io::Result<Saved> {
    super::cancelled(cancel)?;
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limits.memory_bytes.saturating_mul(2) as u64 {
        return Err(invalid("NTFS checkpoint exceeds configured memory profile"));
    }
    let mut input = Input {
        file: BufReader::with_capacity(64 * 1024, file),
        checksum: FNV,
    };
    if &input.take::<8>()? != MAGIC {
        return Err(invalid("unsupported NTFS checkpoint format"));
    }
    let serial = input.u64()?;
    let root = input.u64()?;
    let journal = input.u64()?;
    let cursor = input.u64()? as i64;
    let version = input.u64()?;
    let storage_root = input.u64()?;
    let storage_count = input.u64()?;
    if storage_count > limits.memory_bytes as u64 / 32 {
        return Err(invalid("checkpoint exclusion map exceeds memory profile"));
    }
    let mut storage_parents = std::collections::HashMap::new();
    for _ in 0..storage_count {
        let object = input.u64()?;
        let parent = input.u64()?;
        if object == 0 || parent == 0 || storage_parents.insert(object, parent).is_some() {
            return Err(invalid("invalid checkpoint exclusion map"));
        }
    }

    let count = input.u64()?;
    let length = u16::from_le_bytes(input.take()?) as usize;
    if length == 0
        || length > 128
        || root == 0
        || cursor < 0
        || version == 0
        || count > limits.memory_bytes as u64 / 22
    {
        return Err(invalid("invalid NTFS checkpoint header"));
    }
    let mut guid = Vec::with_capacity(length);
    for _ in 0..length {
        guid.push(input.take::<1>()?[0]);
    }
    let guid = String::from_utf8(guid).map_err(|_| invalid("invalid NTFS volume GUID"))?;
    let mut graph = Graph::new(root, limits);
    for number in 0..count {
        if number % 1024 == 0 {
            super::cancelled(cancel)?;
        }
        let object = input.u64()?;
        let parent = input.u64()?;
        let attributes = u32::from_le_bytes(input.take()?);
        let length = u16::from_le_bytes(input.take()?) as usize;
        if !(1..=255).contains(&length) {
            return Err(invalid("invalid NTFS checkpoint name length"));
        }
        let mut name = Vec::with_capacity(length);
        for _ in 0..length {
            name.push(u16::from_le_bytes(input.take()?));
        }
        graph.add(object, parent, &name, attributes)?;
    }
    let mut checksum = [0; 8];
    input.file.read_exact(&mut checksum)?;
    if u64::from_le_bytes(checksum) != input.checksum {
        return Err(invalid("NTFS checkpoint checksum mismatch"));
    }
    let mut trailing = [0; 1];
    if input.file.read(&mut trailing)? != 0 {
        return Err(invalid("trailing NTFS checkpoint data"));
    }
    graph.validate()?;
    Ok(Saved {
        graph,
        guid,
        serial,
        journal,
        cursor,
        version,
        storage_root,
        storage_parents,
    })
}
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0x1 | 0x8) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
