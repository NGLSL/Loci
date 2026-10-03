//! A resident raw UTF-16 query view over the relationship inventory.
//!
//! The inventory remains keyed by [`EntryId`].  This module caches the
//! derived path only as a query view, so a hard-linked object can have more
//! than one cached path.  Queries walk the path-order BTreeMap and clone only
//! the paths which fit in the caller's limit.  Incremental updates replace the
//! changed relationships and, for a directory rename/move, the affected
//! subtree; they do not clone or rebuild the full inventory.

use crate::{model::EntryId, store};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::sync::Arc;

const DIRECTORY: u32 = 0x10;
const MAX_DEPTH: usize = 64;
const MAX_SCOPE: usize = 32 * 1024;
const MAX_PATH_BYTES: usize = 32 * 1024 * 1024;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn directory(attributes: u32) -> bool {
    attributes & DIRECTORY != 0
}

fn component(name: &[u16]) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != [46]
        && name != [46, 46]
        && !name.iter().any(|unit| [0, 47, 58, 92].contains(unit))
}

fn join_path(parent: &[u16], name: &[u16]) -> io::Result<Vec<u16>> {
    if !component(name) {
        return Err(invalid(
            "query view received an invalid UTF-16 path component",
        ));
    }
    let units = parent
        .len()
        .checked_add(usize::from(!parent.is_empty()))
        .and_then(|n| n.checked_add(name.len()))
        .ok_or_else(|| invalid("derived path length overflow"))?;
    if units > MAX_SCOPE {
        return Err(invalid("derived query path exceeds 32 KiB"));
    }
    let mut result = Vec::with_capacity(units);
    result.extend_from_slice(parent);
    if !parent.is_empty() {
        result.push(92);
    }
    result.extend_from_slice(name);
    Ok(result)
}

/// Build directory prefixes once.  `store::paths` is called by `build` before
/// this helper, so the complete graph validation and its budgets remain shared
/// with the Stage B baseline.
fn directory_prefixes(
    root: u128,
    entries: &BTreeMap<EntryId, u32>,
) -> io::Result<BTreeMap<u128, Vec<u16>>> {
    let mut directories = BTreeMap::<u128, EntryId>::new();
    for (entry, attributes) in entries {
        if directory(*attributes) && directories.insert(entry.object, entry.clone()).is_some() {
            return Err(invalid("one directory object has multiple relationships"));
        }
    }

    let mut result = BTreeMap::from([(root, Vec::new())]);
    let mut cached_bytes = 0usize;
    for object in directories.keys().copied().collect::<Vec<_>>() {
        if result.contains_key(&object) {
            continue;
        }
        let mut chain = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = object;
        while !result.contains_key(&current) {
            if !seen.insert(current) || chain.len() >= MAX_DEPTH {
                return Err(invalid("directory cycle or depth budget"));
            }
            let entry = directories
                .get(&current)
                .ok_or_else(|| invalid("directory chain does not reach root"))?;
            chain.push(entry.clone());
            current = entry.parent;
        }
        for entry in chain.into_iter().rev() {
            let parent = result
                .get(&entry.parent)
                .ok_or_else(|| invalid("directory parent prefix is missing"))?;
            let path = join_path(parent, &entry.name)?;
            cached_bytes = cached_bytes
                .checked_add(path.len().saturating_mul(2))
                .ok_or_else(|| invalid("directory prefix byte count overflow"))?;
            if cached_bytes > MAX_PATH_BYTES {
                return Err(invalid("directory prefix cache exceeds 32 MiB"));
            }
            result.insert(entry.object, path);
        }
    }
    Ok(result)
}

fn path_for(
    root: u128,
    entry: &EntryId,
    prefixes: &BTreeMap<u128, Vec<u16>>,
) -> io::Result<Vec<u16>> {
    if entry.parent == 0
        || entry.object == 0
        || entry.object == root
        || entry.object == entry.parent
    {
        return Err(invalid("query view received an invalid entry identity"));
    }
    let parent = prefixes
        .get(&entry.parent)
        .ok_or_else(|| invalid("query view parent directory prefix is missing"))?;
    join_path(parent, &entry.name)
}

