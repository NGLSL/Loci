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
const MAX_OBJECTS: usize = 200_000;
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
fn bootstrap(root: &Path, volume: &Volume) -> io::Result<(Paths, usize, usize)> {
    let root_id = win::identity(root)?.object;
    let mut map = BTreeMap::new();
    let mut cursor = 0;
    let mut bytes = 0;
    let start = Instant::now();
    let mut batches = 0;
    loop {
        if start.elapsed() > Duration::from_secs(20) {
            return Err(invalid("MFT time budget exceeded; incomplete"));
        }
        match volume.enumerate(cursor, i64::MAX) {
            Err(e) if e.raw_os_error() == Some(38) => break,
            Err(e) => return Err(e),
            Ok((next, records)) => {
                cursor = next;
                batches += 1;
                for r in records {
                    if r.major != 2 {
                        return Err(io::Error::new(io::ErrorKind::Unsupported,"native bootstrap supports V2 NTFS FRNs only; V3 decoding is separately tested"));
                    }
                    bytes += r.name.len() * 2;
                    if bytes > MAX_NAME_BYTES || map.len() >= MAX_OBJECTS {
                        return Err(invalid(
                            "MFT object/name budget exceeded; incomplete, no checkpoint published",
                        ));
                    }
                    if map.insert(r.object, r).is_some() {
                        return Err(invalid("duplicate object in MFT enumeration; rebuild"));
                    }
                }
            }
        }
    }
    // Expand native hardlink names instead of assuming a USN name is every entry.
    let prefix = raw(root);
    if prefix.len() < 7 || prefix[5] != 58 {
        return Err(invalid("extended drive-backed root required"));
    }
    let scoped_prefix = &prefix[6..];
    let mut paths = Paths::new();
    for r in map.values() {
        if r.object == root_id {
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
        // Directory graph uses parents; regular-file names must be expanded.
        if r.attributes & 0x10 != 0 {
            paths.insert(raw(&p));
        } else {
            for name in win::hardlink_names(&root.join(&p))? {
                if name.starts_with(scoped_prefix) && name.get(scoped_prefix.len()) == Some(&92) {
                    paths.insert(name[scoped_prefix.len() + 1..].to_vec());
                }
            }
        }
        if paths.len() > MAX_SCOPED {
            return Err(invalid("scoped entry budget exceeded; incomplete"));
        }
    }
    println!("MFT bootstrap read entire selected volume into bounded transient memory: objects={} batches={} raw_name_bytes={}; no external names logged",map.len(),batches,bytes);
    Ok((paths, map.len(), batches))
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
fn replay_snapshot(
    root: &Path,
    volume: &Volume,
    mut snapshot: Snapshot,
) -> io::Result<(Snapshot, usize)> {
    let start = Instant::now();
    let mut consumed = 0;
    loop {
        snapshot_scope(root, volume, &snapshot)?;
        let journal = volume.query()?;
        snapshot.checkpoint.validate(volume.serial, &journal)?;
        if snapshot.checkpoint.cursor == journal.next {
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
    let changing = scope.clone();
    let worker = thread::spawn(move || -> io::Result<()> {
        thread::sleep(Duration::from_millis(5));
        fs::write(changing.join("added.txt"), b"added")?;
        fs::remove_file(changing.join("delete.txt"))?;
        fs::rename(changing.join("old.txt"), changing.join("new.txt"))?;
        fs::rename(changing.join("old-dir"), changing.join("new-dir"))?;
        Ok(())
    });
    let result = bootstrap(&root, &volume);
    worker
        .join()
        .map_err(|_| invalid("bootstrap writer panicked"))??;
    let (mft, objects, batches) = result?;
    let snapshot = scoped_snapshot(&root, checkpoint)?;
    if snapshot.root != starting_root.object
        || snapshot.checkpoint.volume_serial != starting_root.volume_serial
    {
        return Err(invalid("bootstrap source identity changed; no publication"));
    }
    let scoped = entry_paths(snapshot.root, &snapshot.entries)?;
    let projection_complete = scoped == mft;
    println!(
        "MFT_projection_missing={} MFT_projection_extra={} mft_projection_complete={projection_complete} namespace_supplementation=true",
        scoped.difference(&mft).count(),
        mft.difference(&scoped).count()
    );
    let (snapshot, records) = replay_snapshot(&root, &volume, snapshot)?;
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
    println!("native_complete=true backend=hybrid phase=bootstrap mft_projection_complete={projection_complete} inventory_and_cursor_atomic=true entries={} objects={} batches={} replay_records={} journal_id={:#x} cursor={} elapsed_ms={} handles_before={} handles_after={} total_working_set_bytes={} private_commit_bytes={} kernel_bytes=unmeasured",paths.len(),objects,batches,records,snapshot.checkpoint.journal_id,snapshot.checkpoint.cursor,start.elapsed().as_millis(),before.handles,after.handles,after.working_set,after.private_usage);
    Ok(())
}
pub fn persist_recover(selected: &Path, storage: &Path) -> io::Result<()> {
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
    // No MFT enumeration on resume. Journal/provenance failure leaves disk intact.
    let volume = Volume::open(&spec, 0x80000000)?;
    snapshot_scope(&root, &volume, &saved)?;
    let previous = saved.checkpoint.cursor;
    let (snapshot, records) = replay_snapshot(&root, &volume, saved)?;
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
