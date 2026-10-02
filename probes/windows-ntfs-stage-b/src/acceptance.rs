//! Real engineering fixtures and an independent path-only oracle.
use crate::{backend::Backend, model::EntryId, store::Snapshot, win};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs, io,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

fn fail(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn write_new(path: &Path) -> io::Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(b"loci-stage-b engineering fixture\n")?;
    Ok(())
}
fn raw(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().collect()
}
fn rename_pinned(from: &Path, to: &Path) -> io::Result<()> {
    let began = Instant::now();
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error)
                if error.raw_os_error() == Some(32) && began.elapsed() < Duration::from_secs(2) =>
            {
                thread::sleep(Duration::from_millis(1))
            }
            Err(error) => return Err(error),
        }
    }
}
fn checked_fixture<'a>(
    root: &'a Path,
    storage: &'a Path,
    count: usize,
) -> io::Result<(win::DirectoryPins, win::DirectoryPins)> {
    if count != 1000 && count != 10000 {
        return Err(fail(
            "only authorized 1k and 10k real fixture profiles are supported",
        ));
    }
    let engineering =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-b/run");
    // Pin literal path ancestors before canonicalization; no redirected writes.
    let _run_pin = win::DirectoryPins::hold(&engineering)?;
    let root_pin = win::DirectoryPins::hold(root)?;
    let storage_pin = win::DirectoryPins::hold(storage)?;
    let run = fs::canonicalize(engineering)?;
    let canonical_root = fs::canonicalize(root)?;
    let canonical_storage = fs::canonicalize(storage)?;
    if !canonical_root.starts_with(&run)
        || !canonical_storage.starts_with(&run)
        || canonical_root == run
        || canonical_storage == run
        || canonical_storage.starts_with(&canonical_root)
        || canonical_root.starts_with(&canonical_storage)
    {
        return Err(fail(
            "fixture and storage must be disjoint descendants of this worktree engineering run",
        ));
    }
    Ok((root_pin, storage_pin))
}

pub fn seed(root: &Path, count: usize) -> io::Result<()> {
    if fs::read_dir(root)?.next().is_some() {
        return Err(fail("fixture must initially be empty"));
    }
    let dirs = if count == 1000 { 20 } else { 100 };
    for d in 0..dirs {
        let dir = root.join(format!("bucket-{d:03}"));
        fs::create_dir(&dir)?;
        for f in 0..count / dirs {
            write_new(&dir.join(format!("file-{f:05}.txt")))?;
        }
    }
    for name in [
        "offline-delete.txt",
        "offline-old.txt",
        "offline-source.txt",
        "live-source.txt",
        "race.txt",
        "unicode-中文-😀.txt",
    ] {
        write_new(&root.join(name))?;
    }
    fs::hard_link(
        root.join("offline-source.txt"),
        root.join("offline-old-link.txt"),
    )?;
    fs::hard_link(root.join("live-source.txt"), root.join("live-old-link.txt"))?;
    for name in [
        "offline-dir",
        "offline-move-out",
        "live-dir",
        "live-move-out",
        "race-dir",
    ] {
        let dir = root.join(name);
        fs::create_dir(&dir)?;
        write_new(&dir.join("child.txt"))?;
    }
    // Raw unpaired UTF-16 name exercises storage/query without lossy conversion.
    let mut name: Vec<u16> = "raw-".encode_utf16().collect();
    name.push(0xd800);
    name.extend(".txt".encode_utf16());
    write_new(&root.join(OsString::from_wide(&name)))?;
    let mut nested = root.join("long");
    fs::create_dir(&nested)?;
    for _ in 0..4 {
        nested = nested.join("long-component-012345678901234567890123456789");
        fs::create_dir(&nested)?;
    }
    write_new(&nested.join("long-file-中文.txt"))?;
    // Named stream is content of an existing object, never a directory entry.
    fs::write(root.join("live-source.txt:stage-b-stream"), b"stream")?;
    println!("fixture_seed actual_regular_dataset_files={count} scenario_entries_additional=true raw_utf16=true unicode=true ads_created=true");
    Ok(())
}

