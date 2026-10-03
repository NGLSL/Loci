use loci_experiment::storage::Snapshot;
use loci_experiment::watch::{Inventory, Kind};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "loci-storage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn inventory(&self, name: &str) -> Inventory {
        Inventory {
            entries: [(PathBuf::from(name), Kind::File)].into(),
            complete: true,
            directories: 1,
            examined: 1,
            errors: vec![],
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn real_root_round_trip_and_replacement_preserve_original_paths() {
    let f = Fixture::new();
    fs::write(f.0.join("中文 空格.txt"), "content").unwrap();
    let db = f.0.with_extension("db");
    Snapshot::new(&f.0, f.inventory("中文 空格.txt"))
        .unwrap()
        .save(&db)
        .unwrap();
    let saved = Snapshot::load(&db, &f.0).unwrap();
    assert_eq!(saved.root, fs::canonicalize(&f.0).unwrap());
    assert_eq!(
        saved.inventory.entries,
        [(PathBuf::from("中文 空格.txt"), Kind::File)].into()
    );
    Snapshot::new(&f.0, f.inventory("replacement.txt"))
        .unwrap()
        .save(&db)
        .unwrap();
    assert_eq!(
        Snapshot::load(&db, &f.0).unwrap().inventory.entries,
        [(PathBuf::from("replacement.txt"), Kind::File)].into()
    );
    fs::remove_file(db).unwrap();
}

#[test]
fn failed_replacement_preserves_old_database_and_cleans_only_owned_temporary() {
    let f = Fixture::new();
    let db = f.0.join("snapshot.db");
    let old = Snapshot::new(&f.0, f.inventory("old.txt")).unwrap();
    old.save(&db).unwrap();
    let foreign = f.0.join("snapshot.db.foreign.tmp");
    fs::write(&foreign, "keep").unwrap();
    let mut invalid = old.clone();
    invalid.inventory.complete = false;
    assert!(invalid.save(&db).is_err());
    assert_eq!(
        Snapshot::load(&db, &f.0).unwrap().inventory.entries,
        old.inventory.entries
    );
    let directory_target = f.0.join("directory-target");
    fs::create_dir(&directory_target).unwrap();
    assert!(old.save(&directory_target).is_err());
    assert_eq!(fs::read_to_string(foreign).unwrap(), "keep");
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 3);
    {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        let locked = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&db)
            .unwrap();
        assert!(Snapshot::new(&f.0, f.inventory("new.txt"))
            .unwrap()
            .save(&db)
            .is_err());
        drop(locked);
        assert_eq!(
            Snapshot::load(&db, &f.0).unwrap().inventory.entries,
            old.inventory.entries
        );
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 3);
    }
}

#[test]
fn unsafe_and_inconsistent_inventory_cannot_overwrite_valid_database() {
    let f = Fixture::new();
    let db = f.0.join("snapshot.db");
    let old = Snapshot::new(&f.0, f.inventory("safe.txt")).unwrap();
    old.save(&db).unwrap();
    for path in [
        "../escape",
        "/absolute",
        ".",
        "file/",
        "./file",
        "missing/child",
        "nul\0name",
    ] {
        let candidate = Snapshot {
            root: old.root.clone(),
            inventory: f.inventory(path),
        };
        assert!(candidate.save(&db).is_err(), "accepted {path:?}");
        assert_eq!(
            Snapshot::load(&db, &f.0).unwrap().inventory.entries,
            old.inventory.entries
        );
    }
    assert!(Snapshot::new(&f.0, f.inventory("file:stream")).is_err());

    let mut conflicting = f.inventory("file");
    conflicting.entries.insert("file/child".into(), Kind::File);
    assert!(Snapshot::new(&f.0, conflicting).is_err());
    let mut excluded = f.inventory("target/child");
    excluded.entries.insert("target".into(), Kind::Directory);
    assert!(Snapshot::new(&f.0, excluded).is_err());
}

