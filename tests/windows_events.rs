#![cfg(windows)]

mod events {
    pub use loci_experiment::events::*;
}
#[path = "../src/windows_events.rs"]
mod windows_events;

use events::{Change, EventBatch, EventLimits, EventSource, SourceState};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use windows_events::WindowsEvents;

static SERIAL: Mutex<()> = Mutex::new(());
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "loci-native-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(fs::canonicalize(root).unwrap())
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn until(source: &mut WindowsEvents, predicate: impl Fn(&EventBatch) -> bool) -> EventBatch {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let batch = source.poll().unwrap();
        if predicate(&batch) {
            return batch;
        }
        assert!(
            batch.losses.is_empty(),
            "unexpected loss: {:?}",
            batch.losses
        );
        assert!(Instant::now() < deadline, "native event deadline exceeded");
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn native_create_and_delete_preserve_unicode_and_spaces() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let name = Path::new("原生 file.txt");
    fs::write(fixture.0.join(name), b"fixture").unwrap();
    let batch = until(&mut source, |b| {
        b.changes.contains(&Change::Refresh(name.into()))
    });
    assert!(batch.losses.is_empty());
    assert_eq!(batch.state, SourceState::Watching);
    fs::remove_file(fixture.0.join(name)).unwrap();
    let batch = until(&mut source, |b| {
        b.changes.contains(&Change::Remove(name.into()))
    });
    assert!(batch.losses.is_empty());
    source.stop().unwrap();
}

#[test]
fn native_file_directory_rename_and_recursive_events() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    fs::create_dir(fixture.path("before dir")).unwrap();
    fs::write(fixture.path("before.txt"), b"fixture").unwrap();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    for (from, to) in [
        ("before.txt", "after 原生.txt"),
        ("before dir", "after dir"),
    ] {
        fs::rename(fixture.path(from), fixture.path(to)).unwrap();
        let expected = Change::Rename {
            from: from.into(),
            to: to.into(),
        };
        let batch = until(&mut source, |b| b.changes.contains(&expected));
        assert!(batch.losses.is_empty());
    }
    fs::create_dir(fixture.path("after dir/nested")).unwrap();
    until(&mut source, |b| {
        b.changes
            .contains(&Change::Refresh("after dir/nested".into()))
    });
    fs::write(fixture.path("after dir/nested/deep.txt"), b"fixture").unwrap();
    until(&mut source, |b| {
        b.changes
            .contains(&Change::Refresh("after dir/nested/deep.txt".into()))
    });
    fs::remove_file(fixture.path("after dir/nested/deep.txt")).unwrap();
    until(&mut source, |b| {
        b.changes
            .contains(&Change::Remove("after dir/nested/deep.txt".into()))
    });
    fs::remove_dir(fixture.path("after dir/nested")).unwrap();
    until(&mut source, |b| {
        b.changes
            .contains(&Change::Remove("after dir/nested".into()))
    });
}
#[test]
fn idle_poll_and_repeated_stop_are_prompt_and_stopped_is_empty() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let start = Instant::now();
    for _ in 0..100 {
        assert!(source.poll().unwrap().changes.is_empty());
    }
    assert!(start.elapsed() < Duration::from_secs(1));
    let start = Instant::now();
    source.stop().unwrap();
    source.stop().unwrap();
    assert!(start.elapsed() < Duration::from_secs(1));
    fs::write(fixture.path("after stop.txt"), b"fixture").unwrap();
    let batch = source.poll().unwrap();
    assert_eq!(batch.state, SourceState::Stopped);
    assert!(batch.changes.is_empty() && batch.losses.is_empty());
}
#[test]
fn pending_source_can_move_to_another_thread() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let root = fixture.0.clone();
    thread::spawn(move || {
        let mut source = source;
        fs::write(root.join("moved.txt"), b"fixture").unwrap();
        until(&mut source, |b| {
            b.changes.contains(&Change::Refresh("moved.txt".into()))
        });
        source.stop().unwrap();
    })
    .join()
    .unwrap();
}
#[test]
fn queue_budget_reports_loss_without_unbounded_changes() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::new(1, 4096).unwrap()).unwrap();
    for i in 0..16 {
        fs::write(fixture.path(&format!("budget-{i}.txt")), b"fixture").unwrap();
    }
    let batch = until(&mut source, |b| {
        b.losses.contains(&events::Loss::UserOverflow)
    });
    assert!(batch.changes.len() <= 1);
}
#[test]
fn real_native_storm_reports_kernel_overflow() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::new(256, 4096).unwrap()).unwrap();
    // First completion occupies the application buffer; subsequent unpolled
    // creates overflow the kernel's fixed 4 KiB directory buffer.
    for i in 0..1024 {
        fs::write(
            fixture.path(&format!("storm-{i:04}-{}", "n".repeat(80))),
            b"fixture",
        )
        .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let started = Instant::now();
        let batch = source.poll().unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(batch.changes.len() <= 256);
        if batch.losses.contains(&events::Loss::KernelOverflow) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real kernel overflow was not observed"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn source_reservations_are_bounded_and_released_on_stop_drop_and_open_error() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let handles_before_errors = handle_count();
    for _ in 0..16 {
        assert!(WindowsEvents::open(&fixture.path("missing"), EventLimits::default()).is_err());
    }
    fs::write(fixture.path("regular.txt"), b"fixture").unwrap();
    for _ in 0..16 {
        assert!(WindowsEvents::open(&fixture.path("regular.txt"), EventLimits::default()).is_err());
    }
    let handles_after_errors = handle_count();
    assert!(handles_after_errors <= handles_before_errors);
    eprintln!(
        "native open-error handles: before={handles_before_errors}, after={handles_after_errors}"
    );
    let mut sources: Vec<_> = (0..events::MAX_SOURCES)
        .map(|_| WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap())
        .collect();
    assert!(
        matches!(WindowsEvents::open(&fixture.0, EventLimits::default()), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
    );
    sources[0].stop().unwrap();
    let replacement = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    drop(replacement);
    drop(sources);
    let _reopened = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetCurrentProcess() -> *mut std::ffi::c_void;
    fn GetProcessHandleCount(process: *mut std::ffi::c_void, count: *mut u32) -> i32;
}
fn handle_count() -> u32 {
    let mut count = 0;
    assert_ne!(
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) },
        0
    );
    count
}
#[test]
fn repeated_idle_and_completion_race_cleanup_keeps_handles_stable() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    drop(WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap());
    let baseline = handle_count();
    let source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let active = handle_count();
    assert_eq!(active, baseline + 2);
    drop(source);
    eprintln!("native active handles: baseline={baseline}, active={active}");
    for i in 0..128 {
        let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
        if i % 2 == 0 {
            fs::write(fixture.path(&format!("race-{i}.txt")), b"fixture").unwrap();
        }
        if i % 3 == 0 {
            source.stop().unwrap();
            source.stop().unwrap();
        }
        drop(source);
    }
    let after = handle_count();
    eprintln!("native handles: baseline={baseline}, after={after}");
    assert!(after <= baseline, "handles grew from {baseline} to {after}");
}