/// Uses read_dir and symlink_metadata; neither backend graph nor matcher.
pub fn oracle(root: &Path) -> io::Result<BTreeSet<Vec<u16>>> {
    use std::os::windows::fs::MetadataExt;
    let mut paths = BTreeSet::new();
    let mut stack = vec![(root.to_path_buf(), PathBuf::new(), 0usize)];
    while let Some((absolute, relative, depth)) = stack.pop() {
        if depth > 64 {
            return Err(fail("oracle depth budget"));
        }
        for item in fs::read_dir(absolute)? {
            let item = item?;
            let rel = relative.join(item.file_name());
            let meta = fs::symlink_metadata(item.path())?;
            if !paths.insert(raw(&rel)) {
                return Err(fail("oracle duplicate path"));
            }
            if paths.len() > 32768 {
                return Err(fail("oracle entry budget"));
            }
            if meta.is_dir() && meta.file_attributes() & 0x400 == 0 {
                stack.push((item.path(), rel, depth + 1));
            }
        }
    }
    Ok(paths)
}

fn verify(backend: &Backend, root: &Path, label: &str) -> io::Result<()> {
    let expected = oracle(root)?;
    let actual = backend.query(&[], 32768)?;
    let returned: BTreeSet<_> = actual.paths.into_iter().collect();
    if expected != returned || actual.total != expected.len() {
        return Err(fail(
            "complete path oracle mismatch; failed candidate must not be published",
        ));
    }
    for query in [
        "bucket-001",
        "child.txt",
        "中文",
        "offline",
        "live",
        ":stage-b-stream",
    ] {
        let q: Vec<u16> = query.encode_utf16().collect();
        let wanted: BTreeSet<_> = expected
            .iter()
            .filter(|path| path.windows(q.len()).any(|window| window == q))
            .cloned()
            .collect();
        let found = backend.query(&q, 32768)?;
        if found.total != wanted.len() || found.paths.into_iter().collect::<BTreeSet<_>>() != wanted
        {
            return Err(fail(
                "literal-query full set differs from independent matcher",
            ));
        }
    }
    let raw_query = [0xd800];
    let raw_result = backend.query(&raw_query, 32768)?;
    if raw_result.total != 1 {
        return Err(fail("raw UTF16 query did not retain unpaired name"));
    }
    println!("correctness label={label} all_paths={} full_set_equal=true query_full_sets_equal=true raw_utf16_query=true",expected.len());
    Ok(())
}

fn emit_stats(label: &str, stats: &crate::backend::Stats) {
    println!("sync_stats label={label} total_records={} scoped_records={} directories_visited={} full_scans={} batches={} elapsed_ms={}",stats.total_records,stats.scoped_records,stats.directories_visited,stats.full_scans,stats.batches,stats.elapsed_ms);
    println!(
        "native_records label={label} major_versions={:?} scoped_reason_mask={:#x}",
        stats.journal_major_versions, stats.scoped_reason_mask
    );
    println!(
        "native_link_queries label={label} count={}",
        stats.native_link_queries
    );
}
fn identity_of(snapshot: &Snapshot, name: &str) -> io::Result<u128> {
    let name: Vec<_> = name.encode_utf16().collect();
    snapshot
        .entries
        .keys()
        .find(|entry| entry.parent == snapshot.root && entry.name == name)
        .map(|entry| entry.object)
        .ok_or_else(|| fail("missing pre-change object in persisted inventory"))
}

