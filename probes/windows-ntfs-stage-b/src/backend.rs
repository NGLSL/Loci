//! Private Windows prototype: directory namespace discovery, USN-directed updates.
//! A USN name is an invalidation hint, never the complete hard-link namespace.
use crate::{
    checkpoint::Checkpoint,
    model::{EntryId, Record},
    store::{self, Snapshot},
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
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-b/run");
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
    let pin = DirectoryPins::hold(&path)?;
    let before = win::identity(&path)?;
    if before.object != expected || before.volume_serial != serial || !ordinary(before.attributes) {
        return Err(invalid("directory identity changed during reconciliation"));
    }
    let old_dirs: BTreeSet<_> = entries
        .iter()
        .filter(|(_, a)| ordinary(**a))
        .map(|(e, _)| e.object)
        .collect();
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
    for item in fs::read_dir(&path)? {
        check(cancel, began)?;
        let item = item?;
        let identity = win::identity(&item.path())?;
        if identity.volume_serial != serial {
            return Err(invalid("namespace entry changed volume"));
        }
        let entry = EntryId {
            parent: expected,
            object: identity.object,
            name: item.file_name().encode_wide().collect(),
        };
        if ordinary(identity.attributes) && (recursive || !old_dirs.contains(&identity.object)) {
            children.push((relative.join(item.file_name()), identity.object));
        }
        if discovered.insert(entry, identity.attributes).is_some() {
            return Err(invalid("duplicate directory relationship"));
        }
        if discovered.len() > store::MAX_ENTRIES {
            return Err(invalid("directory exceeds entry budget"));
        }
    }
    let after = win::identity(&path)?;
    if before.object != after.object || before.attributes != after.attributes {
        return Err(invalid("directory changed during enumeration"));
    }
    let new_children: BTreeSet<_> = discovered
        .iter()
        .filter(|(_, attributes)| ordinary(**attributes))
        .map(|(entry, _)| entry.clone())
        .collect();
    let mut topology_changed = old_children != new_children;
    entries.retain(|e, _| e.parent != expected);
    entries.extend(discovered);
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
fn prune(root: u128, entries: &mut BTreeMap<EntryId, u32>) -> io::Result<()> {
    let reachable = directory_paths(root, entries)?;
    entries.retain(|e, _| reachable.contains_key(&e.parent));
    Ok(())
}
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
        let mut stats = Stats {
            full_scans: 1,
            ..Stats::default()
        };
        for attempt in 0..4 {
            snapshot.entries.clear();
            let scanned = scan_directory(
                &root_path,
                Path::new(""),
                snapshot.root,
                volume.serial,
                &mut snapshot.entries,
                true,
                cancel,
                began,
                &mut stats,
                0,
            );
            match scanned {
                Ok(_) => break,
                Err(error) if matches!(error.raw_os_error(), Some(2 | 3)) && attempt < 3 => {
                    stats.full_scans += 1;
                    check(cancel, began)?;
                }
                Err(error) => return Err(error),
            }
        }
        let mut backend = Self {
            root_path,
            storage,
            snapshot,
            volume: Some(volume),
            pins: Some((root_pin, storage_pin)),
            status: Status::Ready,
            stats,
        };
        let mut candidate = backend.snapshot.clone();
        backend.replay_retry(&mut candidate, cancel, began)?;
        candidate.validate()?;
        backend.snapshot = candidate;
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
        let mut backend = Self {
            root_path,
            storage,
            snapshot,
            volume: Some(Volume::open(&spec, 0x80000000)?),
            pins: Some((root_pin, storage_pin)),
            status: Status::Pending,
            stats: Stats::default(),
        };
        backend.sync(cancel)?;
        Ok(backend)
    }
    fn replay(
        &mut self,
        candidate: &mut Snapshot,
        cancel: &AtomicBool,
        began: Instant,
    ) -> io::Result<()> {
        loop {
            check(cancel, began)?;
            let volume = self
                .volume
                .as_ref()
                .ok_or_else(|| invalid("backend stopped"))?;
            let identity = win::identity(&self.root_path)?;
            if identity.object != candidate.root
                || identity.volume_serial != candidate.checkpoint.volume_serial
                || !ordinary(identity.attributes)
            {
                return Err(invalid("root object changed; rebuild required"));
            }
            let journal = volume.query()?;
            candidate.checkpoint.validate(volume.serial, &journal)?;
            if candidate.checkpoint.cursor == journal.next {
                candidate.validate()?;
                return Ok(());
            }
            let (next, records) = volume.read(journal.id, candidate.checkpoint.cursor)?;
            if next <= candidate.checkpoint.cursor
                || records
                    .iter()
                    .any(|r| r.usn < candidate.checkpoint.cursor || r.usn >= next)
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
            let mut dirty = dirty_parents(
                candidate.root,
                &candidate.entries,
                &records,
                &mut self.stats,
            );
            // Recompute paths after each ancestor update: descendants may have moved.
            let mut paths = directory_paths(candidate.root, &candidate.entries)?;
            let spec = drive(&self.root_path)?;
            let objects: BTreeSet<_> = records
                .iter()
                .filter(|record| record.reason & 0x10000 != 0)
                .map(|record| record.object)
                .collect();
            for object in objects {
                check(cancel, began)?;
                self.stats.native_link_queries += 1;
                let parents = crate::native::hardlink_parents(&spec, object, &self.root_path)?;
                for mut parent in parents {
                    // If a new directory is not indexed yet, scan its nearest
                    // known ancestor; recursive discovery supplies all children.
                    loop {
                        if let Some((&id, _)) =
                            paths.iter().find(|(_, relative)| **relative == parent)
                        {
                            dirty.insert(id);
                            break;
                        }
                        if !parent.pop() {
                            return Err(invalid("hardlink scope parent has no indexed root"));
                        }
                    }
                }
            }
            let mut ordered: Vec<_> = dirty
                .into_iter()
                .filter_map(|id| paths.get(&id).map(|p| (p.components().count(), id)))
                .collect();
            ordered.sort();
            for (_, id) in ordered {
                if let Some(relative) = paths.get(&id).cloned() {
                    let topology_changed = scan_directory(
                        &self.root_path,
                        &relative,
                        id,
                        volume.serial,
                        &mut candidate.entries,
                        false,
                        cancel,
                        began,
                        &mut self.stats,
                        0,
                    )?;
                    if topology_changed {
                        paths = directory_paths(candidate.root, &candidate.entries)?;
                    }
                }
            }
            prune(candidate.root, &mut candidate.entries)?;
            candidate.checkpoint.cursor = next;
            // Never publish a map read during mutations with an earlier cursor.
            // Drain again until query observes a quiet journal boundary.
        }
    }
    fn replay_retry(
        &mut self,
        candidate: &mut Snapshot,
        cancel: &AtomicBool,
        began: Instant,
    ) -> io::Result<()> {
        let seed = candidate.clone();
        for attempt in 0..4 {
            *candidate = seed.clone();
            match self.replay(candidate, cancel, began) {
                Ok(()) => return Ok(()),
                Err(error) if matches!(error.raw_os_error(), Some(2 | 3)) && attempt < 3 => {
                    check(cancel, began)?;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("four attempts always return a result")
    }
    pub fn sync(&mut self, cancel: &AtomicBool) -> io::Result<Stats> {
        if self.status == Status::Stopped {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "backend stopped"));
        }
        self.stats = Stats::default();
        let began = Instant::now();
        let mut candidate = self.snapshot.clone();
        match self.replay_retry(&mut candidate, cancel, began) {
            Ok(()) => {
                self.snapshot = candidate;
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
        let paths = store::paths(self.snapshot.root, &self.snapshot.entries)?;
        let matched: Vec<_> = paths
            .into_iter()
            .filter(|p| needle.is_empty() || p.windows(needle.len()).any(|part| part == needle))
            .collect();
        Ok(QueryResult {
            total: matched.len(),
            paths: matched.into_iter().take(limit).collect(),
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
    pub fn status(&self) -> Status {
        self.status
    }
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
}