/// Read a single parent range from the candidate inventory.  EntryId ordering
/// starts with `parent`, so this does not scan all candidate entries.
fn children_of(entries: &BTreeMap<EntryId, u32>, parent: u128) -> io::Result<Vec<(EntryId, u32)>> {
    let start = EntryId {
        parent,
        object: 0,
        name: Vec::new(),
    };
    let mut result = Vec::new();
    let mut names = BTreeSet::<Vec<u16>>::new();
    for (entry, attributes) in entries.range(start..) {
        if entry.parent != parent {
            break;
        }
        if !component(&entry.name) || !names.insert(entry.name.clone()) {
            return Err(invalid(
                "candidate inventory has an invalid or duplicate name",
            ));
        }
        result.push((entry.clone(), *attributes));
    }
    Ok(result)
}

#[derive(Clone, Debug)]
struct IndexedEntry {
    entry: EntryId,
    attributes: u32,
    path: Arc<[u16]>,
}

/// A prepared, local query-view replacement.  It owns only old/new entries in
/// affected relationships.  `apply` is infallible after `prepare_update` has
/// checked all collisions and budgets.
pub struct IndexUpdate {
    root: u128,
    removed: Vec<IndexedEntry>,
    added: Vec<IndexedEntry>,
    /// Number of distinct old/new path strings touched by this update.
    pub touched_paths: usize,
}

impl IndexUpdate {
    /// Alias used by the backend metrics path when reporting local update
    /// work.  It counts distinct old/new cached paths, including both sides
    /// of a directory rename.
    pub fn changed_paths(&self) -> usize {
        self.touched_paths
    }
}

/// Resident path view.  `paths` is ordered by raw UTF-16 path, matching the
/// `BTreeSet<Vec<u16>>` order returned by `store::paths`.
pub struct QueryIndex {
    root: u128,
    // Both maps hold an Arc to the same UTF-16 payload.  The path-order map
    // owns the query ordering; the relationship map owns fast update lookup.
    // This avoids a second full path allocation for every entry while leaving
    // EntryId (the inventory's public key) unchanged.
    paths: BTreeMap<Arc<[u16]>, EntryId>,
    entry_paths: BTreeMap<EntryId, Arc<[u16]>>,
    attributes: BTreeMap<EntryId, u32>,
    children: BTreeMap<u128, BTreeSet<EntryId>>,
    directories: BTreeMap<u128, EntryId>,
    directory_prefixes: BTreeMap<u128, Vec<u16>>,
    entries_by_object: BTreeMap<u128, BTreeSet<EntryId>>,
    object_attributes: BTreeMap<u128, u32>,
}

impl QueryIndex {
    pub fn build(root: u128, entries: &BTreeMap<EntryId, u32>) -> io::Result<Self> {
        // Keep the existing store validator as the exact correctness baseline
        // for the initial view.  The query path itself never calls this again.
        let _ = store::paths(root, entries)?;
        let directory_prefixes = directory_prefixes(root, entries)?;
        let mut index = Self {
            root,
            paths: BTreeMap::new(),
            entry_paths: BTreeMap::new(),
            attributes: BTreeMap::new(),
            children: BTreeMap::new(),
            directories: BTreeMap::new(),
            directory_prefixes,
            entries_by_object: BTreeMap::new(),
            object_attributes: BTreeMap::new(),
        };
        for (entry, attributes) in entries {
            let path = path_for(root, entry, &index.directory_prefixes)?;
            let state = IndexedEntry {
                entry: entry.clone(),
                attributes: *attributes,
                path: Arc::from(path),
            };
            index.install(state);
        }
        Ok(index)
    }

