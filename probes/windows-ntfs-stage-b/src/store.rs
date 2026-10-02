//! Bounded, atomic inventory and platform cursor. Names remain raw UTF-16.
use crate::{checkpoint::Checkpoint, model::EntryId};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read},
    path::Path,
};

const MAGIC: &[u8; 8] = b"LCUSNB2\0";
const VERSION: u32 = 2;
const HEADER: usize = 68;
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 32768;
const MAX_SCOPE: usize = 32768;
const MAX_GUID: usize = 128;
const MAX_DEPTH: usize = 64;
const MAX_PATH_BYTES: usize = 32 * 1024 * 1024;
const DIRECTORY: u32 = 0x10;
const REPARSE: u32 = 0x400;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub checkpoint: Checkpoint,
    pub root: u128,
    pub scope: Vec<u16>,
    pub volume_guid: String,
    pub entries: BTreeMap<EntryId, u32>,
}

fn rebuild(message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("snapshot requires rebuild: {message}"),
    )
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

fn component(name: &[u16]) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != [46]
        && name != [46, 46]
        && !name.iter().any(|c| [0, 47, 58, 92].contains(c))
}

fn valid_guid(guid: &str) -> bool {
    let Some(id) = guid
        .strip_prefix(r"\\?\Volume{")
        .and_then(|s| s.strip_suffix("}\\"))
    else {
        return false;
    };
    id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}

/// Validates the namespace and caches each directory prefix once. The root is
/// implicit: entries describe names beneath it, never a second root object.
fn directory_paths(
    root: u128,
    entries: &BTreeMap<EntryId, u32>,
) -> io::Result<BTreeMap<u128, (Vec<u16>, usize)>> {
    if root == 0 || entries.len() > MAX_ENTRIES {
        return Err(rebuild("invalid root or inventory budget"));
    }
    let mut objects = BTreeMap::new();
    let mut directories = BTreeMap::new();
    let mut names = BTreeSet::new();
    for (entry, attributes) in entries {
        if entry.object == 0
            || entry.parent == 0
            || entry.object == root
            || entry.object == entry.parent
            || !component(&entry.name)
        {
            return Err(rebuild("invalid entry identity/name"));
        }
        if !names.insert((entry.parent, &entry.name)) {
            return Err(rebuild("duplicate parent/name namespace entry"));
        }
        if objects
            .insert(entry.object, *attributes)
            .is_some_and(|previous| previous != *attributes)
        {
            return Err(rebuild("conflicting attributes for one object"));
        }
        if attributes & DIRECTORY != 0 && directories.insert(entry.object, entry).is_some() {
            return Err(rebuild("directory object has multiple name relationships"));
        }
    }
    for entry in entries.keys() {
        if entry.parent != root
            && objects
                .get(&entry.parent)
                .is_none_or(|attr| attr & (DIRECTORY | REPARSE) != DIRECTORY)
        {
            return Err(rebuild("missing, non-directory or reparse parent"));
        }
    }
    let mut cache = BTreeMap::from([(root, (Vec::new(), 0usize))]);
    let mut cached_bytes = 0usize;
    for object in directories.keys() {
        let mut chain = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = *object;
        while !cache.contains_key(&current) {
            if !seen.insert(current) {
                return Err(rebuild("directory parent cycle"));
            }
            if chain.len() >= MAX_DEPTH {
                return Err(rebuild("directory depth exceeds 64"));
            }
            let entry = directories
                .get(&current)
                .ok_or_else(|| rebuild("directory chain does not reach root"))?;
            chain.push(*entry);
            current = entry.parent;
        }
        for entry in chain.into_iter().rev() {
            let (parent_path, parent_depth) = cache.get(&entry.parent).unwrap();
            if *parent_depth >= MAX_DEPTH {
                return Err(rebuild("directory depth exceeds 64"));
            }
            let mut path = parent_path.clone();
            if !path.is_empty() {
                path.push(92);
            }
            path.extend_from_slice(&entry.name);
            cached_bytes += path.len() * 2;
            if path.len() > MAX_SCOPE || cached_bytes > MAX_PATH_BYTES {
                return Err(rebuild("derived directory path budget exceeded"));
            }
            let depth = parent_depth + 1;
            cache.insert(entry.object, (path, depth));
        }
    }
    let mut path_bytes = 0usize;
    for entry in entries.keys() {
        let (parent_path, parent_depth) = cache
            .get(&entry.parent)
            .ok_or_else(|| rebuild("missing directory prefix"))?;
        let units = parent_path.len() + usize::from(!parent_path.is_empty()) + entry.name.len();
        path_bytes += units * 2;
        if parent_depth + 1 > MAX_DEPTH || units > MAX_SCOPE || path_bytes > MAX_PATH_BYTES {
            return Err(rebuild("entry depth or derived path budget exceeded"));
        }
    }
    Ok(cache)
}

