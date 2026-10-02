#![cfg(target_os = "linux")]
mod common;
#[path = "common/fixture_capacity.rs"]
mod fixture_capacity;
use fixture_capacity::{admit_pilot, inode_budget, parse_usage, reported_budget};

// Exact byte-form output from the native btrfs-progs interface; these synthetic
// classification cases are not Btrfs capacity or Engine acceptance evidence.
const USAGE: &str = "Overall:\n    Device size: 8589934592\n    Device allocated: 2147483648\n    Device unallocated: 6442450944\n    Device missing: 0\n    Free (estimated): 7000000000 (min: 6000000000)\nData,single: Size:1073741824, Used:1048576 (0.10%)\nMetadata,DUP: Size:536870912, Used:1048576 (0.20%)\nSystem,DUP: Size:8388608, Used:16384 (0.20%)\n";

#[test]
fn dynamic_inode_counter_does_not_relax_reported_filesystem_guards() {
    assert_eq!(inode_budget(true, 0, 0), Ok(true));
    assert!(inode_budget(false, 0, 0).is_err());
    assert!(inode_budget(true, 1_000_000, 125_000).is_err());
    assert!(inode_budget(false, 1_000_000, 140_000).is_err());
    assert_eq!(inode_budget(false, 1_000_000, 150_000), Ok(false));
    assert!(inode_budget(true, 0, 10).is_err());
    assert!(reported_budget(8_000_000, 800_000, 800_000).is_err());
    assert!(reported_budget(8_000_000, 1_100_000, 800_000).is_err());
}

#[test]
fn dynamic_inode_classification_requires_actual_dup_pilot_and_reserve() {
    let usage = parse_usage(USAGE).unwrap();
    assert!(admit_pilot(&usage, 0, 0).is_err());
    assert!(admit_pilot(&usage, 1000, 0).is_err());
    assert!(admit_pilot(&usage, 999, 1_000_000).is_err());
    let planned = admit_pilot(&usage, 100_000, 150_000_000).unwrap();
    assert_eq!(planned, 281_250_000 + 512 * 1024 * 1024);
    assert!(admit_pilot(&usage, 1000, 100_000_000).is_err());
    let low_free = USAGE.replace("(min: 6000000000)", "(min: 600000000)");
    assert!(admit_pilot(&parse_usage(&low_free).unwrap(), 100_000, 150_000_000).is_err());
    let almost_full = USAGE
        .replace("2147483648", "7516192768")
        .replace("6442450944", "1073741824");
    assert!(admit_pilot(&parse_usage(&almost_full).unwrap(), 100_000, 150_000_000).is_err());
}

#[test]
fn unsupported_partial_or_mismatched_btrfs_usage_is_not_capacity_proof() {
    for raw in [
        USAGE.replace("Metadata,DUP:", "Metadata,single:"),
        USAGE.replace("Data,single:", "Data,RAID1:"),
        USAGE.replace("    Device missing: 0\n", ""),
        USAGE.replace("Device missing: 0", "Device missing: 1024"),
        USAGE.replace(
            "Device unallocated: 6442450944",
            "Device unallocated: 6442450945",
        ),
        USAGE.replace("Metadata,DUP: Size:536870912", "Metadata,DUP: Size:1"),
        USAGE.replace("(min: 6000000000)", ""),
        format!("{USAGE}Metadata,single: Size:1024, Used:0\n"),
    ] {
        assert!(parse_usage(&raw).is_err(), "{raw}");
    }
}

#[test]
fn archived_actual_native_usage_parses_tabs_and_percentages() {
    // Captured only by the ee2e9f3 guest capability probe, not an Engine gate.
    // It has no pilot delta and cannot authorize fixture creation on its own.
    let usage = parse_usage(include_str!("data/btrfs-usage-capability.txt")).unwrap();
    assert!(admit_pilot(&usage, 0, 0).is_err());
}

