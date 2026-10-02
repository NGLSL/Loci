//! Small, retained real filesystem fixture. Scan + watch evidence is explicitly
//! separate from USN build/replay evidence, which needs volume read permission.
use crate::{fallback::Watch, win};
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs, io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::MetadataExt,
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, s)
}
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().collect()
}
fn scan(root: &Path) -> io::Result<BTreeSet<Vec<u16>>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeSet<Vec<u16>>) -> io::Result<()> {
        for e in fs::read_dir(dir)? {
            let e = e?;
            let p = e.path();
            let m = fs::symlink_metadata(&p)?;
            out.insert(wide(
                p.strip_prefix(root)
                    .map_err(|_| invalid("scan escaped root"))?,
            ));
            // FILE_ATTRIBUTE_REPARSE_POINT: never traverse an alternate target.
            if m.is_dir() && m.file_attributes() & 0x400 == 0 {
                walk(root, &p, out)?;
            }
        }
        Ok(())
    }
    let mut out = BTreeSet::new();
    walk(root, root, &mut out)?;
    Ok(out)
}
fn add_expected(expected: &mut BTreeSet<Vec<u16>>, name: impl AsRef<Path>) {
    let normalized: PathBuf = name.as_ref().components().collect();
    expected.insert(wide(&normalized));
}
pub fn run(base: &Path) -> io::Result<()> {
    win::diagnostics()?;
    if !base.is_absolute() || !base.is_dir() {
        return Err(invalid(
            "fixture base must be an existing absolute engineering test root",
        ));
    }
    let base = fs::canonicalize(base)?;
    let engineering = fs::canonicalize(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-a/run"),
    )?;
    if !base.starts_with(&engineering) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "fixture base outside this probe's engineering run directory",
        ));
    }
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let ordinary = base.join(format!("fixture-{}-{suffix}", std::process::id()));
    fs::create_dir(&ordinary)?;
    let root = fs::canonicalize(&ordinary)?; // Windows canonical form supplies \\?\.
    println!("fixture: retained unique synthetic root; all writes below supplied base; no cleanup");
    let start = Instant::now();
    let before = win::metrics()?;
    fs::write(root.join("delete.txt"), b"delete")?;
    fs::write(root.join("old.txt"), b"rename")?;
    fs::create_dir(root.join("old-dir"))?;
    fs::write(root.join("old-dir/child.txt"), b"child")?;
    let initial = scan(&root)?;
    let mut expected = BTreeSet::new();
    for name in ["added.txt", "new.txt", "new-dir", "new-dir/child.txt"] {
        add_expected(&mut expected, name);
    }
    let mut watch = Watch::open(&root)?;
    let changing = root.clone();
    let worker = thread::spawn(move || -> io::Result<()> {
        thread::sleep(Duration::from_millis(20));
        fs::write(changing.join("added.txt"), b"add")?;
        fs::remove_file(changing.join("delete.txt"))?;
        fs::rename(changing.join("old.txt"), changing.join("new.txt"))?;
        fs::rename(changing.join("old-dir"), changing.join("new-dir"))?;
        Ok(())
    });
    let mut observations = BTreeSet::new();
    let mut concurrent_scans = 0;
    while !worker.is_finished() {
        watch.drain()?;
        for r in &watch.records {
            observations.insert((r.action, r.name.clone()));
        }
        // A concurrent traversal may see a rename between directory read/open.
        match scan(&root) {
            Ok(_) => concurrent_scans += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        thread::sleep(Duration::from_millis(2));
    }
    worker
        .join()
        .map_err(|_| invalid("fixture mutator panicked"))??;
    let deadline = Instant::now() + Duration::from_secs(2);
    let required = [
        (1, "added.txt"),
        (2, "delete.txt"),
        (4, "old.txt"),
        (5, "new.txt"),
        (4, "old-dir"),
        (5, "new-dir"),
    ];
    while Instant::now() < deadline {
        watch.drain()?;
        for r in &watch.records {
            observations.insert((r.action, r.name.clone()));
        }
        if required
            .iter()
            .all(|(a, n)| observations.contains(&(*a, wide(Path::new(n)))))
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    if !required
        .iter()
        .all(|(a, n)| observations.contains(&(*a, wide(Path::new(n)))))
    {
        return Err(invalid(
            "fallback watch missed a required real mutation record",
        ));
    }
    let cutoff = scan(&root)?;
    if cutoff != expected || scan(&root)? != cutoff || initial == cutoff {
        return Err(invalid(
            "full raw UTF16 path set differs at stable fixture cutoff",
        ));
    }
    println!(
        "fallback_correctness=PASS initial_entries={} final_entries={} concurrent_scans={} real_watch_records={} stable_full_path_set=true (NOT USN replay)",
        initial.len(),
        cutoff.len(),
        concurrent_scans,
        observations.len()
    );
    watch.stop()?;
    drop(watch);
    fs::write(root.join("hard-a.txt"), b"hardlink")?;
    fs::hard_link(root.join("hard-a.txt"), root.join("hard-b.txt"))?;
    fs::hard_link(
        root.join("hard-a.txt"),
        root.join("new-dir/subtree-link.txt"),
    )?;
    let a = win::identity(&root.join("hard-a.txt"))?;
    let b = win::identity(&root.join("hard-b.txt"))?;
    if a.volume_serial != b.volume_serial || a.object != b.object || a.links < 2 {
        return Err(invalid("hard links did not share native identity"));
    }
    let links = win::hardlink_names(&root.join("hard-a.txt"))?;
    let volume_relative = |name: &str| -> io::Result<Vec<u16>> {
        let w = wide(&root.join(name));
        // This fixture accepts drive-backed NTFS roots only, not UNC roots.
        if w.len() < 7 || w[..4] != [92, 92, 63, 92] || w[5] != 58 {
            return Err(invalid(
                "expected extended drive path for hardlink comparison",
            ));
        }
        Ok(w[6..].to_vec())
    };
    if links
        != BTreeSet::from([
            volume_relative("hard-a.txt")?,
            volume_relative("hard-b.txt")?,
            volume_relative("new-dir/subtree-link.txt")?,
        ])
    {
        return Err(invalid(
            "hardlink API failed complete volume-relative name set comparison",
        ));
    }
    for name in ["hard-a.txt", "hard-b.txt", "new-dir/subtree-link.txt"] {
        add_expected(&mut expected, name);
    }
    println!(
        "hardlink=PASS shared_volume_object=true native_link_count={} enumerated_all_names={}",
        a.links,
        links.len()
    );
    let unicode = PathBuf::from("中文-é-😀.txt");
    fs::write(root.join(&unicode), b"unicode")?;
    add_expected(&mut expected, &unicode);
    let raw = PathBuf::from(OsString::from_wide(&[
        0x72, 0x61, 0x77, 0x2d, 0xd800, 0x2e, 0x74, 0x78, 0x74,
    ]));
    fs::write(root.join(&raw), b"raw")?;
    add_expected(&mut expected, &raw);
    let mut relative = PathBuf::new();
    while root.join(&relative).as_os_str().encode_wide().count() < 280 {
        relative.push("long-component-0123456789");
        fs::create_dir(root.join(&relative))?;
        add_expected(&mut expected, &relative);
    }
    let long = relative.join("long-file.txt");
    fs::write(root.join(&long), b"long")?;
    add_expected(&mut expected, &long);
    fs::write(root.join("hard-a.txt:probe-stream"), b"ads")?;
    if fs::read(root.join("hard-a.txt:probe-stream"))? != b"ads" {
        return Err(invalid("ADS content mismatch"));
    }
    let reparse = root.join("fixture-reparse");
    match std::os::windows::fs::symlink_dir(root.join("new-dir"), &reparse) {
        Ok(()) => {
            add_expected(&mut expected, "fixture-reparse");
            println!("reparse=PASS symlink entry retained; traversal does not follow target");
        }
        Err(e) => println!(
            "reparse=UNVERIFIED symlink creation denied/unsupported os_code={:?}",
            e.raw_os_error()
        ),
    }
    if scan(&root)? != expected {
        return Err(invalid(
            "complete Unicode/raw UTF16/long/hardlink/ADS path set mismatch",
        ));
    }
    crate::sync::scoped_check(&root, &expected)?;
    let subtree_expected = BTreeSet::from([
        wide(Path::new("child.txt")),
        wide(Path::new("subtree-link.txt")),
    ]);
    crate::sync::scoped_check(&root.join("new-dir"), &subtree_expected)?;
    println!("cross_scope_hardlink=PASS complete identity namespace includes internal link whose object also has names outside selected subtree; MFT representative-name choice=UNVERIFIED");
    println!(
        "names=PASS unicode=true unpaired_surrogate_raw_utf16=true extended_long_path_units={} ADS_search_entries=0 complete_set_entries={}; case_sensitive_directory=UNVERIFIED identity_reuse=UNVERIFIED",
        root.join(&long).as_os_str().encode_wide().count(),
        expected.len()
    );
    let handles_before = win::metrics()?.handles;
    for _ in 0..20 {
        let mut w = Watch::open(&root)?;
        w.stop()?;
        w.stop()?;
        drop(w);
    }
    let after = win::metrics()?;
    if after.handles != handles_before {
        return Err(invalid(
            "native watch cancel/drop handle count did not return to baseline",
        ));
    }
    println!(
        "native_lifecycle=PASS idle_cancel_cycles=20 handles_before={} handles_after={} elapsed_ms={} total_working_set_before={} total_working_set_after={} private_commit_before={} private_commit_after={} peak_total_working_set={}; user_space_64KiB_buffer_per_watch=true kernel_memory=UNMEASURED private_working_set=UNMEASURED",
        handles_before,
        after.handles,
        start.elapsed().as_millis(),
        before.working_set,
        after.working_set,
        before.private_usage,
        after.private_usage,
        after.peak_working_set
    );
    Ok(())
}