pub fn build(root: &Path, storage: &Path, count: usize) -> io::Result<()> {
    let _pins = checked_fixture(root, storage, count)?;
    win::diagnostics()?;
    seed(root, count)?;
    let cancel = AtomicBool::new(false);
    let done = Arc::new(AtomicBool::new(false));
    let worker_done = done.clone();
    let worker_root = root.to_path_buf();
    let writer = thread::spawn(move || -> io::Result<usize> {
        thread::sleep(Duration::from_millis(3));
        let mut rounds = 0;
        while !worker_done.load(Ordering::Acquire) && rounds < 100 {
            if rounds == 0 {
                rename_pinned(
                    &worker_root.join("race-dir"),
                    &worker_root.join("race-dir-renamed"),
                )?;
                rename_pinned(
                    &worker_root.join("race-dir-renamed"),
                    &worker_root.join("race-dir"),
                )?;
            }
            let temporary = worker_root.join("race-added.txt");
            write_new(&temporary)?;
            fs::rename(
                worker_root.join("race.txt"),
                worker_root.join("race-renamed.txt"),
            )?;
            fs::rename(
                worker_root.join("race-renamed.txt"),
                worker_root.join("race.txt"),
            )?;
            fs::remove_file(temporary)?;
            rounds += 1;
            thread::sleep(Duration::from_millis(3));
        }
        Ok(rounds)
    });
    let start = Instant::now();
    let built = Backend::build(root, &storage.join("inventory.lcusn"), &cancel);
    done.store(true, Ordering::Release);
    let rounds = writer
        .join()
        .map_err(|_| fail("fixture writer panicked"))??;
    let mut backend = built?;
    emit_stats("bootstrap", backend.stats());
    let live_memory = win::metrics()?;
    println!("active_resources phase=bootstrap handles={} total_working_set_bytes={} private_commit_bytes={} peak_total_working_set_bytes={} kernel_bytes=unmeasured",live_memory.handles,live_memory.working_set,live_memory.private_usage,live_memory.peak_working_set);
    println!(
        "build_resources dataset_files={count} elapsed_ms={} concurrent_rounds={rounds}",
        start.elapsed().as_millis()
    );
    println!(
        "concurrent_directory_rename_operations={} actual_fixture_changes=true",
        if rounds > 0 { 2 } else { 0 }
    );
    if rounds == 0 {
        return Err(fail(
            "no actual concurrent fixture mutation happened during bootstrap",
        ));
    }
    let settled = backend.sync(&cancel)?;
    emit_stats("concurrent-settle", &settled);
    verify(&backend, root, "bootstrap-concurrent")?;
    let before_paths = oracle(root)?;
    let before_checkpoint = backend.snapshot().checkpoint.clone();
    cancel.store(true, Ordering::Release);
    if backend.sync(&cancel).is_ok() {
        return Err(fail("cancelled sync unexpectedly succeeded"));
    }
    if backend.snapshot().checkpoint != before_checkpoint
        || crate::store::paths(backend.snapshot().root, &backend.snapshot().entries)?
            != before_paths
    {
        return Err(fail("cancelled sync changed last published inventory"));
    }
    cancel.store(false, Ordering::Release);
    backend.sync(&cancel)?;

    // A leaf mutation must reconcile its parent, not the complete 10k scope.
    write_new(&root.join("bucket-000/live-added.txt"))?;
    fs::rename(
        root.join("bucket-000/file-00000.txt"),
        root.join("bucket-000/live-renamed.txt"),
    )?;
    fs::remove_file(root.join("bucket-000/file-00001.txt"))?;
    let leaf = backend.sync(&cancel)?;
    emit_stats("leaf-add-delete-rename", &leaf);
    if leaf.full_scans != 0 || leaf.directories_visited > 3 || leaf.scoped_records == 0 {
        return Err(fail(
            "ordinary leaf update failed the local reconciliation/native-record gate",
        ));
    }
    verify(&backend, root, "leaf-local")?;
    // Rename a populated directory: descendants retain object/parent edges.
    let dir_identity = win::identity(&root.join("bucket-001"))?.object;
    fs::rename(root.join("bucket-001"), root.join("bucket-renamed"))?;
    fs::rename(root.join("live-dir"), root.join("live-dir-renamed"))?;
    fs::remove_file(root.join("live-old-link.txt"))?;
    fs::hard_link(root.join("live-source.txt"), root.join("live-new-link.txt"))?;
    let rename = backend.sync(&cancel)?;
    emit_stats("directory-rename-hardlinks", &rename);
    verify(&backend, root, "directory-rename-hardlinks")?;
    if win::identity(&root.join("bucket-renamed"))?.object != dir_identity
        || win::identity(&root.join("live-new-link.txt"))?.object
            != win::identity(&root.join("live-source.txt"))?.object
    {
        return Err(fail("rename/hardlink object identity mismatch"));
    }
    let links = win::hardlink_names(&root.join("live-source.txt"))?;
    if links.len() != 2 || win::identity(&root.join("live-source.txt"))?.links != 2 {
        return Err(fail(
            "native hardlink enumerator did not report both actual names",
        ));
    }

    let evidence = root
        .parent()
        .ok_or_else(|| fail("missing engineering parent"))?;
    let out = evidence.join(format!("live-out-{count}"));
    let incoming = evidence.join(format!("live-in-{count}"));
    fs::rename(root.join("live-move-out"), &out)?;
    fs::create_dir(&incoming)?;
    write_new(&incoming.join("incoming.txt"))?;
    fs::rename(&incoming, root.join("live-move-in"))?;
    let moves = backend.sync(&cancel)?;
    emit_stats("move-in-out", &moves);
    verify(&backend, root, "move-in-out")?;
    std::os::windows::fs::symlink_dir(&out, root.join("reparse-link"))?;
    let reparse = backend.sync(&cancel)?;
    emit_stats("reparse-entry", &reparse);
    verify(&backend, root, "reparse-entry")?;
    let descendant: Vec<u16> = "reparse-link\\child.txt".encode_utf16().collect();
    if backend.query(&descendant, 32768)?.total != 0 {
        return Err(fail(
            "reparse target was followed outside selected namespace",
        ));
    }
    fs::hard_link(
        root.join("bucket-002/file-00000.txt"),
        root.join("bucket-003/cross-parent-link.txt"),
    )?;
    let cross = backend.sync(&cancel)?;
    emit_stats("cross-parent-link-add", &cross);
    verify(&backend, root, "cross-parent-link-add")?;
    fs::remove_file(root.join("bucket-003/cross-parent-link.txt"))?;
    backend.sync(&cancel)?;
    verify(&backend, root, "cross-parent-link-delete")?;
    let outside_source = out.join("outside-source.txt");
    write_new(&outside_source)?;
    fs::hard_link(
        &outside_source,
        root.join("bucket-004/outside-first-link.txt"),
    )?;
    let first_link = backend.sync(&cancel)?;
    emit_stats("outside-source-first-alias", &first_link);
    verify(&backend, root, "outside-source-first-alias")?;
    if win::identity(&root.join("bucket-004/outside-first-link.txt"))?.object
        != win::identity(&outside_source)?.object
    {
        return Err(fail("outside source first alias lost object identity"));
    }
    fs::remove_file(root.join("bucket-004/outside-first-link.txt"))?;
    backend.sync(&cancel)?;
    verify(&backend, root, "outside-source-alias-delete")?;
    // Noise outside selected scope must not scan scope directories.
    write_new(&out.join("outside-noise.txt"))?;
    let noise = backend.sync(&cancel)?;
    emit_stats("outside-noise", &noise);
    if noise.full_scans != 0 || noise.directories_visited != 0 {
        return Err(fail("outside-scope noise triggered namespace scanning"));
    }
    verify(&backend, root, "outside-noise")?;
    backend.save()?;
    let saved = backend.snapshot().checkpoint.clone();
    backend.stop();
    let stopped = backend.query(&[], 32768)?;
    if format!("{:?}", stopped.status) != "Stopped" {
        return Err(fail("query after stop falsely reports active monitoring"));
    }
    if backend.sync(&cancel).is_ok() {
        return Err(fail("stopped backend allowed sync"));
    }
    drop(backend);
    println!("native_complete=true phase=build fixture_files={count} saved_cursor={} cancellation_preserved=true stopped_query=true cross_process_recover_pending=true",saved.cursor);
    Ok(())
}

