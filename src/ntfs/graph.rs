//! Copy-on-write FRN/parent/name pages. Full paths are derived, never retained per file.
use super::{invalid, Limits};
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;

const PAGE: usize = 4096;
const NAME_PAGE: usize = 64 * 1024;
pub(super) const DIRECTORY: u32 = 0x10;
pub(super) const REPARSE: u32 = 0x400;

#[derive(Clone, Copy, Debug)]
pub(super) struct Node {
    pub object: u64,
    pub parent: u64,
    block: u32,
    offset: u32,
    length: u16,
    folded_block: u32,
    folded_offset: u32,
    folded_length: u16,
    pub attributes: u32,
    pub alive: bool,
}

#[derive(Clone)]
pub(super) struct Graph {
    pub root: u64,
    pages: Vec<Arc<Vec<Node>>>,
    names: Vec<Arc<Vec<u16>>>,
    folded: Vec<Arc<Vec<u8>>>,
    directories: Arc<HashMap<u64, u32>>,
    slots: u32,
    pub live: usize,
    pub limits: Limits,
}

impl Graph {
    pub fn new(root: u64, limits: Limits) -> Self {
        Self {
            root,
            pages: Vec::new(),
            names: Vec::new(),
            folded: Vec::new(),
            directories: Arc::new(HashMap::new()),
            slots: 0,
            live: 0,
            limits,
        }
    }
    pub fn memory_bytes(&self) -> usize {
        self.pages.len() * PAGE * std::mem::size_of::<Node>()
            + self.names.len() * NAME_PAGE * 2
            + self.folded.len() * NAME_PAGE
            + self.directories.capacity() * 32
            + (self.pages.capacity() + self.names.capacity()) * 16
    }
    pub fn get(&self, slot: u32) -> &Node {
        &self.pages[slot as usize / PAGE][slot as usize % PAGE]
    }
    pub fn name(&self, node: &Node) -> &[u16] {
        &self.names[node.block as usize]
            [node.offset as usize..node.offset as usize + node.length as usize]
    }
    pub fn folded_name(&self, node: &Node) -> &[u8] {
        &self.folded[node.folded_block as usize]
            [node.folded_offset as usize..node.folded_offset as usize + node.folded_length as usize]
    }
    pub fn nodes(&self) -> impl Iterator<Item = (u32, &Node)> {
        self.pages
            .iter()
            .flat_map(|p| p.iter())
            .enumerate()
            .map(|(i, n)| (i as u32, n))
    }
    pub fn add(
        &mut self,
        object: u64,
        parent: u64,
        name: &[u16],
        attributes: u32,
    ) -> io::Result<u32> {
        if object == 0 || parent == 0 || object == self.root || !valid_name(name) {
            return Err(invalid("invalid NTFS namespace entry"));
        }
        if self.slots == u32::MAX
            || self.slots as usize >= self.limits.memory_bytes / std::mem::size_of::<Node>()
        {
            return Err(invalid("NTFS entry memory budget exhausted"));
        }
        if self
            .memory_bytes()
            .saturating_add(PAGE * std::mem::size_of::<Node>() + NAME_PAGE * 3)
            > self.limits.memory_bytes
        {
            return Err(invalid("NTFS inventory memory budget exhausted"));
        }
        if self
            .names
            .last()
            .is_none_or(|p| p.len() + name.len() > NAME_PAGE)
        {
            let mut p = Vec::new();
            p.try_reserve_exact(NAME_PAGE).map_err(io::Error::other)?;
            self.names.push(Arc::new(p));
        }
        let block = (self.names.len() - 1) as u32;
        let p = Arc::make_mut(self.names.last_mut().unwrap());
        p.try_reserve_exact(NAME_PAGE - p.len())
            .map_err(io::Error::other)?;
        let offset = p.len() as u32;
        p.extend_from_slice(name);
        let lower = super::matching_bytes(name);
        if self
            .folded
            .last()
            .is_none_or(|p| p.len() + lower.len() > NAME_PAGE)
        {
            let mut p = Vec::new();
            p.try_reserve_exact(NAME_PAGE).map_err(io::Error::other)?;
            self.folded.push(Arc::new(p));
        }
        let folded_block = (self.folded.len() - 1) as u32;
        let p = Arc::make_mut(self.folded.last_mut().unwrap());
        p.try_reserve_exact(NAME_PAGE - p.len())
            .map_err(io::Error::other)?;
        let folded_offset = p.len() as u32;
        p.extend_from_slice(&lower);
        let node = Node {
            object,
            parent,
            block,
            offset,
            length: name.len() as u16,
            folded_block,
            folded_offset,
            folded_length: lower.len() as u16,
            attributes,
            alive: true,
        };
        let slot = self.slots;
        if slot as usize % PAGE == 0 {
            let mut p = Vec::new();
            p.try_reserve_exact(PAGE).map_err(io::Error::other)?;
            self.pages.push(Arc::new(p));
        }
        let page = Arc::make_mut(self.pages.last_mut().unwrap());
        page.try_reserve_exact(PAGE - page.len())
            .map_err(io::Error::other)?;
        page.push(node);
        if attributes & (DIRECTORY | REPARSE) == DIRECTORY {
            Arc::make_mut(&mut self.directories).insert(object, slot);
        }
        self.slots += 1;
        self.live += 1;
        Ok(slot)
    }
    pub fn directory(&self, object: u64) -> Option<u32> {
        self.directories
            .get(&object)
            .copied()
            .filter(|slot| self.get(*slot).alive)
    }
    pub fn path(&self, node: &Node) -> io::Result<Vec<u16>> {
        let mut components = Vec::new();
        components.push(self.name(node));
        let mut parent = node.parent;
        for _ in 0..self.limits.max_depth {
            if parent == self.root {
                let mut out = Vec::new();
                for part in components.into_iter().rev() {
                    if !out.is_empty() {
                        out.push(92);
                    }
                    out.extend_from_slice(part);
                }
                if out.len() > 32760 {
                    return Err(invalid("NTFS path exceeds Windows extended-path limit"));
                }
                return Ok(out);
            }
            let slot = self
                .directory(parent)
                .ok_or_else(|| invalid("NTFS parent missing or reparse boundary changed"))?;
            let directory = self.get(slot);
            components.push(self.name(directory));
            parent = directory.parent;
        }
        Err(invalid("NTFS namespace cycle or depth budget exceeded"))
    }
    pub fn directory_path(&self, object: u64) -> io::Result<Vec<u16>> {
        if object == self.root {
            return Ok(Vec::new());
        }
        self.path(
            self.get(
                self.directory(object)
                    .ok_or_else(|| invalid("NTFS directory identity missing"))?,
            ),
        )
    }
    pub fn remove_parents(&mut self, parents: &HashSet<u64>) {
        let dead: Vec<_> = self
            .nodes()
            .filter(|(_, n)| n.alive && parents.contains(&n.parent))
            .map(|(i, _)| i)
            .collect();
        for slot in dead {
            self.remove(slot);
        }
    }
    pub fn remove(&mut self, slot: u32) {
        let node = *self.get(slot);
        if !node.alive {
            return;
        }
        Arc::make_mut(&mut self.pages[slot as usize / PAGE])[slot as usize % PAGE].alive = false;
        if self.directories.get(&node.object) == Some(&slot) {
            Arc::make_mut(&mut self.directories).remove(&node.object);
        }
        self.live -= 1;
    }
    pub fn validate(&self) -> io::Result<()> {
        if self
            .memory_bytes()
            .saturating_add(self.live.saturating_mul(48))
            > self.limits.memory_bytes
        {
            return Err(invalid(
                "NTFS validation scratch exceeds configured memory budget",
            ));
        }
        let mut directory_names = HashSet::new();
        let mut names = HashSet::new();
        for (_, n) in self.nodes().filter(|(_, n)| n.alive) {
            if !names.insert((n.parent, self.name(n))) {
                return Err(invalid("duplicate NTFS parent/name relationship"));
            }
            if n.attributes & (DIRECTORY | REPARSE) == DIRECTORY
                && !directory_names.insert(n.object)
            {
                return Err(invalid(
                    "NTFS directory has multiple parent/name relationships",
                ));
            }
            self.path(n)?;
        }
        Ok(())
    }
    pub fn prune_removed_subtrees(&mut self, old: &Self) {
        let removed: HashSet<_> = old
            .directories
            .keys()
            .copied()
            .filter(|id| self.directory(*id).is_none())
            .collect();
        if removed.is_empty() {
            return;
        }
        let mut dead = Vec::new();
        for (slot, node) in self.nodes().filter(|(_, n)| n.alive) {
            let mut parent = node.parent;
            for _ in 0..self.limits.max_depth {
                if removed.contains(&parent) {
                    dead.push(slot);
                    break;
                }
                if parent == self.root {
                    break;
                }
                parent = if let Some(slot) = self.directory(parent) {
                    self.get(slot).parent
                } else if let Some(slot) = old.directory(parent) {
                    old.get(slot).parent
                } else {
                    break;
                };
            }
        }
        for slot in dead {
            self.remove(slot);
        }
    }
    pub fn compact(&self) -> io::Result<Self> {
        let mut graph = Self::new(self.root, self.limits);
        for (_, n) in self.nodes().filter(|(_, n)| n.alive) {
            graph.add(n.object, n.parent, self.name(n), n.attributes)?;
        }
        Ok(graph)
    }
    pub fn needs_compaction(&self) -> bool {
        self.slots as usize > self.live.saturating_mul(2).max(PAGE)
            || self.memory_bytes() > self.limits.memory_bytes / 2
    }
}
pub(super) fn valid_name(name: &[u16]) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != [46]
        && name != [46, 46]
        && !name.iter().any(|n| matches!(n, 0 | 47 | 58 | 92))
}
