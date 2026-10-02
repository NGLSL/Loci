//! Explicit single-root mount scope. Mount IDs distinguish same-device bind mounts.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
const MAX_MOUNTINFO: u64 = 16 * 1024 * 1024;
pub(super) fn input_identity() -> io::Result<(u64, u64)> {
    let metadata = fs::metadata("/proc/self/mountinfo")?;
    Ok((metadata.dev(), metadata.ino()))
}
#[derive(PartialEq, Eq)]
pub(super) struct Scope {
    pub root_mount: u64,
    nested: BTreeMap<PathBuf, u64>,
}
pub(super) fn mount_id(file: &File) -> io::Result<u64> {
    let text = fs::read_to_string(format!("/proc/self/fdinfo/{}", file.as_raw_fd()))?;
    text.lines()
        .find_map(|line| {
            line.strip_prefix("mnt_id:")
                .and_then(|value| value.trim().parse().ok())
        })
        .ok_or_else(|| io::Error::other("cannot determine selected root mount identity"))
}
impl Scope {
    pub fn read(root: &Path) -> io::Result<Self> {
        let file = crate::engine::open_linux_root(root)?;
        let root_mount = mount_id(&file)?;
        let mut bytes = Vec::new();
        File::open("/proc/self/mountinfo")?
            .take(MAX_MOUNTINFO + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MOUNTINFO {
            return Err(io::Error::other("mount scope table budget exhausted"));
        }
        let mut nested = BTreeMap::new();
        let mut selected_found = false;
        for line in bytes.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
            let fields: Vec<_> = line.split(|byte| *byte == b' ').take(6).collect();
            if fields.len() < 6 || !line.windows(3).any(|field| field == b" - ") {
                return Err(io::Error::other("unrecognized mount scope table record"));
            }
            let id = std::str::from_utf8(fields[0])
                .ok()
                .and_then(|field| field.parse::<u64>().ok())
                .ok_or_else(|| io::Error::other("invalid mount scope identity"))?;
            let mountpoint = PathBuf::from(OsString::from_vec(unescape(fields[4])?));
            if !mountpoint.is_absolute() {
                return Err(io::Error::other("invalid mount scope path"));
            }
            if id == root_mount {
                selected_found = root.starts_with(&mountpoint);
            }
            if mountpoint != root && mountpoint.starts_with(root) {
                nested.insert(mountpoint, id);
            }
        }
        if !selected_found {
            return Err(io::Error::other(
                "selected root mount not represented in mount scope table",
            ));
        }
        Ok(Self { root_mount, nested })
    }
    pub fn changed_boundaries(&self, other: &Self) -> Vec<PathBuf> {
        self.nested
            .keys()
            .chain(other.nested.keys())
            .filter(|path| self.nested.get(*path) != other.nested.get(*path))
            .take(256)
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn boundary(&self, absolute: &Path) -> bool {
        self.nested.contains_key(absolute)
    }
}
fn unescape(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        if i + 3 >= bytes.len()
            || !bytes[i + 1..i + 4]
                .iter()
                .all(|byte| (b'0'..=b'7').contains(byte))
        {
            return Err(io::Error::other("invalid escaped mount scope path"));
        }
        let value = (u16::from(bytes[i + 1] - b'0') << 6)
            | (u16::from(bytes[i + 2] - b'0') << 3)
            | u16::from(bytes[i + 3] - b'0');
        if value > 255 || value == 0 {
            return Err(io::Error::other("invalid mount scope path byte"));
        }
        out.push(value as u8);
        i += 4;
    }
    Ok(out)
}