/// Raw relative paths, including all distinct hard-link directory entries.
/// Materialized output is separately bounded to 32 MiB of UTF-16 payload.
pub fn paths(root: u128, entries: &BTreeMap<EntryId, u32>) -> io::Result<BTreeSet<Vec<u16>>> {
    let directories = directory_paths(root, entries)?;
    let mut result = BTreeSet::new();
    let mut bytes = 0usize;
    for entry in entries.keys() {
        let mut path = directories.get(&entry.parent).unwrap().0.clone();
        if !path.is_empty() {
            path.push(92);
        }
        path.extend_from_slice(&entry.name);
        bytes += path.len() * 2;
        if bytes > MAX_PATH_BYTES {
            return Err(rebuild("derived path output exceeds 32 MiB"));
        }
        if !result.insert(path) {
            return Err(rebuild("duplicate derived path"));
        }
    }
    Ok(result)
}

impl Snapshot {
    pub fn validate(&self) -> io::Result<()> {
        self.encoded_size().map(|_| ())
    }

    fn encoded_size(&self) -> io::Result<usize> {
        if self.checkpoint.cursor < 0 {
            return Err(rebuild("negative USN cursor"));
        }
        if self.root == 0
            || self.scope.is_empty()
            || self.scope.len() > MAX_SCOPE
            || self.scope.contains(&0)
            || !valid_guid(&self.volume_guid)
            || self.entries.len() > MAX_ENTRIES
        {
            return Err(rebuild("invalid root/scope or inventory budget"));
        }
        let mut size = HEADER + self.scope.len() * 2 + self.volume_guid.len() + 8;
        for entry in self.entries.keys() {
            size += 40 + entry.name.len() * 2;
            if size > MAX_BYTES {
                return Err(rebuild("snapshot exceeds 8 MiB budget"));
            }
        }
        directory_paths(self.root, &self.entries)?;
        Ok(size)
    }