pub fn recover(root: &Path, storage: &Path, count: usize) -> io::Result<()> {
    let _pins = checked_fixture(root, storage, count)?;
    win::diagnostics()?;
    let old = Snapshot::load(&storage.join("inventory.lcusn"))?;
    let old_file = identity_of(&old, "offline-old.txt")?;
    let old_directory = identity_of(&old, "offline-dir")?;
    let old_source = identity_of(&old, "offline-source.txt")?;
    for gone in [
        "offline-delete.txt",
        "offline-old.txt",
        "offline-dir",
        "offline-old-link.txt",
        "offline-move-out",
    ] {
        if root.join(gone).exists() {
            return Err(fail(
                "runner did not perform all expected actual offline deletions/renames",
            ));
        }
    }
    for present in [
        "offline-new.txt",
        "offline-dir-new/child.txt",
        "offline-added.txt",
        "offline-new-link.txt",
        "offline-move-in/childin.txt",
    ] {
        if !root.join(present).exists() {
            return Err(fail(
                "runner did not perform all expected actual offline additions/move-ins",
            ));
        }
    }
    if win::identity(&root.join("offline-new.txt"))?.object != old_file
        || win::identity(&root.join("offline-dir-new"))?.object != old_directory
        || win::identity(&root.join("offline-new-link.txt"))?.object != old_source
    {
        return Err(fail("cross-process identity continuity failed"));
    }
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let mut backend = Backend::open(root, &storage.join("inventory.lcusn"), &cancel)?;
    emit_stats("cross-process-offline", backend.stats());
    if backend.stats().scoped_records == 0 || backend.stats().full_scans != 0 {
        return Err(fail(
            "offline recovery did not use actual native scoped USN records/local reconciliation",
        ));
    }
    verify(&backend, root, "cross-process-offline")?;
    let links = win::hardlink_names(&root.join("offline-source.txt"))?;
    if links.len() != 2 {
        return Err(fail("offline hardlink membership mismatch"));
    }
    backend.save()?;
    let checkpoint = backend.snapshot().checkpoint.clone();
    backend.stop();
    drop(backend);
    let loaded = Snapshot::load(&storage.join("inventory.lcusn"))?;
    if loaded.checkpoint != checkpoint {
        return Err(fail("durable namespace and cursor differ"));
    }
    if crate::store::paths(loaded.root, &loaded.entries)? != oracle(root)? {
        return Err(fail(
            "persisted complete path set differs from independent traversal",
        ));
    }
    println!("native_complete=true phase=recover fixture_files={count} elapsed_ms={} old_cursor={} saved_cursor={} hardlink_names=2 full_saved_set_equal=true",start.elapsed().as_millis(),old.checkpoint.cursor,checkpoint.cursor);
    Ok(())
}

