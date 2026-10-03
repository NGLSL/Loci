//! Private Windows prototype: directory namespace discovery, USN-directed updates.
//! A USN name is an invalidation hint, never the complete hard-link namespace.
use crate::{
    checkpoint::Checkpoint,
    model::{EntryId, Record},
    query::QueryIndex,
    store::{self, Snapshot},
    transaction::{JournalUndo, Namespace},
    win::{self, DirectoryPins, Volume},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs, io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const DIRECTORY: u32 = 0x10;
const REPARSE: u32 = 0x400;
const MAX_RECORDS: usize = 1_000_000;
const DEADLINE: Duration = Duration::from_secs(30);
const MAX_ATTEMPTS: usize = 32;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    Ready,
    Pending,
    Stopped,
}
#[derive(Default, Clone, Debug)]
pub struct Stats {
    pub total_records: usize,
    pub scoped_records: usize,
    pub directories_visited: usize,
    pub full_scans: usize,
    pub batches: usize,
    pub elapsed_ms: u128,
    pub journal_major_versions: BTreeSet<u16>,
    pub scoped_reason_mask: u32,
    pub native_link_queries: usize,
    pub retry_attempts: usize,
    pub namespace_batches: usize,
    pub undo_entries: usize,
    pub query_paths_updated: usize,
}
#[derive(Debug)]
pub struct QueryResult {
    pub total: usize,
    pub paths: Vec<Vec<u16>>,
    pub status: Status,
}
pub struct Backend {
    root_path: PathBuf,
    storage: PathBuf,
    snapshot: Snapshot,
    volume: Option<Volume>,
    pins: Option<(DirectoryPins, DirectoryPins)>,
    status: Status,
    stats: Stats,
    namespace: Namespace,
    query_index: Option<QueryIndex>,
}
#[cfg(test)]
thread_local! {
    // A deterministic namespace transaction fault, never a native USN result.
    static REPLAY_TEST_HOOK: std::cell::Cell<Option<fn(&mut Backend, &mut JournalUndo) -> io::Result<()>>> = const { std::cell::Cell::new(None) };
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn raw(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().collect()
}
fn ordinary(attributes: u32) -> bool {
    attributes & DIRECTORY != 0 && attributes & REPARSE == 0
}
fn native_references_supported(records: &[Record]) -> io::Result<()> {
    if records
        .iter()
        .any(|record| record.object > u64::MAX as u128 || record.parent > u64::MAX as u128)
    {
        return Err(io::Error::new(io::ErrorKind::Unsupported,"native namespace uses complete 64-bit NTFS references; wider USN object/parent identities require rebuild with a compatible source"));
    }
    Ok(())
}
fn check(cancel: &AtomicBool, began: Instant) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "operation cancelled; previous publication preserved",
        ));
    }
    if began.elapsed() > DEADLINE {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "operation exceeded 30 second work deadline",
        ));
    }
    Ok(())
}
fn diagnosed<T>(phase: &str, result: io::Result<T>) -> io::Result<T> {
    if let Err(error) = &result {
        eprintln!(
            "backend_phase={phase} error_kind={:?} os_code={:?}",
            error.kind(),
            error.raw_os_error()
        );
    }
    result
}
fn retry_wait(cancel: &AtomicBool, began: Instant, attempt: usize) -> io::Result<()> {
    let millis = (5u64 << attempt.min(6)).min(250);
    let until = Instant::now() + Duration::from_millis(millis);
    while Instant::now() < until {
        check(cancel, began)?;
        std::thread::sleep(Duration::from_millis(5));
    }
    check(cancel, began)
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetVolumeNameForVolumeMountPointW(root: *const u16, name: *mut u16, length: u32) -> i32;
}
fn guid(spec: &str) -> io::Result<String> {
    let mut mount: Vec<u16> = format!("{spec}\\").encode_utf16().collect();
    mount.push(0);
    let mut out = [0u16; 128];
    if unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), out.as_mut_ptr(), out.len() as u32)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let length = out
        .iter()
        .position(|n| *n == 0)
        .ok_or_else(|| invalid("unterminated volume identity"))?;
    String::from_utf16(&out[..length]).map_err(|_| invalid("invalid volume GUID"))
}
fn confined(
    root: &Path,
    storage: &Path,
) -> io::Result<(PathBuf, PathBuf, DirectoryPins, DirectoryPins)> {
    // Check literal ancestors before any canonicalization follows a junction.
    let allowed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-performance/run");
    let allowed_pin = DirectoryPins::hold(&allowed)?;
    let allowed = fs::canonicalize(&allowed)?;
    if !root.is_absolute()
        || !storage.is_absolute()
        || root
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        || storage
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(invalid("explicit absolute engineering paths required"));
    }
    let root_pin = DirectoryPins::hold(root)?;
    let parent = storage
        .parent()
        .ok_or_else(|| invalid("storage parent missing"))?;
    let storage_pin = DirectoryPins::hold(parent)?;
    let root = fs::canonicalize(root)?;
    let parent = fs::canonicalize(parent)?;
    if !root.starts_with(&allowed)
        || !parent.starts_with(&allowed)
        || parent.starts_with(&root)
        || root == allowed
    {
        return Err(invalid(
            "root/storage outside engineering run or storage inside watched root",
        ));
    }
    let storage = parent.join(
        storage
            .file_name()
            .ok_or_else(|| invalid("storage filename missing"))?,
    );
    if let Ok(meta) = fs::symlink_metadata(&storage) {
        if meta.file_attributes() & REPARSE != 0 || !meta.is_file() {
            return Err(invalid("storage must be an ordinary file"));
        }
    }
    drop(allowed_pin);
    Ok((root, storage, root_pin, storage_pin))
}
fn drive(root: &Path) -> io::Result<String> {
    let value = raw(root);
    // canonicalize on Windows returns an extended drive-letter path.
    let at = if value.starts_with(&[92, 92, 63, 92]) {
        4
    } else {
        0
    };
    if value.len() < at + 2 || value[at + 1] != 58 || value[at] > 127 {
        return Err(invalid("drive-letter NTFS source required"));
    }
    Ok(format!("{}:", char::from(value[at] as u8)))
}
#[cfg(test)]
fn directory_paths(
    root: u128,
    entries: &BTreeMap<EntryId, u32>,
) -> io::Result<BTreeMap<u128, PathBuf>> {
    let dirs: BTreeMap<_, _> = entries
        .iter()
        .filter(|(_, a)| ordinary(**a))
        .map(|(e, _)| (e.object, e))
        .collect();
    let mut result = BTreeMap::from([(root, PathBuf::new())]);
    for &object in dirs.keys() {
        let mut current = object;
        let mut parts = Vec::new();
        let mut seen = BTreeSet::new();
        while current != root {
            if !seen.insert(current) || parts.len() >= 64 {
                return Err(invalid("directory cycle/depth budget"));
            }
            let Some(entry) = dirs.get(&current) else {
                break;
            };
            parts.push(OsString::from_wide(&entry.name));
            current = entry.parent;
        }
        if current == root {
            let mut path = PathBuf::new();
            for part in parts.into_iter().rev() {
                path.push(part);
            }
            result.insert(object, path);
        }
    }
    Ok(result)
}
fn scan_directory(
    root: &Path,
    relative: &Path,
    expected: u128,
    serial: u64,
    entries: &mut BTreeMap<EntryId, u32>,
    namespace: &mut Namespace,
    undo: &mut JournalUndo,
    recursive: bool,
    cancel: &AtomicBool,
    began: Instant,
    stats: &mut Stats,
    depth: usize,
) -> io::Result<bool> {
    check(cancel, began)?;
    if depth > 64 {
        return Err(invalid("namespace depth budget exceeded"));
    }
    let path = root.join(relative);
    let pin = diagnosed("scan_directory_pin", DirectoryPins::hold(&path))?;
    let before = diagnosed("scan_directory_identity_before", win::identity(&path))?;
    if before.object != expected || before.volume_serial != serial || !ordinary(before.attributes) {
        return Err(invalid("directory identity changed during reconciliation"));
    }
    let old_children: BTreeSet<_> = entries
        .range(
            EntryId {
                parent: expected,
                object: 0,
                name: Vec::new(),
            }..,
        )
        .take_while(|(entry, _)| entry.parent == expected)
        .filter(|(_, attributes)| ordinary(**attributes))
        .map(|(entry, _)| entry.clone())
        .collect();
    let mut children = Vec::new();
    let mut discovered = BTreeMap::new();
    let listing = diagnosed(
        "scan_directory_batch",
        crate::enumerate::list_directory(&path, expected, serial, cancel, began),
    )?;
    stats.namespace_batches += listing.batches;
    for (entry, attributes) in listing.entries {
        check(cancel, began)?;
        if ordinary(attributes) && (recursive || !namespace.is_directory(entry.object)) {
            children.push((
                relative.join(OsString::from_wide(&entry.name)),
                entry.object,
            ));
        }
        if discovered.insert(entry, attributes).is_some() {
            return Err(invalid("duplicate directory relationship"));
        }
        if discovered.len() > store::MAX_ENTRIES {
            return Err(invalid("directory exceeds entry budget"));
        }
    }
    let after = diagnosed("scan_directory_identity_after", win::identity(&path))?;
    if before.object != after.object || before.attributes != after.attributes {
        return Err(invalid("directory changed during enumeration"));
    }
    let new_children: BTreeSet<_> = discovered
        .iter()
        .filter(|(_, attributes)| ordinary(**attributes))
        .map(|(entry, _)| entry.clone())
        .collect();
    let mut topology_changed = old_children != new_children;
    let old_keys: Vec<_> = entries
        .range(
            EntryId {
                parent: expected,
                object: 0,
                name: Vec::new(),
            }..,
        )
        .take_while(|(entry, _)| entry.parent == expected)
        .map(|(entry, _)| entry.clone())
        .collect();
    for key in old_keys {
        if !discovered.contains_key(&key) {
            undo.set(entries, namespace, key, None)?;
        }
    }
    for (key, attributes) in discovered {
        undo.set(entries, namespace, key, Some(attributes))?;
    }
    if entries.len() > store::MAX_ENTRIES {
        return Err(invalid("namespace exceeds 32768-entry budget"));
    }
    stats.directories_visited += 1;
    for (child, object) in children {
        topology_changed |= scan_directory(
            root,
            &child,
            object,
            serial,
            entries,
            namespace,
            undo,
            true,
            cancel,
            began,
            stats,
            depth + 1,
        )?;
    }
    drop(pin);
    Ok(topology_changed)
}
#[cfg(test)]
fn prune(root: u128, entries: &mut BTreeMap<EntryId, u32>) -> io::Result<()> {
    let reachable = directory_paths(root, entries)?;
    entries.retain(|e, _| reachable.contains_key(&e.parent));
    Ok(())
}
#[cfg(test)]
fn dirty_parents(
    root: u128,
    entries: &BTreeMap<EntryId, u32>,
    records: &[Record],
    stats: &mut Stats,
) -> BTreeSet<u128> {
    let dirs: BTreeSet<_> = entries
        .iter()
        .filter(|(_, a)| ordinary(**a))
        .map(|(e, _)| e.object)
        .chain(std::iter::once(root))
        .collect();
    let mut aliases: BTreeMap<u128, BTreeSet<u128>> = BTreeMap::new();
    for entry in entries.keys() {
        aliases
            .entry(entry.object)
            .or_default()
            .insert(entry.parent);
    }
    let mut dirty = BTreeSet::new();
    for record in records {
        let mut relevant = false;
        if dirs.contains(&record.parent) {
            dirty.insert(record.parent);
            relevant = true;
        }
        if let Some(parents) = aliases.get(&record.object) {
            dirty.extend(parents);
            relevant = true;
        }
        if relevant {
            stats.scoped_records += 1;
            stats.scoped_reason_mask |= record.reason;
        }
        stats.journal_major_versions.insert(record.major);
    }
    dirty
}
fn initial_scan(
    root: &Path,
    snapshot: &mut Snapshot,
    cancel: &AtomicBool,
    began: Instant,
    stats: &mut Stats,
) -> io::Result<()> {
    for attempt in 0..MAX_ATTEMPTS {
        snapshot.entries.clear();
        let mut namespace = Namespace::new(snapshot.root, &snapshot.entries)?;
        let mut undo = JournalUndo::untracked();
        stats.full_scans += 1;
        match scan_directory(
            root,
            Path::new(""),
            snapshot.root,
            snapshot.checkpoint.volume_serial,
            &mut snapshot.entries,
            &mut namespace,
            &mut undo,
            true,
            cancel,
            began,
            stats,
            0,
        ) {
            Ok(_) => return Ok(()),
            Err(error) => {
                eprintln!("backend_phase=initial_namespace_scan attempt={} candidate_entries={} os_code={:?}",attempt+1,snapshot.entries.len(),error.raw_os_error());
                if matches!(error.raw_os_error(), Some(2 | 3)) && attempt + 1 < MAX_ATTEMPTS {
                    stats.retry_attempts += 1;
                    retry_wait(cancel, began, attempt)?;
                } else {
                    return Err(error);
                }
            }
        }
    }
    unreachable!("bounded scan attempts return a result")
}
impl Backend {
    pub fn build(root: &Path, storage: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        let began = Instant::now();
        check(cancel, began)?;
        let (root_path, storage, root_pin, storage_pin) = confined(root, storage)?;
        if storage.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "build must not replace existing checkpoint",
            ));
        }
        let spec = drive(&root_path)?;
        let volume = Volume::open(&spec, 0x80000000)?;
        let journal = volume.query()?;
        let identity = win::identity(&root_path)?;
        let mut snapshot = Snapshot {
            checkpoint: Checkpoint {
                volume_serial: volume.serial,
                journal_id: journal.id,
                cursor: journal.next,
            },
            root: identity.object,
            scope: raw(&root_path),
            volume_guid: guid(&spec)?,
            entries: BTreeMap::new(),
        };
        let mut stats = Stats::default();
        initial_scan(&root_path, &mut snapshot, cancel, began, &mut stats)?;
        let namespace = Namespace::new(snapshot.root, &snapshot.entries)?;
        let mut backend = Self {
            root_path,
            storage,
            snapshot,
            volume: Some(volume),
            pins: Some((root_pin, storage_pin)),
            status: Status::Ready,
            stats,
            namespace,
            query_index: None,
        };
        backend.replay_retry(cancel, began)?;
        backend.snapshot.validate()?;
        backend.query_index = Some(QueryIndex::build(
            backend.snapshot.root,
            &backend.snapshot.entries,
        )?);
        check(cancel, began)?;
        backend.stats.elapsed_ms = began.elapsed().as_millis();
        backend.snapshot.save_new(&backend.storage)?;
        Ok(backend)
    }
    pub fn open(root: &Path, storage: &Path, cancel: &AtomicBool) -> io::Result<Self> {
        let began = Instant::now();
        check(cancel, began)?;
        let (root_path, storage, root_pin, storage_pin) = confined(root, storage)?;
        let snapshot = Snapshot::load(&storage)?;
        let spec = drive(&root_path)?;
        if snapshot.scope != raw(&root_path) || snapshot.volume_guid != guid(&spec)? {
            return Err(invalid(
                "checkpoint source scope/volume GUID changed; rebuild required",
            ));
        }
        let namespace = Namespace::new(snapshot.root, &snapshot.entries)?;
        let query_index = Some(QueryIndex::build(snapshot.root, &snapshot.entries)?);
        let mut backend = Self {
            root_path,
            storage,
            snapshot,
            volume: Some(Volume::open(&spec, 0x80000000)?),
            pins: Some((root_pin, storage_pin)),
            status: Status::Pending,
            stats: Stats::default(),
            namespace,
            query_index,
        };
        backend.sync(cancel)?;
        Ok(backend)
    }
    fn replay(
        &mut self,
        undo: &mut JournalUndo,
        cancel: &AtomicBool,
        began: Instant,
    ) -> io::Result<()> {
        #[cfg(test)]
        if let Some(hook) = REPLAY_TEST_HOOK.with(|slot| slot.get()) {
            return hook(self, undo);
        }
        loop {
            check(cancel, began)?;
            let volume = self
                .volume
                .as_ref()
                .ok_or_else(|| invalid("backend stopped"))?;
            let identity = win::identity(&self.root_path)?;
            if identity.object != self.snapshot.root
                || identity.volume_serial != self.snapshot.checkpoint.volume_serial
                || !ordinary(identity.attributes)
            {
                return Err(invalid("root object changed; rebuild required"));
            }
            let journal = volume.query()?;
            self.snapshot.checkpoint.validate(volume.serial, &journal)?;
            if self.snapshot.checkpoint.cursor == journal.next {
                self.snapshot.validate()?;
                return Ok(());
            }
            let (next, records) = volume.read(journal.id, self.snapshot.checkpoint.cursor)?;
            native_references_supported(&records)?;
            if next <= self.snapshot.checkpoint.cursor
                || records
                    .iter()
                    .any(|r| r.usn < self.snapshot.checkpoint.cursor || r.usn >= next)
            {
                return Err(invalid(
                    "journal cursor/record range invalid; rebuild required",
                ));
            }
            self.stats.total_records += records.len();
            self.stats.batches += 1;
            if self.stats.total_records > MAX_RECORDS {
                return Err(invalid("replay record work budget exceeded"));
            }
            let mut dirty = BTreeSet::new();
            for record in &records {
                let mut relevant = false;
                if self.namespace.is_directory(record.parent) {
                    dirty.insert(record.parent);
                    relevant = true;
                }
                let aliases = self.namespace.alias_parents(record.object);
                if !aliases.is_empty() {
                    dirty.extend(aliases);
                    relevant = true;
                }
                if relevant {
                    self.stats.scoped_records += 1;
                    self.stats.scoped_reason_mask |= record.reason;
                }
                self.stats.journal_major_versions.insert(record.major);
            }
            let spec = drive(&self.root_path)?;
            let objects: BTreeSet<_> = records
                .iter()
                .filter(|record| record.reason & 0x10000 != 0)
                .map(|record| record.object)
                .collect();
            for object in objects {
                check(cancel, began)?;
                self.stats.native_link_queries += 1;
                let parents = diagnosed(
                    "hardlink_all_names",
                    crate::native::hardlink_parents(&spec, object, &self.root_path),
                )?;
                for mut parent in parents {
                    // If a new directory is not indexed yet, scan its nearest
                    // known ancestor; recursive discovery supplies all children.
                    loop {
                        let absolute = self.root_path.join(&parent);
                        match win::identity(&absolute) {
                            Ok(identity)
                                if identity.volume_serial
                                    == self.snapshot.checkpoint.volume_serial
                                    && self.namespace.is_directory(identity.object) =>
                            {
                                dirty.insert(identity.object);
                                break;
                            }
                            Ok(_) => {}
                            Err(error) => return Err(error),
                        }
                        if !parent.pop() {
                            return Err(invalid("hardlink scope parent has no indexed root"));
                        }
                    }
                }
            }
            let mut ordered = Vec::new();
            for id in dirty {
                if let Some(path) = self.namespace.relative_path(id)? {
                    ordered.push((path.components().count(), id));
                }
            }
            ordered.sort();
            let mut topology_changed = false;
            for (_, id) in ordered {
                if let Some(relative) = self.namespace.relative_path(id)? {
                    topology_changed |= scan_directory(
                        &self.root_path,
                        &relative,
                        id,
                        volume.serial,
                        &mut self.snapshot.entries,
                        &mut self.namespace,
                        undo,
                        false,
                        cancel,
                        began,
                        &mut self.stats,
                        0,
                    )?;
                }
            }
            if topology_changed {
                let mut removed = Vec::new();
                for entry in self.snapshot.entries.keys() {
                    if self.namespace.relative_path(entry.parent)?.is_none() {
                        removed.push(entry.clone());
                    }
                }
                for entry in removed {
                    undo.set(&mut self.snapshot.entries, &mut self.namespace, entry, None)?;
                }
            }
            self.snapshot.checkpoint.cursor = next;
            // Never publish a map read during mutations with an earlier cursor.
            // Drain again until query observes a quiet journal boundary.
        }
    }
    fn replay_retry(&mut self, cancel: &AtomicBool, began: Instant) -> io::Result<()> {
        let seed_cursor = self.snapshot.checkpoint.cursor;
        for attempt in 0..MAX_ATTEMPTS {
            let mut undo = JournalUndo::new();
            let result = self.replay(&mut undo, cancel, began).and_then(|()| {
                if let Some(index) = &self.query_index {
                    let update = index.prepare_update(
                        self.snapshot.root,
                        &self.snapshot.entries,
                        &undo.changed_keys(),
                    )?;
                    check(cancel, began)?;
                    self.stats.query_paths_updated += update.changed_paths();
                    self.query_index.as_mut().unwrap().apply(update);
                }
                Ok(())
            });
            self.stats.undo_entries += undo.saved_entries();
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    undo.rollback(&mut self.snapshot.entries, &mut self.namespace);
                    self.snapshot.checkpoint.cursor = seed_cursor;
                    eprintln!(
                        "backend_phase=journal_replay attempt={} os_code={:?}",
                        attempt + 1,
                        error.raw_os_error()
                    );
                    if matches!(error.raw_os_error(), Some(2 | 3)) && attempt + 1 < MAX_ATTEMPTS {
                        self.stats.retry_attempts += 1;
                        retry_wait(cancel, began, attempt)?;
                    } else {
                        return Err(error);
                    }
                }
            }
        }
        unreachable!("bounded replay attempts return a result")
    }
    pub fn sync(&mut self, cancel: &AtomicBool) -> io::Result<Stats> {
        if self.status == Status::Stopped {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "backend stopped"));
        }
        self.stats = Stats::default();
        let began = Instant::now();
        match self.replay_retry(cancel, began) {
            Ok(()) => {
                self.status = Status::Ready;
                self.stats.elapsed_ms = began.elapsed().as_millis();
                Ok(self.stats.clone())
            }
            Err(error) => {
                self.status = Status::Pending;
                self.stats.elapsed_ms = began.elapsed().as_millis();
                Err(error)
            }
        }
    }
    pub fn query(&self, needle: &[u16], limit: usize) -> io::Result<QueryResult> {
        if needle.len() > 512 || limit > store::MAX_ENTRIES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "query/response budget exceeded",
            ));
        }
        let (total, paths) = self
            .query_index
            .as_ref()
            .ok_or_else(|| invalid("query index not published"))?
            .search(needle, limit);
        Ok(QueryResult {
            total,
            paths,
            status: self.status,
        })
    }
    pub fn save(&self) -> io::Result<()> {
        if self.status != Status::Ready {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "only complete Ready state may be saved",
            ));
        }
        self.snapshot.save(&self.storage)
    }
    pub fn stop(&mut self) {
        self.volume.take();
        self.pins.take();
        self.status = Status::Stopped;
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn stats(&self) -> &Stats {
        &self.stats
    }
    #[cfg(test)]
    pub fn status(&self) -> Status {
        self.status
    }
}
/// Explicit, bounded, read-only volume projection. This diagnostic does not
/// publish an inventory or cursor: ENUM names do not enumerate all hard links.
pub fn inspect_mft(root: &Path, cancel: &AtomicBool) -> io::Result<()> {
    let allowed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-performance/run");
    let _allowed_pin = DirectoryPins::hold(&allowed)?;
    let _root_pin = DirectoryPins::hold(root)?;
    let allowed = fs::canonicalize(allowed)?;
    let root = fs::canonicalize(root)?;
    if !root.starts_with(&allowed) || root == allowed {
        return Err(invalid(
            "MFT diagnostic scope must be an explicit engineering fixture",
        ));
    }
    let spec = drive(&root)?;
    let volume = Volume::open(&spec, 0x80000000)?;
    let journal = volume.query()?;
    let identity = win::identity(&root)?;
    let mut snapshot = Snapshot {
        checkpoint: Checkpoint {
            volume_serial: volume.serial,
            journal_id: journal.id,
            cursor: journal.next,
        },
        root: identity.object,
        scope: raw(&root),
        volume_guid: guid(&spec)?,
        entries: BTreeMap::new(),
    };
    let mut stats = Stats::default();
    initial_scan(&root, &mut snapshot, cancel, Instant::now(), &mut stats)?;
    snapshot.validate()?;
    let parents: BTreeSet<_> = snapshot
        .entries
        .iter()
        .filter(|(_, a)| ordinary(**a))
        .map(|(e, _)| e.object)
        .chain(std::iter::once(snapshot.root))
        .collect();
    let projection = crate::enumerate::enumerate_scope(&volume, &parents, journal.next, cancel)?;
    let retained = volume.query()?;
    snapshot.checkpoint.validate(volume.serial, &retained)?;
    let after = win::identity(&root)?;
    if after.object != snapshot.root || after.volume_serial != volume.serial {
        return Err(invalid("MFT diagnostic root identity changed"));
    }
    let missing = snapshot
        .entries
        .keys()
        .filter(|e| !projection.entries.contains_key(*e))
        .count();
    let extra = projection
        .entries
        .keys()
        .filter(|e| !snapshot.entries.contains_key(*e))
        .count();
    println!("mft_projection_complete=true volume={spec} readonly=true inventory_published=false all_hardlink_names_proven=false scoped_namespace_entries={} projection_entries={} missing_relationships={} extra_relationships={} total_volume_records={} batches={} elapsed_ms={} journal_id={} beginning_cursor={} namespace_batches={} scope_parent_count={} final_mft_cursor={} partial_name_projection={}",snapshot.entries.len(),projection.entries.len(),missing,extra,projection.total_records,projection.batches,projection.elapsed_ms,journal.id,journal.next,stats.namespace_batches,parents.len(),projection.final_cursor,projection.partial_name_projection);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(parent: u128, object: u128, name: &str) -> EntryId {
        EntryId {
            parent,
            object,
            name: name.encode_utf16().collect(),
        }
    }
    fn record(parent: u128, object: u128) -> Record {
        Record {
            parent,
            object,
            name: vec![97],
            usn: 1,
            reason: 0x10000,
            attributes: 0,
            major: 2,
        }
    }
    #[test]
    fn native_namespace_rejects_wide_object_and_parent_before_invalidation() {
        assert!(native_references_supported(&[record(1, u64::MAX as u128)]).is_ok());
        for wide in [record(1, 1u128 << 64), record(1u128 << 64, 2)] {
            assert_eq!(
                native_references_supported(&[wide]).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
    }
    #[test]
    fn directory_rename_derives_descendants_without_rewriting_children() {
        let mut entries =
            BTreeMap::from([(entry(1, 2, "old"), DIRECTORY), (entry(2, 3, "child"), 0)]);
        entries.remove(&entry(1, 2, "old"));
        entries.insert(entry(1, 2, "new"), DIRECTORY);
        let paths = store::paths(1, &entries).unwrap();
        assert!(paths.contains(&"new\\child".encode_utf16().collect::<Vec<_>>()));
    }
    #[test]
    fn outside_hardlink_event_dirties_all_known_alias_parents() {
        let entries = BTreeMap::from([
            (entry(1, 2, "dir"), DIRECTORY),
            (entry(1, 3, "a"), 0),
            (entry(2, 3, "b"), 0),
        ]);
        let mut stats = Stats::default();
        assert_eq!(
            dirty_parents(1, &entries, &[record(99, 3)], &mut stats),
            BTreeSet::from([1, 2])
        );
        assert_eq!(stats.scoped_records, 1);
    }
    #[test]
    fn moved_out_subtree_prunes_all_descendants_but_preserves_external_alias() {
        let mut entries = BTreeMap::from([
            (entry(2, 3, "nested"), DIRECTORY),
            (entry(3, 4, "child"), 0),
            (entry(1, 4, "alias"), 0),
        ]);
        prune(1, &mut entries).unwrap();
        assert_eq!(entries, BTreeMap::from([(entry(1, 4, "alias"), 0)]));
    }
    #[test]
    fn unrelated_volume_events_do_not_trigger_directory_scans() {
        let entries = BTreeMap::from([(entry(1, 2, "dir"), DIRECTORY)]);
        let mut stats = Stats::default();
        assert!(dirty_parents(1, &entries, &[record(99, 100)], &mut stats).is_empty());
        assert_eq!(stats.scoped_records, 0);
    }
    #[test]
    fn cancellation_is_an_error_before_any_work() {
        assert_eq!(
            check(&AtomicBool::new(true), Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }
    #[test]
    fn failed_sync_preserves_published_snapshot_and_blocks_save() {
        let snapshot = Snapshot {
            checkpoint: Checkpoint {
                volume_serial: 1,
                journal_id: 2,
                cursor: 3,
            },
            root: 1,
            scope: "D:\\fixture".encode_utf16().collect(),
            volume_guid: r"\\?\Volume{00000000-0000-0000-0000-000000000001}\".into(),
            entries: BTreeMap::from([(entry(1, 2, "previous"), 0)]),
        };
        let mut backend = Backend {
            root_path: PathBuf::new(),
            storage: PathBuf::new(),
            snapshot: snapshot.clone(),
            volume: None,
            pins: None,
            status: Status::Ready,
            stats: Stats::default(),
            namespace: Namespace::new(snapshot.root, &snapshot.entries).unwrap(),
            query_index: Some(QueryIndex::build(snapshot.root, &snapshot.entries).unwrap()),
        };
        assert_eq!(
            backend.sync(&AtomicBool::new(true)).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(backend.snapshot(), &snapshot);
        assert_eq!(backend.status(), Status::Pending);
        assert_eq!(
            backend.save().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let old = backend
            .query(&"previous".encode_utf16().collect::<Vec<_>>(), 0)
            .unwrap();
        assert_eq!(old.total, 1);
        assert!(old.paths.is_empty());
        assert_eq!(old.status, Status::Pending);
        backend.stop();
        assert_eq!(backend.status(), Status::Stopped);
        assert_eq!(
            backend.sync(&AtomicBool::new(false)).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
    #[test]
    fn real_directory_reconcile_injected_failure_rolls_back_inventory_cursor_and_query_view() {
        fn reconcile(backend: &mut Backend, undo: &mut JournalUndo) -> io::Result<()> {
            scan_directory(
                &backend.root_path,
                Path::new(""),
                backend.snapshot.root,
                backend.snapshot.checkpoint.volume_serial,
                &mut backend.snapshot.entries,
                &mut backend.namespace,
                undo,
                false,
                &AtomicBool::new(false),
                Instant::now(),
                &mut backend.stats,
                0,
            )?;
            backend.snapshot.checkpoint.cursor += 1;
            backend.snapshot.validate()
        }
        fn fail_after_reconcile(backend: &mut Backend, undo: &mut JournalUndo) -> io::Result<()> {
            reconcile(backend, undo)?;
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "injected transaction failure after real namespace scan, not native USN",
            ))
        }
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../.scratch/windows-ntfs-performance/run");
        let _parent_pin = DirectoryPins::hold(base.parent().unwrap()).unwrap();
        match fs::create_dir(&base) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("{error}"),
        }
        let _base_pin = DirectoryPins::hold(&base).unwrap();
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = base.join(format!("rollback-real-{}-{suffix}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let _root_pin = DirectoryPins::hold(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        fs::write(root.join("old.txt"), []).unwrap();
        let root_info = win::identity(&root).unwrap();
        let file_info = win::identity(&root.join("old.txt")).unwrap();
        let old = Snapshot {
            checkpoint: Checkpoint {
                volume_serial: root_info.volume_serial,
                journal_id: 7,
                cursor: 3,
            },
            root: root_info.object,
            scope: raw(&root),
            volume_guid: guid(&drive(&root).unwrap()).unwrap(),
            entries: BTreeMap::from([(
                entry(root_info.object, file_info.object, "old.txt"),
                file_info.attributes,
            )]),
        };
        let mut backend = Backend {
            root_path: root.clone(),
            storage: base.join(format!("unused-rollback-{suffix}.lcusn")),
            snapshot: old.clone(),
            volume: None,
            pins: None,
            status: Status::Ready,
            stats: Stats::default(),
            namespace: Namespace::new(old.root, &old.entries).unwrap(),
            query_index: Some(QueryIndex::build(old.root, &old.entries).unwrap()),
        };
        fs::rename(root.join("old.txt"), root.join("new.txt")).unwrap();
        fs::write(root.join("added.txt"), []).unwrap();
        REPLAY_TEST_HOOK.with(|slot| slot.set(Some(fail_after_reconcile)));
        let result = backend.sync(&AtomicBool::new(false));
        REPLAY_TEST_HOOK.with(|slot| slot.set(None));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(backend.snapshot, old);
        assert_eq!(backend.status, Status::Pending);
        assert_eq!(backend.stats.undo_entries, 3);
        assert_eq!(backend.stats.query_paths_updated, 0);
        assert_eq!(
            backend.query(&[], 50).unwrap().paths,
            vec!["old.txt".encode_utf16().collect::<Vec<_>>()]
        );
        assert_eq!(
            backend.save().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        REPLAY_TEST_HOOK.with(|slot| slot.set(Some(reconcile)));
        let result = backend.sync(&AtomicBool::new(false));
        REPLAY_TEST_HOOK.with(|slot| slot.set(None));
        result.unwrap();
        assert_eq!(backend.status, Status::Ready);
        assert_eq!(backend.snapshot.checkpoint.cursor, 4);
        assert_eq!(
            backend
                .query(&[], 50)
                .unwrap()
                .paths
                .into_iter()
                .collect::<BTreeSet<_>>(),
            crate::acceptance::oracle(&root).unwrap()
        );
        eprintln!("transaction_fault injected=true actual_usn_sync=false real_namespace=true rollback_exact=true touched_entries=3");
    }
    #[test]
    fn retry_backoff_respects_cancel_and_original_deadline() {
        assert_eq!(
            retry_wait(&AtomicBool::new(true), Instant::now(), 6)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(
            retry_wait(
                &AtomicBool::new(false),
                Instant::now() - Duration::from_secs(31),
                6
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::TimedOut
        );
    }
}