#[test]
fn native_utf16_names_keep_os_representation() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let name = PathBuf::from(OsString::from_wide(&[0x006e, 0xd800, 0x002e, 0x0074]));
    fs::write(fixture.0.join(&name), b"fixture").unwrap();
    let batch = until(&mut source, |b| {
        b.changes.contains(&Change::Refresh(name.clone()))
    });
    assert!(batch.losses.is_empty());
}
#[test]
fn native_long_renames_across_many_reads_keep_pairing() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let names: Vec<_> = (0..16)
        .map(|i| {
            (
                format!("old-{i}-{}", "n".repeat(180)),
                format!("new-{i}-{}", "n".repeat(180)),
            )
        })
        .collect();
    for (from, _) in &names {
        fs::write(fixture.path(from), b"fixture").unwrap();
    }
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::new(256, 4096).unwrap()).unwrap();
    for (from, to) in &names {
        fs::rename(fixture.path(from), fixture.path(to)).unwrap();
        let expected = Change::Rename {
            from: from.into(),
            to: to.into(),
        };
        let batch = until(&mut source, |b| b.changes.contains(&expected));
        assert!(batch.losses.is_empty());
    }
}
#[test]
fn native_out_of_root_move_never_guesses_a_rename() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    fs::create_dir(fixture.path("watched")).unwrap();
    fs::create_dir(fixture.path("outside")).unwrap();
    fs::write(fixture.path("watched/departing.txt"), b"fixture").unwrap();
    let mut source = WindowsEvents::open(
        &fs::canonicalize(fixture.path("watched")).unwrap(),
        EventLimits::default(),
    )
    .unwrap();
    fs::rename(
        fixture.path("watched/departing.txt"),
        fixture.path("outside/departing.txt"),
    )
    .unwrap();
    let batch = until(&mut source, |b| {
        b.changes.contains(&Change::Remove("departing.txt".into()))
            || b.losses.contains(&events::Loss::UnpairedRename)
    });
    eprintln!(
        "out-of-root move: changes={:?}, losses={:?}",
        batch.changes, batch.losses
    );
    assert!(!batch
        .changes
        .iter()
        .any(|c| matches!(c, Change::Rename { .. })));
}

