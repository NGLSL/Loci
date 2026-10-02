//! Local namespace and undo transaction for incremental inventory updates.
//!
//! The inventory remains the source of truth.  [`Namespace`] is a small
//! derived view which lets a caller find directory prefixes and all parent
//! relationships for an object without rebuilding the complete inventory.
//! [`JournalUndo`] records only the first value observed for each relationship
//! touched by one replay attempt.  Together they form the transaction seam
//! used by the backend: a failed replay can remove the changed relationships,
//! restore the old values, and leave the published snapshot untouched.

use crate::model::EntryId;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io,
    os::windows::ffi::OsStringExt,
    path::PathBuf,
};

const DIRECTORY: u32 = 0x10;
const REPARSE: u32 = 0x400;
const MAX_DEPTH: usize = 64;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn ordinary(attributes: u32) -> bool {
    attributes & DIRECTORY != 0 && attributes & REPARSE == 0
}

/// A cached view over the relationship inventory.
///
/// `directories` keeps one usable name for each ordinary directory object.
/// NTFS directories do not normally have hard links, but an incremental
/// replay can briefly expose two relationships while a rename is being
/// reconciled.  The final snapshot validator owns that invariant; this view
/// deterministically keeps the first relationship it saw and can promote an
/// alternate one when its current relationship is removed.
///
/// `aliases` keeps every [`EntryId`] for an object.  It is deliberately a
/// relationship index rather than an object-to-one-path map: a hard-linked
/// file can have several search paths.
#[derive(Debug)]
pub struct Namespace {
    root: u128,
    directories: BTreeMap<u128, EntryId>,
    aliases: BTreeMap<u128, BTreeSet<EntryId>>,
}

impl Namespace {
    /// Build the derived view without requiring a complete valid graph.
    ///
    /// Initial discovery and a journal replay may temporarily contain an
    /// orphan or two relationships for a directory.  `Snapshot::validate`
    /// remains the publication gate.  Keeping this constructor permissive
    /// lets callers use the same transaction machinery while a candidate is
    /// being repaired; `relative_path` reports an orphan as `None` and a
    /// cycle/depth violation as an error.
    pub fn new(root: u128, entries: &BTreeMap<EntryId, u32>) -> io::Result<Self> {
        if root == 0 {
            return Err(invalid("namespace root must be nonzero"));
        }

        let mut namespace = Self {
            root,
            directories: BTreeMap::new(),
            aliases: BTreeMap::new(),
        };
        for (entry, attributes) in entries {
            namespace
                .aliases
                .entry(entry.object)
                .or_default()
                .insert(entry.clone());
            if ordinary(*attributes) {
                // Keep a deterministic representative while allowing the
                // final validator to reject multiple directory relationships.
                namespace
                    .directories
                    .entry(entry.object)
                    .or_insert_with(|| entry.clone());
            }
        }
        Ok(namespace)
    }

    /// Return whether an object currently has an ordinary directory
    /// relationship in the namespace view.
    pub fn is_directory(&self, object: u128) -> bool {
        object == self.root || self.directories.contains_key(&object)
    }

    /// Return every parent object which currently contains an entry for this
    /// object.  The returned set is owned so callers can sort or retain it
    /// while applying another local update.
    pub fn alias_parents(&self, object: u128) -> BTreeSet<u128> {
        self.aliases
            .get(&object)
            .into_iter()
            .flat_map(|entries| entries.iter().map(|entry| entry.parent))
            .collect()
    }

