//! Bounded, atomic inventory and platform cursor. Names remain raw UTF-16.
use crate::{checkpoint::Checkpoint, model::EntryId};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read},
    path::Path,
};

const MAGIC: &[u8; 8] = b"LCUSNSN\0";
const VERSION: u32 = 1;
const HEADER: usize = 64;
const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 8192;
const MAX_SCOPE: usize = 32768;
const DIRECTORY: u32 = 0x10;
const REPARSE: u32 = 0x400;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub checkpoint: Checkpoint,
    pub root: u128,
    pub scope: Vec<u16>,
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

impl Snapshot {
    fn validate(&self) -> io::Result<usize> {
        if self.checkpoint.cursor < 0 {
            return Err(rebuild("negative USN cursor"));
        }
        if self.root == 0
            || self.scope.is_empty()
            || self.scope.len() > MAX_SCOPE
            || self.scope.contains(&0)
            || self.entries.len() > MAX_ENTRIES
        {
            return Err(rebuild("invalid root/scope or inventory budget"));
        }
        let mut size = HEADER + self.scope.len() * 2 + 8;
        let mut objects = BTreeMap::<u128, u32>::new();
        let mut directories = BTreeMap::<u128, u128>::new();
        let mut names = BTreeSet::new();
        for (entry, attributes) in &self.entries {
            if entry.object == 0
                || entry.parent == 0
                || entry.object == self.root
                || entry.object == entry.parent
                || !component(&entry.name)
            {
                return Err(rebuild("invalid entry identity/name or root entry"));
            }
            if !names.insert((entry.parent, &entry.name)) {
                return Err(rebuild("duplicate parent/name namespace entry"));
            }
            if let Some(previous) = objects.insert(entry.object, *attributes) {
                if previous != *attributes {
                    return Err(rebuild("conflicting attributes for one object"));
                }
            }
            if attributes & DIRECTORY != 0
                && directories.insert(entry.object, entry.parent).is_some()
            {
                return Err(rebuild("directory object has multiple name relationships"));
            }
            size += 40 + entry.name.len() * 2;
            if size > MAX_BYTES {
                return Err(rebuild("snapshot exceeds 2 MiB budget"));
            }
        }
        for entry in self.entries.keys() {
            if entry.parent != self.root
                && objects
                    .get(&entry.parent)
                    .is_none_or(|attr| attr & (DIRECTORY | REPARSE) != DIRECTORY)
            {
                return Err(rebuild("missing, non-directory or reparse parent"));
            }
        }
        // Iterative parent validation avoids attacker-controlled recursion.
        let mut rooted = BTreeSet::from([self.root]);
        for object in directories.keys() {
            let mut chain = BTreeSet::new();
            let mut current = *object;
            while !rooted.contains(&current) {
                if !chain.insert(current) {
                    return Err(rebuild("directory parent cycle"));
                }
                current = *directories
                    .get(&current)
                    .ok_or_else(|| rebuild("directory chain does not reach root"))?;
            }
            rooted.extend(chain);
        }
        Ok(size)
    }

    fn encode(&self) -> io::Result<Vec<u8>> {
        let size = self.validate()?;
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
        for unit in &self.scope {
            b.extend_from_slice(&unit.to_le_bytes());
        }
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

    fn decode(bytes: &[u8]) -> io::Result<Self> {
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
        if scope_len > MAX_SCOPE || count > MAX_ENTRIES {
            return Err(rebuild("declared scope/inventory exceeds budget"));
        }
        let scope = r.name(scope_len)?;
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
    }
    #[test]
    fn rejects_corruption_unknown_versions_lengths_and_records() {
        let good = fixture().encode().unwrap();
        for length in [0, 63, good.len() - 1] {
            assert!(Snapshot::decode(&good[..length]).is_err());
        }
        for offset in [0, 8, 12, 64 + fixture().scope.len() * 2] {
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
        let record = b[HEADER + s.scope.len() * 2..b.len() - 8].to_vec();
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
        for object in 2..8194 {
            let mut name: Vec<u16> = object.to_string().encode_utf16().collect();
            name.resize(255, 65);
            s.entries.insert(entry(1, object, &name), 0);
        }
        assert!(s.encode().is_err());
    }
}
