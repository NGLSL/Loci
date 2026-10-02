use crate::engine::ScaleBudgets;
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

const BLOCK: usize = 1024;
pub(super) type EntryId = u32;
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
    entries: Vec<Entry>,
    names: Vec<u8>,
}
impl Clone for Segment {
    fn clone(&self) -> Self {
        let mut entries = Vec::with_capacity(self.entries.capacity());
        entries.extend_from_slice(&self.entries);
        let mut names = Vec::with_capacity(self.names.capacity());
        names.extend_from_slice(&self.names);
        Self { entries, names }
    }
}
impl Segment {
    fn allocated_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.entries.capacity() * std::mem::size_of::<Entry>()
            + self.names.capacity()
    }
}
#[derive(Clone)]
pub(super) struct Data {
    segments: Vec<Arc<Segment>>,
    pub slots: usize,
    pub epoch: u64,
    allocated: usize,
}
impl Data {
    pub fn allocated_bytes(&self) -> usize {
        self.allocated
    }
    pub fn accounted_bytes(&self, seen: &mut HashSet<usize>) -> usize {
        std::mem::size_of::<Self>()
            + self.segments.capacity() * std::mem::size_of::<Arc<Segment>>()
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
    lookup: HashMap<(EntryId, u64), EntryId>,
    collisions: HashMap<(EntryId, u64), Vec<EntryId>>,
    pub entries: usize,
    pub directories: usize,
    pub directory_ids: std::collections::BTreeSet<EntryId>,
    pub touched: usize,
    pub copied_entries: usize,
    pub copied_segments: usize,
    pub name_bytes: usize,
    pub live_name_bytes: usize,
    children: HashMap<EntryId, Vec<EntryId>>,
    positions: Vec<usize>,
    budgets: ScaleBudgets,
    allocation_credit: usize,
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
                allocated: std::mem::size_of::<Data>(),
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
        }
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
        let key = (parent, hash(name));
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
            self.allocation(BLOCK * std::mem::size_of::<Entry>() + std::mem::size_of::<Segment>())?;
            self.data.segments.push(Arc::new(Segment {
                entries: Vec::with_capacity(BLOCK),
                names: vec![],
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
            self.positions.push(siblings.len());
            siblings.push(id);
            self.entries += 1;
        } else {
            self.positions.push(0);
        }
        if kind == Kind::Directory {
            self.directory_ids.insert(id);
            self.directories += 1;
        }
        Ok(id)
    }
    fn add_lookup(&mut self, parent: EntryId, name: &[u8], id: EntryId) {
        let key = (parent, hash(name));
        if let std::collections::hash_map::Entry::Vacant(entry) = self.lookup.entry(key) {
            entry.insert(id);
        } else {
            self.collisions.entry(key).or_default().push(id);
        }
    }
    fn remove_lookup(&mut self, id: EntryId) {
        let key = (self.data.entry(id).parent, hash(self.data.name(id)));
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
        }
        Ok(Arc::make_mut(&mut self.data.segments[block]))
    }
    fn reserve_names(&mut self, block: usize, extra: usize) -> io::Result<()> {
        let segment = &self.data.segments[block];
        let required = segment
            .names
            .len()
            .checked_add(extra)
            .ok_or_else(|| io::Error::other("name arena overflow"))?;
        if required > segment.names.capacity() {
            let target = (segment.names.capacity() * 2).max(required).max(64);
            self.allocation(target - segment.names.capacity())?;
            let segment = self.segment_mut(block)?;
            segment.names.reserve_exact(target - segment.names.len());
        }
        Ok(())
    }
    fn detach_child(&mut self, parent: EntryId, id: EntryId) {
        let siblings = self.children.get_mut(&parent).unwrap();
        let position = self.positions[id as usize];
        siblings.swap_remove(position);
        if let Some(moved) = siblings.get(position) {
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
            self.positions[id as usize] = siblings.len();
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
        Ok(())
    }
}
