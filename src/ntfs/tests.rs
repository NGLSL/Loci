use super::*;
use std::os::windows::ffi::OsStrExt;
use std::time::Instant;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}
fn synthetic(graph: Graph) -> NtfsIndex {
    let records = graph.live;
    NtfsIndex {
        volume: "D:".into(),
        checkpoint: PathBuf::new(),
        guid: "fixture-volume".into(),
        serial: 1,
        graph: Arc::new(graph),
        status: IndexStatus {
            state: "ready".into(),
            records,
            version: 1,
            journal_id: 7,
            cursor: 12,
            error: None,
        },
        limits: Limits::default(),
        storage_root: 0,
        storage_parents: Arc::new(HashMap::new()),
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "loci-ntfs-index-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn million_inventory_query_and_streaming_checkpoint_are_real_work_not_ignored() {
    let began = Instant::now();
    let mut graph = Graph::new(5, Limits::default());
    graph.add(50, 5, &wide("目录"), DIRECTORY).unwrap();
    for i in 0..1_000_000 {
        graph
            .add(100 + i, 50, &wide(&format!("report_{i:06}.TXT")), 0)
            .unwrap();
    }
    let build = began.elapsed();
    graph.validate().unwrap();
    assert!(
        graph.memory_bytes() < 128 * 1024 * 1024,
        "memory={}",
        graph.memory_bytes()
    );
    let index = synthetic(graph);
    let started = Instant::now();
    let short = index.search("report", 50, "documents").unwrap();
    let short_time = started.elapsed();
    assert_eq!(short.items.len(), 50);
    assert!(!short.total_exact);
    let started = Instant::now();
    let absent = index.search("absent_filename", 50, "all").unwrap();
    let absent_time = started.elapsed();
    assert!(absent.items.is_empty());
    assert!(absent.total_exact);
    assert_eq!(absent.total, 0);
    let started = Instant::now();
    let last = index
        .search("目录 REPORT_999999 ext:txt", 50, "all")
        .unwrap();
    let rare_time = started.elapsed();
    assert_eq!(last.items.len(), 1);
    assert_eq!(last.items[0].path, "D:\\目录\\report_999999.TXT");
    let fixture = Fixture::new();
    let file = fixture.0.join("million.loci");
    let started = Instant::now();
    checkpoint::save(
        &file,
        &checkpoint::Saved {
            graph: (*index.graph).clone(),
            guid: index.guid.clone(),
            serial: 1,
            journal: 7,
            cursor: 12,
            version: 1,
            storage_root: 0,
            storage_parents: HashMap::new(),
        },
    )
    .unwrap();
    let save = started.elapsed();
    let started = Instant::now();
    let loaded = checkpoint::load(&file, Limits::default()).unwrap();
    let load = started.elapsed();
    assert_eq!(loaded.graph.live, 1_000_001);
    assert_eq!(
        synthetic(loaded.graph)
            .search("report_999999 ext:txt", 50, "documents")
            .unwrap()
            .items[0]
            .name,
        "report_999999.TXT"
    );
    println!("million_ntfs_synthetic,entries={},memory_bytes={},build_ms={},short_ms={},absent_ms={},rare_ms={},save_ms={},load_ms={},checkpoint_bytes={}",index.graph.live,index.graph.memory_bytes(),build.as_millis(),short_time.as_millis(),absent_time.as_millis(),rare_time.as_millis(),save.as_millis(),load.as_millis(),std::fs::metadata(file).unwrap().len());
    if !cfg!(debug_assertions) {
        assert!(
            short_time.as_millis() < 500,
            "short query exceeded client deadline"
        );
        assert!(
            absent_time.as_millis() < 500,
            "absent query exceeded client deadline"
        );
    }
}

#[test]
fn utf16_identity_query_categories_and_surrogate_open_paths_are_preserved() {
    let mut graph = Graph::new(5, Limits::default());
    graph.add(50, 5, &wide("项目"), DIRECTORY).unwrap();
    graph.add(100, 50, &wide("报告Β.PDF"), 0).unwrap();
    graph.add(100, 50, &wide("Alias.PDF"), 0).unwrap();
    graph.add(101, 50, &[0xd800, 46, 112, 100, 102], 0).unwrap();
    for (i, name) in ["IMAGE.PNG", "movie.MP4", "sound.MP3", "backup.ZIP"]
        .iter()
        .enumerate()
    {
        graph.add(200 + i as u64, 50, &wide(name), 0).unwrap();
    }
    let index = synthetic(graph);
    assert_eq!(
        index
            .search("项目 报告β ext:pdf", 50, "documents")
            .unwrap()
            .items
            .len(),
        1
    );
    assert_eq!(
        index
            .search("d:/项目/alias", 50, "all")
            .unwrap()
            .items
            .len(),
        1
    );
    for filter in ["images", "videos", "audio", "archives", "folders"] {
        assert_eq!(index.search("", 50, filter).unwrap().items.len(), 1);
    }
    let page = index.search("ext:pdf", 50, "all").unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.skipped_invalid_names, 1);
    assert!(page.items.iter().all(|n| !n.path.contains('\u{fffd}')));
    assert_eq!(index.search("�", 50, "all").unwrap().total, 0);
    assert_eq!(
        index
            .graph
            .nodes()
            .filter(|(_, n)| n.alive && n.object == 100)
            .count(),
        2
    );
}