    /// Derive a raw UTF-16 relative path for an indexed directory object.
    ///
    /// The implicit source root is represented by an empty path.  A directory
    /// which is no longer indexed, or whose parent chain does not reach that
    /// root, returns `Ok(None)`.  Cycles and chains longer than the bounded
    /// namespace depth are malformed candidate state and return an error.
    pub fn relative_path(&self, directory: u128) -> io::Result<Option<PathBuf>> {
        if directory == self.root {
            return Ok(Some(PathBuf::new()));
        }
        if !self.directories.contains_key(&directory) {
            return Ok(None);
        }

        let mut current = directory;
        let mut parts = Vec::<Vec<u16>>::new();
        let mut seen = BTreeSet::new();
        while current != self.root {
            if !seen.insert(current) {
                return Err(invalid("namespace directory parent cycle"));
            }
            if parts.len() >= MAX_DEPTH {
                return Err(invalid("namespace directory depth exceeds 64"));
            }

            let Some(entry) = self.directories.get(&current) else {
                // The object is present in a relationship index but has no
                // reachable ordinary directory parent after a move/delete.
                return Ok(None);
            };
            parts.push(entry.name.clone());
            current = entry.parent;
        }

        let mut path = PathBuf::new();
        for name in parts.into_iter().rev() {
            path.push(OsString::from_wide(&name));
        }
        Ok(Some(path))
    }

    fn apply_change(
        &mut self,
        key: &EntryId,
        old: Option<u32>,
        new: Option<u32>,
        entries: &BTreeMap<EntryId, u32>,
    ) {
        debug_assert_ne!(old, new);

        match (old, new) {
            (None, Some(_)) => {
                self.aliases
                    .entry(key.object)
                    .or_default()
                    .insert(key.clone());
            }
            (Some(_), None) => {
                if let Some(aliases) = self.aliases.get_mut(&key.object) {
                    aliases.remove(key);
                    if aliases.is_empty() {
                        self.aliases.remove(&key.object);
                    }
                }
            }
            (Some(_), Some(_)) => {}
            (None, None) => unreachable!("Namespace::apply_change called for a no-op"),
        }

        let old_directory = old.is_some_and(ordinary);
        let new_directory = new.is_some_and(ordinary);

        if new_directory && (!old_directory || !self.directories.contains_key(&key.object)) {
            // A new relationship may be observed before the old rename
            // relationship is removed.  Let the newest relationship drive
            // path derivation; removing the old key below only promotes a
            // replacement when it was still the cached representative.
            self.directories.insert(key.object, key.clone());
        }

        if old_directory && !new_directory {
            let was_primary = self
                .directories
                .get(&key.object)
                .is_some_and(|primary| primary == key);
            if was_primary {
                let replacement = self.aliases.get(&key.object).and_then(|aliases| {
                    aliases.iter().find(|candidate| {
                        entries
                            .get(*candidate)
                            .is_some_and(|attributes| ordinary(*attributes))
                    })
                });
                if let Some(replacement) = replacement {
                    self.directories.insert(key.object, replacement.clone());
                } else {
                    self.directories.remove(&key.object);
                }
            }
        }
    }
}

/// First-write undo log for one candidate inventory transaction.
///
/// The log stores at most one old value per changed [`EntryId`].  An
/// untracked log is used by first discovery, where there is no published
/// inventory to restore and retaining a second copy would only add memory
/// pressure.
#[derive(Debug)]
pub struct JournalUndo {
    original: BTreeMap<EntryId, Option<u32>>,
    tracked: bool,
}

impl Default for JournalUndo {
    fn default() -> Self {
        Self::new()
    }
}

impl JournalUndo {
    /// Start a rollback-capable transaction.
    pub fn new() -> Self {
        Self {
            original: BTreeMap::new(),
            tracked: true,
        }
    }

    /// Start a transaction without retaining old values.  Used by initial
    /// discovery, where the candidate map itself is disposable.
    pub fn untracked() -> Self {
        Self {
            original: BTreeMap::new(),
            tracked: false,
        }
    }