    pub fn encode(&self) -> io::Result<Vec<u8>> {
        let size = self.encoded_size()?;
        let mut b = Vec::with_capacity(size);
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&(size as u32).to_le_bytes());
        b.extend_from_slice(&self.checkpoint.volume_serial.to_le_bytes());
        b.extend_from_slice(&self.checkpoint.journal_id.to_le_bytes());
        b.extend_from_slice(&self.checkpoint.cursor.to_le_bytes());
        b.extend_from_slice(&self.root.to_le_bytes());
        b.extend_from_slice(&(self.scope.len() as u32).to_le_bytes());
        b.extend_from_slice(&(self.entries.len() as u32).to_le_bytes());
        b.extend_from_slice(&(self.volume_guid.len() as u32).to_le_bytes());
        for unit in &self.scope {
            b.extend_from_slice(&unit.to_le_bytes());
        }
        b.extend_from_slice(self.volume_guid.as_bytes());
        for (entry, attributes) in &self.entries {
            b.extend_from_slice(&1u16.to_le_bytes()); // name relationship record
            b.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
            b.extend_from_slice(&attributes.to_le_bytes());
            b.extend_from_slice(&entry.parent.to_le_bytes());
            b.extend_from_slice(&entry.object.to_le_bytes());
            for unit in &entry.name {
                b.extend_from_slice(&unit.to_le_bytes());
            }
        }
        b.extend_from_slice(&checksum(&b).to_le_bytes());
        debug_assert_eq!(b.len(), size);
        Ok(b)
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < HEADER + 8 || bytes.len() > MAX_BYTES {
            return Err(rebuild("invalid snapshot length/budget"));
        }
        let end = bytes.len() - 8;
        if checksum(&bytes[..end]) != u64::from_le_bytes(bytes[end..].try_into().unwrap()) {
            return Err(rebuild("snapshot checksum mismatch"));
        }
        let mut r = Reader {
            bytes: &bytes[..end],
            offset: 0,
        };
        if r.take::<8>()? != *MAGIC || r.u32()? != VERSION {
            return Err(rebuild("unknown snapshot magic/version"));
        }
        if r.u32()? as usize != bytes.len() {
            return Err(rebuild("declared snapshot length mismatch"));
        }
        let checkpoint = Checkpoint {
            volume_serial: r.u64()?,
            journal_id: r.u64()?,
            cursor: i64::from_le_bytes(r.take()?),
        };
        let root = r.u128()?;
        let scope_len = r.u32()? as usize;
        let count = r.u32()? as usize;
        let guid_len = r.u32()? as usize;
        if scope_len > MAX_SCOPE || count > MAX_ENTRIES || guid_len > MAX_GUID {
            return Err(rebuild("declared scope/inventory exceeds budget"));
        }
        let scope = r.name(scope_len)?;
        let guid_bytes = r.slice(guid_len)?;
        let volume_guid = std::str::from_utf8(guid_bytes)
            .map_err(|_| rebuild("invalid UTF-8 volume GUID"))?
            .to_owned();
        // Every entry needs its 40-byte header and at least one UTF-16 unit.
        if count > (r.bytes.len() - r.offset) / 42 {
            return Err(rebuild("declared entry count exceeds remaining bytes"));
        }
        let mut entries = BTreeMap::new();
        for _ in 0..count {
            if r.u16()? != 1 {
                return Err(rebuild("unknown snapshot record type"));
            }
            let name_len = r.u16()? as usize;
            if name_len > 255 {
                return Err(rebuild("entry name exceeds budget"));
            }
            let attributes = r.u32()?;
            let entry = EntryId {
                parent: r.u128()?,
                object: r.u128()?,
                name: r.name(name_len)?,
            };
            if entries.insert(entry, attributes).is_some() {
                return Err(rebuild("duplicate snapshot record"));
            }
        }
        if r.offset != end {
            return Err(rebuild("trailing or unknown snapshot data"));
        }
        let snapshot = Self {
            checkpoint,
            root,
            scope,
            volume_guid,
            entries,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        crate::checkpoint::atomic_save(path, &self.encode()?)
    }