struct ActualDirectory {
    volume: Volume,
    base: Vec<u16>,
}
impl NamespaceSource for ActualDirectory {
    fn root(&self) -> u64 {
        self.volume.root
    }
    fn list(&self, relative: &[u16], parent: u64, cancel: &AtomicBool) -> io::Result<Vec<Record>> {
        let mut path = self.base.clone();
        if !relative.is_empty() {
            path.push(92);
            path.extend_from_slice(relative);
        }
        self.volume.list_directory(&path, parent, cancel)
    }
}
fn record(object: u64, parent: u64, name: &str, attributes: u32, reason: u32) -> Record {
    Record {
        object,
        parent,
        usn: 13,
        name: wide(name),
        attributes,
        reason,
    }
}

#[test]
fn native_directory_replay_preserves_renamed_descendants_hardlinks_and_old_snapshot() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.0.join("old")).unwrap();
    std::fs::write(fixture.0.join("old/original.txt"), "contents").unwrap();
    std::fs::hard_link(
        fixture.0.join("old/original.txt"),
        fixture.0.join("old/alias.txt"),
    )
    .unwrap();
    let base: Vec<u16> = fixture.0.as_os_str().encode_wide().collect();
    let volume = native::test_directory(&base).unwrap();
    let source = ActualDirectory { volume, base };
    let root = source.root();
    let listing = source.list(&[], root, &AtomicBool::new(false)).unwrap();
    let dir = listing[0].object;
    let mut graph = Graph::new(root, Limits::default());
    graph.add(dir, root, &wide("old"), DIRECTORY).unwrap();
    for entry in source
        .list(&wide("old"), dir, &AtomicBool::new(false))
        .unwrap()
    {
        graph
            .add(entry.object, dir, &entry.name, entry.attributes)
            .unwrap();
    }
    let old = synthetic(graph.clone());
    std::fs::rename(fixture.0.join("old"), fixture.0.join("new")).unwrap();
    let next = apply_records(
        &source,
        &graph,
        &[
            record(dir, root, "old", DIRECTORY, 0x1000),
            record(dir, root, "new", DIRECTORY, 0x2000),
        ],
        0,
        &AtomicBool::new(false),
    )
    .unwrap();
    let next = next.0;
    let current = synthetic(next.clone());
    assert_eq!(
        current
            .search("new ext:txt", 50, "all")
            .unwrap()
            .items
            .len(),
        2
    );
    assert_eq!(current.search("old", 50, "all").unwrap().items.len(), 0);
    assert_eq!(old.search("old ext:txt", 50, "all").unwrap().items.len(), 2);
    let original = source
        .list(&wide("new"), dir, &AtomicBool::new(false))
        .unwrap()[0]
        .object;
    std::fs::remove_file(fixture.0.join("new/alias.txt")).unwrap();
    let next = apply_records(
        &source,
        &next,
        &[record(original, dir, "alias.txt", 0, 0x10000)],
        0,
        &AtomicBool::new(false),
    )
    .unwrap();
    let next = next.0;
    assert_eq!(
        synthetic(next.clone())
            .search("ext:txt", 50, "all")
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(apply_records(
        &source,
        &next,
        &[record(999, 999, "orphan.txt", 0, 0x100)],
        0,
        &AtomicBool::new(false)
    )
    .is_err());
    assert!(apply_records(&source, &next, &[], 0, &AtomicBool::new(true)).is_err());
}

