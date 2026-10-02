use std::collections::HashMap;
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
#[derive(Clone, Default)]
struct Segment {
    entries: Vec<Entry>,
    names: Vec<u8>,
}
#[derive(Clone)]
pub(super) struct Data {
    segments: Vec<Arc<Segment>>,
    pub slots: usize,
    pub epoch: u64,
}
impl Data {
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
    pub touched: usize,
    pub copied_entries: usize,
    pub copied_segments: usize,
}
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, byte| {
        (h ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}
impl Inventory {
    pub fn new(epoch: u64) -> Self {
        let mut out = Self {
            data: Data {
                segments: vec![],
                slots: 0,
                epoch,
            },
            lookup: HashMap::new(),
            collisions: HashMap::new(),
            entries: 0,
            directories: 0,
            touched: 0,
            copied_entries: 0,
            copied_segments: 0,
        };
        out.insert(0, b"", Kind::Directory, 0, 0).unwrap();
        out
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
        let id = u32::try_from(self.data.slots)
            .map_err(|_| io::Error::other("scale entry ID budget exhausted"))?;
        if self.data.slots.is_multiple_of(BLOCK) {
            self.data.segments.push(Arc::new(Segment::default()));
        }
        let segment = self.segment_mut(self.data.segments.len() - 1);
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
        self.touched += 1;
        if id != 0 {
            self.add_lookup(parent, name, id);
            self.entries += 1;
        }
        if kind == Kind::Directory {
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
    fn edit(&mut self, id: EntryId) -> &mut Entry {
        self.touched += 1;
        &mut self.segment_mut(id as usize / BLOCK).entries[id as usize % BLOCK]
    }
    fn segment_mut(&mut self, block: usize) -> &mut Segment {
        if Arc::strong_count(&self.data.segments[block]) > 1 {
            self.copied_segments += 1;
            self.copied_entries += self.data.segments[block].entries.len();
        }
        Arc::make_mut(&mut self.data.segments[block])
    }
    pub fn reset_work(&mut self) {
        self.touched = 0;
        self.copied_entries = 0;
        self.copied_segments = 0;
    }
    pub fn remove(&mut self, id: EntryId) {
        if id == 0 || !self.data.entry(id).alive {
            return;
        }
        if self.data.entry(id).kind == Kind::Directory {
            let children: Vec<_> = (1..self.data.slots as u32)
                .filter(|child| {
                    self.data.entry(*child).alive && self.data.entry(*child).parent == id
                })
                .collect();
            for child in children {
                self.remove(child);
            }
            self.directories -= 1;
        }
        self.remove_lookup(id);
        self.edit(id).alive = false;
        self.entries -= 1;
    }
    pub fn rename(&mut self, id: EntryId, parent: EntryId, name: &[u8]) -> io::Result<()> {
        self.remove_lookup(id);
        self.touched += 1;
        let segment = self.segment_mut(id as usize / BLOCK);
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
        self.add_lookup(parent, name, id);
        Ok(())
    }
}
