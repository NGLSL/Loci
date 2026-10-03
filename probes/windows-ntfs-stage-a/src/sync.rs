//! Opt-in, bounded MFT bootstrap + conservative journal-triggered reconciliation.
//! This is a technical experiment, not a million-entry production index.
use crate::{
    checkpoint::Checkpoint,
    model::{EntryId, Record},
    snapshot::Snapshot,
    win::{self, Volume},
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
    thread,
    time::{Duration, Instant},
};
// Calibrated from the real D: run: 2M records took 3.8 s and did not reach EOF.
// This is an I/O work budget; retained namespace and time limits stay fixed.
const MAX_SCANNED: usize = 10_000_000;
const MAX_NAME_BYTES: usize = 32 * 1024 * 1024;
const MAX_SCOPED: usize = 8192;
type Paths = BTreeSet<Vec<u16>>;
fn invalid(s: &str) -> io::Error {
    io::Error::other(s)
}
fn raw(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().collect()
}

// Candidate namespace builder is distinct from the path-only oracle. Each
// directory entry has parent/object/raw-name identity, including hardlinks
// whose MFT representative name is outside the selected subtree.
fn inventory(root: &Path) -> io::Result<(u128, BTreeMap<EntryId, u32>)> {
    fn collect(
        dir: &Path,
        parent: u128,
        entries: &mut BTreeMap<EntryId, u32>,
        dirs: &mut BTreeMap<u128, Record>,
        depth: usize,
    ) -> io::Result<()> {
        if depth > 32 {
            return Err(invalid("namespace depth budget exceeded"));
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            let id = win::identity(&e.path())?;
            let name: Vec<u16> = e.file_name().encode_wide().collect();
            let r = Record {
                object: id.object,
                parent,
                usn: 0,
                reason: 0,
                attributes: id.attributes,
                name,
                major: 2,
            };
            if entries.insert(r.entry_id(), r.attributes).is_some() {
                return Err(invalid("duplicate namespace entry"));
            }
            if entries.len() > MAX_SCOPED {
                return Err(invalid("namespace entry budget; incomplete"));
            }
            if id.attributes & 0x10 != 0 && id.attributes & 0x400 == 0 {
                dirs.insert(id.object, r);
                collect(&e.path(), id.object, entries, dirs, depth + 1)?;
            }
        }
        Ok(())
    }
    let root_id = win::identity(root)?.object;
    let mut entries = BTreeMap::new();
    let mut dirs = BTreeMap::new();
    collect(root, root_id, &mut entries, &mut dirs, 0)?;
    Ok((root_id, entries))
}
fn entry_paths(root_id: u128, entries: &BTreeMap<EntryId, u32>) -> io::Result<Paths> {
    let dirs: BTreeMap<_, _> = entries
        .iter()
        .filter(|(_, a)| **a & 0x10 != 0 && **a & 0x400 == 0)
        .map(|(e, a)| {
            (
                e.object,
                Record {
                    object: e.object,
                    parent: e.parent,
                    name: e.name.clone(),
                    attributes: *a,
                    major: 2,
                    usn: 0,
                    reason: 0,
                },
            )
        })
        .collect();
    let mut paths = Paths::new();
    for entry in entries.keys() {
        let parent = relative(entry.parent, root_id, &dirs)?
            .ok_or_else(|| invalid("namespace parent missing"))?;
        let mut p = parent;
        p.push(OsString::from_wide(&entry.name));
        paths.insert(raw(&p));
    }
    if paths.len() != entries.len() {
        return Err(invalid("namespace path collision; incomplete"));
    }
    Ok(paths)
}
fn reconcile(root: &Path) -> io::Result<Paths> {
    let (root_id, entries) = inventory(root)?;
    entry_paths(root_id, &entries)
}

fn scoped_snapshot(root: &Path, checkpoint: Checkpoint) -> io::Result<Snapshot> {
    let (root_id, entries) = inventory(root)?;
    Ok(Snapshot {
        root: root_id,
        scope: raw(root),
        entries,
        checkpoint,
    })
}

