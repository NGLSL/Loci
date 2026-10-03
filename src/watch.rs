//! Conservative recovery: every signal invalidates the snapshot; bounded rescans
//! publish only with complete coverage and unchanged generation. No incremental WAL.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub entries: usize,
    pub directories: usize,
    pub depth: usize,
    pub queue: usize,
    pub retries: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 4096,
            directories: 128,
            depth: 16,
            queue: 256,
            retries: 4,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
}
#[derive(Default, Clone, Debug)]
pub struct Inventory {
    pub entries: BTreeMap<PathBuf, Kind>,
    pub directories: usize,
    pub examined: usize,
    pub complete: bool,
    pub errors: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Signal {
    Startup,
    Change,
    KernelOverflow,
    UserOverflow,
    WatchLost,
    UnknownWatch,
    ScanIncomplete,
    GenerationRace,
    RetryLimit,
    Periodic,
    Restart,
}
pub struct Recovery {
    pub generation: u64,
    pub dirty: bool,
    pub reasons: BTreeSet<Signal>,
    pub inventory: Inventory,
    pub errors: Vec<String>,
    queue: VecDeque<Signal>,
    capacity: usize,
}
impl Recovery {
    pub fn new(capacity: usize) -> Self {
        Self {
            generation: 1,
            dirty: true,
            reasons: BTreeSet::from([Signal::Startup]),
            inventory: Inventory::default(),
            errors: Vec::new(),
            queue: VecDeque::new(),
            capacity,
        }
    }
    pub fn restored(inventory: Inventory, capacity: usize) -> Self {
        let mut state = Self::new(capacity);
        state.inventory = inventory;
        state.reasons.insert(Signal::Restart);
        state
    }
    pub fn signal(&mut self, event: Signal) {
        self.generation = self.generation.wrapping_add(1);
        self.dirty = true;
        self.reasons.insert(event);
        if self.queue.len() >= self.capacity {
            self.queue.clear();
            self.reasons.insert(Signal::UserOverflow);
        } else {
            self.queue.push_back(event);
        }
    }
    pub fn pending(&self) -> usize {
        self.queue.len()
    }
    pub fn ticket(&self) -> u64 {
        self.generation
    }
    pub fn publish(&mut self, ticket: u64, candidate: Inventory) -> bool {
        if !candidate.complete {
            self.errors = candidate.errors;
            self.signal(Signal::ScanIncomplete);
            return false;
        }
        if ticket != self.generation {
            self.signal(Signal::GenerationRace);
            return false;
        }
        self.inventory = candidate;
        self.errors.clear();
        self.queue.clear();
        self.reasons.clear();
        self.dirty = false;
        true
    }
}
pub(crate) fn skip(path: &Path) -> bool {
    path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
        [
            "target",
            ".git",
            "work",
            "baseline-v1",
            "results",
            "results-v2",
            "results-v3",
            "baseline-stage2",
        ]
        .contains(&n)
    })
}
fn failure(out: &mut Inventory, text: String) {
    out.complete = false;
    // Bounded diagnostic strings too, including permission errors.
    if out.errors.len() < 32 {
        out.errors.push(text);
    }
}
pub fn scan(
    root: &Path,
    limits: Limits,
    mut before_directory: impl FnMut(&Path) -> io::Result<()>,
) -> Inventory {
    let mut out = Inventory {
        complete: true,
        ..Inventory::default()
    };
    let mut todo = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = todo.pop() {
        if out.directories >= limits.directories || depth > limits.depth {
            failure(&mut out, "directory/depth budget exhausted".into());
            break;
        }
        // Register before listing; new directory contents before registration are found by this scan.
        if let Err(e) = before_directory(&dir) {
            failure(&mut out, format!("watch registration: {e}"));
            continue;
        }
        out.directories += 1;
        let list = match fs::read_dir(&dir) {
            Ok(x) => x,
            Err(e) => {
                failure(&mut out, format!("read_dir: {e}"));
                continue;
            }
        };
        for item in list {
            if visited >= limits.entries {
                failure(&mut out, "entry budget exhausted".into());
                return out;
            }
            visited += 1;
            out.examined += 1;
            let item = match item {
                Ok(x) => x,
                Err(e) => {
                    failure(&mut out, format!("entry: {e}"));
                    continue;
                }
            };
            let path = item.path();
            let meta = match fs::symlink_metadata(&path) {
                Ok(x) => x,
                Err(e) => {
                    failure(&mut out, format!("metadata: {e}"));
                    continue;
                }
            };
            {
                use std::os::windows::fs::MetadataExt;
                if meta.file_attributes() & 0x400 != 0 {
                    continue;
                }
            }
            if meta.file_type().is_symlink() {
                continue;
            }
            let relative = match path.strip_prefix(root) {
                Ok(x) => x.to_path_buf(),
                Err(_) => {
                    failure(&mut out, "out of root".into());
                    continue;
                }
            };
            if meta.is_dir() {
                if skip(&path) {
                    continue;
                }
                out.entries.insert(relative, Kind::Directory);
                if todo.len() >= limits.directories {
                    failure(&mut out, "pending directory budget exhausted".into());
                    return out;
                }
                todo.push((path, depth + 1));
            } else if meta.is_file() {
                out.entries.insert(relative, Kind::File);
            }
        }
    }
    out
}

