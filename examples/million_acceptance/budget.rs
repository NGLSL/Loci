use crate::process;
use crate::protocol::{invalid, n, Json};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
#[derive(Clone)]
pub struct Budget {
    root: PathBuf,
    limit: u64,
    checkpoint: Option<PathBuf>,
}
impl Budget {
    pub fn new(root: &Path, limit: u64) -> io::Result<Self> {
        process::owned_directory(root)?;
        Ok(Self {
            root: root.to_owned(),
            limit,
            checkpoint: None,
        })
    }
    pub fn with_checkpoint(root: &Path, limit: u64, checkpoint: &Path) -> io::Result<Self> {
        let mut budget = Self::new(root, limit)?;
        if !checkpoint.starts_with(root) {
            budget.checkpoint = Some(checkpoint.to_owned());
        }
        Ok(budget)
    }
    pub fn used(&self) -> io::Result<u64> {
        let mut todo = vec![self.root.clone()];
        let mut bytes = 0u64;
        let mut entries = 0usize;
        while let Some(dir) = todo.pop() {
            for entry in fs::read_dir(dir)? {
                let path = entry?.path();
                let meta = fs::symlink_metadata(&path)?;
                entries += 1;
                if entries > 100000 {
                    return Err(invalid("artifact entrycountbudget"));
                }
                if meta.uid() != process::uid() || meta.file_type().is_symlink() {
                    return Err(invalid("artifactforeignowner/symlink"));
                }
                if meta.is_dir() {
                    todo.push(path);
                } else if meta.is_file() {
                    bytes = bytes
                        .checked_add(meta.len())
                        .ok_or_else(|| invalid("artifactsizeoverflow"))?;
                } else {
                    return Err(invalid("artifactspecialfile"));
                }
            }
        }
        if let Some(path) = &self.checkpoint {
            match fs::symlink_metadata(path) {
                Ok(meta) => {
                    if !meta.is_file()
                        || meta.uid() != process::uid()
                        || meta.file_type().is_symlink()
                    {
                        return Err(invalid("checkpoint artifact ownership/type"));
                    }
                    bytes = bytes
                        .checked_add(meta.len())
                        .ok_or_else(|| invalid("checkpoint aggregate overflow"))?;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(e),
            }
        }
        Ok(bytes)
    }
    pub fn check(&self, extra: u64) -> io::Result<Json> {
        let used = self.used()?;
        if used.checked_add(extra).is_none_or(|v| v > self.limit) {
            return Err(io::Error::new(io::ErrorKind::StorageFull,"run aggregate artifact budget; retain failed artifacts and choose explicitly larger budget"));
        }
        let fs = process::preflight(&self.root)?;
        if fs.get("available_bytes")?.number()? < extra {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "artifact filesystem extra reserve",
            ));
        }
        Ok(Json::object([
            ("aggregate_used_bytes", n(used)),
            ("aggregate_limit_bytes", n(self.limit)),
            ("admitted_extra_bytes", n(extra)),
        ]))
    }
}
