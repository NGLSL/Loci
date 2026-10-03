//! NTFS volume inventory. MFT discovery is supplemented with every directory's
//! native name relationships, including hard links. NTFS internal metadata
//! (reserved MFT slots below 24, except the root) is outside the search scope.
//! A failed enumeration never publishes or persists a complete inventory.
mod checkpoint;
mod graph;
mod native;
mod usn;

use graph::{Graph, DIRECTORY, REPARSE};
use native::Volume;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use usn::{Journal, Record};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Per-inventory capacity, including names and directory identity lookup.
    /// Copy-on-write readers may retain an older generation separately.
    pub memory_bytes: usize,
    pub replay_bytes: usize,
    pub max_depth: usize,
    pub catch_up_passes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: 1024 * 1024 * 1024,
            replay_bytes: 64 * 1024 * 1024,
            max_depth: 1024,
            catch_up_passes: 8,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct IndexStatus {
    pub state: String,
    pub records: usize,
    pub version: u64,
    pub journal_id: u64,
    pub cursor: i64,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct SearchItem {
    /// Valid UTF-16 paths only; entries containing unpaired surrogates are skipped.
    pub path: String,
    pub name: String,
    pub is_directory: bool,
    /// Exact native names remain available alongside their valid Unicode form.
    pub path_utf16: Vec<u16>,
    pub name_utf16: Vec<u16>,
}
#[derive(Clone, Debug, Serialize)]
pub struct SearchPage {
    pub items: Vec<SearchItem>,
    pub total: usize,
    pub version: u64,
    pub status: IndexStatus,
    /// False means total is the observed lower bound, not an exact count.
    pub total_exact: bool,
    pub skipped_invalid_names: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct RefreshReport {
    pub changed: usize,
    pub rebuild: bool,
    pub version: u64,
}

/// Cloning pins immutable inventory pages. Refresh on a clone does not block
/// or mutate queries on the previous copy; the service can publish it by swap.
#[derive(Clone)]
pub struct NtfsIndex {
    volume: String,
    checkpoint: PathBuf,
    guid: String,
    serial: u64,
    graph: Arc<Graph>,
    status: IndexStatus,
    limits: Limits,
    storage_root: u64,
    storage_parents: Arc<HashMap<u64, u64>>,
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn cancelled(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "NTFS operation cancelled; previous inventory preserved",
        ))
    } else {
        Ok(())
    }
}
fn internal(object: u64, root: u64) -> bool {
    object != root && object & 0x0000_ffff_ffff_ffff < 24
}
fn retained(journal: &Journal, id: u64, cursor: i64) -> bool {
    journal.id == id && cursor >= journal.first.max(journal.lowest) && cursor <= journal.next
}
#[derive(Debug)]
struct ReplayBudget;
impl std::fmt::Display for ReplayBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("USN replay exceeds configured temporary memory budget; full rebuild required")
    }
}
impl std::error::Error for ReplayBudget {}
fn collect_replay(
    mut cursor: i64,
    cutoff: i64,
    budget: usize,
    cancel: &AtomicBool,
    mut read: impl FnMut(i64) -> io::Result<(i64, Vec<Record>)>,
) -> io::Result<Vec<Record>> {
    let mut records = Vec::new();
    let mut bytes = 0usize;
    while cursor < cutoff {
        cancelled(cancel)?;
        let (next, batch) = read(cursor)?;
        if next <= cursor {
            return Err(invalid("USN replay failed to advance"));
        }
        for record in batch {
            if record.usn < cursor || record.usn >= next {
                return Err(invalid("invalid USN replay record range"));
            }
            if record.usn >= cutoff {
                continue;
            }
            bytes = bytes.saturating_add(std::mem::size_of::<Record>() + record.name.len() * 2);
            if bytes > budget {
                return Err(io::Error::new(io::ErrorKind::OutOfMemory, ReplayBudget));
            }
            records.push(record);
        }
        cursor = next.min(cutoff);
    }
    Ok(records)
}
fn recover_budget<T>(
    result: io::Result<T>,
    rebuild: impl FnOnce() -> io::Result<T>,
) -> io::Result<(T, bool)> {
    match result {
        Ok(value) => Ok((value, false)),
        Err(error)
            if error
                .get_ref()
                .is_some_and(|inner| inner.is::<ReplayBudget>()) =>
        {
            rebuild().map(|value| (value, true))
        }
        Err(error) => Err(error),
    }
}
impl NtfsIndex {
    pub fn open(volume: &str, checkpoint: &Path) -> io::Result<Self> {
        Self::open_cancel(volume, checkpoint, &AtomicBool::new(false))
    }
    pub fn open_cancel(volume: &str, checkpoint: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        Self::open_with_limits(volume, checkpoint, Limits::default(), cancel)
    }
    pub fn open_with_limits(
        volume: &str,
        checkpoint: &Path,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        if limits.memory_bytes < 1024 * 1024
            || limits.replay_bytes < 65536
            || limits.max_depth == 0
            || limits.catch_up_passes == 0
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid NTFS resource limits",
            ));
        }
        cancelled(cancel)?;
        let native = Volume::open(volume)?;
        let journal = native.query()?;
        let parent = std::fs::canonicalize(
            checkpoint
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        let (storage_guid, storage_frn) = native::directory_source(&parent)?;
        let storage_root = if storage_guid == native.guid {
            storage_frn
        } else {
            0
        };
        if storage_root == native.root {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NTFS checkpoint must be inside a dedicated storage directory, not the volume root",
            ));
        }
        let loaded = match checkpoint::load_cancel(checkpoint, limits, cancel) {
            Ok(saved)
                if saved.guid == native.guid
                    && saved.serial == native.serial
                    && saved.graph.root == native.root
                    && saved.storage_root == storage_root
                    && retained(&journal, saved.journal, saved.cursor) =>
            {
                Some(saved)
            }
            Ok(_) => None,
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.kind() == io::ErrorKind::InvalidData
                    || error.kind() == io::ErrorKind::UnexpectedEof =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let mut index = if let Some(saved) = loaded {
            let status = IndexStatus {
                state: "pending".into(),
                records: saved.graph.live,
                version: saved.version,
                journal_id: saved.journal,
                cursor: saved.cursor,
                error: None,
            };
            Self {
                volume: format!("{}:", native.letter),
                checkpoint: checkpoint.to_path_buf(),
                guid: native.guid.clone(),
                serial: native.serial,
                graph: Arc::new(saved.graph),
                status,
                limits,
                storage_root,
                storage_parents: Arc::new(saved.storage_parents),
            }
        } else {
            let (graph, storage_parents) = bootstrap(&native, limits, storage_root, cancel)?;
            let status = IndexStatus {
                state: "pending".into(),
                records: graph.live,
                version: 1,
                journal_id: journal.id,
                cursor: journal.next,
                error: None,
            };
            Self {
                volume: format!("{}:", native.letter),
                checkpoint: checkpoint.to_path_buf(),
                guid: native.guid.clone(),
                serial: native.serial,
                graph: Arc::new(graph),
                status,
                limits,
                storage_root,
                storage_parents: Arc::new(storage_parents),
            }
        };
        let first = index.catch_up(&native, cancel);
        recover_budget(first, || {
            index.rebuild_from(&native, cancel)?;
            index.catch_up(&native, cancel)
        })?;
        index.persist(cancel)?;
        Ok(index)
    }
    pub fn status(&self) -> IndexStatus {
        self.status.clone()
    }
    pub fn refresh(&mut self) -> io::Result<RefreshReport> {
        self.refresh_cancel(&AtomicBool::new(false))
    }
    pub fn refresh_cancel(&mut self, cancel: &AtomicBool) -> io::Result<RefreshReport> {
        let result = self.refresh_inner(cancel);
        if let Err(error) = &result {
            self.status.state = "pending".into();
            self.status.error = Some(error.to_string());
        }
        result
    }
    fn refresh_inner(&mut self, cancel: &AtomicBool) -> io::Result<RefreshReport> {
        cancelled(cancel)?;
        let native = Volume::open(&self.volume)?;
        let journal = native.query()?;
        let mut rebuild = native.guid != self.guid
            || native.serial != self.serial
            || native.root != self.graph.root
            || !retained(&journal, self.status.journal_id, self.status.cursor);
        let mut candidate = self.clone();
        if rebuild {
            let (graph, storage_parents) =
                bootstrap(&native, self.limits, self.storage_root, cancel)?;
            candidate.graph = Arc::new(graph);
            candidate.storage_parents = Arc::new(storage_parents);
            candidate.guid = native.guid.clone();
            candidate.serial = native.serial;
            candidate.status.journal_id = journal.id;
            candidate.status.cursor = journal.next;
            candidate.status.version = self
                .status
                .version
                .checked_add(1)
                .ok_or_else(|| invalid("NTFS version overflow"))?;
        }
        let first = candidate.catch_up(&native, cancel);
        let (changed, recovered) = recover_budget(first, || {
            candidate.rebuild_from(&native, cancel)?;
            candidate.catch_up(&native, cancel)
        })?;
        rebuild |= recovered;
        cancelled(cancel)?;
        if rebuild || changed > 0 || self.status.state != "ready" {
            candidate.persist(cancel)?;
        }
        let version = candidate.status.version;
        *self = candidate;
        Ok(RefreshReport {
            changed,
            rebuild,
            version,
        })
    }
    fn rebuild_from(&mut self, native: &Volume, cancel: &AtomicBool) -> io::Result<()> {
        cancelled(cancel)?;
        let beginning = native.query()?;
        let (graph, parents) = bootstrap(native, self.limits, self.storage_root, cancel)?;
        self.graph = Arc::new(graph);
        self.storage_parents = Arc::new(parents);
        self.guid = native.guid.clone();
        self.serial = native.serial;
        self.status.journal_id = beginning.id;
        self.status.cursor = beginning.next;
        self.status.version = self
            .status
            .version
            .checked_add(1)
            .ok_or_else(|| invalid("NTFS version overflow"))?;
        self.status.state = "pending".into();
        Ok(())
    }
    fn catch_up(&mut self, native: &Volume, cancel: &AtomicBool) -> io::Result<usize> {
        let mut total = 0;
        for _ in 0..self.limits.catch_up_passes {
            cancelled(cancel)?;
            let journal = native.query()?;
            if !retained(&journal, self.status.journal_id, self.status.cursor) {
                return Err(invalid(
                    "USN journal gap or identity changed during catch-up; rebuild required",
                ));
            }
            if self.status.cursor == journal.next {
                self.status.state = "ready".into();
                self.status.error = None;
                self.status.records = self.graph.live;
                return Ok(total);
            }
            let records = collect_replay(
                self.status.cursor,
                journal.next,
                self.limits.replay_bytes,
                cancel,
                |cursor| native.read(journal.id, cursor),
            )?;
            let records = scope_records(
                &self.graph,
                Arc::make_mut(&mut self.storage_parents),
                self.storage_root,
                records,
            );
            let (next, changed) =
                apply_records(native, &self.graph, &records, self.storage_root, cancel)?;
            cancelled(cancel)?;
            self.graph = Arc::new(next);
            self.status.cursor = journal.next;
            if changed > 0 {
                self.status.version = self
                    .status
                    .version
                    .checked_add(1)
                    .ok_or_else(|| invalid("NTFS version overflow"))?;
            }
            total += changed;
            // A finite observed journal boundary is the checkpoint position.
            // Authoritative directory reads may observe newer changes; those
            // remain replayable beyond this cursor. Do not wait for an entire
            // busy volume to become quiet before publishing valid relationships.
            let after = native.query()?;
            if !retained(&after, self.status.journal_id, self.status.cursor) {
                return Err(invalid(
                    "USN retention changed while reconciling; rebuild required",
                ));
            }
            self.status.state = "ready".into();
            self.status.error = None;
            self.status.records = self.graph.live;
            return Ok(total);
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "NTFS volume is still changing; previous complete publication preserved",
        ))
    }
    fn persist(&self, cancel: &AtomicBool) -> io::Result<()> {
        if self.status.state != "ready" {
            return Err(invalid("refuse incomplete NTFS checkpoint"));
        }
        checkpoint::save_cancel(
            &self.checkpoint,
            &checkpoint::Saved {
                graph: (*self.graph).clone(),
                guid: self.guid.clone(),
                serial: self.serial,
                journal: self.status.journal_id,
                cursor: self.status.cursor,
                version: self.status.version,
                storage_root: self.storage_root,
                storage_parents: (*self.storage_parents).clone(),
            },
            cancel,
        )
    }
    pub fn search(&self, query: &str, limit: usize, filter: &str) -> io::Result<SearchPage> {
        if query.len() > 4096 || limit > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "query or result page budget exceeded",
            ));
        }
        let (filter, extensions): (_, &[&str]) = match filter {
            "" | "all" => (0, &[]),
            "files" | "file" => (1, &[]),
            "folders" | "directories" | "folder" => (2, &[]),
            "images" => (
                1,
                &[
                    "jpg", "jpeg", "png", "gif", "webp", "bmp", "ico", "svg", "heic", "heif",
                    "avif", "tif", "tiff",
                ],
            ),
            "documents" => (
                1,
                &[
                    "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "rtf", "odt",
                    "ods", "odp", "csv",
                ],
            ),
            "videos" => (
                1,
                &[
                    "mp4", "mkv", "mov", "avi", "wmv", "webm", "flv", "m4v", "mpeg", "mpg",
                ],
            ),
            "audio" => (
                1,
                &["mp3", "wav", "flac", "aac", "m4a", "ogg", "wma", "opus"],
            ),
            "archives" => (1, &["zip", "7z", "rar", "tar", "gz", "bz2", "xz", "iso"]),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unknown file category",
                ))
            }
        };
        let query = PreparedQuery::new(query);
        let mut items = Vec::new();
        let mut total = 0;
        let mut total_exact = true;
        let mut skipped = 0;
        let mut prefixes: HashMap<u64, (Vec<u16>, Vec<u8>)> = HashMap::new();
        for (_, node) in self.graph.nodes().filter(|(_, n)| n.alive) {
            let is_directory = node.attributes & DIRECTORY != 0;
            if (filter == 1 && is_directory) || (filter == 2 && !is_directory) {
                continue;
            }
            let folded = self.graph.folded_name(node);
            if !extensions.is_empty()
                && !folded.iter().rposition(|b| *b == b'.').is_some_and(|dot| {
                    extensions
                        .iter()
                        .any(|ext| folded[dot + 1..] == *ext.as_bytes())
                })
            {
                continue;
            }
            if !query.extension_matches(folded) {
                continue;
            }
            if query.matches_name(folded) {
                // No parent path allocation is needed for this candidate.
            } else {
                if !prefixes.contains_key(&node.parent) {
                    let p = self.graph.directory_path(node.parent)?;
                    let mut f = self.volume.to_lowercase().into_bytes();
                    f.push(b'/');
                    f.extend(matching_bytes(&p));
                    prefixes.insert(node.parent, (p, f));
                }
                if !query.matches_parts(folded, &prefixes[&node.parent].1) {
                    continue;
                }
            }
            total += 1;
            if items.len() >= limit {
                total_exact = false;
                break;
            }
            let prefix = if let Some(p) = prefixes.get(&node.parent) {
                p.0.clone()
            } else {
                self.graph.directory_path(node.parent)?
            };
            let name = self.graph.name(node);
            let mut raw = Vec::with_capacity(prefix.len() + name.len() + 4);
            raw.extend(self.volume.encode_utf16());
            raw.push(92);
            if !prefix.is_empty() {
                raw.extend(prefix);
                raw.push(92);
            }
            raw.extend_from_slice(name);
            let Ok(path) = String::from_utf16(&raw) else {
                skipped += 1;
                continue;
            };
            let name_text = String::from_utf16(name)
                .map_err(|_| invalid("unexpected invalid name after valid path"))?;
            items.push(SearchItem {
                path,
                name: name_text,
                is_directory,
                path_utf16: raw,
                name_utf16: name.to_vec(),
            });
        }
        Ok(SearchPage {
            items,
            total,
            version: self.status.version,
            status: self.status(),
            total_exact,
            skipped_invalid_names: skipped,
        })
    }
}
/// Invalid UTF-16 units delimit valid runs, so queries cannot match a lossy
/// replacement glyph or bridge distinct native names.
fn matching_bytes(raw: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut run = String::new();
    for scalar in char::decode_utf16(raw.iter().copied()) {
        match scalar {
            Ok('\\') => run.push('/'),
            Ok(c) => run.push(c),
            Err(_) => {
                bytes.extend(run.to_lowercase().as_bytes());
                run.clear();
                bytes.push(255);
            }
        }
    }
    bytes.extend(run.to_lowercase().as_bytes());
    bytes
}
struct PreparedQuery {
    tokens: Vec<Vec<u8>>,
    extension: Option<Vec<u8>>,
}
impl PreparedQuery {
    fn new(raw: &str) -> Self {
        let mut q = Self {
            tokens: Vec::new(),
            extension: None,
        };
        for t in raw.split_whitespace() {
            if let Some(ext) = t.strip_prefix("ext:") {
                q.extension = Some(ext.to_lowercase().into_bytes());
            } else {
                q.tokens
                    .push(t.replace('\\', "/").to_lowercase().into_bytes());
            }
        }
        q
    }
    fn extension_matches(&self, name: &[u8]) -> bool {
        self.extension.as_ref().is_none_or(|ext| {
            name.iter()
                .rposition(|b| *b == b'.')
                .is_some_and(|dot| &name[dot + 1..] == ext)
        })
    }
    fn matches_name(&self, name: &[u8]) -> bool {
        self.tokens.iter().all(|t| contains(name, t))
    }
    fn matches_parts(&self, name: &[u8], parent: &[u8]) -> bool {
        self.tokens.iter().all(|t| {
            if contains(name, t) || contains(parent, t) {
                return true;
            }
            if !t.contains(&b'/') {
                return false;
            }
            let mut path = Vec::with_capacity(parent.len() + name.len() + 1);
            path.extend_from_slice(parent);
            path.push(b'/');
            path.extend_from_slice(name);
            contains(&path, t)
        })
    }
}
fn contains(hay: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || (hay.len() >= needle.len() && hay.windows(needle.len()).any(|w| w == needle))
}
fn absolute(volume: &Volume, relative: &[u16]) -> Vec<u16> {
    let mut p: Vec<_> = format!("\\\\?\\{}:\\", volume.letter)
        .encode_utf16()
        .collect();
    p.extend_from_slice(relative);
    p
}
fn bootstrap(
    native: &Volume,
    limits: Limits,
    storage_root: u64,
    cancel: &AtomicBool,
) -> io::Result<(Graph, HashMap<u64, u64>)> {
    let mut seed = Graph::new(native.root, limits);
    let mut cursor = 0u64;
    loop {
        cancelled(cancel)?;
        match native.enumerate(cursor, i64::MAX) {
            Ok((next, records)) => {
                for record in records {
                    if internal(record.object, native.root)
                        || record.object == native.root
                        || internal(record.parent, native.root)
                    {
                        continue;
                    }
                    seed.add(
                        record.object,
                        record.parent,
                        &record.name,
                        record.attributes,
                    )?;
                }
                cursor = next;
            }
            Err(error) if error.raw_os_error() == Some(38) => break,
            Err(error) => return Err(error),
        }
    }
    let storage_parents: HashMap<_, _> = seed
        .nodes()
        .filter(|(_, n)| {
            n.alive
                && n.attributes & (DIRECTORY | REPARSE) == DIRECTORY
                && excluded(&seed, &HashMap::new(), storage_root, n.object)
        })
        .map(|(_, n)| (n.object, n.parent))
        .collect();
    // MFT provides one representative name. Native directory batches establish
    // every actual relationship, including all aliases, without child opens.
    let mut result = Graph::new(native.root, limits);
    let mut todo = vec![native.root];
    let mut seen = HashSet::new();
    while let Some(parent) = todo.pop() {
        cancelled(cancel)?;
        if !seen.insert(parent) {
            return Err(invalid("duplicate or cyclic NTFS directory discovery"));
        }
        let relative = if parent == native.root {
            Vec::new()
        } else {
            result
                .directory_path(parent)
                .or_else(|_| seed.directory_path(parent))?
        };
        for record in native.list_directory(&absolute(native, &relative), parent, cancel)? {
            if internal(record.object, native.root) || record.object == storage_root {
                continue;
            }
            result.add(record.object, parent, &record.name, record.attributes)?;
            if record.attributes & (DIRECTORY | REPARSE) == DIRECTORY {
                todo.push(record.object);
            }
        }
    }
    result.validate()?;
    Ok((result, storage_parents))
}
fn excluded(graph: &Graph, parents: &HashMap<u64, u64>, root: u64, mut object: u64) -> bool {
    if root == 0 {
        return false;
    }
    for _ in 0..graph.limits.max_depth {
        if object == root {
            return true;
        }
        if object == graph.root {
            return false;
        }
        object = if let Some(p) = parents.get(&object) {
            *p
        } else if let Some(slot) = graph.directory(object) {
            graph.get(slot).parent
        } else {
            return false;
        };
    }
    false
}
fn scope_records(
    graph: &Graph,
    parents: &mut HashMap<u64, u64>,
    root: u64,
    records: Vec<Record>,
) -> Vec<Record> {
    if root == 0 {
        return records;
    }
    let mut result = Vec::new();
    for record in records {
        let was_excluded = excluded(graph, parents, root, record.object);
        let parent_excluded = excluded(graph, parents, root, record.parent);
        if record.attributes & (DIRECTORY | REPARSE) == DIRECTORY
            && (was_excluded || parent_excluded)
        {
            parents.insert(record.object, record.parent);
        }
        if record.object == root || parent_excluded {
            continue;
        }
        result.push(record);
    }
    result
}
trait NamespaceSource {
    fn root(&self) -> u64;
    fn list(&self, relative: &[u16], parent: u64, cancel: &AtomicBool) -> io::Result<Vec<Record>>;
}
impl NamespaceSource for Volume {
    fn root(&self) -> u64 {
        self.root
    }
    fn list(&self, relative: &[u16], parent: u64, cancel: &AtomicBool) -> io::Result<Vec<Record>> {
        self.list_directory(&absolute(self, relative), parent, cancel)
    }
}
fn apply_records(
    native: &impl NamespaceSource,
    old: &Graph,
    records: &[Record],
    storage_root: u64,
    cancel: &AtomicBool,
) -> io::Result<(Graph, usize)> {
    cancelled(cancel)?;
    let mut graph = old.clone();
    let mut dirty = HashSet::new();
    let mut affected = HashSet::new();
    for record in records {
        if internal(record.object, native.root()) || internal(record.parent, native.root()) {
            continue;
        }
        affected.insert(record.object);
        dirty.insert(record.parent);
    }
    if dirty.is_empty() {
        return Ok((graph, 0));
    }
    // All existing aliases must be corrected on deletion or hard-link changes.
    for (_, node) in old
        .nodes()
        .filter(|(_, n)| n.alive && affected.contains(&n.object))
    {
        dirty.insert(node.parent);
    }
    // New directory names locate the final directory listing after a rename.
    let mut directories: HashMap<u64, &Record> = HashMap::new();
    for record in records {
        if record.attributes & (DIRECTORY | REPARSE) == DIRECTORY
            && record.reason & (0x100 | 0x2000) != 0
        {
            directories.insert(record.object, record);
        }
    }
    let moved: Vec<_> = graph
        .nodes()
        .filter(|(_, n)| n.alive && directories.contains_key(&n.object))
        .map(|(i, _)| i)
        .collect();
    for slot in moved {
        graph.remove(slot);
    }
    for record in directories.values() {
        if !internal(record.object, native.root()) {
            graph.add(
                record.object,
                record.parent,
                &record.name,
                record.attributes,
            )?;
        }
    }
    let mut listings = Vec::new();
    for parent in dirty.iter().copied() {
        cancelled(cancel)?;
        if parent != native.root() && graph.directory(parent).is_none() {
            // An unobserved directory cannot prove complete coverage. Only a
            // matching deletion permits omitting its final listing.
            if records
                .iter()
                .any(|r| r.object == parent && r.reason & 0x200 != 0)
            {
                continue;
            }
            return Err(invalid(
                "USN parent unavailable; full NTFS reconciliation required",
            ));
        }
        let path = graph.directory_path(parent)?;
        match native.list(&path, parent, cancel) {
            Ok(list) => listings.push((parent, list)),
            Err(error)
                if matches!(error.raw_os_error(), Some(2 | 3))
                    && records
                        .iter()
                        .any(|r| r.object == parent && r.reason & 0x200 != 0) => {}
            Err(error) => return Err(error),
        }
    }
    // Content-only records or duplicate journal notifications do not require
    // rewriting a complete checkpoint when all namespace fields are unchanged.
    let mut previous: HashMap<u64, HashMap<Vec<u16>, (u64, u32)>> = HashMap::new();
    for (_, node) in old
        .nodes()
        .filter(|(_, n)| n.alive && dirty.contains(&n.parent))
    {
        previous
            .entry(node.parent)
            .or_default()
            .insert(old.name(node).to_vec(), (node.object, node.attributes));
    }
    let mut unchanged = true;
    for (parent, list) in &listings {
        let current: HashMap<_, _> = list
            .iter()
            .filter(|r| !internal(r.object, native.root()) && r.object != storage_root)
            .map(|r| (r.name.clone(), (r.object, r.attributes)))
            .collect();
        if previous.remove(parent).unwrap_or_default() != current {
            unchanged = false;
        }
    }
    if unchanged && previous.is_empty() {
        return Ok((old.clone(), 0));
    }
    graph.remove_parents(&dirty);
    let mut discover = Vec::new();
    for (parent, list) in listings {
        for record in list {
            if internal(record.object, native.root()) || record.object == storage_root {
                continue;
            }
            if record.attributes & (DIRECTORY | REPARSE) == DIRECTORY
                && old.directory(record.object).is_none()
            {
                discover.push(record.object);
            }
            graph.add(record.object, parent, &record.name, record.attributes)?;
        }
    }
    let mut seen = HashSet::new();
    while let Some(parent) = discover.pop() {
        cancelled(cancel)?;
        if !seen.insert(parent) {
            continue;
        }
        let path = graph.directory_path(parent)?;
        for record in native.list(&path, parent, cancel)? {
            if internal(record.object, native.root()) || record.object == storage_root {
                continue;
            }
            if record.attributes & (DIRECTORY | REPARSE) == DIRECTORY {
                discover.push(record.object);
            }
            graph.add(record.object, parent, &record.name, record.attributes)?;
        }
    }
    graph.prune_removed_subtrees(old);
    graph.validate()?;
    if graph.needs_compaction() {
        graph = graph.compact()?;
    }
    Ok((graph, dirty.len()))
}

#[cfg(test)]
mod tests;