// Separate directory traversal oracle; it never consults USN or the MFT map.
fn oracle(root: &Path) -> io::Result<Paths> {
    fn walk(root: &Path, dir: &Path, out: &mut Paths, depth: usize) -> io::Result<()> {
        if depth > 32 {
            return Err(invalid("oracle depth budget exceeded"));
        }
        for e in fs::read_dir(dir)? {
            let e = e?;
            let p = e.path();
            let m = fs::symlink_metadata(&p)?;
            out.insert(raw(p
                .strip_prefix(root)
                .map_err(|_| invalid("oracle escaped root"))?));
            if out.len() > MAX_SCOPED {
                return Err(invalid("scoped entry budget exceeded; incomplete"));
            }
            if m.is_dir() && m.file_attributes() & 0x400 == 0 {
                walk(root, &p, out, depth + 1)?;
            }
        }
        Ok(())
    }
    let mut out = Paths::new();
    walk(root, root, &mut out, 0)?;
    Ok(out)
}
pub fn scoped_check(root: &Path, expected: &Paths) -> io::Result<()> {
    let candidate = reconcile(root)?;
    if &candidate != expected || candidate != oracle(root)? {
        return Err(invalid(
            "identity namespace differs from independent complete raw path set",
        ));
    }
    println!("scoped_identity_namespace=PASS complete_path_set=true entries={} (directory namespace evidence, NOT MFT/USN evidence)",candidate.len());
    Ok(())
}
fn relative(object: u128, root: u128, map: &BTreeMap<u128, Record>) -> io::Result<Option<PathBuf>> {
    let mut object = object;
    let mut parts = Vec::new();
    let mut seen = BTreeSet::new();
    while object != root {
        if !seen.insert(object) {
            return Err(invalid("MFT parent cycle; incomplete"));
        }
        let Some(r) = map.get(&object) else {
            return Ok(None);
        };
        if r.parent == object {
            return Ok(None);
        };
        if parts.len() > 64 {
            return Err(invalid("MFT ancestry budget exceeded"));
        }
        // A name must be a single component; ADS are not search entries.
        if r.name.iter().any(|c| matches!(*c, 0 | 47 | 92 | 58))
            || r.name == [46]
            || r.name == [46, 46]
        {
            return Err(invalid("invalid MFT name component"));
        }
        parts.push(OsString::from_wide(&r.name));
        object = r.parent;
    }
    let mut p = PathBuf::new();
    for n in parts.into_iter().rev() {
        p.push(n);
    }
    Ok(Some(p))
}
// Hybrid scope reducer: directory namespace seeds define a small parent graph.
// The whole-volume MFT stream is never retained. A representative name outside
// these parents may hide an in-scope hardlink; namespace supplementation remains
// necessary. Duplicate detection covers retained native objects only.
struct ScopedMftReducer {
    directories: BTreeSet<u128>,
    map: BTreeMap<u128, Record>,
    seen_native: BTreeSet<u128>,
    scanned: usize,
    name_bytes: usize,
    scanned_limit: usize,
    retained_limit: usize,
    name_limit: usize,
}
impl ScopedMftReducer {
    fn seeded(root: u128, entries: &BTreeMap<EntryId, u32>) -> io::Result<Self> {
        let mut reducer = Self {
            directories: BTreeSet::from([root]),
            map: BTreeMap::new(),
            seen_native: BTreeSet::new(),
            scanned: 0,
            name_bytes: 0,
            scanned_limit: MAX_SCANNED,
            retained_limit: MAX_SCOPED,
            name_limit: MAX_NAME_BYTES,
        };
        for (e, attributes) in entries {
            if attributes & 0x10 != 0 && attributes & 0x400 == 0 {
                reducer.directories.insert(e.object);
                let record = Record {
                    object: e.object,
                    parent: e.parent,
                    usn: 0,
                    reason: 0,
                    attributes: *attributes,
                    name: e.name.clone(),
                    major: 2,
                };
                reducer.name_bytes += record.name.len() * 2;
                if reducer.map.insert(record.object, record).is_some() {
                    return Err(invalid("duplicate namespace directory object; incomplete"));
                }
                if reducer.map.len() > reducer.retained_limit
                    || reducer.name_bytes > reducer.name_limit
                {
                    return Err(invalid("MFT directory seed budget exceeded; incomplete"));
                }
            }
        }
        Ok(reducer)
    }
    fn push(&mut self, record: Record) -> io::Result<()> {
        self.scanned += 1;
        if self.scanned > self.scanned_limit {
            return Err(invalid(
                "MFT scanned record budget exceeded; incomplete, no checkpoint published",
            ));
        }
        if record.major != 2 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native bootstrap supports V2 NTFS FRNs only; V3 decoding is separately tested",
            ));
        }
        if !self.directories.contains(&record.parent) && !self.directories.contains(&record.object)
        {
            return Ok(());
        }
        if self.seen_native.contains(&record.object) {
            return Err(invalid(
                "duplicate retained object in MFT enumeration; rebuild",
            ));
        }
        let previous = self.map.get(&record.object).map_or(0, |r| r.name.len() * 2);
        let new_bytes = self.name_bytes - previous + record.name.len() * 2;
        let new_count = self.map.len() + usize::from(!self.map.contains_key(&record.object));
        if new_bytes > self.name_limit || new_count > self.retained_limit {
            return Err(invalid(
                "MFT retained object/name budget exceeded; incomplete, no checkpoint published",
            ));
        }
        self.name_bytes = new_bytes;
        self.seen_native.insert(record.object);
        self.map.insert(record.object, record);
        Ok(())
    }
}
#[derive(Default)]
struct ProjectionStatus {
    missing_file_os2: usize,
    missing_path_os3: usize,
    changed_identity: usize,
}
impl ProjectionStatus {
    fn stale_error(&mut self, error: io::Error) -> io::Result<bool> {
        match error.raw_os_error() {
            Some(2) => self.missing_file_os2 += 1,
            Some(3) => self.missing_path_os3 += 1,
            _ => return Err(error),
        }
        Ok(false)
    }
    fn current(&mut self, path: &Path, object: u128, serial: u64) -> io::Result<bool> {
        match win::identity(path) {
            Ok(id) if id.object == object && id.volume_serial == serial => Ok(true),
            Ok(_) => {
                self.changed_identity += 1;
                Ok(false)
            }
            Err(error) => self.stale_error(error),
        }
    }
    fn degraded(&self) -> bool {
        self.missing_file_os2 != 0 || self.missing_path_os3 != 0 || self.changed_identity != 0
    }
}
fn bootstrap(
    root: &Path,
    volume: &Volume,
    root_id: u128,
    entries: BTreeMap<EntryId, u32>,
) -> io::Result<(Paths, usize, usize, bool)> {
    let start = Instant::now();
    // Seed discovery is ordinary bounded traversal of the engineering fixture.
    // These directory records are graph scaffolding, not native MFT evidence.
    let mut reducer = ScopedMftReducer::seeded(root_id, &entries)?;
    drop(entries);
    let mut cursor = 0;
    let mut batches = 0;
    let enumeration = (|| -> io::Result<()> {
        loop {
            if start.elapsed() > Duration::from_secs(20) {
                return Err(invalid("MFT time budget exceeded; incomplete"));
            }
            match volume.enumerate(cursor, i64::MAX) {
                Err(e) if e.raw_os_error() == Some(38) => return Ok(()),
                Err(e) => return Err(e),
                Ok((next, records)) => {
                    batches += 1;
                    if next <= cursor {
                        return Err(invalid(
                            "MFT enumeration cursor did not advance; incomplete",
                        ));
                    }
                    cursor = next;
                    // enumerate uses a 64 KiB API buffer. Only this decoded
                    // batch and scope-retained records are live here.
                    for record in records {
                        reducer.push(record)?;
                    }
                }
            }
        }
    })();
    println!("MFT_stream_complete={} scanned_objects={} retained_graph_objects={} retained_native_objects={} batches={} retained_raw_name_bytes={} namespace_directory_seeds={} hybrid_scope_filter=true whole_volume_dedup=false external_names_logged=false",
        enumeration.is_ok(), reducer.scanned, reducer.map.len(), reducer.seen_native.len(), batches,
        reducer.name_bytes, reducer.directories.len());
    enumeration?;
    let map = &reducer.map;
    // Expand native hardlink names instead of assuming a USN name is every entry.
    let prefix = raw(root);
    if prefix.len() < 7 || prefix[5] != 58 {
        return Err(invalid("extended drive-backed root required"));
    }
    let scoped_prefix = &prefix[6..];
    let mut paths = Paths::new();
    let mut projection = ProjectionStatus::default();
    for r in map.values() {
        // Unobserved seeds serve parent resolution only; their own names are
        // not evidence from native MFT enumeration.
        if r.object == root_id || !reducer.seen_native.contains(&r.object) {
            continue;
        }
        let Some(p) = relative(r.object, root_id, &map)? else {
            continue;
        };
        if r.attributes & 0x400 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "MFT reparse resolution not established; fallback required",
            ));
        }
        let native_path = root.join(&p);
        if !projection.current(&native_path, r.object, volume.serial)? {
            continue;
        }
        // Directory graph uses parents; regular-file names must be expanded.
        if r.attributes & 0x10 != 0 {
            paths.insert(raw(&p));
        } else {
            let names = match win::hardlink_names(&native_path) {
                Ok(names) => names,
                Err(error) => {
                    projection.stale_error(error)?;
                    continue;
                }
            };
            // Expansion uses a path API; verify again before merging names so
            // a path reused for another object cannot contribute its links.
            if !projection.current(&native_path, r.object, volume.serial)? {
                continue;
            }
            for name in names {
                if name.starts_with(scoped_prefix) && name.get(scoped_prefix.len()) == Some(&92) {
                    paths.insert(name[scoped_prefix.len() + 1..].to_vec());
                }
            }
        }
        if paths.len() > MAX_SCOPED {
            return Err(invalid("scoped entry budget exceeded; incomplete"));
        }
    }
    println!("native_projection_degraded={} disappeared_os2={} disappeared_os3={} changed_identity={} names_logged=false",
        projection.degraded(), projection.missing_file_os2, projection.missing_path_os3, projection.changed_identity);
    Ok((paths, reducer.scanned, batches, projection.degraded()))
}
fn persisted_paths(selected: &Path, storage: &Path) -> io::Result<(PathBuf, PathBuf, String)> {
    let root = fs::canonicalize(selected)?;
    let storage = fs::canonicalize(storage)?;
    let prefix = fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-a/run"),
    )?;
    if !root.starts_with(&prefix)
        || !storage.starts_with(&prefix)
        || storage.starts_with(&root)
        || !root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("fixture-"))
    {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,"native persistence requires engineering fixture-* scope and a checkpoint directory outside that scope"));
    }
    let units = raw(&root);
    if units.len() < 7
        || units[..4] != [92, 92, 63, 92]
        || units[5] != 58
        || units[6] != 92
        || units[4] > 127
        || !(units[4] as u8).is_ascii_alphabetic()
    {
        return Err(invalid("drive-backed extended root required"));
    }
    let volume = format!("{}:", units[4] as u8 as char);
    Ok((root, storage.join("inventory.lcusn"), volume))
}
fn snapshot_scope(root: &Path, volume: &Volume, snapshot: &Snapshot) -> io::Result<()> {
    let current = win::identity(root)?;
    if current.object != snapshot.root
        || current.volume_serial != snapshot.checkpoint.volume_serial
        || raw(root) != snapshot.scope
    {
        return Err(invalid(
            "snapshot requires rebuild: scope/root identity changed",
        ));
    }
    snapshot
        .checkpoint
        .validate(volume.serial, &volume.query()?)
}
fn fixed_scope(expected: &Snapshot, candidate: &Snapshot) -> io::Result<()> {
    if expected.root != candidate.root
        || expected.scope != candidate.scope
        || expected.checkpoint.volume_serial != candidate.checkpoint.volume_serial
        || expected.checkpoint.journal_id != candidate.checkpoint.journal_id
    {
        return Err(invalid(
            "snapshot requires rebuild: reconciliation changed fixed source identity",
        ));
    }
    Ok(())
}
struct ExpectedUsnAction {
    object: u128,
    parent: u128,
    name: Vec<u16>,
    mask: u32,
    matched: bool,
}
#[derive(Default)]
struct UsnTrace {
    // Constructed only from the six owned fixture actions (or empty on ordinary
    // recovery). No journal filenames from outside the fixture are retained.
    actions: Vec<ExpectedUsnAction>,
    majors: BTreeSet<u16>,
}
impl UsnTrace {
    fn fixture(
        parent: u128,
        added: u128,
        deleted: u128,
        file: u128,
        dir: u128,
        names: [&str; 6],
    ) -> Self {
        let objects = [added, deleted, file, file, dir, dir];
        let masks = [0x100, 0x200, 0x1000, 0x2000, 0x1000, 0x2000];
        Self {
            actions: (0..6)
                .map(|i| ExpectedUsnAction {
                    object: objects[i],
                    parent,
                    name: names[i].encode_utf16().collect(),
                    mask: masks[i],
                    matched: false,
                })
                .collect(),
            majors: BTreeSet::new(),
        }
    }
    fn observe(&mut self, records: &[Record]) -> io::Result<()> {
        for record in records {
            if !matches!(record.major, 2 | 3) {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "unsupported replay USN version",
                ));
            }
            self.majors.insert(record.major);
            for action in &mut self.actions {
                if record.object == action.object
                    && record.parent == action.parent
                    && record.name == action.name
                    && record.reason & action.mask == action.mask
                {
                    action.matched = true;
                }
            }
        }
        Ok(())
    }
    fn require_complete(&self) -> io::Result<()> {
        let matched = self.actions.iter().filter(|a| a.matched).count();
        println!("required_usn_actions={} matched_usn_actions={} read_record_major_versions={:?} external_names_retained=false", self.actions.len(), matched, self.majors);
        if matched != self.actions.len() {
            return Err(invalid(
                "required owned fixture USN actions missing; snapshot not published",
            ));
        }
        Ok(())
    }
}
fn replay_snapshot(
    root: &Path,
    volume: &Volume,
    mut snapshot: Snapshot,
    trace: &mut UsnTrace,
) -> io::Result<(Snapshot, usize)> {
    let start = Instant::now();
    let mut consumed = 0;
    loop {
        snapshot_scope(root, volume, &snapshot)?;
        let journal = volume.query()?;
        snapshot.checkpoint.validate(volume.serial, &journal)?;
        if snapshot.checkpoint.cursor == journal.next {
            trace.require_complete()?;
            return Ok((snapshot, consumed));
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(invalid(
                "persistent replay budget exceeded; last snapshot preserved",
            ));
        }
        let (next, records) = volume.read(journal.id, snapshot.checkpoint.cursor)?;
        if next <= snapshot.checkpoint.cursor
            || records
                .iter()
                .any(|r| r.usn < snapshot.checkpoint.cursor || r.usn >= next)
        {
            return Err(invalid(
                "non-progressing/invalid journal batch; requires rebuild",
            ));
        }
        consumed += records.len();
        if consumed > 100_000 {
            return Err(invalid(
                "persistent replay record budget; last snapshot preserved",
            ));
        }
        trace.observe(&records)?;
        let checkpoint = Checkpoint {
            cursor: next,
            ..snapshot.checkpoint.clone()
        };
        let next_snapshot = scoped_snapshot(root, checkpoint)?;
        fixed_scope(&snapshot, &next_snapshot)?;
        snapshot_scope(root, volume, &next_snapshot)?;
        snapshot = next_snapshot;
    }
}
fn stable_snapshot(root: &Path, volume: &Volume, snapshot: &Snapshot) -> io::Result<Paths> {
    let candidate = entry_paths(snapshot.root, &snapshot.entries)?;
    let independent = oracle(root)?;
    snapshot_scope(root, volume, snapshot)?;
    let journal = volume.query()?;
    snapshot.checkpoint.validate(volume.serial, &journal)?;
    if candidate != independent || snapshot.checkpoint.cursor != journal.next {
        return Err(invalid(
            "namespace/oracle/cutoff differs; snapshot not published",
        ));
    }
    Ok(candidate)
}
pub fn persist_bootstrap(selected: &Path, storage: &Path) -> io::Result<()> {
    win::diagnostics()?;
    let start = Instant::now();
    let before = win::metrics()?;
    let run = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("probe worktree parent missing"))?
        .join(".scratch/windows-ntfs-stage-a/run");
    let run_pins = win::DirectoryPins::hold(&run)?;
    let (root, disk, spec) = persisted_paths(selected, storage)?;
    let root_pins = win::DirectoryPins::hold(&root)?;
    let storage_pins = win::DirectoryPins::hold(
        disk.parent()
            .ok_or_else(|| invalid("storage parent missing"))?,
    )?;
    if disk.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "bootstrap refuses to replace a pre-existing snapshot",
        ));
    }
    println!("bounded_volume_enumeration_requested={spec}; personal_names_not_logged=true");
    let volume = Volume::open(&spec, 0x80000000)?;
    let journal = volume.query()?;
    let checkpoint = Checkpoint {
        volume_serial: volume.serial,
        journal_id: journal.id,
        cursor: journal.next,
    };
    let starting_root = win::identity(&root)?;
    let scope = root.join(format!("native-building-{}", std::process::id()));
    fs::create_dir(&scope)?;
    fs::write(scope.join("delete.txt"), b"delete")?;
    fs::write(scope.join("old.txt"), b"rename")?;
    fs::create_dir(scope.join("old-dir"))?;
    fs::write(scope.join("old-dir/child.txt"), b"child")?;
    fs::write(scope.join("link-outer.txt"), b"linked")?;
    fs::hard_link(
        scope.join("link-outer.txt"),
        scope.join("old-dir/link-inner.txt"),
    )?;
    fs::write(scope.join("中文-é-😀.txt"), b"unicode")?;
    fs::write(
        scope.join(PathBuf::from(OsString::from_wide(&[
            114, 97, 119, 45, 0xd800, 46, 116, 120, 116,
        ]))),
        b"raw",
    )?;
    let mut long = scope.clone();
    while raw(&long).len() < 280 {
        long.push("long-component-0123456789");
        fs::create_dir(&long)?;
    }
    fs::write(long.join("long.txt"), b"long")?;
    fs::write(scope.join("stream.txt"), b"base")?;
    fs::write(scope.join("stream.txt:native-probe"), b"ads")?;
    // Collect seeds before our writer starts. Native MFT enumeration still
    // overlaps add/delete/rename; scope discovery itself need not race them.
    let (seed_root, seed_entries) = inventory(&root)?;
    let action_parent = win::identity(&scope)?.object;
    let deleted_object = win::identity(&scope.join("delete.txt"))?.object;
    let renamed_file = win::identity(&scope.join("old.txt"))?.object;
    let renamed_dir = win::identity(&scope.join("old-dir"))?.object;
    let changing = scope.clone();
    let worker = thread::spawn(move || -> io::Result<()> {
        thread::sleep(Duration::from_millis(5));
        fs::write(changing.join("added.txt"), b"added")?;
        fs::remove_file(changing.join("delete.txt"))?;
        fs::rename(changing.join("old.txt"), changing.join("new.txt"))?;
        fs::rename(changing.join("old-dir"), changing.join("new-dir"))?;
        Ok(())
    });
    let result = bootstrap(&root, &volume, seed_root, seed_entries);
    worker
        .join()
        .map_err(|_| invalid("bootstrap writer panicked"))??;
    let (mft, objects, batches, projection_degraded) = result?;
    let snapshot = scoped_snapshot(&root, checkpoint)?;
    if snapshot.root != starting_root.object
        || snapshot.checkpoint.volume_serial != starting_root.volume_serial
    {
        return Err(invalid("bootstrap source identity changed; no publication"));
    }
    let scoped = entry_paths(snapshot.root, &snapshot.entries)?;
    let projection_complete = !projection_degraded && scoped == mft;
    println!(
        "MFT_projection_missing={} MFT_projection_extra={} mft_projection_complete={projection_complete} namespace_supplementation=true",
        scoped.difference(&mft).count(),
        mft.difference(&scoped).count()
    );
    let added_identity = win::identity(&scope.join("added.txt"))?;
    if added_identity.volume_serial != volume.serial
        || win::identity(&scope)?.object != action_parent
    {
        return Err(invalid(
            "owned bootstrap fixture identity changed; snapshot not published",
        ));
    }
    let mut trace = UsnTrace::fixture(
        action_parent,
        added_identity.object,
        deleted_object,
        renamed_file,
        renamed_dir,
        [
            "added.txt",
            "delete.txt",
            "old.txt",
            "new.txt",
            "old-dir",
            "new-dir",
        ],
    );
    let (snapshot, records) = replay_snapshot(&root, &volume, snapshot, &mut trace)?;
    let paths = stable_snapshot(&root, &volume, &snapshot)?;
    let links = win::hardlink_names(&scope.join("link-outer.txt"))?;
    if links.len() != 2
        || !paths.contains(&raw(&PathBuf::from(format!(
            "native-building-{}",
            std::process::id()
        ))
        .join("new-dir")
        .join("link-inner.txt")))
    {
        return Err(invalid("hardlink completeness check failed"));
    }
    snapshot.save_new(&disk)?;
    drop(volume);
    drop((storage_pins, root_pins, run_pins));
    let after = win::metrics()?;
    println!("native_complete=true backend=hybrid phase=bootstrap mft_projection_complete={projection_complete} inventory_and_cursor_atomic=true entries={} scanned_objects={} batches={} replay_records={} journal_id={:#x} cursor={} elapsed_ms={} handles_before={} handles_after={} total_working_set_bytes={} private_commit_bytes={} kernel_bytes=unmeasured",paths.len(),objects,batches,records,snapshot.checkpoint.journal_id,snapshot.checkpoint.cursor,start.elapsed().as_millis(),before.handles,after.handles,after.working_set,after.private_usage);
    Ok(())
}
pub fn persist_recover(selected: &Path, storage: &Path) -> io::Result<()> {
    persist_recover_impl(selected, storage, false)
}
pub fn persist_recover_verified(selected: &Path, storage: &Path) -> io::Result<()> {
    persist_recover_impl(selected, storage, true)
}
fn persist_recover_impl(selected: &Path, storage: &Path, verify_fixture: bool) -> io::Result<()> {
    win::diagnostics()?;
    let start = Instant::now();
    let before = win::metrics()?;
    let run = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("probe worktree parent missing"))?
        .join(".scratch/windows-ntfs-stage-a/run");
    let run_pins = win::DirectoryPins::hold(&run)?;
    let (root, disk, spec) = persisted_paths(selected, storage)?;
    let root_pins = win::DirectoryPins::hold(&root)?;
    let storage_pins = win::DirectoryPins::hold(
        disk.parent()
            .ok_or_else(|| invalid("storage parent missing"))?,
    )?;
    let saved = Snapshot::load(&disk)?;
    let mut trace = if verify_fixture {
        let old_object = |name: &str| -> io::Result<u128> {
            let raw_name: Vec<u16> = name.encode_utf16().collect();
            saved
                .entries
                .keys()
                .find(|entry| entry.parent == saved.root && entry.name == raw_name)
                .map(|entry| entry.object)
                .ok_or_else(|| {
                    invalid(
                        "required saved offline fixture identity missing; snapshot not published",
                    )
                })
        };
        let added = win::identity(&root.join("offline-added.txt"))?;
        if added.volume_serial != saved.checkpoint.volume_serial {
            return Err(invalid(
                "owned offline fixture volume changed; snapshot not published",
            ));
        }
        UsnTrace::fixture(
            saved.root,
            added.object,
            old_object("offline-delete.txt")?,
            old_object("offline-rename-before.txt")?,
            old_object("offline-dir-before")?,
            [
                "offline-added.txt",
                "offline-delete.txt",
                "offline-rename-before.txt",
                "offline-rename-after.txt",
                "offline-dir-before",
                "offline-dir-after",
            ],
        )
    } else {
        UsnTrace::default()
    };
    // No MFT enumeration on resume. Journal/provenance failure leaves disk intact.
    let volume = Volume::open(&spec, 0x80000000)?;
    snapshot_scope(&root, &volume, &saved)?;
    let previous = saved.checkpoint.cursor;
    let (snapshot, records) = replay_snapshot(&root, &volume, saved, &mut trace)?;
    let paths = stable_snapshot(&root, &volume, &snapshot)?;
    snapshot.save(&disk)?;
    drop(volume);
    drop((storage_pins, root_pins, run_pins));
    let after = win::metrics()?;
    println!("native_complete=true backend=hybrid phase=recover separate_process_inventory=true entries={} replay_records={} previous_cursor={} cursor={} elapsed_ms={} handles_before={} handles_after={} total_working_set_bytes={} private_commit_bytes={} kernel_bytes=unmeasured",paths.len(),records,previous,snapshot.checkpoint.cursor,start.elapsed().as_millis(),before.handles,after.handles,after.working_set,after.private_usage);
    Ok(())
}