fn envelope(root: &std::path::Path, entries: &[(u8, &str)], count: Option<u32>) -> Vec<u8> {
    let mut payload = vec![];
    let root = root.to_str().unwrap().as_bytes();
    payload.extend_from_slice(&(root.len() as u32).to_le_bytes());
    payload.extend_from_slice(root);
    payload.extend_from_slice(&count.unwrap_or(entries.len() as u32).to_le_bytes());
    for (kind, path) in entries {
        payload.push(*kind);
        payload.extend_from_slice(&(path.len() as u32).to_le_bytes());
        payload.extend_from_slice(path.as_bytes());
    }
    let mut bytes = b"LOCISNP1".to_vec();
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend(payload);
    resign(&mut bytes);
    bytes
}
fn resign(bytes: &mut Vec<u8>) {
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    });
    bytes.extend_from_slice(&hash.to_le_bytes());
}

#[test]
fn hostile_database_child() {
    if std::env::var_os("LOCI_STORAGE_HOSTILE_CHILD").is_none() {
        return;
    }
    let f = Fixture::new();
    let root = fs::canonicalize(&f.0).unwrap();
    let db = f.0.join("hostile.db");
    let valid = envelope(&root, &[(0, "safe")], None);
    let mut cases = vec![
        b"LOCIEXP2".to_vec(),
        b"LOCWATCH1".to_vec(),
        vec![],
        envelope(&root, &[], Some(u32::MAX)),
        envelope(&root, &[(2, "bad")], None),
        envelope(&root, &[(0, "same"), (0, "same")], None),
        envelope(&root, &[(0, "../escape")], None),
        envelope(&root, &[(0, "missing/child")], None),
        envelope(&root, &[(1, "target"), (0, "target/child")], None),
        envelope(&root, &[(0, "nul\0path")], None),
    ];
    for end in 0..valid.len() {
        cases.push(valid[..end].to_vec());
    }
    let mut corrupt = valid.clone();
    corrupt[valid.len() - 9] ^= 1;
    cases.push(corrupt);
    for offset in [8, 12, 16, 20] {
        let mut malicious = valid[..valid.len() - 8].to_vec();
        malicious[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        resign(&mut malicious);
        cases.push(malicious);
    }
    let path_offset = 20 + 4 + root.to_str().unwrap().len() + 4 + 1;
    let mut malicious = valid[..valid.len() - 8].to_vec();
    malicious[path_offset..path_offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    resign(&mut malicious);
    cases.push(malicious);
    let mut non_utf8 = valid[..valid.len() - 8].to_vec();
    non_utf8[path_offset + 4] = 255;
    resign(&mut non_utf8);
    cases.push(non_utf8);
    for bytes in cases {
        fs::write(&db, &bytes).unwrap();
        assert!(
            Snapshot::load(&db, &f.0).is_err(),
            "accepted malformed database length {}",
            bytes.len()
        );
    }
    for old in [b"LOCIEXP2".as_slice(), b"LOCWATCH1".as_slice()] {
        fs::write(&db, old).unwrap();
        assert!(Snapshot::load(&db, &f.0)
            .unwrap_err()
            .to_string()
            .contains("rebuild"));
    }
    let file = fs::File::create(&db).unwrap();
    file.set_len(64 * 1024 * 1024).unwrap();
    drop(file);
    assert!(Snapshot::load(&db, &f.0).is_err());
}

#[test]
fn collision_database_child() {
    if std::env::var_os("LOCI_STORAGE_HOSTILE_CHILD").is_none() {
        return;
    }
    let f = Fixture::new();
    let root = fs::canonicalize(&f.0).unwrap();
    let db = f.0.join("collision.db");
    fs::write(&db, envelope(&root, &[(0, "old")], None)).unwrap();
    // This is a fresh process; no Snapshot::save has advanced the name sequence.
    let foreign: Vec<_> = (0..16)
        .map(|i| {
            f.0.join(format!("collision.db.{}.{i}.tmp", std::process::id()))
        })
        .collect();
    for path in &foreign {
        fs::write(path, "foreign").unwrap();
    }
    let error = Snapshot::new(&f.0, f.inventory("new"))
        .unwrap()
        .save(&db)
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        Snapshot::load(&db, &f.0).unwrap().inventory.entries,
        [(PathBuf::from("old"), Kind::File)].into()
    );
    for path in foreign {
        assert_eq!(fs::read_to_string(path).unwrap(), "foreign");
    }
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 17);
}

