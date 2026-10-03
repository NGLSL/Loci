//! Candidates are built outside the publication lock, including failed-volume retries.
use super::*;
use crate::ntfs::NtfsIndex;
use std::collections::BTreeMap;
use std::sync::{atomic::Ordering, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
struct VolumeView {
    index: Option<NtfsIndex>,
    error: Option<String>,
}
#[derive(Default)]
struct Publication {
    volumes: BTreeMap<String, VolumeView>,
    error: Option<String>,
    version: u64,
}
#[derive(Default)]
pub struct IndexOwner {
    publication: Mutex<Publication>,
}
impl IndexOwner {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub fn maintain(self: Arc<Self>, stopped: Arc<AtomicBool>) {
        if let Err(error) = windows::enable_backup_privilege() {
            let mut view = self.publication.lock().unwrap();
            view.error = Some(error.to_string());
            view.version += 1;
            return;
        }
        let directory = match windows::secure_data_directory() {
            Ok(directory) => directory,
            Err(error) => {
                let mut view = self.publication.lock().unwrap();
                view.error = Some(error.to_string());
                view.version += 1;
                return;
            }
        };
        while !stopped.load(Ordering::Acquire) {
            let volumes = windows::ntfs_volumes();
            {
                let mut view = self.publication.lock().unwrap();
                let previous = view.volumes.len();
                view.volumes.retain(|volume, _| volumes.contains(volume));
                let error = if volumes.is_empty() {
                    Some("No fixed NTFS volumes are available".to_owned())
                } else {
                    None
                };
                let changed = previous != view.volumes.len() || view.error != error;
                view.error = error;
                for volume in &volumes {
                    view.volumes.entry(volume.clone()).or_default();
                }
                if changed || previous != view.volumes.len() {
                    view.version += 1;
                }
            }
            for volume in &volumes {
                if stopped.load(Ordering::Acquire) {
                    return;
                }
                let mut candidate = self
                    .publication
                    .lock()
                    .unwrap()
                    .volumes
                    .get(volume)
                    .cloned()
                    .unwrap_or_default();
                let before = signature(&candidate);
                let previous_index = candidate.index.clone();
                let checkpoint = directory.join(format!(
                    "volume-{}.snapshot",
                    volume.chars().next().unwrap()
                ));
                let result = (|| {
                    if let Ok(metadata) = std::fs::symlink_metadata(&checkpoint) {
                        use std::os::windows::fs::MetadataExt;
                        if metadata.file_attributes() & 0x400 != 0 {
                            return Err(io::Error::other("Snapshot is a reparse point"));
                        }
                    }
                    if let Some(index) = &mut candidate.index {
                        index.refresh_cancel(&stopped)?;
                    } else {
                        candidate.index =
                            Some(NtfsIndex::open_cancel(volume, &checkpoint, &stopped)?);
                    }
                    Ok(())
                })();
                if stopped.load(Ordering::Acquire) {
                    return;
                }
                candidate.error = result.err().map(|error| {
                    candidate.index = previous_index;
                    error.to_string()
                });
                let changed = before != signature(&candidate);
                let mut view = self.publication.lock().unwrap();
                view.volumes.insert(volume.clone(), candidate);
                if changed {
                    view.version += 1;
                }
            }
            for _ in 0..20 {
                if stopped.load(Ordering::Acquire) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    fn view(&self) -> (Vec<NtfsIndex>, Response) {
        let view = self.publication.lock().unwrap();
        let indexes: Vec<_> = view
            .volumes
            .values()
            .filter_map(|volume| volume.index.clone())
            .collect();
        let errors: Vec<_> = view
            .error
            .iter()
            .cloned()
            .chain(view.volumes.iter().filter_map(|(volume, state)| {
                state
                    .error
                    .as_ref()
                    .map(|error| format!("{volume}: {error}"))
            }))
            .collect();
        let ready = !indexes.is_empty()
            && indexes.len() == view.volumes.len()
            && errors.is_empty()
            && indexes
                .iter()
                .all(|index| index.status().state.eq_ignore_ascii_case("ready"));
        let response = Response {
            ready,
            pending: !ready,
            error: if errors.is_empty() {
                None
            } else {
                Some(errors.join("; "))
            },
            version: view.version,
            total: indexes.iter().map(|index| index.status().records).sum(),
            total_exact: ready,
            skipped_invalid_names: 0,
            files: vec![],
        };
        (indexes, response)
    }
}
fn signature(view: &VolumeView) -> (Option<(u64, String, Option<String>)>, Option<String>) {
    (
        view.index.as_ref().map(|index| {
            let status = index.status();
            (status.version, status.state, status.error)
        }),
        view.error.clone(),
    )
}
impl Backend for IndexOwner {
    fn start(self: Arc<Self>, stopped: Arc<AtomicBool>) -> Option<std::thread::JoinHandle<()>> {
        Some(std::thread::spawn(move || self.maintain(stopped)))
    }
    fn status(&self) -> Response {
        self.view().1
    }
    fn query(&self, query: &str, filter: Filter, limit: usize) -> Response {
        let (indexes, mut response) = self.view();
        response.total = 0;
        let filter = match filter {
            Filter::All => "all",
            Filter::Images => "images",
            Filter::Documents => "documents",
            Filter::Videos => "videos",
            Filter::Audio => "audio",
            Filter::Archives => "archives",
            Filter::Folders => "folders",
        };
        for index in indexes {
            match index.search(query, limit, filter) {
                Ok(page) => {
                    response.total = response.total.saturating_add(page.total);
                    response.total_exact &= page.total_exact;
                    response.skipped_invalid_names += page.skipped_invalid_names;
                    response
                        .files
                        .extend(page.items.into_iter().filter_map(|item| {
                            if item.path.encode_utf16().ne(item.path_utf16.iter().copied())
                                || item.name.encode_utf16().ne(item.name_utf16.iter().copied())
                            {
                                response.skipped_invalid_names += 1;
                                return None;
                            }
                            Some(FileResult {
                                path: item.path,
                                name: item.name,
                                is_directory: item.is_directory,
                            })
                        }));
                }
                Err(error) => {
                    response.ready = false;
                    response.pending = true;
                    response.total_exact = false;
                    response.error = Some(error.to_string());
                }
            }
        }
        response
            .files
            .sort_by_cached_key(|file| file.path.to_lowercase());
        response.files.truncate(limit);
        response
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_owner_status_is_immediate_and_missing_volume_is_not_ready() {
        let owner = IndexOwner::new();
        assert!(owner.status().pending);
        let mut view = owner.publication.lock().unwrap();
        view.volumes.insert(
            "C:\\".into(),
            VolumeView {
                index: None,
                error: Some("access denied".into()),
            },
        );
        drop(view);
        let response = owner.status();
        assert!(!response.ready);
        assert!(response.error.unwrap().contains("access denied"));
        owner
            .publication
            .lock()
            .unwrap()
            .volumes
            .get_mut("C:\\")
            .unwrap()
            .error = None;
        assert!(owner.status().error.is_none());
        assert!(owner.status().pending);
    }
}
