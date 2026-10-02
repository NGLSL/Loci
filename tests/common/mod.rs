use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
pub struct Fixture {
    pub base: PathBuf,
    pub root: PathBuf,
}
impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}
impl Fixture {
    pub fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let work = std::env::current_dir().unwrap().join("work");
        fs::create_dir_all(&work).unwrap();
        let base = work.join(format!(
            "live-fixture-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&base).unwrap();
        let root = base.join("data");
        fs::create_dir(&root).unwrap();
        Self {
            base: fs::canonicalize(base).unwrap(),
            root: fs::canonicalize(root).unwrap(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let work = fs::canonicalize(std::env::current_dir().unwrap().join("work")).unwrap();
        let base = fs::canonicalize(&self.base).unwrap();
        assert!(
            base.starts_with(work)
                && base
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("live-fixture-")
        );
        fs::remove_dir_all(base).unwrap();
    }
}
