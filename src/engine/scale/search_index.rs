//! Snapshot-owned derived filters. Full normalized paths are cached only for
//! directories; other entries retain compact trigram and pair filters.
use super::inventory::{EntryId, Kind};
use crate::signatures::{trigram_signature, PairSignature, ShortSignature};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const SEGMENT: usize = 1024;
const SHORT_BLOCK: usize = 64;
#[derive(Clone)]
struct Filters {
    grams: [u128; SEGMENT],
    pairs: [PairSignature; SEGMENT],
    short: [ShortSignature; SEGMENT / SHORT_BLOCK],
    ready: [u64; SEGMENT / 64],
}
impl Default for Filters {
    fn default() -> Self {
        Self {
            grams: [0; SEGMENT],
            pairs: [PairSignature::default(); SEGMENT],
            short: std::array::from_fn(|_| ShortSignature::default()),
            ready: [0; SEGMENT / 64],
        }
    }
}
#[derive(Clone)]
pub(super) struct SearchIndex {
    filters: Vec<Arc<Filters>>,
    directories: Arc<HashMap<EntryId, Arc<Vec<u8>>>>,
    pub derived: usize,
    allocated: usize,
}
impl Default for SearchIndex {
    fn default() -> Self {
        Self {
            filters: Vec::new(),
            directories: Arc::new(HashMap::new()),
            derived: 0,
            allocated: std::mem::size_of::<HashMap<EntryId, Arc<Vec<u8>>>>() + 16,
        }
    }
}
/// Lowercase each valid run separately. FF cannot occur in valid UTF-8, and
/// prevents accidental adjacency across one or more undecodable bytes.
fn append_normalized(raw: &[u8], out: &mut Vec<u8>) {
    if raw.is_ascii() {
        out.extend(raw.iter().map(u8::to_ascii_lowercase));
        return;
    }
    let mut rest = raw;
    while !rest.is_empty() {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.extend_from_slice(text.to_lowercase().as_bytes());
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                out.extend_from_slice(
                    std::str::from_utf8(&rest[..valid])
                        .unwrap()
                        .to_lowercase()
                        .as_bytes(),
                );
                out.push(255);
                let invalid = error.error_len().unwrap_or(rest.len() - valid);
                rest = &rest[valid + invalid..];
            }
        }
    }
}
impl SearchIndex {
    pub fn allocated_bytes(&self) -> usize {
        self.allocated
    }
    pub fn accounted_bytes(&self, seen: &mut HashSet<usize>) -> usize {
        let mut bytes = self.filters.capacity() * std::mem::size_of::<Arc<Filters>>();
        for filter in &self.filters {
            if seen.insert(Arc::as_ptr(filter) as usize) {
                bytes += std::mem::size_of::<Filters>() + 2 * std::mem::size_of::<usize>();
            }
        }
        if seen.insert(Arc::as_ptr(&self.directories) as usize) {
            bytes += std::mem::size_of::<HashMap<EntryId, Arc<Vec<u8>>>>()
                + 16
                + self.directories.capacity() * 64;
        }
        for prefix in self.directories.values() {
            if seen.insert(Arc::as_ptr(prefix) as usize) {
                bytes += std::mem::size_of::<Vec<u8>>()
                    + prefix.capacity()
                    + 2 * std::mem::size_of::<usize>();
            }
        }
        bytes
    }
    pub fn has_parent(&self, parent: EntryId) -> bool {
        self.directories.contains_key(&parent)
    }
    pub fn allocation(&self, id: EntryId, parent: EntryId, kind: Kind, name: &[u8]) -> usize {
        let block = id as usize / SEGMENT;
        let mut bytes = 0;
        if block >= self.filters.len() {
            bytes += std::mem::size_of::<Filters>() + 16;
            if self.filters.len() == self.filters.capacity() {
                bytes += self.filters.capacity().max(4) * std::mem::size_of::<Arc<Filters>>();
            }
        } else if Arc::strong_count(&self.filters[block]) > 1 {
            bytes += std::mem::size_of::<Filters>() + 16;
        }
        if kind == Kind::Directory {
            if Arc::strong_count(&self.directories) > 1 {
                bytes += self.directories.capacity() * 64
                    + std::mem::size_of::<HashMap<EntryId, Arc<Vec<u8>>>>();
            }
            if self.directories.len() == self.directories.capacity() {
                bytes += self.directories.capacity().max(4) * 128;
            }
            // Unicode lowercase expands at most threefold; include Vec and Arc.
            bytes += self.directories.get(&parent).map_or(1, |p| p.len()) + name.len() * 3 + 1 + 64;
        }
        bytes
    }
    pub fn set(&mut self, id: EntryId, parent: EntryId, kind: Kind, name: &[u8]) {
        let mut path = self.normalized_path(parent, name);
        let block = id as usize / SEGMENT;
        let old_capacity = self.filters.capacity();
        while self.filters.len() <= block {
            self.filters.push(Arc::new(Filters::default()));
            self.allocated += std::mem::size_of::<Filters>() + 16;
        }
        self.allocated +=
            (self.filters.capacity() - old_capacity) * std::mem::size_of::<Arc<Filters>>();
        let filter = Arc::make_mut(&mut self.filters[block]);
        let word = id as usize % SEGMENT / 64;
        let bit = 1u64 << (id as usize % 64);
        if filter.ready[word] & bit == 0 {
            self.derived += 1;
            filter.ready[word] |= bit;
        }
        filter.grams[id as usize % SEGMENT] = trigram_signature(&path);
        let mut entry_pairs = PairSignature::default();
        entry_pairs.insert(&path);
        filter.pairs[id as usize % SEGMENT] = entry_pairs;
        // Union-only block filters remain safe when an entry is renamed/deleted.
        filter.short[id as usize % SEGMENT / SHORT_BLOCK].insert(&path);
        if kind == Kind::Directory {
            if id != 0 {
                path.push(b'/');
            }
            path.shrink_to_fit();
            let old_capacity = self.directories.capacity();
            let new_bytes = std::mem::size_of::<Vec<u8>>() + path.capacity() + 16;
            let old = Arc::make_mut(&mut self.directories).insert(id, Arc::new(path));
            self.allocated = self.allocated + new_bytes + self.directories.capacity() * 64
                - old_capacity * 64
                - old.map_or(0, |prefix| {
                    std::mem::size_of::<Vec<u8>>() + prefix.capacity() + 16
                });
        }
    }
    pub fn normalized_path(&self, parent: EntryId, name: &[u8]) -> Vec<u8> {
        let mut path = Vec::new();
        self.fill_path(parent, name, &mut path);
        path
    }
    pub fn fill_path(&self, parent: EntryId, name: &[u8], path: &mut Vec<u8>) {
        path.clear();
        path.extend_from_slice(
            self.directories
                .get(&parent)
                .map_or(b"/".as_slice(), |prefix| prefix.as_slice()),
        );
        append_normalized(name, path);
    }
    pub fn admits(
        &self,
        id: EntryId,
        grams: u128,
        short: &ShortSignature,
        pairs: &PairSignature,
    ) -> bool {
        let filter = &self.filters[id as usize / SEGMENT];
        filter.grams[id as usize % SEGMENT] & grams == grams
            && filter.pairs[id as usize % SEGMENT].contains(pairs)
            && filter.short[id as usize % SEGMENT / SHORT_BLOCK].contains(short)
    }
}