    pub fn save_new(&self, path: &Path) -> io::Result<()> {
        crate::checkpoint::atomic_create(path, &self.encode()?)
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl Reader<'_> {
    fn slice(&mut self, len: usize) -> io::Result<&[u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| rebuild("length overflow"))?;
        let data = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| rebuild("truncated snapshot"))?;
        self.offset = end;
        Ok(data)
    }
    fn take<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or_else(|| rebuild("length overflow"))?;
        let data = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| rebuild("truncated snapshot"))?;
        self.offset = end;
        Ok(data.try_into().unwrap())
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn u128(&mut self) -> io::Result<u128> {
        Ok(u128::from_le_bytes(self.take()?))
    }
    fn name(&mut self, len: usize) -> io::Result<Vec<u16>> {
        if len > (self.bytes.len() - self.offset) / 2 {
            return Err(rebuild("truncated UTF-16 field"));
        }
        (0..len).map(|_| self.u16()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(parent: u128, object: u128, name: &[u16]) -> EntryId {
        EntryId {
            parent,
            object,
            name: name.to_vec(),
        }
    }
    fn fixture() -> Snapshot {
        let object = (1u128 << 120) | 42;
        Snapshot {
            checkpoint: Checkpoint {
                volume_serial: 7,
                journal_id: 11,
                cursor: 99,
            },
            root: 1,
            scope: vec![68, 58, 92, 0xd800],
            volume_guid: String::from(r"\\?\Volume{3b1b89c0-c213-495c-b692-f1e2d9987c9d}\"),
            entries: BTreeMap::from([
                (entry(1, 2, &[65]), DIRECTORY),
                (entry(1, object, &[0xd800, 66]), 0x20),
                (entry(2, object, &[67]), 0x20),
            ]),
        }
    }
    fn rehash(b: &mut [u8]) {
        let end = b.len() - 8;
        let hash = checksum(&b[..end]);
        b[end..].copy_from_slice(&hash.to_le_bytes());
    }
    #[test]
    fn codec_preserves_hardlinks_raw_names_and_full_identity() {
        let snapshot = fixture();
        assert_eq!(
            Snapshot::decode(&snapshot.encode().unwrap()).unwrap(),
            snapshot
        );
        assert_eq!(
            paths(snapshot.root, &snapshot.entries).unwrap(),
            BTreeSet::from([vec![65], vec![0xd800, 66], vec![65, 92, 67],])
        );
    }
    #[test]
    fn rejects_corruption_unknown_versions_lengths_and_records() {
        let good = fixture().encode().unwrap();
        for length in [0, 63, good.len() - 1] {
            assert!(Snapshot::decode(&good[..length]).is_err());
        }
        for offset in [
            0,
            8,
            12,
            HEADER + fixture().scope.len() * 2 + fixture().volume_guid.len(),
        ] {
            let mut b = good.clone();
            b[offset] ^= 0x80;
            rehash(&mut b);
            assert!(Snapshot::decode(&b).is_err());
        }
        let mut b = good.clone();
        b[24] ^= 1;
        assert!(Snapshot::decode(&b).is_err());
        assert!(Snapshot::decode(&vec![0; MAX_BYTES + 1]).is_err());
        let mut b = good.clone();
        b.extend([0; 8]);
        let len = b.len() as u32;
        b[12..16].copy_from_slice(&len.to_le_bytes());
        rehash(&mut b);
        assert!(Snapshot::decode(&b).is_err());
    }
    #[test]
    fn rejects_invalid_graphs_names_and_conflicting_objects() {
        let good = fixture();
        for (e, attr) in [
            (entry(1, 1, &[68]), 0),
            (entry(4, 4, &[68]), DIRECTORY),
            (entry(99, 4, &[68]), 0),
            (entry(1, 3, &[65]), 0),
            (entry(1, 2, &[68]), DIRECTORY),
            (entry(1, (1u128 << 120) | 42, &[68]), DIRECTORY),
        ] {
            let mut s = good.clone();
            s.entries.insert(e, attr);
            assert!(s.encode().is_err());
        }
        for name in [
            vec![],
            vec![0],
            vec![47],
            vec![58],
            vec![92],
            vec![46],
            vec![46, 46],
            vec![65; 256],
        ] {
            let mut s = good.clone();
            s.entries.insert(entry(1, 4, &name), 0);
            assert!(s.encode().is_err());
        }
        let mut s = good.clone();
        *s.entries.get_mut(&entry(1, 2, &[65])).unwrap() = DIRECTORY | REPARSE;
        assert!(s.encode().is_err());
        let mut s = good;
        s.entries = BTreeMap::from([
            (entry(3, 2, &[65]), DIRECTORY),
            (entry(2, 3, &[66]), DIRECTORY),
        ]);
        assert!(s.encode().is_err());
    }
    #[test]
    fn rejects_duplicate_records_and_budgets() {
        let mut s = fixture();
        s.entries = BTreeMap::from([(entry(1, 2, &[65]), 0)]);
        let mut b = s.encode().unwrap();
        let record = b[HEADER + s.scope.len() * 2 + s.volume_guid.len()..b.len() - 8].to_vec();
        b.truncate(b.len() - 8);
        b.extend(record);
        b.extend([0; 8]);
        let len = b.len() as u32;
        b[12..16].copy_from_slice(&len.to_le_bytes());
        b[60..64].copy_from_slice(&2u32.to_le_bytes());
        rehash(&mut b);
        assert!(Snapshot::decode(&b).is_err());
        s.scope = vec![65; MAX_SCOPE + 1];
        assert!(s.encode().is_err());
        s.scope = vec![65];
        s.checkpoint.cursor = -1;
        assert!(s.encode().is_err());
        s.checkpoint.cursor = 1;
        s.entries.clear();
        for object in 2..=(MAX_ENTRIES as u128 + 2) {
            let name: Vec<u16> = object.to_string().encode_utf16().collect();
            s.entries.insert(entry(1, object, &name), 0);
        }
        assert!(s.encode().is_err());
        s.entries.clear();
        for object in 2..20002 {
            let mut name: Vec<u16> = object.to_string().encode_utf16().collect();
            name.resize(255, 65);
            s.entries.insert(entry(1, object, &name), 0);
        }
        assert!(s.encode().is_err());
    }

    #[test]
    fn directory_depth_and_path_api_reject_invalid_graphs() {
        let mut s = fixture();
        s.entries.clear();
        for depth in 1..=64u128 {
            s.entries.insert(entry(depth, depth + 1, &[65]), DIRECTORY);
        }
        assert!(s.validate().is_ok());
        assert_eq!(paths(s.root, &s.entries).unwrap().len(), 64);
        s.entries.insert(entry(65, 66, &[66]), 0x20);
        assert!(s.validate().is_err());
        assert!(paths(s.root, &s.entries).is_err());
        assert!(paths(1, &BTreeMap::from([(entry(7, 2, &[65]), 0)])).is_err());
        assert!(paths(
            1,
            &BTreeMap::from([
                (entry(3, 2, &[65]), DIRECTORY),
                (entry(2, 3, &[66]), DIRECTORY),
            ])
        )
        .is_err());
    }

    #[test]
    fn rejects_invalid_guid_and_declared_allocation_lengths() {
        let good = fixture();
        for guid in [
            "",
            "D:",
            "\\\\?\\Volume{not-a-guid}\\",
            "\\\\?\\Volume{3b1b89c0-c213-495c-b692-f1e2d9987c9d}\\\0",
        ] {
            let mut s = good.clone();
            s.volume_guid = guid.into();
            assert!(s.validate().is_err());
        }
        for offset in [56, 60, 64] {
            let mut b = good.encode().unwrap();
            b[offset..offset + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            rehash(&mut b);
            assert!(Snapshot::decode(&b).is_err());
        }
        let mut b = good.encode().unwrap();
        b[60..64].copy_from_slice(&(MAX_ENTRIES as u32).to_le_bytes());
        rehash(&mut b);
        assert!(Snapshot::decode(&b).is_err());
    }

    #[test]
    fn atomic_public_api_preserves_inventory_and_cursor_on_failures() {
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };
        let base =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-b/run");
        // The caller chooses an engineering root; reject redirection before writes.
        let base = std::path::absolute(base).unwrap();
        let mut prefix = std::path::PathBuf::new();
        for part in base.components() {
            prefix.push(part);
            if !prefix.exists() {
                fs::create_dir(&prefix).unwrap();
            }
            let meta = fs::symlink_metadata(&prefix).unwrap();
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                assert_eq!(
                    meta.file_attributes() & REPARSE,
                    0,
                    "test root cannot contain reparse ancestors"
                );
            }
            assert!(meta.is_dir());
        }
        let dir = base.join(format!(
            "store-unit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("inventory.lcusn");
        let old = fixture();
        old.save_new(&path).unwrap();
        assert_eq!(Snapshot::load(&path).unwrap(), old);
        let mut next = old.clone();
        next.checkpoint.cursor += 1;
        next.entries.insert(entry(1, 99, &[68]), 0x20);
        let create_error = next.save_new(&path).unwrap_err();
        assert!(create_error.raw_os_error().is_some());
        println!(
            "store atomic_create occupied raw_os_code={:?}",
            create_error.raw_os_error()
        );
        assert_eq!(Snapshot::load(&path).unwrap(), old);
        #[cfg(windows)]
        {
            use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
            let locked = OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap();
            let replace_error = next.save(&path).unwrap_err();
            assert!(replace_error.raw_os_error().is_some());
            println!(
                "store atomic_save locked raw_os_code={:?}",
                replace_error.raw_os_error()
            );
            drop(locked);
            assert_eq!(Snapshot::load(&path).unwrap(), old);
        }
        next.save(&path).unwrap();
        let reopened = Snapshot::load(&path).unwrap();
        assert_eq!(reopened, next);
        let journal = crate::model::Journal {
            id: next.checkpoint.journal_id,
            first: next.checkpoint.cursor + 1,
            next: next.checkpoint.cursor + 20,
            lowest: 0,
            min_major: 2,
            max_major: 4,
        };
        assert!(
            reopened
                .checkpoint
                .validate(next.checkpoint.volume_serial, &journal)
                .is_err(),
            "injected retention gap requires rebuild"
        );
        let mut invalid = next.clone();
        invalid.entries.insert(entry(999, 100, &[69]), 0);
        assert!(invalid.save(&path).is_err());
        assert_eq!(Snapshot::load(&path).unwrap(), next);
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "failed atomic updates leave no temporary files"
        );
        // Keep this tiny fixture as inspectable test evidence; no recursive deletion.
    }
}