    /// Count matching paths and clone at most `limit` cached paths.
    pub fn search(&self, needle: &[u16], limit: usize) -> (usize, Vec<Vec<u16>>) {
        if needle.is_empty() {
            let result = self
                .paths
                .keys()
                .take(limit)
                .map(|path| path.to_vec())
                .collect();
            return (self.paths.len(), result);
        }
        let mut total = 0usize;
        let mut result = Vec::new();
        for path in self.paths.keys() {
            if path
                .as_ref()
                .windows(needle.len())
                .any(|part| part == needle)
            {
                total += 1;
                if result.len() < limit {
                    result.push(path.to_vec());
                }
            }
        }
        (total, result)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entry_paths.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entry_paths.is_empty()
    }

    /// Prepare a local update from the old/new relationship-key union.
    ///
    /// A directory relationship in `changed` expands to its old and new
    /// subtrees.  File updates and hard-link relationship updates touch only
    /// the supplied keys.  No index field is mutated until the entire update
    /// has been prepared successfully.
    pub fn prepare_update(
        &self,
        root: u128,
        entries: &BTreeMap<EntryId, u32>,
        changed: &[EntryId],
    ) -> io::Result<IndexUpdate> {
        if root != self.root {
            return Err(invalid("query update root does not match the index"));
        }
        if entries.len() > store::MAX_ENTRIES {
            return Err(invalid("candidate inventory exceeds entry budget"));
        }

        let changed_set: BTreeSet<EntryId> = changed.iter().cloned().collect();
        let mut old_ids = BTreeSet::new();
        let mut new_ids = BTreeSet::new();
        let mut directory_roots = BTreeSet::new();
        for entry in changed_set {
            if self.attributes.contains_key(&entry) {
                old_ids.insert(entry.clone());
            }
            if let Some(attributes) = entries.get(&entry) {
                new_ids.insert(entry.clone());
                if directory(*attributes) {
                    directory_roots.insert(entry.object);
                }
            }
            if self
                .attributes
                .get(&entry)
                .is_some_and(|attributes| directory(*attributes))
            {
                directory_roots.insert(entry.object);
            }
        }

        // Directory moves/renames invalidate every derived descendant path.
        // The children maps let both walks scale with the affected subtrees.
        for object in directory_roots.iter().copied() {
            old_ids.extend(self.old_subtree(object));
            if let Some(root_entry) = changed.iter().find(|entry| {
                entry.object == object && entries.get(entry).is_some_and(|a| directory(*a))
            }) {
                new_ids.insert(root_entry.clone());
                new_ids.extend(Self::new_subtree(entries, object)?);
            }
        }

        let mut current_directories = BTreeMap::new();
        for entry in &new_ids {
            if let Some(attributes) = entries.get(entry) {
                validate_entry(self.root, entry)?;
                if directory(*attributes) {
                    if current_directories
                        .insert(entry.object, entry.clone())
                        .is_some()
                    {
                        return Err(invalid(
                            "candidate directory object has multiple relationships",
                        ));
                    }
                }
            }
        }

        let mut new_prefixes = BTreeMap::new();
        let mut visiting = BTreeSet::new();
        for object in current_directories.keys().copied().collect::<Vec<_>>() {
            compute_prefix(
                self.root,
                object,
                &current_directories,
                &self.directory_prefixes,
                &mut new_prefixes,
                &mut visiting,
            )?;
        }

        let mut removed = Vec::new();
        for entry in old_ids.iter() {
            if let (Some(path), Some(attributes)) =
                (self.entry_paths.get(entry), self.attributes.get(entry))
            {
                removed.push(IndexedEntry {
                    entry: entry.clone(),
                    attributes: *attributes,
                    path: path.clone(),
                });
            }
        }

        let mut added = Vec::new();
        for entry in new_ids.iter() {
            let Some(attributes) = entries.get(entry) else {
                continue;
            };
            let parent_prefix = if entry.parent == self.root {
                &[][..]
            } else if let Some(prefix) = new_prefixes.get(&entry.parent) {
                prefix.as_slice()
            } else {
                self.directory_prefixes
                    .get(&entry.parent)
                    .ok_or_else(|| invalid("candidate entry parent directory is missing"))?
                    .as_slice()
            };
            let path = join_path(parent_prefix, &entry.name)?;
            added.push(IndexedEntry {
                entry: entry.clone(),
                attributes: *attributes,
                path: Arc::from(path),
            });
        }

        // Prevent a new path from colliding with an unaffected path or with a
        // second relationship in this one update, without cloning `self.paths`.
        let mut incoming = BTreeMap::<Arc<[u16]>, EntryId>::new();
        for state in &added {
            if let Some(owner) = self.paths.get(&state.path) {
                if !old_ids.contains(owner) {
                    return Err(invalid("candidate update creates a duplicate path"));
                }
            }
            if incoming
                .insert(state.path.clone(), state.entry.clone())
                .is_some()
            {
                return Err(invalid(
                    "candidate update creates duplicate path relationships",
                ));
            }
        }

        // Preserve the one-object/one-attribute invariant without walking the
        // complete inventory.  Existing hard links outside the affected set
        // remain visible through `entries_by_object`.
        let mut incoming_attributes = BTreeMap::<u128, u32>::new();
        for state in &added {
            if let Some(previous) = incoming_attributes.insert(state.entry.object, state.attributes)
            {
                if previous != state.attributes {
                    return Err(invalid("candidate hard links disagree on attributes"));
                }
            }
            let has_unremoved = self
                .entries_by_object
                .get(&state.entry.object)
                .is_some_and(|ids| ids.iter().any(|id| !old_ids.contains(id)));
            if has_unremoved
                && self
                    .object_attributes
                    .get(&state.entry.object)
                    .is_some_and(|previous| *previous != state.attributes)
            {
                return Err(invalid("candidate hard link changes object attributes"));
            }
        }

        let mut touched = BTreeSet::<Arc<[u16]>>::new();
        touched.extend(removed.iter().map(|state| state.path.clone()));
        touched.extend(added.iter().map(|state| state.path.clone()));
        Ok(IndexUpdate {
            root: self.root,
            removed,
            added,
            touched_paths: touched.len(),
        })
    }