fn path_bytes(path: &Path) -> io::Result<Vec<u8>> {
    path.to_str()
        .map(|s| s.as_bytes().to_vec())
        .ok_or_else(|| io::Error::other("non-UTF-8 checkpoint path on this platform"))
}

fn decode_path(bytes: Vec<u8>) -> io::Result<PathBuf> {
    String::from_utf8(bytes)
        .map(PathBuf::from)
        .map_err(io::Error::other)
}
fn safe_relative(p: &Path) -> bool {
    !p.as_os_str().is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))
}
fn checksum(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf29ce484222325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}
fn put_path(data: &mut Vec<u8>, p: &Path) -> io::Result<()> {
    let bytes = path_bytes(p)?;
    if bytes.len() > 4096 {
        return Err(io::Error::other("checkpoint path budget"));
    }
    if data.len().saturating_add(4 + bytes.len()) > 8 * 1024 * 1024 {
        return Err(io::Error::other("checkpoint bytes budget"));
    }
    data.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    data.extend(bytes);
    Ok(())
}
pub fn save_checkpoint(path: &Path, root: &Path, inventory: &Inventory) -> io::Result<()> {
    if !inventory.complete || inventory.entries.len() > 4096 {
        return Err(io::Error::other("refuse incomplete/oversize checkpoint"));
    }
    let mut data = b"LOCWATCH1".to_vec();
    put_path(&mut data, root)?;
    data.extend_from_slice(&(inventory.entries.len() as u32).to_le_bytes());
    for (p, kind) in &inventory.entries {
        if !safe_relative(p) {
            return Err(io::Error::other("unsafe checkpoint path"));
        }
        data.push(if *kind == Kind::File { 0 } else { 1 });
        put_path(&mut data, p)?;
    }
    if data.len() > 8 * 1024 * 1024 {
        return Err(io::Error::other("checkpoint bytes budget"));
    }
    data.extend_from_slice(&checksum(&data).to_le_bytes());
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let temp = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(&data)?;
    file.sync_all()?;
    drop(file);
    // Windows may refuse replacement; preserve the old file.
    if let Err(e) = fs::rename(&temp, path) {
        let _ = fs::remove_file(temp);
        return Err(e);
    }

    Ok(())
}
fn u32_at(data: &[u8], pos: &mut usize) -> io::Result<usize> {
    let end = pos
        .checked_add(4)
        .ok_or_else(|| io::Error::other("overflow"))?;
    let bytes = data
        .get(*pos..end)
        .ok_or_else(|| io::Error::other("truncated"))?;
    *pos = end;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()) as usize)
}
fn get_path(data: &[u8], pos: &mut usize) -> io::Result<PathBuf> {
    let len = u32_at(data, pos)?;
    if len > 4096 {
        return Err(io::Error::other("path budget"));
    }
    let end = pos
        .checked_add(len)
        .ok_or_else(|| io::Error::other("overflow"))?;
    let bytes = data
        .get(*pos..end)
        .ok_or_else(|| io::Error::other("truncated path"))?;
    *pos = end;
    decode_path(bytes.to_vec())
}
pub fn load_checkpoint(path: &Path, root: &Path, limits: Limits) -> io::Result<Inventory> {
    let mut data = vec![];
    fs::File::open(path)?
        .take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut data)?;
    if data.len() > 8 * 1024 * 1024 || data.len() < 21 || !data.starts_with(b"LOCWATCH1") {
        return Err(io::Error::other("invalid checkpoint"));
    }
    let body = &data[..data.len() - 8];
    let stored = u64::from_le_bytes(data[data.len() - 8..].try_into().unwrap());
    if checksum(body) != stored {
        return Err(io::Error::other("checkpoint checksum mismatch"));
    }
    let mut pos = 9;
    if get_path(body, &mut pos)? != root {
        return Err(io::Error::other("checkpoint root mismatch"));
    }
    let count = u32_at(body, &mut pos)?;
    if count > limits.entries || count > 4096 {
        return Err(io::Error::other("entry budget"));
    }
    let mut out = Inventory {
        complete: true,
        ..Inventory::default()
    };
    for _ in 0..count {
        let kind = match body.get(pos) {
            Some(0) => Kind::File,
            Some(1) => Kind::Directory,
            _ => return Err(io::Error::other("invalid kind")),
        };
        pos += 1;
        let path = get_path(body, &mut pos)?;
        if !safe_relative(&path) || out.entries.insert(path, kind).is_some() {
            return Err(io::Error::other("unsafe/duplicate checkpoint path"));
        }
    }
    if pos != body.len() {
        return Err(io::Error::other("checkpoint trailing bytes"));
    }
    Ok(out)
}