#[test]
fn actual_native_small_fixture_keeps_the_existing_reported_capacity_guard() {
    let fixture = common::Fixture::new();
    assert!(
        fixture_capacity::hundred_thousand_fixture_preflight(&fixture.base),
        "small helper test requires actual capacity evidence, not a skipped fixture"
    );
    std::fs::write(fixture.root.join("small-native.txt"), b"").unwrap();
    let engine = loci_experiment::engine::Engine::open_with_options(
        &fixture.root,
        None,
        loci_experiment::engine::EngineOptions::scale(),
    )
    .unwrap();
    let page = engine
        .query()
        .lease()
        .unwrap()
        .page(
            "small-native",
            None,
            50,
            &Default::default(),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(page.paths, [fixture.root.join("small-native.txt")]);
    assert!(page.validated_at_start_and_finish);
}

#[test]
fn supplied_receipt_rejects_self_attestation_stale_scope_source_and_hash() {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};
    let fixture = common::Fixture::new();
    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let directory = std::fs::File::open(&cwd).unwrap();
    let fdinfo =
        std::fs::read_to_string(format!("/proc/self/fdinfo/{}", directory.as_raw_fd())).unwrap();
    let mount = fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))
        .unwrap()
        .trim();
    let metadata = cwd.metadata().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let source = "a".repeat(40);
    let mut hash = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    hash.stdin
        .take()
        .unwrap()
        .write_all(USAGE.as_bytes())
        .unwrap();
    let output = hash.wait_with_output().unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    let digest = output.split_whitespace().next().unwrap();
    let raw = format!("schema=loci.btrfs-capacity.v1\nsource_sha={source}\nrun_id=synthetic-classification-only\ncreated_unix_seconds={now}\nworkdir={}\nworkdir_device={}\nworkdir_inode={}\nworkdir_mount_id={mount}\npilot_entries=100000\npilot_delta_physical=150000000\nusage_sha256={digest}\n\n{USAGE}", cwd.display(), metadata.dev(), metadata.ino());
    let receipt = fixture.base.join("capacity.receipt");
    std::fs::write(&receipt, &raw).unwrap();
    std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o644)).unwrap();
    let verify = || fixture_capacity::provided_receipt_admission(&fixture.base, &receipt, &source);
    if receipt.metadata().unwrap().uid() != 0 {
        assert!(
            verify().is_err(),
            "ordinary users cannot attest root capacity receipts"
        );
        return;
    }
    // This verifies receipt mechanics on the current filesystem. Synthetic usage
    // is never fed to the Btrfs preflight or credited as native Btrfs evidence.
    assert!(verify().is_ok());
    for invalid in [
        raw.replace(
            &format!("created_unix_seconds={now}"),
            &format!("created_unix_seconds={}", now - 601),
        ),
        raw.replace(
            &format!("created_unix_seconds={now}"),
            &format!("created_unix_seconds={}", now + 60),
        ),
        raw.replace(
            &format!("source_sha={source}"),
            &format!("source_sha={}", "b".repeat(40)),
        ),
        raw.replace(
            &format!("workdir_inode={}", metadata.ino()),
            "workdir_inode=0",
        ),
        raw.replace(&format!("workdir_mount_id={mount}"), "workdir_mount_id=0"),
        raw.replace(
            &format!("usage_sha256={digest}"),
            &format!("usage_sha256={}", "0".repeat(64)),
        ),
        format!("source_sha={source}\n{raw}"),
        raw.replace("pilot_delta_physical=150000000", "pilot_delta_physical=0"),
    ] {
        std::fs::write(&receipt, invalid).unwrap();
        assert!(verify().is_err());
    }
    std::fs::write(&receipt, raw).unwrap();
    std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(verify().is_err());
    std::fs::set_permissions(&receipt, std::fs::Permissions::from_mode(0o644)).unwrap();
    let link = fixture.base.join("symlink.receipt");
    std::os::unix::fs::symlink(&receipt, &link).unwrap();
    assert!(fixture_capacity::provided_receipt_admission(&fixture.base, &link, &source).is_err());
}
