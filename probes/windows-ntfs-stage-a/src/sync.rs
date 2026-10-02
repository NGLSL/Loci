//! Opt-in, bounded MFT bootstrap + conservative journal-triggered reconciliation.
//! This is a technical experiment, not a million-entry production index.
use crate::{
    checkpoint::Checkpoint,
    model::{EntryId, Record},
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
fn reconcile(root: &Path) -> io::Result<Paths> {
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
fn replay(
    root: &Path,
    volume: &Volume,
    checkpoint: &Checkpoint,
    mut paths: Paths,
) -> io::Result<(Paths, Checkpoint, usize)> {
    let mut cursor = checkpoint.cursor;
    let start = Instant::now();
    let mut consumed = 0;
    loop {
        let journal = volume.query()?;
        Checkpoint {
            cursor,
            ..*checkpoint
        }
        .validate(volume.serial, &journal)?;
        let cutoff = journal.next;
        if cursor == cutoff {
            return Ok((
                paths,
                Checkpoint {
                    cursor,
                    ..*checkpoint
                },
                consumed,
            ));
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(invalid("journal replay time budget; incomplete"));
        }
        let (next, records) = volume.read(journal.id, cursor)?;
        if next == cursor {
            return Err(invalid(
                "journal made no progress before cutoff; incomplete",
            ));
        }
        if records.iter().any(|r| r.usn < cursor) {
            return Err(invalid("journal record behind cursor"));
        }
        consumed += records.len();
        if consumed > 100_000 {
            return Err(invalid("replay record budget; incomplete"));
        }
        // Deliberately conservative: reconcile the ENTIRE small scoped root on
        // any volume event. This tests continuity, not O(delta) performance.
        paths = reconcile(root)?;
        // The cursor is from before the scan. Repeat through the post-scan
        // boundary until the final scan has no new journal records.
        cursor = next;
    }
}
pub fn verify(selected: &Path) -> io::Result<()> {
    let root = fs::canonicalize(selected)?;
    let prefix = fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-a/run"),
    )?;
    if !root.starts_with(&prefix)
        || !root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with("fixture-"))
    {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,"ntfs writes are permitted only in retained fixture-* roots under this probe's engineering run directory"));
    }
    let units = raw(&root);
    let letter = char::from_u32(units[4] as u32).ok_or_else(|| invalid("invalid drive"))?;
    println!("EXPLICIT OPT-IN: read-only MFT enumeration of volume {letter}:; no user filenames logged; write scope=selected synthetic fixture only");
    let start = Instant::now();
    let before = win::metrics()?;
    let volume = Volume::open(&format!("{letter}:"), 0x80000000)?;
    let journal = volume.query()?;
    let initial = Checkpoint {
        volume_serial: volume.serial,
        journal_id: journal.id,
        cursor: journal.next,
    };
    let scope = root.join(format!("ntfs-concurrent-{}", std::process::id()));
    fs::create_dir(&scope)?;
    fs::write(scope.join("delete.txt"), b"delete")?;
    fs::write(scope.join("old.txt"), b"rename")?;
    fs::create_dir(scope.join("old-dir"))?;
    fs::write(scope.join("old-dir/child.txt"), b"child")?;
    let changing = scope.clone();
    let worker = thread::spawn(move || -> io::Result<()> {
        thread::sleep(Duration::from_millis(5));
        fs::write(changing.join("added.txt"), b"add")?;
        fs::remove_file(changing.join("delete.txt"))?;
        fs::rename(changing.join("old.txt"), changing.join("new.txt"))?;
        fs::rename(changing.join("old-dir"), changing.join("new-dir"))?;
        Ok(())
    });
    let bootstrap_result = bootstrap(&root, &volume);
    worker.join().map_err(|_| invalid("mutator panic"))??;
    let (mft_candidate, _, _) = bootstrap_result?;
    let candidate = reconcile(&root)?;
    println!("MFT projection vs complete identity namespace: missing_paths={} extra_paths={}; scoped namespace supplementation is required for general hardlink coverage, not claimed as pure MFT enumeration correctness",candidate.difference(&mft_candidate).count(),mft_candidate.difference(&candidate).count());
    let (candidate, saved, records) = replay(&root, &volume, &initial, candidate)?;
    if candidate != oracle(&root)? {
        return Err(invalid(
            "MFT/replayed inventory differs from independent complete path set; no checkpoint",
        ));
    }
    let after_check = volume.query()?;
    saved.validate(volume.serial, &after_check)?;
    if after_check.next != saved.cursor {
        return Err(invalid(
            "journal advanced across independent oracle; retry required, no complete state",
        ));
    }
    // Checkpoint is outside the monitored fixture, to avoid self-generated USN.
    let disk = prefix.join(format!("native-{}-checkpoint.bin", std::process::id()));
    saved.save(&disk)?;
    drop(volume);
    fs::write(scope.join("offline-add.txt"), b"offline")?;
    fs::remove_file(scope.join("added.txt"))?;
    fs::rename(scope.join("new.txt"), scope.join("offline-renamed.txt"))?;
    let loaded = Checkpoint::load(&disk)?;
    let volume = Volume::open(&format!("{letter}:"), 0x80000000)?;
    let (recovered, final_checkpoint, offline) = replay(&root, &volume, &loaded, candidate)?;
    let recovery_oracle = oracle(&root)?;
    let after_recovery = volume.query()?;
    final_checkpoint.validate(volume.serial, &after_recovery)?;
    if recovered != recovery_oracle || after_recovery.next != final_checkpoint.cursor {
        return Err(invalid(
            "offline recovery differs at complete stable cutoff",
        ));
    }
    drop(volume);
    let after = win::metrics()?;
    println!("NTFS hybrid_correctness=PASS complete_path_set=true entries={} replay_records={} offline_records={} cursor={} elapsed_ms={} handles_before={} handles_after={} total_working_set_bytes={} private_commit_bytes={}; kernel_bytes=UNMEASURED",recovered.len(),records,offline,final_checkpoint.cursor,start.elapsed().as_millis(),before.handles,after.handles,after.working_set,after.private_usage);
    Ok(())
}