#[test]
fn journal_provenance_and_checkpoint_failure_preserve_complete_inventory() {
    let journal = Journal {
        id: 7,
        first: 10,
        lowest: 11,
        next: 100,
    };
    assert!(retained(&journal, 7, 12));
    assert!(!retained(&journal, 8, 12));
    assert!(!retained(&journal, 7, 10));
    assert!(!retained(&journal, 7, 101));
    let fixture = Fixture::new();
    let file = fixture.0.join("state");
    let mut graph = Graph::new(5, Limits::default());
    graph.add(50, 5, &wide("kept.txt"), 0).unwrap();
    let saved = checkpoint::Saved {
        graph,
        guid: "volume".into(),
        serial: 1,
        journal: 7,
        cursor: 12,
        version: 1,
        storage_root: 0,
        storage_parents: HashMap::new(),
    };
    checkpoint::save(&file, &saved).unwrap();
    let before = std::fs::read(&file).unwrap();
    let mut bad = saved;
    bad.graph.add(51, 999, &wide("bad.txt"), 0).unwrap();
    assert!(checkpoint::save(&file, &bad).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), before);
    let mut corrupt = before;
    corrupt[30] ^= 1;
    std::fs::write(&file, corrupt).unwrap();
    assert!(checkpoint::load(&file, Limits::default()).is_err());
    let mut tiny = Graph::new(
        5,
        Limits {
            memory_bytes: 1024 * 1024,
            ..Limits::default()
        },
    );
    let mut hit = false;
    for i in 0..100_000 {
        if tiny.add(100 + i, 5, &wide("name"), 0).is_err() {
            hit = true;
            break;
        }
    }
    assert!(hit);
}

#[test]
fn current_token_native_volume_permission_is_observed_without_elevation() {
    match Volume::open("D:") {
        Ok(volume) => {
            match volume.query() {
                Ok(journal) => println!(
                    "native_D_readable,journal_id={},cursor={}",
                    journal.id, journal.next
                ),
                Err(error) => {
                    // A readable NTFS volume need not have an active journal
                    // (for example, the GitHub runner's temporary D: drive).
                    println!(
                        "native_D_journal,error_kind={:?},os_code={:?}",
                        error.kind(),
                        error.raw_os_error()
                    );
                    assert!(error.raw_os_error().is_some());
                }
            }
        }
        Err(error) => {
            println!(
                "native_D_access,error_kind={:?},os_code={:?}",
                error.kind(),
                error.raw_os_error()
            );
            assert!(error.raw_os_error().is_some());
        }
    }
}

#[test]
fn private_checkpoint_scope_does_not_publish_or_rewrite_itself() {
    let mut graph = Graph::new(5, Limits::default());
    graph.add(100, 5, &wide("visible.txt"), 0).unwrap();
    let mut parents = HashMap::from([(50, 5), (51, 50)]);
    let records = vec![
        record(60, 50, "inventory.tmp", 0, 0x100),
        record(61, 51, "checkpoint.loci", 0, 0x2000),
        record(52, 51, "new-storage-child", DIRECTORY, 0x100),
        record(62, 52, "child.tmp", 0, 0x100),
    ];
    let scoped = scope_records(&graph, &mut parents, 50, records);
    assert!(scoped.is_empty());
    assert_eq!(parents[&52], 51);
    let fixture = Fixture::new();
    let base: Vec<u16> = fixture.0.as_os_str().encode_wide().collect();
    let volume = native::test_directory(&base).unwrap();
    let source = ActualDirectory { volume, base };
    let (same, changed) =
        apply_records(&source, &graph, &scoped, 50, &AtomicBool::new(false)).unwrap();
    assert_eq!(changed, 0);
    assert_eq!(same.live, 1);
    assert!(synthetic(same)
        .search("inventory", 50, "all")
        .unwrap()
        .items
        .is_empty());
    let file = fixture.0.join("saved");
    checkpoint::save(
        &file,
        &checkpoint::Saved {
            graph,
            guid: "volume".into(),
            serial: 1,
            journal: 7,
            cursor: 12,
            version: 1,
            storage_root: 50,
            storage_parents: parents.clone(),
        },
    )
    .unwrap();
    let loaded = checkpoint::load(&file, Limits::default()).unwrap();
    assert_eq!(loaded.storage_root, 50);
    assert_eq!(loaded.storage_parents, parents);
}