    /// Publish a previously prepared local update.  All fallible validation is
    /// deliberately in `prepare_update`; this method only applies known-good
    /// map/set operations.
    pub fn apply(&mut self, update: IndexUpdate) {
        debug_assert_eq!(update.root, self.root);
        for state in &update.removed {
            self.uninstall(state);
        }
        for state in &update.added {
            self.install(state.clone());
        }
    }

    fn old_subtree(&self, object: u128) -> BTreeSet<EntryId> {
        let mut result = BTreeSet::new();
        let mut queue = Vec::new();
        if let Some(root_entry) = self.directories.get(&object) {
            result.insert(root_entry.clone());
            queue.push(object);
        }
        while let Some(parent) = queue.pop() {
            let Some(children) = self.children.get(&parent) else {
                continue;
            };
            for child in children {
                if result.insert(child.clone())
                    && self
                        .attributes
                        .get(child)
                        .is_some_and(|attributes| directory(*attributes))
                {
                    queue.push(child.object);
                }
            }
        }
        result
    }

    fn new_subtree(
        entries: &BTreeMap<EntryId, u32>,
        object: u128,
    ) -> io::Result<BTreeSet<EntryId>> {
        let mut result = BTreeSet::new();
        let mut queue = vec![object];
        while let Some(parent) = queue.pop() {
            for (child, attributes) in children_of(entries, parent)? {
                if result.insert(child.clone()) && directory(attributes) {
                    queue.push(child.object);
                }
            }
        }
        Ok(result)
    }

    fn install(&mut self, state: IndexedEntry) {
        let entry = state.entry.clone();
        let object = entry.object;
        let attributes = state.attributes;
        let path = state.path;
        let previous = self.paths.insert(path.clone(), entry.clone());
        debug_assert!(previous.is_none());
        let previous = self.entry_paths.insert(entry.clone(), path.clone());
        debug_assert!(previous.is_none());
        let previous = self.attributes.insert(entry.clone(), attributes);
        debug_assert!(previous.is_none());
        self.children
            .entry(entry.parent)
            .or_default()
            .insert(entry.clone());
        self.entries_by_object
            .entry(object)
            .or_default()
            .insert(entry.clone());
        self.object_attributes.insert(object, attributes);
        if directory(attributes) {
            let previous = self.directories.insert(object, entry.clone());
            debug_assert!(previous.is_none());
            self.directory_prefixes.insert(object, path.to_vec());
        }
    }

