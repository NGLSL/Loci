//! Immutable hash partitions. Changed partitions are rebuilt; others share Arc payloads.
//! Metadata comparison is still O(n), intentionally bounded by live snapshot limits.
use crate::index::{Index, Query};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
pub const PARTITIONS: usize = 64;
struct Partition {
    index: Index,
    paths: Vec<PathBuf>,
}
pub(crate) struct Shards {
    parts: Vec<Arc<Partition>>,
}
pub(crate) struct Found {
    pub paths: Vec<PathBuf>,
    pub matches: usize,
    pub cancelled: bool,
}
impl Shards {
    pub fn build(
        paths: Vec<PathBuf>,
        strings: Vec<String>,
        previous: Option<&Self>,
    ) -> (Self, usize, usize) {
        let mut groups: Vec<Vec<(PathBuf, String)>> = (0..PARTITIONS).map(|_| vec![]).collect();
        for (path, text) in paths.into_iter().zip(strings) {
            let hash = text.bytes().fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
            });
            groups[hash as usize % PARTITIONS].push((path, text));
        }
        let mut changed = 0;
        let mut records = 0;
        let mut parts = vec![];
        for (i, group) in groups.into_iter().enumerate() {
            if let Some(old) = previous.and_then(|p| p.parts.get(i)) {
                if old.paths.len() == group.len()
                    && old.paths.iter().zip(&group).all(|(a, (b, _))| a == b)
                {
                    parts.push(old.clone());
                    continue;
                }
            }
            changed += 1;
            records += group.len();
            let (paths, strings): (Vec<_>, Vec<_>) = group.into_iter().unzip();
            parts.push(Arc::new(Partition {
                index: Index::from_paths(strings.into_iter()),
                paths,
            }));
        }
        (Self { parts }, changed, records)
    }
    pub fn search(
        &self,
        q: &Query,
        first50: bool,
        cancel: &AtomicBool,
        progress: &AtomicUsize,
    ) -> Found {
        let mut paths = vec![];
        let mut matches = 0usize;
        let mut checked = 0usize;
        let mut cancelled = false;
        for part in &self.parts {
            if cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let out = part.index.search(
                q,
                true,
                if first50 { 50 } else { usize::MAX },
                cancel,
                &AtomicUsize::new(0),
            );
            checked += out.checked;
            progress.store(checked, Ordering::Release);
            matches += out.matches;
            paths.extend(out.ids.iter().map(|id| part.paths[*id as usize].clone()));
            paths.sort();
            paths.truncate(50); // Merge each partition's lexicographic first 50.
            if out.cancelled {
                cancelled = true;
                break;
            }
        }
        Found {
            paths,
            matches: if first50 { matches.min(50) } else { matches },
            cancelled,
        }
    }
}
