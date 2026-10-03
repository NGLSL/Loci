use crate::engine::ScaleBudgets;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const BLOCK: usize = 1024;
pub(super) type EntryId = u32;
// Preserve all parent/hash bits without the u64 alignment padding of a tuple.
// With the u32 entry value, each lookup bucket is 16 rather than 24 bytes.
#[derive(Clone, Copy, Hash, PartialEq, Eq)]
struct LookupKey([u8; 12]);
impl LookupKey {
    fn new(parent: EntryId, name_hash: u64) -> Self {
        let mut bytes = [0; 12];
        bytes[..4].copy_from_slice(&parent.to_le_bytes());
        bytes[4..].copy_from_slice(&name_hash.to_le_bytes());
        Self(bytes)
    }
}

// Most scan directories have small child lists. Keep their IDs in the writer
// hash table rather than separate persistent allocator chunks that pin free
// pages between maintenance epochs. Large directories retain a Vec fallback.
struct Children {
    inline: [EntryId; 64],
    length: usize,
    overflow: Vec<EntryId>,
}
impl Default for Children {
    fn default() -> Self {
        Self {
            inline: [0; 64],
            length: 0,
            overflow: Vec::new(),
        }
    }
}
impl std::ops::Deref for Children {
    type Target = [EntryId];
    fn deref(&self) -> &[EntryId] {
        if self.overflow.capacity() == 0 {
            &self.inline[..self.length]
        } else {
            &self.overflow
        }
    }
}
impl Children {
    fn push(&mut self, id: EntryId) {
        if self.overflow.capacity() != 0 {
            self.overflow.push(id);
        } else if self.length < self.inline.len() {
            self.inline[self.length] = id;
            self.length += 1;
        } else {
            self.overflow.reserve_exact(self.inline.len() * 2);
            self.overflow.extend_from_slice(&self.inline);
            self.overflow.push(id);
        }
    }
    fn swap_remove(&mut self, index: usize) {
        if self.overflow.capacity() != 0 {
            self.overflow.swap_remove(index);
        } else {
            assert!(index < self.length);
            self.length -= 1;
            self.inline[index] = self.inline[self.length];
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    File,
    Directory,
    // Reserved in the new model; scope/raw-name work will admit link entries.
    #[allow(dead_code)]
    Symlink,
}
#[derive(Clone)]
pub(super) struct Entry {
    pub parent: EntryId,
    pub kind: Kind,
    pub dev: u64,
    pub ino: u64,
    pub alive: bool,
    offset: u32,
    length: u32,
}
#[derive(Default)]
struct Segment {
    entries: super::mapped::Buffer<Entry>,
    names: super::mapped::Buffer<u8>,
}
impl Segment {
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            entries: self.entries.try_clone()?,
            names: self.names.try_clone()?,
        })
    }
    fn allocated_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.entries.allocated_bytes() + self.names.allocated_bytes()
    }
}
#[derive(Clone)]
pub(super) struct Data {
    segments: Vec<Arc<Segment>>,
    pub search: super::search_index::SearchIndex,
    pub slots: usize,
    pub epoch: u64,
    allocated: usize,
    // Last: request only after raw/search fields finish freeing their buffers.
    reclaim: super::memory::ReclaimOnDrop,
}
impl Drop for Data {
    fn drop(&mut self) {
        let unique_raw_bytes = self
            .segments
            .iter()
            .filter(|segment| Arc::strong_count(segment) == 1)
            .map(|segment| segment.allocated_bytes())
            .fold(0usize, usize::saturating_add);
        self.reclaim
            .arm(unique_raw_bytes.saturating_add(self.search.uniquely_owned_bytes()));
    }
}
impl Data {
    pub fn allocated_bytes(&self) -> usize {
        self.allocated + self.search.allocated_bytes()
    }
    pub fn accounted_bytes(&self, seen: &mut HashSet<usize>) -> usize {
        std::mem::size_of::<Self>()
            + self.segments.capacity() * std::mem::size_of::<Arc<Segment>>()
            + self.search.accounted_bytes(seen)
            + self
                .segments
                .iter()
                .filter(|segment| seen.insert(Arc::as_ptr(segment) as usize))
                .map(|segment| segment.allocated_bytes())
                .sum::<usize>()
    }
    pub fn entry(&self, id: EntryId) -> &Entry {
        &self.segments[id as usize / BLOCK].entries[id as usize % BLOCK]
    }
    pub fn name(&self, id: EntryId) -> &[u8] {
        let segment = &self.segments[id as usize / BLOCK];
        let entry = self.entry(id);
        &segment.names[entry.offset as usize..(entry.offset + entry.length) as usize]
    }
    pub fn path(&self, mut id: EntryId) -> PathBuf {
        let mut parts = Vec::new();
        while id != 0 {
            parts.push(OsString::from_vec(self.name(id).to_vec()));
            id = self.entry(id).parent;
        }
        parts.into_iter().rev().collect()
    }
}
pub(super) struct Inventory {
    pub data: Data,
    lookup: HashMap<LookupKey, EntryId>,
    collisions: HashMap<LookupKey, Vec<EntryId>>,
    pub entries: usize,
    pub directories: usize,
    pub directory_ids: std::collections::BTreeSet<EntryId>,
    pub touched: usize,
    pub copied_entries: usize,
    pub copied_segments: usize,
    pub name_bytes: usize,
    pub live_name_bytes: usize,
    children: HashMap<EntryId, Children>,
    positions: Vec<u32>,
    budgets: ScaleBudgets,
    allocation_credit: usize,
    // Last: all unique writer lookup/graph allocations have dropped first.
    reclaim: super::memory::ReclaimOnDrop,
}
impl Drop for Inventory {
    fn drop(&mut self) {
        let lookup_bytes = self
            .lookup
            .capacity()
            .saturating_mul(std::mem::size_of::<(LookupKey, EntryId)>());
        let position_bytes = self
            .positions
            .capacity()
            .saturating_mul(std::mem::size_of::<u32>());
        self.reclaim.arm(
            lookup_bytes.saturating_add(position_bytes).saturating_add(
                self.children.capacity() * std::mem::size_of::<(EntryId, Children)>(),
            ),
        );
    }
}
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, byte| {
        (h ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}
impl Inventory {
    pub fn empty(epoch: u64, budgets: ScaleBudgets) -> Self {
        Self {
            data: Data {
                segments: vec![],
                slots: 0,
                epoch,
                search: Default::default(),
                allocated: std::mem::size_of::<Data>(),
                reclaim: Default::default(),
            },
            lookup: HashMap::new(),
            collisions: HashMap::new(),
            entries: 0,
            directories: 0,
            directory_ids: Default::default(),
            touched: 0,
            copied_entries: 0,
            copied_segments: 0,
            name_bytes: 0,
            live_name_bytes: 0,
            children: HashMap::new(),
            positions: vec![],
            budgets,
            allocation_credit: budgets.max_retained_bytes,
            reclaim: Default::default(),
        }
    }
    pub fn correction_candidate(&mut self, epoch: u64) -> Self {
        // A correction keeps the old immutable Data searchable, but it never
        // applies events or traverses the old writer indexes before replacement.
        // Reuse these unique capacities instead of leaving successive large
        // hash/position allocations as resident free chunks in the allocator.
        let mut candidate = Self::empty(epoch, self.budgets);
        candidate.lookup = std::mem::take(&mut self.lookup);
        candidate.lookup.clear();
        candidate.collisions = std::mem::take(&mut self.collisions);
        candidate.collisions.clear();
        candidate.children = std::mem::take(&mut self.children);
        candidate.children.clear();
        candidate.positions = std::mem::take(&mut self.positions);
        candidate.positions.clear();
        candidate
    }
    pub fn validate_restored_lookup(&self) -> io::Result<()> {
        let mut names = HashSet::new();
        for id in 1..self.data.slots as u32 {
            let entry = self.data.entry(id);
            if entry.alive && !names.insert((entry.parent, self.data.name(id))) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "checkpoint duplicate live directory entry",
                ));
            }
        }
        Ok(())
    }
    pub fn insert_restored(
        &mut self,
        parent: EntryId,
        name: &[u8],
        kind: Kind,
        dev: u64,
        ino: u64,
        alive: bool,
    ) -> io::Result<EntryId> {
        let id = self.insert(parent, name, kind, dev, ino)?;
        if !alive {
            self.live_name_bytes -= name.len();
            self.detach_child(parent, id);
            self.remove_lookup(id);
            self.edit(id)?.alive = false;
            self.entries -= 1;
            if kind == Kind::Directory {
                self.directories -= 1;
                self.directory_ids.remove(&id);
            }
        }
        Ok(id)
    }
    pub fn set_allocation_credit(&mut self, credit: usize) {
        self.allocation_credit = credit;
    }
    fn credit(&mut self, bytes: usize) -> io::Result<()> {
        if bytes > self.allocation_credit {
            return Err(io::Error::other("retained snapshot byte budget exhausted"));
        }
        self.allocation_credit -= bytes;
        Ok(())
    }
    fn allocation(&mut self, bytes: usize) -> io::Result<()> {
        if self.data.allocated_bytes().saturating_add(bytes) > self.budgets.max_snapshot_bytes {
            return Err(io::Error::other("snapshot byte budget exhausted"));
        }
        self.credit(bytes)?;
        self.data.allocated += bytes;
        Ok(())
    }
    fn name_budget(&self, bytes: usize) -> io::Result<()> {
        if self.name_bytes.saturating_add(bytes) > self.budgets.max_name_bytes {
            return Err(io::Error::other("scale name byte budget exhausted"));
        }
        Ok(())
    }
    pub fn child(&self, parent: EntryId, name: &[u8]) -> Option<EntryId> {
        let key = LookupKey::new(parent, hash(name));
        self.lookup
            .get(&key)
            .copied()
            .filter(|id| self.data.name(*id) == name)
            .or_else(|| {
                self.collisions
                    .get(&key)?
                    .iter()
                    .copied()
                    .find(|id| self.data.name(*id) == name)
            })
    }
    pub fn find(&self, path: &Path) -> Option<EntryId> {
        let mut parent = 0;
        for part in path.components() {
            let Component::Normal(name) = part else {
                return None;
            };
            if self.data.entry(parent).kind != Kind::Directory {
                return None;
            }
            parent = self.child(parent, name.as_bytes())?;
        }
        Some(parent)
    }
    pub fn insert(
        &mut self,
        parent: EntryId,
        name: &[u8],
        kind: Kind,
        dev: u64,
        ino: u64,
    ) -> io::Result<EntryId> {
        if self.data.slots >= self.budgets.max_slots {
            return Err(io::Error::other("scale physical slot budget exhausted"));
        }
        self.name_budget(name.len())?;
        let id = u32::try_from(self.data.slots)
            .map_err(|_| io::Error::other("scale entry ID budget exhausted"))?;
        if self.data.slots.is_multiple_of(BLOCK) {
            if self.data.segments.len() == self.data.segments.capacity() {
                let target = (self.data.segments.capacity() * 2).max(4);
                self.allocation(
                    (target - self.data.segments.capacity()) * std::mem::size_of::<Arc<Segment>>(),
                )?;
                self.data
                    .segments
                    .reserve_exact(target - self.data.segments.len());
            }
            self.allocation(
                super::mapped::allocation_bytes::<Entry>(BLOCK)? + std::mem::size_of::<Segment>(),
            )?;
            self.data.segments.push(Arc::new(Segment {
                entries: super::mapped::Buffer::with_capacity(BLOCK)?,
                names: super::mapped::Buffer::default(),
            }));
        }
        let block = self.data.segments.len() - 1;
        self.reserve_names(block, name.len())?;
        let segment = self.segment_mut(block)?;
        let offset = u32::try_from(segment.names.len())
            .map_err(|_| io::Error::other("scale name arena exhausted"))?;
        let length = u32::try_from(name.len())
            .map_err(|_| io::Error::other("scale name length exhausted"))?;
        offset
            .checked_add(length)
            .ok_or_else(|| io::Error::other("scale name arena exhausted"))?;
        segment.names.extend_from_slice(name);
        segment.entries.push(Entry {
            parent,
            kind,
            dev,
            ino,
            alive: true,
            offset,
            length,
        });
        self.data.slots += 1;
        self.name_bytes += name.len();
        self.live_name_bytes += name.len();
        self.touched += 1;
        if id != 0 {
            self.add_lookup(parent, name, id);
            let siblings = self.children.entry(parent).or_default();
            self.positions.push(
                u32::try_from(siblings.len())
                    .map_err(|_| io::Error::other("child position exhausted"))?,
            );
            siblings.push(id);
            self.entries += 1;
        } else {
            self.positions.push(0);
        }
        if kind == Kind::Directory {
            self.directory_ids.insert(id);
            self.directories += 1;
        }
        if id == 0 || self.data.search.has_parent(parent) {
            self.derive_entry(id)?;
        }
        Ok(id)
    }
    pub fn derive_entry(&mut self, id: EntryId) -> io::Result<()> {
        let entry = self.data.entry(id);
        let (parent, kind) = (entry.parent, entry.kind);
        let bytes = self
            .data
            .search
            .allocation(id, parent, kind, self.data.name(id));
        if bytes != 0 {
            if self.data.allocated_bytes().saturating_add(bytes) > self.budgets.max_snapshot_bytes {
                return Err(io::Error::other("snapshot byte budget exhausted"));
            }
            self.credit(bytes)?;
        }
        let name = self.data.name(id).to_vec();
        self.data.search.set(id, parent, kind, &name)?;
        Ok(())
    }
    /// Compaction inserts parent-first and has already derived every entry.
    /// Checkpoint records can contain forward parent IDs and need this traversal
    /// after the graph has been validated. Published snapshots never lack filters.
    pub fn finish_derived(&mut self, cancel: &std::sync::atomic::AtomicBool) -> io::Result<()> {
        if self.data.search.derived == self.data.slots {
            return Ok(());
        }
        let mut stack = vec![0];
        while let Some(id) = stack.pop() {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "derived index cancelled",
                ));
            }
            self.derive_entry(id)?;
            if let Some(children) = self.children.get(&id) {
                stack.extend(
                    children
                        .iter()
                        .copied()
                        .filter(|child| self.data.entry(*child).alive),
                );
            }
        }
        Ok(())
    }
    fn add_lookup(&mut self, parent: EntryId, name: &[u8], id: EntryId) {
        let key = LookupKey::new(parent, hash(name));
        if let std::collections::hash_map::Entry::Vacant(entry) = self.lookup.entry(key) {
            entry.insert(id);
        } else {
            self.collisions.entry(key).or_default().push(id);
        }
    }
    fn remove_lookup(&mut self, id: EntryId) {
        let key = LookupKey::new(self.data.entry(id).parent, hash(self.data.name(id)));
        if self.lookup.get(&key) == Some(&id) {
            if let Some(replacement) = self.collisions.get_mut(&key).and_then(|ids| ids.pop()) {
                self.lookup.insert(key, replacement);
            } else {
                self.lookup.remove(&key);
            }
        } else if let Some(ids) = self.collisions.get_mut(&key) {
            ids.retain(|entry| *entry != id);
        }
        if self.collisions.get(&key).is_some_and(Vec::is_empty) {
            self.collisions.remove(&key);
        }
    }
    fn edit(&mut self, id: EntryId) -> io::Result<&mut Entry> {
        self.touched += 1;
        Ok(&mut self.segment_mut(id as usize / BLOCK)?.entries[id as usize % BLOCK])
    }
    fn segment_mut(&mut self, block: usize) -> io::Result<&mut Segment> {
        if Arc::strong_count(&self.data.segments[block]) > 1 {
            self.credit(self.data.segments[block].allocated_bytes())?;
            self.copied_segments += 1;
            self.copied_entries += self.data.segments[block].entries.len();
            self.data.segments[block] = Arc::new(self.data.segments[block].try_clone()?);
        }
        Ok(Arc::get_mut(&mut self.data.segments[block]).expect("exclusive writer segment"))
    }
    fn reserve_names(&mut self, block: usize, extra: usize) -> io::Result<()> {
        let segment = &self.data.segments[block];
        let required = segment
            .names
            .len()
            .checked_add(extra)
            .ok_or_else(|| io::Error::other("name arena overflow"))?;
        if required > segment.names.capacity() {
            let target = super::mapped::allocation_bytes::<u8>(
                (segment.names.capacity() * 2).max(required).max(64),
            )?;
            let previous = segment.names.allocated_bytes();
            self.allocation(target - previous)?;
            // Growth temporarily holds both mappings, in addition to any COW copy.
            self.credit(previous)?;
            let segment = self.segment_mut(block)?;
            segment.names.reserve_capacity(target)?;
        }
        Ok(())
    }
    fn detach_child(&mut self, parent: EntryId, id: EntryId) {
        let siblings = self.children.get_mut(&parent).unwrap();
        let position = self.positions[id as usize];
        siblings.swap_remove(position as usize);
        if let Some(moved) = siblings.get(position as usize) {
            self.positions[*moved as usize] = position;
        }
    }
    pub fn reset_work(&mut self) {
        self.touched = 0;
        self.copied_entries = 0;
        self.copied_segments = 0;
    }
    pub fn child_at(&self, parent: EntryId, offset: usize) -> Option<EntryId> {
        self.children.get(&parent)?.get(offset).copied()
    }
    pub fn needs_compaction(&self, slots: usize, names: usize) -> bool {
        let dead = self.data.slots.saturating_sub(self.entries + 1);
        let obsolete = self.name_bytes.saturating_sub(self.live_name_bytes);
        (dead > 0
            && (dead >= (self.data.slots / 4).max(1)
                || self.data.slots.saturating_add(slots) >= self.budgets.max_slots))
            || (obsolete > 0
                && (obsolete >= (self.name_bytes / 4).max(1)
                    || self.name_bytes.saturating_add(names) >= self.budgets.max_name_bytes))
    }
    pub fn remove(&mut self, id: EntryId) -> io::Result<()> {
        if id == 0 || !self.data.entry(id).alive {
            return Ok(());
        }
        if self.data.entry(id).kind == Kind::Directory {
            while let Some(child) = self.children.get(&id).and_then(|ids| ids.last()).copied() {
                self.remove(child)?;
            }
            self.children.remove(&id);
            self.directories -= 1;
            self.directory_ids.remove(&id);
        }
        self.detach_child(self.data.entry(id).parent, id);
        self.live_name_bytes -= self.data.name(id).len();
        self.remove_lookup(id);
        self.edit(id)?.alive = false;
        self.entries -= 1;
        Ok(())
    }
    pub fn rename(&mut self, id: EntryId, parent: EntryId, name: &[u8]) -> io::Result<()> {
        self.name_budget(name.len())?;
        self.reserve_names(id as usize / BLOCK, name.len())?;
        let previous_parent = self.data.entry(id).parent;
        let previous_length = self.data.name(id).len();
        if previous_parent != parent {
            self.detach_child(previous_parent, id);
            let siblings = self.children.entry(parent).or_default();
            self.positions[id as usize] = u32::try_from(siblings.len())
                .map_err(|_| io::Error::other("child position exhausted"))?;
            siblings.push(id);
        }
        self.remove_lookup(id);
        self.touched += 1;
        let segment = self.segment_mut(id as usize / BLOCK)?;
        let offset = u32::try_from(segment.names.len())
            .map_err(|_| io::Error::other("scale name arena exhausted"))?;
        let length = u32::try_from(name.len())
            .map_err(|_| io::Error::other("scale name length exhausted"))?;
        offset
            .checked_add(length)
            .ok_or_else(|| io::Error::other("scale name arena exhausted"))?;
        segment.names.extend_from_slice(name);
        let entry = &mut segment.entries[id as usize % BLOCK];
        entry.parent = parent;
        entry.offset = offset;
        entry.length = length;
        self.name_bytes += name.len();
        self.live_name_bytes = self.live_name_bytes - previous_length + name.len();
        self.add_lookup(parent, name, id);
        // A directory rename changes every descendant's searchable ancestry.
        // Old leases retain the old cache and filter segments through Arc COW.
        let mut stack = vec![id];
        while let Some(entry) = stack.pop() {
            self.derive_entry(entry)?;
            if self.data.entry(entry).kind == Kind::Directory {
                if let Some(children) = self.children.get(&entry) {
                    stack.extend(children.iter().copied());
                }
            }
        }
        Ok(())
    }
}