    fn uninstall(&mut self, state: &IndexedEntry) {
        self.paths.remove(&state.path);
        self.entry_paths.remove(&state.entry);
        self.attributes.remove(&state.entry);
        if let Some(children) = self.children.get_mut(&state.entry.parent) {
            children.remove(&state.entry);
            if children.is_empty() {
                self.children.remove(&state.entry.parent);
            }
        }
        if let Some(objects) = self.entries_by_object.get_mut(&state.entry.object) {
            objects.remove(&state.entry);
            if objects.is_empty() {
                self.entries_by_object.remove(&state.entry.object);
                self.object_attributes.remove(&state.entry.object);
            }
        }
        if directory(state.attributes)
            && self
                .directories
                .get(&state.entry.object)
                .is_some_and(|entry| entry == &state.entry)
        {
            self.directories.remove(&state.entry.object);
            self.directory_prefixes.remove(&state.entry.object);
        }
    }
}

fn validate_entry(root: u128, entry: &EntryId) -> io::Result<()> {
    if entry.parent == 0
        || entry.object == 0
        || entry.object == root
        || entry.object == entry.parent
        || !component(&entry.name)
    {
        return Err(invalid("candidate relationship has invalid identity/name"));
    }
    Ok(())
}

fn compute_prefix(
    root: u128,
    object: u128,
    current: &BTreeMap<u128, EntryId>,
    old: &BTreeMap<u128, Vec<u16>>,
    output: &mut BTreeMap<u128, Vec<u16>>,
    visiting: &mut BTreeSet<u128>,
) -> io::Result<Vec<u16>> {
    if object == root {
        return Ok(Vec::new());
    }
    if let Some(path) = output.get(&object) {
        return Ok(path.clone());
    }
    if !visiting.insert(object) {
        return Err(invalid("candidate directory cycle"));
    }
    let entry = current
        .get(&object)
        .ok_or_else(|| invalid("candidate directory relationship is missing"))?
        .clone();
    let parent = if entry.parent == root {
        Vec::new()
    } else if current.contains_key(&entry.parent) {
        compute_prefix(root, entry.parent, current, old, output, visiting)?
    } else {
        old.get(&entry.parent)
            .cloned()
            .ok_or_else(|| invalid("candidate directory parent prefix is missing"))?
    };
    let path = join_path(&parent, &entry.name)?;
    visiting.remove(&object);
    output.insert(object, path.clone());
    Ok(path)
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

    fn fixture() -> BTreeMap<EntryId, u32> {
        BTreeMap::from([
            (entry(1, 2, &[100, 105, 114]), DIRECTORY),
            (entry(2, 3, &[99, 104, 105, 108, 100]), 0),
            (entry(1, 4, &[0xd800, 97]), 0),
            (entry(1, 5, &[97]), 0),
            (entry(2, 5, &[98]), 0),
        ])
    }

    fn baseline(
        root: u128,
        entries: &BTreeMap<EntryId, u32>,
        needle: &[u16],
        limit: usize,
    ) -> (usize, Vec<Vec<u16>>) {
        let mut matches = store::paths(root, entries)
            .unwrap()
            .into_iter()
            .filter(|path| {
                needle.is_empty() || path.windows(needle.len()).any(|part| part == needle)
            });
        let all: Vec<_> = matches.by_ref().collect();
        (all.len(), all.into_iter().take(limit).collect())
    }

    fn assert_same(
        index: &QueryIndex,
        root: u128,
        entries: &BTreeMap<EntryId, u32>,
        needle: &[u16],
        limit: usize,
    ) {
        assert_eq!(
            index.search(needle, limit),
            baseline(root, entries, needle, limit)
        );
    }

    #[test]
    fn literal_empty_limit_zero_and_raw_utf16_match_baseline() {
        let entries = fixture();
        let index = QueryIndex::build(1, &entries).unwrap();
        assert_same(&index, 1, &entries, &[99, 104], 2);
        assert_same(&index, 1, &entries, &[], usize::MAX);
        let (total, returned) = index.search(&[], 0);
        assert_eq!(total, entries.len());
        assert!(returned.is_empty());
        assert_same(&index, 1, &entries, &[0xd800], 10);
    }

    #[test]
    fn hardlinks_are_separate_cached_paths() {
        let entries = fixture();
        let index = QueryIndex::build(1, &entries).unwrap();
        let (total, paths) = index.search(&[], usize::MAX);
        assert_eq!(total, 5);
        assert!(paths.contains(&vec![97]));
        assert!(paths.contains(&vec![100, 105, 114, 92, 98]));
    }

    #[test]
    fn optimized_build_keeps_search_and_path_sets_populated() {
        let entries = fixture();
        let index = QueryIndex::build(1, &entries).unwrap();
        assert_eq!(index.entry_paths.len(), entries.len());
        assert_eq!(index.paths.len(), entries.len());
        assert_same(&index, 1, &entries, &[], usize::MAX);
        assert_same(&index, 1, &entries, &[0xd800], 1);
    }

    #[test]
    fn file_add_delete_rename_move_and_failed_prepare_preserve_old_view() {
        let old = fixture();
        let mut index = QueryIndex::build(1, &old).unwrap();

        let old_file = entry(1, 6, &[111, 108, 100]);
        let new_file = entry(1, 6, &[110, 101, 119]);
        let mut next = old.clone();
        next.insert(old_file.clone(), 0);
        let add = index
            .prepare_update(1, &next, std::slice::from_ref(&old_file))
            .unwrap();
        assert_eq!(add.touched_paths, 1);
        index.apply(add);
        assert_same(&index, 1, &next, &[], usize::MAX);

        next.remove(&old_file);
        next.insert(new_file.clone(), 0);
        let rename = index
            .prepare_update(1, &next, &[old_file.clone(), new_file.clone()])
            .unwrap();
        assert_eq!(rename.touched_paths, 2);
        index.apply(rename);
        assert_same(&index, 1, &next, &[110], 10);

        let mut invalid_next = next.clone();
        let bad = entry(1, 6, &[0]);
        invalid_next.insert(bad.clone(), 0);
        assert!(index
            .prepare_update(1, &invalid_next, std::slice::from_ref(&bad))
            .is_err());
        assert_same(&index, 1, &next, &[], usize::MAX);

        let mut moved = next.clone();
        let second_dir = entry(1, 7, &[100, 50]);
        moved.insert(second_dir.clone(), DIRECTORY);
        let current = moved
            .iter()
            .find(|(id, _)| id.object == 6)
            .map(|(id, _)| id.clone())
            .unwrap();
        moved.remove(&current);
        let moved_file = entry(7, 6, &[110, 101, 119]);
        moved.insert(moved_file.clone(), 0);
        // The directory addition is also supplied, so both its relationship
        // and the moved file are published as one local transaction.
        let update = index
            .prepare_update(1, &moved, &[second_dir, current, moved_file])
            .unwrap();
        index.apply(update);
        assert_same(&index, 1, &moved, &[110], 10);
    }

    #[test]
    fn directory_rename_replaces_only_subtree_paths() {
        let old = fixture();
        let mut index = QueryIndex::build(1, &old).unwrap();
        let old_dir = entry(1, 2, &[100, 105, 114]);
        let new_dir = entry(1, 2, &[114, 101, 110]);
        let mut next = old.clone();
        next.remove(&old_dir);
        next.insert(new_dir.clone(), DIRECTORY);
        let update = index
            .prepare_update(1, &next, &[old_dir.clone(), new_dir.clone()])
            .unwrap();
        assert_eq!(update.touched_paths, 6);
        index.apply(update);
        assert_same(&index, 1, &next, &[], usize::MAX);
        assert_eq!(index.search(&[100, 105, 114], 10).0, 0);
        assert_eq!(index.search(&[114], 10).0, 3);
    }
}