    /// Apply one relationship value and remember only its first old value.
    ///
    /// Existing keys update their attribute in place, so changing metadata
    /// does not clone their UTF-16 name.  A new relationship necessarily owns
    /// one key in each of the inventory and alias indexes.
    pub fn set(
        &mut self,
        entries: &mut BTreeMap<EntryId, u32>,
        namespace: &mut Namespace,
        key: EntryId,
        value: Option<u32>,
    ) -> io::Result<()> {
        let old = entries.get(&key).copied();
        if old == value {
            return Ok(());
        }

        if self.tracked && !self.original.contains_key(&key) {
            self.original.insert(key.clone(), old);
        }

        match value {
            Some(attributes) => {
                if entries.contains_key(&key) {
                    // Keep the existing BTreeMap key and its UTF-16 name.
                    *entries
                        .get_mut(&key)
                        .expect("contains_key and get_mut disagree") = attributes;
                    namespace.apply_change(&key, old, Some(attributes), entries);
                } else {
                    let cache_key = key.clone();
                    entries.insert(key, attributes);
                    namespace.apply_change(&cache_key, old, Some(attributes), entries);
                }
            }
            None => {
                entries.remove(&key);
                namespace.apply_change(&key, old, None, entries);
            }
        }
        Ok(())
    }

    /// Return changed relationships in deterministic key order.
    pub fn changed_keys(&self) -> Vec<EntryId> {
        self.original.keys().cloned().collect()
    }

    /// Number of old relationship values retained by this transaction.
    pub fn saved_entries(&self) -> usize {
        self.original.len()
    }