#[test]
fn bounded_native_rename_loss_does_not_publish_a_guessed_pair() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.path("before.txt"), b"fixture").unwrap();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::new(1, 4096).unwrap()).unwrap();
    fs::rename(fixture.path("before.txt"), fixture.path("after.txt")).unwrap();
    let batch = until(&mut source, |b| {
        b.losses.contains(&events::Loss::UserOverflow)
    });
    assert!(batch.losses.contains(&events::Loss::UnpairedRename));
    assert!(!batch
        .changes
        .iter()
        .any(|c| matches!(c, Change::Rename { .. })));
    assert!(source.poll().unwrap().changes.is_empty());
}
#[test]
fn native_attribute_changes_refresh_the_entry() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let path = fixture.path("attribute.txt");
    fs::write(&path, b"fixture").unwrap();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    let batch = until(&mut source, |b| {
        b.changes.contains(&Change::Refresh("attribute.txt".into()))
    });
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).unwrap();
    assert!(batch.losses.is_empty());
}

// The following tests inject FILE_NOTIFY_INFORMATION completion payloads and
// virtual timestamps. They verify decoder/poll state, not native delivery.
fn injected_record(action: u32, name: &str) -> Vec<u8> {
    let wide: Vec<u16> = name.encode_utf16().collect();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&action.to_le_bytes());
    bytes.extend_from_slice(&((wide.len() * 2) as u32).to_le_bytes());
    for unit in wide {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}
#[test]
fn injected_old_then_new_in_separate_completions_never_exposes_reliable_pending_batch() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let now = Instant::now();
    source.inject_completion_for_test(injected_record(4, "before.txt"), now);
    let old_only = source.poll().unwrap();
    assert_eq!(old_only.state, SourceState::Watching);
    assert!(old_only.changes.is_empty());
    assert!(old_only.losses.contains(&events::Loss::UnpairedRename));
    source.advance_clock_for_test(now + Duration::from_millis(1));
    let waiting = source.poll().unwrap();
    assert!(waiting.changes.is_empty());
    assert!(waiting.losses.contains(&events::Loss::UnpairedRename));
    source.inject_completion_for_test(
        injected_record(5, "after.txt"),
        now + Duration::from_millis(2),
    );
    let paired = source.poll().unwrap();
    assert_eq!(
        paired.changes,
        vec![Change::Rename {
            from: "before.txt".into(),
            to: "after.txt".into()
        }]
    );
    assert!(paired.losses.is_empty());
}

#[test]
fn injected_old_only_expires_at_deadline_and_late_new_is_unpaired() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let now = Instant::now();
    source.inject_completion_for_test(injected_record(4, "expired.txt"), now);
    assert!(source
        .poll()
        .unwrap()
        .losses
        .contains(&events::Loss::UnpairedRename));
    source.advance_clock_for_test(now + Duration::from_millis(99));
    assert!(source
        .poll()
        .unwrap()
        .losses
        .contains(&events::Loss::UnpairedRename));
    source.advance_clock_for_test(now + Duration::from_millis(100));
    let expired = source.poll().unwrap();
    assert!(expired.changes.is_empty());
    assert!(expired.losses.contains(&events::Loss::UnpairedRename));
    source.advance_clock_for_test(now + Duration::from_millis(101));
    let idle = source.poll().unwrap();
    assert!(idle.changes.is_empty() && idle.losses.is_empty());
    source.inject_completion_for_test(
        injected_record(5, "too-late.txt"),
        now + Duration::from_millis(102),
    );
    let late = source.poll().unwrap();
    assert!(late.changes.is_empty());
    assert!(late.losses.contains(&events::Loss::UnpairedRename));
}
#[test]
fn injected_pending_rename_stop_discards_staged_new_and_releases_native_handles() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let baseline = handle_count();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let now = Instant::now();
    source.inject_completion_for_test(injected_record(4, "before.txt"), now);
    assert!(source
        .poll()
        .unwrap()
        .losses
        .contains(&events::Loss::UnpairedRename));
    source.inject_completion_for_test(
        injected_record(5, "after.txt"),
        now + Duration::from_millis(1),
    );
    source.stop().unwrap();
    source.stop().unwrap();
    assert_eq!(handle_count(), baseline);
    for _ in 0..3 {
        let batch = source.poll().unwrap();
        assert_eq!(batch.state, SourceState::Stopped);
        assert!(batch.changes.is_empty() && batch.losses.is_empty());
    }
}
#[test]
fn injected_trailing_record_bytes_are_loss_and_cannot_pair_with_old_name() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let now = Instant::now();
    source.inject_completion_for_test(injected_record(4, "before.txt"), now);
    source.poll().unwrap();
    let mut malformed = injected_record(5, "after.txt");
    malformed.extend_from_slice(&[0; 4]); // beyond possible DWORD padding
    source.inject_completion_for_test(malformed, now + Duration::from_millis(1));
    let batch = source.poll().unwrap();
    assert!(batch.losses.contains(&events::Loss::InvalidEvent));
    assert!(batch.losses.contains(&events::Loss::UnpairedRename));
    assert!(batch.changes.is_empty());
}

