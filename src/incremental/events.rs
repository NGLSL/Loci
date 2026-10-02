//! Pure bounded inotify event interpretation, also exercised on Windows without FFI.
use super::Change;
use crate::watch::{RawEvent, Signal};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
pub const CREATE: u32 = 0x100;
pub const DELETE: u32 = 0x200;
pub const FROM: u32 = 0x40;
pub const TO: u32 = 0x80;
pub const ATTRIB: u32 = 4;
pub const DELETE_SELF: u32 = 0x400;
pub const MOVE_SELF: u32 = 0x800;
pub const IS_DIR: u32 = 0x40000000;
pub struct Translated {
    pub changes: Vec<Change>,
    pub ignored: Vec<i32>,
}
fn child(root: &Path, parent: &Path, name: &[u8]) -> Result<PathBuf, Signal> {
    if name.contains(&0) || name.contains(&b'/') || name == b"." || name == b".." {
        return Err(Signal::GenerationRace);
    }
    #[cfg(target_os = "linux")]
    let name = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(name.to_vec())
    };
    #[cfg(not(target_os = "linux"))]
    let name = std::ffi::OsString::from(
        String::from_utf8(name.to_vec()).map_err(|_| Signal::GenerationRace)?,
    );
    parent
        .join(name)
        .strip_prefix(root)
        .map(|p| p.to_path_buf())
        .map_err(|_| Signal::UnknownWatch)
}
pub fn translate(
    root: &Path,
    watches: &BTreeMap<i32, PathBuf>,
    expected_ignored: &BTreeSet<i32>,
    events: &[RawEvent],
) -> Result<Translated, Signal> {
    let mut pairs: BTreeMap<u32, (Option<usize>, Option<usize>)> = BTreeMap::new();
    for (i, e) in events.iter().enumerate() {
        if e.mask & crate::watch::IN_Q_OVERFLOW != 0 {
            return Err(Signal::KernelOverflow);
        }
        if e.mask & crate::watch::IN_UNMOUNT != 0 {
            return Err(Signal::WatchLost);
        }
        if e.mask & (FROM | TO) != 0 {
            if e.cookie == 0 || e.mask & (FROM | TO) == (FROM | TO) {
                return Err(Signal::GenerationRace);
            }
            let pair = pairs.entry(e.cookie).or_default();
            let slot = if e.mask & FROM != 0 {
                &mut pair.0
            } else {
                &mut pair.1
            };
            if slot.replace(i).is_some() {
                return Err(Signal::GenerationRace);
            }
        }
    }
    for (from, to) in pairs.values() {
        if let (Some(a), Some(b)) = (from, to) {
            if a >= b || (events[*a].mask & IS_DIR) != (events[*b].mask & IS_DIR) {
                return Err(Signal::GenerationRace);
            }
        }
    }
    let mut map = watches.clone();
    let mut removed = expected_ignored.clone();
    let mut moved = BTreeSet::new();
    let mut sources = BTreeMap::new();
    let mut changes = vec![];
    let mut ignored = vec![];
    for e in events {
        if e.mask & (crate::watch::IN_IGNORED | DELETE_SELF | MOVE_SELF) != 0 {
            continue;
        }
        if removed.contains(&e.wd) {
            continue;
        }
        let parent = map.get(&e.wd).ok_or(Signal::UnknownWatch)?;
        let path = child(root, parent, &e.name)?;
        if path.as_os_str().is_empty() {
            if e.mask == ATTRIB {
                continue;
            }
            return Err(Signal::WatchLost);
        }
        if e.mask & FROM != 0 {
            if pairs.get(&e.cookie).is_some_and(|p| p.1.is_some()) {
                sources.insert(e.cookie, path);
                continue;
            }
            changes.push(Change::Remove(path.clone()));
            if e.mask & IS_DIR != 0 {
                let absolute = root.join(path);
                let ids: Vec<_> = map
                    .iter()
                    .filter(|(_, p)| p.starts_with(&absolute))
                    .map(|(wd, _)| *wd)
                    .collect();
                for wd in ids {
                    map.remove(&wd);
                    removed.insert(wd);
                }
            }
        } else if e.mask & TO != 0 {
            if let Some(from) = sources.remove(&e.cookie) {
                if e.mask & IS_DIR != 0 {
                    let old = root.join(&from);
                    let new = root.join(&path);
                    // Destination watchers represent the overwritten inode and must be retired.
                    let ids: Vec<_> = map
                        .iter()
                        .filter(|(_, p)| p.starts_with(&new))
                        .map(|(wd, _)| *wd)
                        .collect();
                    for wd in ids {
                        map.remove(&wd);
                        removed.insert(wd);
                    }
                    for (wd, p) in &mut map {
                        if let Ok(tail) = p.strip_prefix(&old) {
                            *p = new.join(tail);
                            moved.insert(*wd);
                        }
                    }
                }
                changes.push(Change::Rename { from, to: path });
            } else {
                changes.push(Change::Refresh(path));
            }
        } else if e.mask & DELETE != 0 {
            changes.push(Change::Remove(path.clone()));
            if e.mask & IS_DIR != 0 {
                let absolute = root.join(path);
                let ids: Vec<_> = map
                    .iter()
                    .filter(|(_, p)| p.starts_with(&absolute))
                    .map(|(wd, _)| *wd)
                    .collect();
                for wd in ids {
                    map.remove(&wd);
                    removed.insert(wd);
                }
            }
        } else if e.mask & (CREATE | ATTRIB) != 0 {
            changes.push(Change::Refresh(path));
        } else {
            return Err(Signal::GenerationRace);
        }
    }
    for e in events {
        if e.mask & crate::watch::IN_IGNORED != 0 {
            if !removed.contains(&e.wd) {
                return Err(Signal::WatchLost);
            }
            ignored.push(e.wd);
        } else if e.mask & DELETE_SELF != 0 {
            if !removed.contains(&e.wd) {
                return Err(Signal::WatchLost);
            }
        } else if e.mask & MOVE_SELF != 0 && !removed.contains(&e.wd) && !moved.contains(&e.wd) {
            return Err(Signal::WatchLost);
        }
    }
    Ok(Translated { changes, ignored })
}