#[cfg(test)]
mod persistent_tests {
    use super::*;
    fn injected_action_trace() -> UsnTrace {
        UsnTrace::fixture(
            10,
            11,
            12,
            13,
            14,
            ["added", "deleted", "old", "new", "old-dir", "new-dir"],
        )
    }
    fn action_records(trace: &UsnTrace) -> Vec<Record> {
        trace
            .actions
            .iter()
            .map(|action| Record {
                object: action.object,
                parent: action.parent,
                name: action.name.clone(),
                reason: action.mask | 0x80000000,
                attributes: 0,
                major: 2,
                usn: 1,
            })
            .collect()
    }
    #[test]
    fn usn_trace_injected_all_six_actions_require_identity_name_and_reason() {
        let mut trace = injected_action_trace();
        let mut records = action_records(&trace);
        records[5].major = 3;
        trace.observe(&records[..3]).unwrap();
        assert!(trace.require_complete().is_err());
        trace.observe(&records[3..]).unwrap();
        trace.require_complete().unwrap();
        assert_eq!(trace.majors, BTreeSet::from([2, 3]));
        assert_eq!(trace.actions.len(), 6);
    }
    #[test]
    fn usn_trace_injected_wrong_object_parent_raw_name_or_mask_cannot_publish() {
        for mismatch in 0..4 {
            let mut trace = injected_action_trace();
            let mut records = action_records(&trace);
            match mismatch {
                0 => records[3].object += 1,
                1 => records[3].parent += 1,
                2 => records[3].name = vec![0xd800],
                _ => records[3].reason = 0x1000,
            }
            trace.observe(&records).unwrap();
            assert_eq!(trace.actions.iter().filter(|a| a.matched).count(), 5);
            assert!(trace.require_complete().is_err());
        }
    }
    #[test]
    fn usn_trace_injected_unknown_version_is_fatal_and_empty_trace_is_optional() {
        let mut trace = injected_action_trace();
        let mut records = action_records(&trace);
        records[0].major = 4;
        assert_eq!(
            trace.observe(&records).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert!(!trace.actions.iter().any(|a| a.matched));
        assert!(UsnTrace::default().require_complete().is_ok());
    }
    fn synthetic_record(object: u128, parent: u128, name: &[u16], attributes: u32) -> Record {
        Record {
            object,
            parent,
            name: name.to_vec(),
            attributes,
            major: 2,
            usn: 1,
            reason: 0,
        }
    }
    fn seeded_reducer() -> ScopedMftReducer {
        let dir = synthetic_record(2, 1, &[100], 0x10);
        let reparse = synthetic_record(3, 1, &[114], 0x410);
        let entries = BTreeMap::from([
            (dir.entry_id(), dir.attributes),
            (reparse.entry_id(), reparse.attributes),
        ]);
        ScopedMftReducer::seeded(1, &entries).unwrap()
    }
    #[test]
    fn mft_reducer_streamed_scope_filter_retains_only_seed_parents_and_directories() {
        let mut reducer = seeded_reducer();
        // Out-of-scope representative names (including an external hardlink)
        // remain absent; namespace supplementation must supply any scope links.
        reducer.push(synthetic_record(10, 999, &[120], 0)).unwrap();
        reducer.push(synthetic_record(11, 3, &[120], 0)).unwrap();
        reducer.push(synthetic_record(12, 1, &[97], 0)).unwrap();
        reducer.push(synthetic_record(13, 2, &[98], 0)).unwrap();
        assert_eq!(reducer.scanned, 4);
        assert_eq!(reducer.map.keys().copied().collect::<Vec<_>>(), [2, 12, 13]);
        assert_eq!(reducer.seen_native.len(), 2);
        assert!(!reducer.directories.contains(&3));
    }
    #[test]
    fn mft_reducer_native_directory_overwrites_seed_without_false_duplicate_and_accounts_names() {
        let mut reducer = seeded_reducer();
        reducer
            .push(synthetic_record(2, 999, &[110, 101, 119], 0x10))
            .unwrap();
        assert_eq!(reducer.map[&2].parent, 999);
        assert_eq!(reducer.name_bytes, 6);
        assert_eq!(reducer.map.len(), 1);
        assert_eq!(reducer.seen_native.len(), 1);
        assert!(reducer.push(synthetic_record(2, 1, &[100], 0x10)).is_err());
        // No claim of whole-volume duplicate detection: discarded identities
        // may repeat, without any additional retained bookkeeping.
        reducer.push(synthetic_record(88, 999, &[120], 0)).unwrap();
        reducer.push(synthetic_record(88, 999, &[120], 0)).unwrap();
        assert_eq!(reducer.seen_native.len(), 1);
    }
    #[test]
    fn mft_reducer_injected_small_streaming_budgets_fail_without_publishing_partial_state() {
        let mut reducer = seeded_reducer();
        reducer.scanned_limit = 1;
        reducer.push(synthetic_record(9, 999, &[120], 0)).unwrap();
        assert!(reducer.push(synthetic_record(10, 999, &[120], 0)).is_err());
        assert_eq!(reducer.scanned, 2);
        let mut reducer = seeded_reducer();
        reducer.retained_limit = 1;
        assert!(reducer.push(synthetic_record(9, 1, &[120], 0)).is_err());
        assert_eq!(reducer.map.len(), 1);
        assert!(reducer.seen_native.is_empty());
        let mut reducer = seeded_reducer();
        reducer.name_limit = 2;
        assert!(reducer
            .push(synthetic_record(2, 1, &[120, 121], 0x10))
            .is_err());
        assert_eq!(reducer.name_bytes, 2);
        assert_eq!(reducer.map[&2].name, [100]);
    }
    #[test]
    fn mft_reducer_unsupported_version_is_rejected_even_outside_scope() {
        let mut reducer = seeded_reducer();
        let mut record = synthetic_record(99, 999, &[120], 0);
        record.major = 3;
        assert_eq!(
            reducer.push(record).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(reducer.scanned, 1);
        assert!(reducer.seen_native.is_empty());
    }
    #[test]
    fn projection_injected_access_denied_is_fatal_and_stale_codes_are_counted() {
        let mut status = ProjectionStatus::default();
        assert!(!status.stale_error(io::Error::from_raw_os_error(2)).unwrap());
        assert!(!status.stale_error(io::Error::from_raw_os_error(3)).unwrap());
        assert_eq!(
            status
                .stale_error(io::Error::from_raw_os_error(5))
                .unwrap_err()
                .raw_os_error(),
            Some(5)
        );
        assert_eq!(status.missing_file_os2, 1);
        assert_eq!(status.missing_path_os3, 1);
        assert!(status.degraded());
    }
    #[test]
    fn projection_real_engineering_deleted_renamed_and_reused_paths_are_skipped() {
        let run =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-a/run");
        fs::create_dir_all(&run).unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = run.join(format!("projection-unit-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let mut status = ProjectionStatus::default();
        let removed = root.join("removed");
        fs::write(&removed, b"a").unwrap();
        let id = win::identity(&removed).unwrap();
        assert!(status
            .current(&removed, id.object, id.volume_serial)
            .unwrap());
        fs::remove_file(&removed).unwrap();
        assert!(!status
            .current(&removed, id.object, id.volume_serial)
            .unwrap());
        let source = root.join("source");
        let moved = root.join("moved");
        fs::write(&source, b"original").unwrap();
        let original = win::identity(&source).unwrap();
        fs::rename(&source, &moved).unwrap();
        assert!(!status
            .current(&source, original.object, original.volume_serial)
            .unwrap());
        // Keep the original object alive at another name, then reuse its old
        // path for a distinct object. No FRN reuse is simulated as a kernel fact.
        fs::write(&source, b"replacement").unwrap();
        assert!(!status
            .current(&source, original.object, original.volume_serial)
            .unwrap());
        assert!(status
            .current(&moved, original.object, original.volume_serial)
            .unwrap());
        let dir = root.join("old-dir");
        let new_dir = root.join("new-dir");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("child"), b"child").unwrap();
        let child = win::identity(&dir.join("child")).unwrap();
        fs::rename(&dir, &new_dir).unwrap();
        assert!(!status
            .current(&dir.join("child"), child.object, child.volume_serial)
            .unwrap());
        assert_eq!(status.missing_file_os2, 2);
        assert_eq!(status.missing_path_os3, 1);
        assert_eq!(status.changed_identity, 1);
        assert!(status.degraded());
        fs::remove_file(source).unwrap();
        fs::remove_file(moved).unwrap();
        fs::remove_file(new_dir.join("child")).unwrap();
        fs::remove_dir(new_dir).unwrap();
        fs::remove_dir(root).unwrap();
    }
    #[test]
    fn child_snapshot_reader() {
        let Some(path) = std::env::var_os("LOCI_SNAPSHOT_TEST_INPUT") else {
            return;
        };
        let disk = fs::canonicalize(PathBuf::from(path)).unwrap();
        let engineering = fs::canonicalize(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-a/run"),
        )
        .unwrap();
        assert!(disk.starts_with(engineering));
        let saved = Snapshot::load(&disk).unwrap();
        let root = PathBuf::from(OsString::from_wide(&saved.scope));
        assert_eq!(saved.root, win::identity(&root).unwrap().object);
        assert_eq!(
            entry_paths(saved.root, &saved.entries).unwrap(),
            oracle(&root).unwrap()
        );
    }
    #[test]
    fn real_inventory_file_survives_process_exit_and_failed_recovery() {
        let run =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-a/run");
        fs::create_dir_all(&run).unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = run.join(format!("persistent-unit-{}-{nonce}", std::process::id()));
        fs::create_dir(&base).unwrap();
        let root = base.join("fixture-snapshot");
        let store = base.join("store");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&store).unwrap();
        let root = fs::canonicalize(root).unwrap();
        fs::create_dir(root.join("child")).unwrap();
        fs::write(root.join("a.txt"), b"a").unwrap();
        fs::hard_link(root.join("a.txt"), root.join("child/link.txt")).unwrap();
        fs::write(
            root.join(OsString::from_wide(&[114, 0xd800, 46, 116, 120, 116])),
            b"raw",
        )
        .unwrap();
        // Real file/process I/O, explicitly injected journal provenance. This
        // does NOT demonstrate native USN continuity or offline journal replay.
        let snapshot = scoped_snapshot(
            &root,
            Checkpoint {
                volume_serial: win::identity(&root).unwrap().volume_serial,
                journal_id: u64::MAX,
                cursor: 0,
            },
        )
        .unwrap();
        let disk = store.join("inventory.lcusn");
        snapshot.save_new(&disk).unwrap();
        let original = fs::read(&disk).unwrap();
        let overwrite_error = snapshot.save_new(&disk).unwrap_err();
        assert!(overwrite_error.raw_os_error().is_some());
        println!(
            "real_bootstrap_overwrite_refused=true os_code={:?}",
            overwrite_error.raw_os_error()
        );
        assert_eq!(fs::read(&disk).unwrap(), original);
        {
            let storage_pins = win::DirectoryPins::hold(&store).unwrap();
            let root_pins = win::DirectoryPins::hold(&root).unwrap();
            let store_error = fs::rename(&store, base.join("store-moved")).unwrap_err();
            let root_error = fs::rename(&root, base.join("fixture-moved")).unwrap_err();
            assert!(store_error.raw_os_error().is_some() && root_error.raw_os_error().is_some());
            snapshot.save(&disk).unwrap();
            assert_eq!(fs::read(&disk).unwrap(), original);
            println!("real_directory_swap_refused=true storage_os_code={:?} root_os_code={:?} child_file_atomic_replace=true",store_error.raw_os_error(),root_error.raw_os_error());
            drop((storage_pins, root_pins));
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sync::persistent_tests::child_snapshot_reader",
                "--nocapture",
            ])
            .env("LOCI_SNAPSHOT_TEST_INPUT", &disk)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(Snapshot::load(&disk).unwrap(), snapshot);
        fs::write(root.join("offline-added.txt"), b"offline").unwrap();
        assert_ne!(
            entry_paths(snapshot.root, &snapshot.entries).unwrap(),
            oracle(&root).unwrap()
        );
        let recovery_error = persist_recover(&root, &store).unwrap_err();
        println!(
            "failed_recovery_kind={:?} failed_recovery_os_code={:?}",
            recovery_error.kind(),
            recovery_error.raw_os_error()
        );
        assert_eq!(
            fs::read(&disk).unwrap(),
            original,
            "unreadable or incompatible journal must preserve the last inventory+cursor"
        );
        println!("real_snapshot_cross_process=true journal_provenance=injected native_usn_replay=unverified");
        // Keep this uniquely owned small test case in the ignored engineering root.
    }
    #[test]
    fn injected_reconciliation_cannot_adopt_replaced_source_identity() {
        let original = Snapshot {
            root: 10,
            scope: vec![68, 58, 92, 116],
            entries: BTreeMap::new(),
            checkpoint: Checkpoint {
                volume_serial: 1,
                journal_id: 2,
                cursor: 3,
            },
        };
        let mut candidate = original.clone();
        candidate.root = 11;
        assert!(fixed_scope(&original, &candidate).is_err());
        candidate = original.clone();
        candidate.scope.push(120);
        assert!(fixed_scope(&original, &candidate).is_err());
        candidate = original.clone();
        candidate.checkpoint.volume_serial += 1;
        assert!(fixed_scope(&original, &candidate).is_err());
        candidate = original.clone();
        candidate.checkpoint.journal_id += 1;
        assert!(fixed_scope(&original, &candidate).is_err());
        candidate = original.clone();
        candidate.checkpoint.cursor += 1;
        assert!(fixed_scope(&original, &candidate).is_ok());
    }
}
