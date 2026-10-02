//! Test-side capacity admission for the two opt-in 100k native fixtures.
//! A Btrfs zero inode counter is unavailable evidence, never capacity proof.
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const RESERVED_INODES: u64 = 125_000;
const OUTPUT_BYTES: u64 = 512 * 1024 * 1024;
#[derive(Debug)]
enum Failure {
    Unverified(String),
    Rejected(String),
}
fn unavailable(message: impl ToString) -> Failure {
    Failure::Unverified(message.to_string())
}
fn rejected(message: impl ToString) -> Failure {
    Failure::Rejected(message.to_string())
}
fn command(program: &str, args: &[&str], path: &Path) -> Result<String, Failure> {
    let output = Command::new(program)
        .args(args)
        .arg(path)
        .env("LC_ALL", "C")
        .output()
        .map_err(unavailable)?;
    if !output.status.success() || output.stdout.len() > 64 * 1024 {
        return Err(unavailable(format!(
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout).map_err(unavailable)
}
pub fn reported_budget(total: u64, available: u64, reserve: u64) -> Result<(), String> {
    if total == 0
        || available > total
        || available <= reserve
        || u128::from(available) * 100 < u128::from(total) * 15
    {
        return Err(format!(
            "insufficient reported budget: total={total} available={available} reserve={reserve}"
        ));
    }
    Ok(())
}
/// Returns true only for the confirmed Btrfs unavailable/dynamic inode metric.
/// This classification still requires independent physical metadata admission.
pub fn inode_budget(btrfs: bool, total: u64, available: u64) -> Result<bool, String> {
    if btrfs && total == 0 && available == 0 {
        Ok(true)
    } else {
        reported_budget(total, available, RESERVED_INODES)?;
        Ok(false)
    }
}
fn df(base: &Path, flag: &str) -> Result<(u64, u64), Failure> {
    let text = command("df", &[flag], base)?;
    let values: Vec<_> = text
        .lines()
        .nth(1)
        .ok_or_else(|| unavailable("missing df row"))?
        .split_whitespace()
        .collect();
    if values.len() < 5 {
        return Err(unavailable("invalid df row"));
    }
    Ok((
        values[1].parse().map_err(unavailable)?,
        values[3].parse().map_err(unavailable)?,
    ))
}
#[derive(Debug)]
pub struct Usage {
    device: u64,
    allocated: u64,
    unallocated: u64,
    min_free: u64,
    physical_used: u64,
}
pub fn parse_usage(raw: &str) -> Result<Usage, String> {
    let value = |label: &str| -> Result<u64, String> {
        let rows: Vec<_> = raw
            .lines()
            .filter_map(|line| line.trim().strip_prefix(label))
            .collect();
        if rows.len() != 1 {
            return Err(format!("missing/duplicate Btrfs {label}"));
        }
        rows[0]
            .trim()
            .parse()
            .map_err(|_| format!("non-byte Btrfs {label}"))
    };
    let profile = |prefix: &str, expected: &str| -> Result<(u64, u64), String> {
        let rows: Vec<_> = raw
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with(prefix))
            .collect();
        if rows.len() != 1 {
            return Err(format!("missing/multiple {prefix} profiles"));
        }
        let rest = rows[0]
            .strip_prefix(expected)
            .ok_or("unsupported Btrfs profile")?;
        let (size, used) = rest.trim().split_once(',').ok_or("invalid profile sizes")?;
        let size: u64 = size
            .trim()
            .strip_prefix("Size:")
            .ok_or("missing Size")?
            .trim()
            .parse()
            .map_err(|_| "invalid Size")?;
        let used: u64 = used
            .trim()
            .strip_prefix("Used:")
            .ok_or("missing Used")?
            .split_whitespace()
            .next()
            .ok_or("missing Used bytes")?
            .parse()
            .map_err(|_| "invalid Used")?;
        if used > size {
            return Err("profile Used exceeds Size".into());
        }
        Ok((size, used))
    };
    let device = value("Device size:")?;
    let allocated = value("Device allocated:")?;
    let unallocated = value("Device unallocated:")?;
    if device == 0
        || allocated.checked_add(unallocated) != Some(device)
        || value("Device missing:")? != 0
    {
        return Err("missing/inconsistent Btrfs physical device capacity".into());
    }
    let free: Vec<_> = raw
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Free (estimated):"))
        .collect();
    if free.len() != 1 {
        return Err("missing/duplicate estimated free".into());
    }
    let min_free: u64 = free[0]
        .split_once("(min:")
        .ok_or("missing minimum free")?
        .1
        .trim()
        .strip_suffix(')')
        .ok_or("invalid minimum free")?
        .trim()
        .parse()
        .map_err(|_| "invalid minimum free")?;
    let (_, metadata_used) = profile("Metadata,", "Metadata,DUP:")?;
    let (_, data_used) = profile("Data,", "Data,single:")?;
    let physical_used = metadata_used
        .checked_mul(2)
        .and_then(|n| n.checked_add(data_used))
        .ok_or("physical used overflow")?;
    if physical_used > allocated || min_free > device {
        return Err("inconsistent Btrfs used/minimum-free physical bytes".into());
    }
    Ok(Usage {
        device,
        allocated,
        unallocated,
        min_free,
        physical_used,
    })
}
pub fn admit_pilot(usage: &Usage, entries: u64, delta_physical: u64) -> Result<u64, String> {
    if entries < 1000 || delta_physical == 0 {
        return Err("missing positive measured metadata pilot".into());
    }
    // 1.5x measured physical (DUP metadata plus data) slope, with headroom for
    // 125k entries and an additional 512 MiB for checkpoint, data and logs.
    let denominator = u128::from(entries) * 2;
    let projected =
        (u128::from(delta_physical) * u128::from(RESERVED_INODES) * 3).div_ceil(denominator);
    let extra = u64::try_from(projected + u128::from(OUTPUT_BYTES))
        .map_err(|_| "fixture projection overflow")?;
    if extra > usage.unallocated
        || extra > usage.min_free
        || u128::from(usage.allocated) + u128::from(extra) > u128::from(usage.device) * 85 / 100
    {
        return Err(format!("Btrfs fixture exceeds physical/unallocated/min-free/15% reserve: {usage:?} projected_extra={extra}"));
    }
    Ok(extra)
}
fn mount_id(directory: &Path) -> Result<u64, Failure> {
    let file = File::open(directory).map_err(unavailable)?;
    let text = fs::read_to_string(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))
        .map_err(unavailable)?;
    text.lines()
        .find_map(|line| line.strip_prefix("mnt_id:"))
        .ok_or_else(|| unavailable("missing mount ID"))?
        .trim()
        .parse()
        .map_err(unavailable)
}
fn root_receipt(
    base: &Path,
    receipt: &Path,
    expected_source: &str,
) -> Result<(Usage, u64, u64), Failure> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(0o400000)
        .open(receipt)
        .map_err(unavailable)?; // O_NOFOLLOW
    let metadata = file.metadata().map_err(unavailable)?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 64 * 1024
    {
        return Err(unavailable(
            "capacity receipt must be bounded regular root-owned and not group/world writable",
        ));
    }
    let mut text = String::new();
    file.take(64 * 1024 + 1)
        .read_to_string(&mut text)
        .map_err(unavailable)?;
    if text.len() > 64 * 1024 {
        return Err(unavailable("capacity receipt too large"));
    }
    let (headers, raw) = text
        .split_once("\n\n")
        .ok_or_else(|| unavailable("invalid capacity receipt framing"))?;
    let mut fields = std::collections::HashMap::new();
    for line in headers.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| unavailable("invalid receipt header"))?;
        if fields.insert(key, value).is_some() {
            return Err(unavailable("duplicate receipt header"));
        }
    }
    let field = |key| {
        fields
            .get(key)
            .copied()
            .ok_or_else(|| unavailable(format!("missing receipt {key}")))
    };
    let number = |key| field(key)?.parse::<u64>().map_err(unavailable);
    let source = field("source_sha")?;
    if field("schema")? != "loci.btrfs-capacity.v1"
        || source.len() != 40
        || !source
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || expected_source != source
        || field("run_id")?.is_empty()
    {
        return Err(unavailable(
            "receipt schema/run/source does not bind the frozen native test input",
        ));
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(unavailable)?
        .as_secs();
    if now
        .checked_sub(number("created_unix_seconds")?)
        .is_none_or(|age| age > 600)
    {
        return Err(unavailable("capacity receipt is stale or from the future"));
    }
    let workdir =
        fs::canonicalize(std::env::current_dir().map_err(unavailable)?).map_err(unavailable)?;
    let scope = Path::new(field("workdir")?);
    let scope_metadata = workdir.metadata().map_err(unavailable)?;
    if scope.as_os_str() != workdir.as_os_str()
        || fs::canonicalize(scope).map_err(unavailable)?.as_os_str() != scope.as_os_str()
        || scope_metadata.dev() != number("workdir_device")?
        || scope_metadata.ino() != number("workdir_inode")?
        || mount_id(scope)? != number("workdir_mount_id")?
        || fs::canonicalize(base).map_err(unavailable)? != base
        || !base.starts_with(workdir.join("work"))
        || base.metadata().map_err(unavailable)?.dev() != scope_metadata.dev()
        || mount_id(base)? != number("workdir_mount_id")?
    {
        return Err(unavailable(
            "receipt does not bind this native fixture path/device/mount",
        ));
    }
    let mut digest = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(unavailable)?;
    digest
        .stdin
        .take()
        .unwrap()
        .write_all(raw.as_bytes())
        .map_err(unavailable)?;
    let output = digest.wait_with_output().map_err(unavailable)?;
    let actual = String::from_utf8(output.stdout).map_err(unavailable)?;
    if !output.status.success() || actual.split_whitespace().next() != Some(field("usage_sha256")?)
    {
        return Err(unavailable("receipt raw usage checksum mismatch"));
    }
    eprintln!("LOCI_BTRFS_CAPACITY_RECEIPT_VALIDATED source_sha={source} run_id={} workdir={} mount_id={} raw_usage_sha256={}", field("run_id")?, scope.display(), number("workdir_mount_id")?, field("usage_sha256")?);
    Ok((
        parse_usage(raw).map_err(unavailable)?,
        number("pilot_entries")?,
        number("pilot_delta_physical")?,
    ))
}
fn configured_receipt(base: &Path) -> Result<(Usage, u64, u64), Failure> {
    let receipt = std::env::var_os("LOCI_BTRFS_CAPACITY_RECEIPT")
        .ok_or_else(|| unavailable("no native usage access or root capacity receipt"))?;
    let source = std::env::var("LOCI_TEST_SOURCE_SHA").map_err(unavailable)?;
    root_receipt(base, Path::new(&receipt), &source)
}
/// Validate a supplied environment receipt without elevating the native test.
#[allow(dead_code)] // Direct receipt validation is also exercised by its dedicated tests.
pub fn provided_receipt_admission(
    base: &Path,
    receipt: &Path,
    source: &str,
) -> Result<u64, String> {
    let (usage, entries, delta) =
        root_receipt(base, receipt, source).map_err(|error| format!("{error:?}"))?;
    admit_pilot(&usage, entries, delta)
}
fn direct_pilot(base: &Path) -> Result<(Usage, u64, u64), Failure> {
    let tool = std::env::var("LOCI_TEST_BTRFS_TOOL").unwrap_or_else(|_| "btrfs".into());
    command(&tool, &["filesystem", "sync"], base)?;
    let before_raw = command(&tool, &["filesystem", "usage", "-b"], base)?;
    let before = parse_usage(&before_raw).map_err(unavailable)?;
    // Admit the bounded pilot itself before any writes. A full proof still
    // needs its actual measured delta, not this preliminary 32 MiB allowance.
    let pilot_allowance = OUTPUT_BYTES + 32 * 1024 * 1024;
    if pilot_allowance > before.unallocated
        || pilot_allowance > before.min_free
        || u128::from(before.allocated) + u128::from(pilot_allowance)
            > u128::from(before.device) * 85 / 100
    {
        return Err(rejected(
            "Btrfs lacks physical/15% reserve even for the bounded pilot",
        ));
    }
    let pilot = base.join(".loci-capacity-pilot");
    fs::create_dir(&pilot).map_err(unavailable)?;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove owned capacity pilot");
        }
    }
    let cleanup = Cleanup(pilot.clone());
    for directory in 0..20 {
        let parent = pilot.join(format!("dir-{directory:04}"));
        fs::create_dir(&parent).map_err(unavailable)?;
        for entry in 0..49 {
            File::create(parent.join(format!("metadata-pilot-{entry:08}.txt")))
                .map_err(unavailable)?;
        }
    }
    command(&tool, &["filesystem", "sync"], base)?;
    let after_raw = command(&tool, &["filesystem", "usage", "-b"], base)?;
    let after = parse_usage(&after_raw).map_err(unavailable)?;
    let delta = after
        .physical_used
        .checked_sub(before.physical_used)
        .filter(|delta| *delta > 0)
        .ok_or_else(|| unavailable("Btrfs pilot did not produce a positive physical delta"))?;
    drop(cleanup);
    command(&tool, &["filesystem", "sync"], base)?;
    eprintln!("LOCI_BTRFS_DIRECT_PILOT_BEFORE_BEGIN\n{before_raw}LOCI_BTRFS_DIRECT_PILOT_BEFORE_END\nLOCI_BTRFS_DIRECT_PILOT_AFTER_BEGIN\n{after_raw}LOCI_BTRFS_DIRECT_PILOT_AFTER_END");
    Ok((after, 1000, delta))
}
fn check(base: &Path) -> Result<(), Failure> {
    let (total, available) = df(base, "-Pk")?;
    reported_budget(total, available, 800_000).map_err(rejected)?;
    let btrfs = command("stat", &["-f", "-c", "%t"], base)?.trim() == "9123683e";
    let (total, available) = df(base, "-Pi")?;
    let dynamic = inode_budget(btrfs, total, available).map_err(rejected)?;
    if btrfs {
        let (usage, entries, delta) = if std::env::var_os("LOCI_BTRFS_CAPACITY_RECEIPT").is_some() {
            configured_receipt(base)?
        } else {
            match direct_pilot(base) {
                Ok(proof) => proof,
                Err(Failure::Unverified(_)) => configured_receipt(base)?,
                Err(error) => return Err(error),
            }
        };
        let extra = admit_pilot(&usage, entries, delta).map_err(rejected)?;
        eprintln!("LOCI_BTRFS_FIXTURE_CAPACITY inode_metric={} pilot_entries={entries} pilot_physical_delta={delta} planned_extra_bytes={extra} device_bytes={} unallocated_bytes={} min_free_bytes={}", if dynamic { "unavailable_dynamic" } else { "reported" }, usage.device, usage.unallocated, usage.min_free);
    }
    Ok(())
}
pub fn hundred_thousand_fixture_preflight(base: &Path) -> bool {
    match check(base) {
        Ok(()) => true,
        Err(Failure::Unverified(reason)) => {
            eprintln!("SKIP: UNVERIFIED: 100k native fixture preflight: {reason}");
            false
        }
        Err(Failure::Rejected(reason)) => panic!("100k native fixture capacity rejected: {reason}"),
    }
}