    /// Remove all current changed relationships, then restore all old values.
    ///
    /// Removing the complete changed-key union first is intentional.  A
    /// directory rename can temporarily have old and new parent relationships;
    /// restoring in map order would otherwise let a transient relationship
    /// choose the wrong directory representative or leave a hard-link alias
    /// behind.
    pub fn rollback(&mut self, entries: &mut BTreeMap<EntryId, u32>, namespace: &mut Namespace) {
        let keys: Vec<EntryId> = self.original.keys().cloned().collect();

        for key in &keys {
            let Some(current) = entries.remove(key) else {
                continue;
            };
            namespace.apply_change(key, Some(current), None, entries);
        }

        for (key, old) in &self.original {
            let Some(attributes) = old else {
                continue;
            };
            entries.insert(key.clone(), *attributes);
            namespace.apply_change(key, None, Some(*attributes), entries);
        }

        self.original.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStrExt;

    const FILE: u32 = 0x20;

    fn entry(parent: u128, object: u128, name: &[u16]) -> EntryId {
        EntryId {
            parent,
            object,
            name: name.to_vec(),
        }
    }

    fn fixture() -> BTreeMap<EntryId, u32> {
        BTreeMap::from([
            (
                entry(1, 2, &[b'd' as u16, b'i' as u16, b'r' as u16]),
                DIRECTORY,
            ),
            (
                entry(1, 3, &[b'f' as u16, b'i' as u16, b'l' as u16, b'e' as u16]),
                FILE,
            ),
            (
                entry(2, 4, &[b'l' as u16, b'e' as u16, b'a' as u16, b'f' as u16]),
                FILE,
            ),
        ])
    }

    #[test]
    fn no_op_and_repeated_updates_keep_a_small_first_write_log() {
        let mut entries = fixture();
        let original = entries.clone();
        let mut namespace = Namespace::new(1, &entries).unwrap();
        let file = entry(1, 3, &[b'f' as u16, b'i' as u16, b'l' as u16, b'e' as u16]);
        let mut undo = JournalUndo::new();

        undo.set(&mut entries, &mut namespace, file.clone(), Some(FILE))
            .unwrap();
        assert_eq!(undo.saved_entries(), 0);
        assert!(undo.changed_keys().is_empty());

        undo.set(&mut entries, &mut namespace, file.clone(), None)
            .unwrap();
        undo.set(&mut entries, &mut namespace, file.clone(), Some(DIRECTORY))
            .unwrap();
        undo.set(&mut entries, &mut namespace, file.clone(), Some(FILE))
            .unwrap();
        assert_eq!(undo.saved_entries(), 1);
        assert_eq!(undo.changed_keys(), vec![file.clone()]);
        assert_eq!(entries.get(&file), Some(&FILE));

        undo.rollback(&mut entries, &mut namespace);
        assert_eq!(entries, original);
        assert_eq!(namespace.alias_parents(3), BTreeSet::from([1]));
        assert_eq!(
            namespace
                .relative_path(2)
                .unwrap()
                .unwrap()
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>(),
            b"dir".iter().map(|c| *c as u16).collect::<Vec<_>>()
        );
    }

    #[test]
    fn hard_link_aliases_and_attribute_transitions_update_locally() {
        let mut entries = BTreeMap::from([
            (entry(1, 2, &[b'a' as u16]), FILE),
            (entry(1, 2, &[b'b' as u16]), FILE),
        ]);
        let mut namespace = Namespace::new(1, &entries).unwrap();
        assert!(namespace.is_directory(1));
        assert_eq!(namespace.alias_parents(2), BTreeSet::from([1]));
        assert!(!namespace.is_directory(2));

        let mut undo = JournalUndo::new();
        undo.set(
            &mut entries,
            &mut namespace,
            entry(1, 2, &[b'a' as u16]),
            Some(DIRECTORY),
        )
        .unwrap();
        assert!(namespace.is_directory(2));
        undo.set(
            &mut entries,
            &mut namespace,
            entry(1, 2, &[b'a' as u16]),
            Some(FILE),
        )
        .unwrap();
        assert!(!namespace.is_directory(2));
        undo.set(
            &mut entries,
            &mut namespace,
            entry(1, 2, &[b'a' as u16]),
            None,
        )
        .unwrap();
        assert_eq!(namespace.alias_parents(2), BTreeSet::from([1]));
        assert_eq!(namespace.aliases.get(&2).unwrap().len(), 1);
        assert_eq!(undo.saved_entries(), 1);
    }

    #[test]
    fn directory_rename_moves_descendant_prefix_and_orphan_is_none() {
        let mut entries = BTreeMap::from([
            (
                entry(1, 2, &[b'o' as u16, b'l' as u16, b'd' as u16]),
                DIRECTORY,
            ),
            (entry(2, 3, &[b'c' as u16]), DIRECTORY),
            (entry(3, 4, &[b'f' as u16]), FILE),
        ]);
        let mut namespace = Namespace::new(1, &entries).unwrap();
        let old_path: Vec<u16> = namespace
            .relative_path(3)
            .unwrap()
            .unwrap()
            .as_os_str()
            .encode_wide()
            .collect();
        assert_eq!(old_path, "old\\c".encode_utf16().collect::<Vec<_>>());

        let mut undo = JournalUndo::new();
        undo.set(
            &mut entries,
            &mut namespace,
            entry(1, 2, &[b'o' as u16, b'l' as u16, b'd' as u16]),
            None,
        )
        .unwrap();
        assert!(namespace.relative_path(3).unwrap().is_none());
        undo.set(
            &mut entries,
            &mut namespace,
            entry(1, 2, &[b'n' as u16, b'e' as u16, b'w' as u16]),
            Some(DIRECTORY),
        )
        .unwrap();
        let new_path: Vec<u16> = namespace
            .relative_path(3)
            .unwrap()
            .unwrap()
            .as_os_str()
            .encode_wide()
            .collect();
        assert_eq!(new_path, "new\\c".encode_utf16().collect::<Vec<_>>());
        assert_eq!(namespace.alias_parents(2), BTreeSet::from([1]));
    }

    #[test]
    fn directory_rename_with_two_live_relationships_does_not_delete_new_name() {
        let old = entry(1, 2, &[b'o' as u16]);
        let parent = entry(1, 5, &[b'p' as u16]);
        let new = entry(5, 2, &[b'n' as u16]);
        let mut entries = BTreeMap::from([(old.clone(), DIRECTORY), (parent, DIRECTORY)]);
        let mut namespace = Namespace::new(1, &entries).unwrap();
        let mut undo = JournalUndo::new();

        // A rename can publish the new relationship before its old USN
        // relationship is removed.  The newest relationship drives path
        // derivation, and deleting the old key must not remove the new one.
        undo.set(&mut entries, &mut namespace, new.clone(), Some(DIRECTORY))
            .unwrap();
        assert_eq!(namespace.alias_parents(2), BTreeSet::from([1, 5]));
        let new_path: Vec<u16> = namespace
            .relative_path(2)
            .unwrap()
            .unwrap()
            .as_os_str()
            .encode_wide()
            .collect();
        assert_eq!(new_path, "p\\n".encode_utf16().collect::<Vec<_>>());

        undo.set(&mut entries, &mut namespace, old, None).unwrap();
        assert_eq!(namespace.alias_parents(2), BTreeSet::from([5]));
        let retained_path: Vec<u16> = namespace
            .relative_path(2)
            .unwrap()
            .unwrap()
            .as_os_str()
            .encode_wide()
            .collect();
        assert_eq!(retained_path, "p\\n".encode_utf16().collect::<Vec<_>>());
    }

    #[test]
    fn rollback_restores_hard_links_utf16_and_all_cached_paths() {
        let raw_name = [0xd800, b'x' as u16];
        let mut entries = BTreeMap::from([
            (entry(1, 2, &[b'd' as u16]), DIRECTORY),
            (entry(2, 3, &raw_name), FILE),
            (entry(1, 3, &[b'a' as u16]), FILE),
        ]);
        let original = entries.clone();
        let mut namespace = Namespace::new(1, &entries).unwrap();
        let old_path: Vec<u16> = namespace
            .relative_path(2)
            .unwrap()
            .unwrap()
            .as_os_str()
            .encode_wide()
            .collect();
        assert_eq!(old_path, "d".encode_utf16().collect::<Vec<_>>());
        assert_eq!(namespace.alias_parents(3), BTreeSet::from([1, 2]));

        let mut undo = JournalUndo::new();
        undo.set(
            &mut entries,
            &mut namespace,
            entry(1, 2, &[b'd' as u16]),
            None,
        )
        .unwrap();
        undo.set(
            &mut entries,
            &mut namespace,
            entry(1, 2, &[b'n' as u16]),
            Some(DIRECTORY),
        )
        .unwrap();
        undo.set(
            &mut entries,
            &mut namespace,
            entry(1, 3, &[b'a' as u16]),
            None,
        )
        .unwrap();
        assert_eq!(undo.saved_entries(), 3);
        undo.rollback(&mut entries, &mut namespace);

        assert_eq!(entries, original);
        assert_eq!(namespace.alias_parents(3), BTreeSet::from([1, 2]));
        let restored: Vec<u16> = namespace
            .relative_path(2)
            .unwrap()
            .unwrap()
            .as_os_str()
            .encode_wide()
            .collect();
        assert_eq!(restored, "d".encode_utf16().collect::<Vec<_>>());
        assert_eq!(namespace.aliases.get(&3).unwrap().len(), 2);
    }

    #[test]
    fn orphan_cycle_and_depth_have_distinct_results() {
        let orphan = BTreeMap::from([(entry(99, 2, &[b'd' as u16]), DIRECTORY)]);
        let namespace = Namespace::new(1, &orphan).unwrap();
        assert!(namespace.relative_path(2).unwrap().is_none());

        let cycle = BTreeMap::from([
            (entry(2, 3, &[b'a' as u16]), DIRECTORY),
            (entry(3, 2, &[b'b' as u16]), DIRECTORY),
        ]);
        let namespace = Namespace::new(1, &cycle).unwrap();
        assert!(namespace.relative_path(2).is_err());

        let mut deep = BTreeMap::new();
        for object in 2..=66 {
            deep.insert(entry(object - 1, object, &[b'd' as u16]), DIRECTORY);
        }
        let namespace = Namespace::new(1, &deep).unwrap();
        assert!(namespace.relative_path(66).is_err());
    }

    #[test]
    fn untracked_discovery_does_not_retain_undo_values() {
        let mut entries = BTreeMap::new();
        let mut namespace = Namespace::new(1, &entries).unwrap();
        let mut undo = JournalUndo::untracked();
        undo.set(
            &mut entries,
            &mut namespace,
            entry(1, 2, &[0xd800]),
            Some(FILE),
        )
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(undo.saved_entries(), 0);
        assert!(undo.changed_keys().is_empty());
    }
}