#[test]
fn removed_directory_prunes_descendants_but_keeps_external_hardlink_alias() {
    let mut graph = Graph::new(5, Limits::default());
    let directory = graph.add(50, 5, &wide("old"), DIRECTORY).unwrap();
    graph.add(51, 50, &wide("nested"), DIRECTORY).unwrap();
    graph.add(100, 51, &wide("inside.txt"), 0).unwrap();
    graph.add(100, 5, &wide("outside.txt"), 0).unwrap();
    let old = graph.clone();
    graph.remove(directory);
    graph.prune_removed_subtrees(&old);
    graph.validate().unwrap();
    let page = synthetic(graph).search("ext:txt", 50, "all").unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].name, "outside.txt");
}

#[test]
fn replay_budget_rebuilds_once_without_string_matching_or_retry_loop() {
    let calls = std::cell::Cell::new(0);
    // A retained old checkpoint can still exceed the replay budget. Exercise
    // the production replay collector with a small configurable limit.
    let journal = Journal {
        id: 7,
        first: 0,
        next: 100,
        lowest: 0,
    };
    assert!(retained(&journal, 7, 10));
    let mut change = record(100, 5, "changed.txt", 0, 0x8000);
    change.usn = 11;
    let result = collect_replay(10, journal.next, 1, &AtomicBool::new(false), |_| {
        Ok((journal.next, vec![change.clone()]))
    });
    let (value, rebuilt) = recover_budget(result, || {
        calls.set(calls.get() + 1);
        // A rebuild starts at the newly observed boundary, rather than retrying
        // the old retained cursor indefinitely.
        collect_replay(
            journal.next,
            journal.next,
            1,
            &AtomicBool::new(false),
            |_| panic!("new rebuild boundary must not replay the old cursor"),
        )
    })
    .unwrap();
    assert_eq!((value.len(), rebuilt, calls.get()), (0, true, 1));
    let unrelated: io::Result<usize> = Err(io::Error::other(ReplayBudget.to_string()));
    assert!(recover_budget(unrelated, || {
        calls.set(calls.get() + 1);
        Ok(0)
    })
    .is_err());
    assert_eq!(calls.get(), 1);
    let failed: io::Result<usize> = Err(io::Error::new(io::ErrorKind::OutOfMemory, ReplayBudget));
    assert!(recover_budget(failed, || {
        calls.set(calls.get() + 1);
        Err(io::Error::new(io::ErrorKind::OutOfMemory, ReplayBudget))
    })
    .is_err());
    assert_eq!(calls.get(), 2);
}

#[test]
fn cancelled_checkpoint_keeps_the_previous_file_and_scope_is_enforced_on_relisting() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join("visible.txt"), "data").unwrap();
    std::fs::create_dir(fixture.0.join("storage")).unwrap();
    std::fs::write(fixture.0.join("storage/private.loci"), "private").unwrap();
    let base: Vec<_> = fixture.0.as_os_str().encode_wide().collect();
    let volume = native::test_directory(&base).unwrap();
    let source = ActualDirectory { volume, base };
    let listing = source
        .list(&[], source.root(), &AtomicBool::new(false))
        .unwrap();
    let storage = listing
        .iter()
        .find(|r| r.name == wide("storage"))
        .unwrap()
        .object;
    let visible = listing
        .iter()
        .find(|r| r.name == wide("visible.txt"))
        .unwrap();
    let mut graph = Graph::new(source.root(), Limits::default());
    graph
        .add(
            visible.object,
            source.root(),
            &visible.name,
            visible.attributes,
        )
        .unwrap();
    let (same, changed) = apply_records(
        &source,
        &graph,
        &[record(
            visible.object,
            source.root(),
            "visible.txt",
            visible.attributes,
            0x8000,
        )],
        storage,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(changed, 0);
    assert_eq!(same.live, 1);
    let file = fixture.0.join("storage/state");
    let saved = checkpoint::Saved {
        graph,
        guid: "volume".into(),
        serial: 1,
        journal: 7,
        cursor: 12,
        version: 1,
        storage_root: storage,
        storage_parents: HashMap::new(),
    };
    checkpoint::save(&file, &saved).unwrap();
    let bytes = std::fs::read(&file).unwrap();
    assert!(checkpoint::save_cancel(&file, &saved, &AtomicBool::new(true)).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), bytes);
    assert!(checkpoint::load_cancel(&file, Limits::default(), &AtomicBool::new(true)).is_err());
}