#[test]
fn injected_malformed_completions_are_loss_and_retire_pending_rename() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let mut malformed = Vec::new();
    malformed.push(vec![0; 11]);
    for (field, value) in [
        (8usize, 0u32),
        (8, 1),
        (8, 4098),
        (8, u32::MAX),
        (0, 1),
        (0, 12),
        (0, 4096),
        (0, u32::MAX),
    ] {
        let mut bytes = injected_record(5, "after.txt");
        bytes[field..field + 4].copy_from_slice(&value.to_le_bytes());
        malformed.push(bytes);
    }
    let mut truncated_name = injected_record(5, "after.txt");
    truncated_name.pop();
    malformed.push(truncated_name);
    malformed.push(injected_record(99, "after.txt"));
    for name in [
        "C:\\escape.txt",
        "\\escape.txt",
        "\\\\host\\share\\escape.txt",
        "..\\escape.txt",
        "dir\\..\\escape.txt",
        ".\\escape.txt",
        "dir\\\\escape.txt",
        "entry:stream",
        "entry\0name",
        "",
    ] {
        malformed.push(injected_record(5, name));
    }
    let mut bad_next_header = injected_record(1, "valid.txt");
    let next = bad_next_header.len().next_multiple_of(4);
    bad_next_header[0..4].copy_from_slice(&(next as u32).to_le_bytes());
    bad_next_header.resize(next + 1, 0);
    malformed.push(bad_next_header);
    malformed.push(vec![0; EventLimits::default().buffer_bytes() + 4]);
    let now = Instant::now();
    for (case, bytes) in malformed.into_iter().enumerate() {
        let at = now + Duration::from_millis(case as u64 * 3);
        source.inject_completion_for_test(injected_record(4, "before.txt"), at);
        assert!(source
            .poll()
            .unwrap()
            .losses
            .contains(&events::Loss::UnpairedRename));
        source.inject_completion_for_test(bytes, at + Duration::from_millis(1));
        let bad = source.poll().unwrap();
        assert!(
            bad.losses.contains(&events::Loss::InvalidEvent),
            "malformed case {case}"
        );
        assert!(
            bad.losses.contains(&events::Loss::UnpairedRename),
            "malformed case {case}"
        );
        assert!(
            !bad.changes
                .iter()
                .any(|c| matches!(c, Change::Rename { .. })),
            "malformed case {case}"
        );
        source.inject_completion_for_test(
            injected_record(5, "later.txt"),
            at + Duration::from_millis(2),
        );
        let later = source.poll().unwrap();
        assert!(
            later.changes.is_empty(),
            "stale old-name paired in case {case}"
        );
        assert!(later.losses.contains(&events::Loss::UnpairedRename));
    }
}
#[test]
fn injected_zero_byte_completion_is_kernel_loss_and_breaks_rename_pairing() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let now = Instant::now();
    source.inject_completion_for_test(injected_record(4, "before.txt"), now);
    source.poll().unwrap();
    source.inject_completion_for_test(Vec::new(), now + Duration::from_millis(1));
    let overflow = source.poll().unwrap();
    assert!(overflow.losses.contains(&events::Loss::KernelOverflow));
    assert!(overflow.losses.contains(&events::Loss::UnpairedRename));
    source.inject_completion_for_test(
        injected_record(5, "after.txt"),
        now + Duration::from_millis(2),
    );
    let new_only = source.poll().unwrap();
    assert!(new_only.changes.is_empty());
    assert!(new_only.losses.contains(&events::Loss::UnpairedRename));
}
#[test]
fn injected_intervening_record_cannot_pair_two_different_rename_observations() {
    let _serial = SERIAL.lock().unwrap();
    let fixture = Fixture::new();
    let mut source = WindowsEvents::open(&fixture.0, EventLimits::default()).unwrap();
    let now = Instant::now();
    source.inject_completion_for_test(injected_record(4, "before.txt"), now);
    source.poll().unwrap();
    source.inject_completion_for_test(
        injected_record(1, "unrelated.txt"),
        now + Duration::from_millis(1),
    );
    let interrupted = source.poll().unwrap();
    assert!(interrupted.losses.contains(&events::Loss::UnpairedRename));
    source.inject_completion_for_test(
        injected_record(5, "after.txt"),
        now + Duration::from_millis(2),
    );
    let new_only = source.poll().unwrap();
    assert!(new_only.changes.is_empty());
    assert!(new_only.losses.contains(&events::Loss::UnpairedRename));
}