#[test]
fn corrupted_truncated_oversize_and_untrusted_lengths_fail_in_bounded_process() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    for test in ["hostile_database_child", "collision_database_child"] {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("LOCI_STORAGE_HOSTILE_CHILD", "1")
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("hostile database parse exceeded deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn hostile_destination_is_rejected_without_creating_temporary_files() {
    let f = Fixture::new();
    let snapshot = Snapshot::new(&f.0, f.inventory("safe")).unwrap();
    assert!(snapshot.save(&f.0.join("nul\0name")).is_err());
    {
        let base = f.0.join("base.db");
        fs::write(&base, "keep").unwrap();
        let error = snapshot.save(&f.0.join("base.db:stream")).unwrap_err();
        assert!(error.to_string().contains("ADS"));
        assert_eq!(fs::read_to_string(base).unwrap(), "keep");
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
    }

    assert!(Snapshot::load(&f.0, &f.0).is_err());
}

#[test]
fn snapshot_is_bound_to_canonical_root_and_resource_profile() {
    let f = Fixture::new();
    let other = Fixture::new();
    let db = f.0.join("snapshot.db");
    Snapshot::new(&f.0.join("."), f.inventory("safe"))
        .unwrap()
        .save(&db)
        .unwrap();
    assert!(Snapshot::load(&db, &other.0)
        .unwrap_err()
        .to_string()
        .contains("root mismatch"));
    let mut many = f.inventory("safe");
    many.entries = (0..4097)
        .map(|i| (PathBuf::from(format!("entry{i}")), Kind::File))
        .collect();
    assert!(Snapshot::new(&f.0, many).is_err());
    let mut dirs = f.inventory("safe");
    dirs.entries = (0..128)
        .map(|i| (PathBuf::from(format!("dir{i}")), Kind::Directory))
        .collect();
    assert!(Snapshot::new(&f.0, dirs).is_err());
    let mut deep = f.inventory("safe");
    let mut path = PathBuf::new();
    for _ in 0..17 {
        path.push("dir");
        deep.entries.insert(path.clone(), Kind::Directory);
    }
    assert!(Snapshot::new(&f.0, deep).is_err());
    assert!(Snapshot::new(&f.0, f.inventory(&"a".repeat(4097))).is_err());
    let mut input = f.inventory("safe");
    input.entries = (0..4096)
        .map(|i| (PathBuf::from(format!("{i}{}", "x".repeat(256))), Kind::File))
        .collect();
    assert!(Snapshot::new(&f.0, input).is_err());
    assert_eq!(
        Snapshot::load(&db, &f.0).unwrap().inventory.entries,
        [(PathBuf::from("safe"), Kind::File)].into()
    );
}

#[test]
fn empty_root_and_complete_directory_inventory_round_trip() {
    let f = Fixture::new();
    let db = f.0.join("snapshot.db");
    let empty = Inventory {
        complete: true,
        directories: 1,
        ..Inventory::default()
    };
    Snapshot::new(&f.0, empty).unwrap().save(&db).unwrap();
    let loaded = Snapshot::load(&db, &f.0).unwrap();
    assert!(loaded.inventory.complete);
    assert!(loaded.inventory.entries.is_empty());
    assert_eq!(loaded.inventory.directories, 1);
    fs::create_dir(f.0.join("文档 空格")).unwrap();
    fs::write(f.0.join("文档 空格").join("README.TXT"), "hello").unwrap();
    let inventory = Inventory {
        entries: [
            (PathBuf::from("文档 空格"), Kind::Directory),
            (PathBuf::from("文档 空格").join("README.TXT"), Kind::File),
        ]
        .into(),
        complete: true,
        directories: 2,
        examined: 2,
        errors: vec![],
    };
    Snapshot::new(&f.0, inventory.clone())
        .unwrap()
        .save(&db)
        .unwrap();
    let loaded = Snapshot::load(&db, &f.0).unwrap();
    assert_eq!(loaded.inventory.entries, inventory.entries);
    assert_eq!(loaded.inventory.directories, 2);
    assert_eq!(loaded.inventory.examined, 2);
}