// Ordinary-token correctness can test actual namespace + codec without USN.
pub fn scan_fixture(root: &Path, storage: &Path, count: usize) -> io::Result<()> {
    let _pins = checked_fixture(root, storage, count)?;
    seed(root, count)?;
    let root = fs::canonicalize(root)?;
    let root = root.as_path();
    let root_id = win::identity(root)?;
    let mut entries = BTreeMap::new();
    let mut pending = vec![(root.to_path_buf(), root_id.object)];
    while let Some((parent, parent_id)) = pending.pop() {
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let info = win::identity(&entry.path())?;
            entries.insert(
                EntryId {
                    parent: parent_id,
                    object: info.object,
                    name: entry.file_name().encode_wide().collect(),
                },
                info.attributes,
            );
            if info.attributes & (0x10 | 0x400) == 0x10 {
                pending.push((entry.path(), info.object));
            }
        }
    }
    let snapshot = Snapshot {
        checkpoint: crate::checkpoint::Checkpoint {
            volume_serial: root_id.volume_serial,
            journal_id: 0,
            cursor: 0,
        },
        root: root_id.object,
        scope: raw(&fs::canonicalize(root)?),
        volume_guid: "\\\\?\\Volume{00000000-0000-0000-0000-000000000001}\\".into(),
        entries,
    };
    snapshot.save_new(&storage.join("inventory.lcusn"))?;
    let loaded = Snapshot::load(&storage.join("inventory.lcusn"))?;
    if loaded != snapshot || crate::store::paths(loaded.root, &loaded.entries)? != oracle(root)? {
        return Err(fail("ordinary real fixture codec/path oracle mismatch"));
    }
    println!("ordinary_fixture_complete=true dataset_files={count} all_paths={} actual_usn_sync=false simulated_checkpoint=true",loaded.entries.len());
    Ok(())
}
