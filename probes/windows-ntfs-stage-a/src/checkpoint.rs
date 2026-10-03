//! Minimal provenance/cursor checkpoint. Does not represent a complete index.
use crate::model::Journal;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(test)]
const MAGIC: &[u8; 8] = b"LCUSNCP\0";
#[cfg(test)]
const SIZE: usize = 48;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    pub volume_serial: u64,
    pub journal_id: u64,
    pub cursor: i64,
}

fn rebuild(message: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("checkpoint requires rebuild: {message}"),
    )
}
#[cfg(test)]
fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

impl Checkpoint {
    pub fn validate(&self, volume_serial: u64, journal: &Journal) -> io::Result<()> {
        if self.volume_serial != volume_serial {
            return Err(rebuild("volume identity changed"));
        }
        if self.journal_id != journal.id {
            return Err(rebuild("journal identity changed"));
        }
        if journal.first < 0
            || journal.lowest < 0
            || journal.next < journal.first.max(journal.lowest)
        {
            return Err(rebuild("invalid journal boundaries"));
        }
        if self.cursor < journal.first.max(journal.lowest) {
            return Err(rebuild("cursor no longer retained in journal"));
        }
        if self.cursor > journal.next {
            return Err(rebuild("cursor exceeds journal end"));
        }
        Ok(())
    }

    #[cfg(test)]
    fn encode(&self) -> [u8; SIZE] {
        let mut b = [0; SIZE];
        b[..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&1u32.to_le_bytes());
        b[16..24].copy_from_slice(&self.volume_serial.to_le_bytes());
        b[24..32].copy_from_slice(&self.journal_id.to_le_bytes());
        b[32..40].copy_from_slice(&self.cursor.to_le_bytes());
        let hash = checksum(&b[..40]);
        b[40..].copy_from_slice(&hash.to_le_bytes());
        b
    }

    #[cfg(test)]
    pub fn load(path: &Path) -> io::Result<Self> {
        use std::io::Read;
        let file = fs::File::open(path)?;
        let mut b = Vec::with_capacity(SIZE + 1);
        file.take((SIZE + 1) as u64).read_to_end(&mut b)?;
        if b.len() != SIZE
            || &b[..8] != MAGIC
            || b[8..12] != 1u32.to_le_bytes()
            || b[12..16] != [0; 4]
        {
            return Err(rebuild("invalid checkpoint format/version/length"));
        }
        if checksum(&b[..40]) != u64::from_le_bytes(b[40..48].try_into().unwrap()) {
            return Err(rebuild("checkpoint checksum mismatch"));
        }
        Ok(Self {
            volume_serial: u64::from_le_bytes(b[16..24].try_into().unwrap()),
            journal_id: u64::from_le_bytes(b[24..32].try_into().unwrap()),
            cursor: i64::from_le_bytes(b[32..40].try_into().unwrap()),
        })
    }

    /// Same-directory create_new temporary, flush, atomic replacement. On any
    /// pre-replacement failure the previous checkpoint remains intact.
    #[cfg(test)]
    pub fn save(&self, path: &Path) -> io::Result<()> {
        atomic_save(path, &self.encode())
    }
}

/// One durable file carries inventory and cursor together; callers never
/// advance a separate cursor file ahead of the last committed namespace.
pub(crate) fn atomic_save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write(path, bytes, true)
}

/// Bootstrap cannot replace a snapshot created concurrently by another run.
pub(crate) fn atomic_create(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write(path, bytes, false)
}

fn atomic_write(path: &Path, bytes: &[u8], replace_existing: bool) -> io::Result<()> {
    let filename = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "checkpoint needs a filename")
    })?;
    let mut name = filename.to_os_string();
    name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = path.with_file_name(name);
    let mut created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        created = true;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace(&temp, path, replace_existing)
    })();
    if result.is_err() && created {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(windows)]
fn replace(from: &Path, to: &Path, replace_existing: bool) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut v: Vec<u16> = path.as_os_str().encode_wide().collect();
        if v.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in checkpoint path",
            ));
        }
        v.push(0);
        Ok(v)
    }
    let from = wide(from)?;
    let to = wide(to)?;
    let flags = 8 | u32::from(replace_existing);
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(not(windows))]
fn replace(from: &Path, to: &Path, replace_existing: bool) -> io::Result<()> {
    if replace_existing {
        fs::rename(from, to)
    } else {
        fs::hard_link(from, to)?;
        fs::remove_file(from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn journal() -> Journal {
        Journal {
            id: 20,
            first: 100,
            lowest: 120,
            next: 200,
            min_major: 2,
            max_major: 3,
        }
    }
    #[test]
    fn injected_invalid_provenance_and_cursor_require_rebuild() {
        let valid = Checkpoint {
            volume_serial: 10,
            journal_id: 20,
            cursor: 150,
        };
        assert!(valid.validate(10, &journal()).is_ok());
        assert!(valid.validate(11, &journal()).is_err());
        let mut recreated = journal();
        recreated.id += 1;
        assert!(valid.validate(10, &recreated).is_err());
        for cursor in [119, 201, -1] {
            let c = Checkpoint {
                cursor,
                ..valid.clone()
            };
            assert!(c.validate(10, &journal()).is_err());
        }
        let mut wrapped = journal();
        wrapped.first = 160;
        assert!(valid.validate(10, &wrapped).is_err());
        for cursor in [120, 200] {
            assert!(Checkpoint {
                cursor,
                ..valid.clone()
            }
            .validate(10, &journal())
            .is_ok());
        }
    }
    #[test]
    fn file_roundtrip_corruption_and_failed_replace() {
        // Pure storage fixture; no kernel journal rollover/recreation claimed.
        let test_base =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.scratch/windows-ntfs-stage-a/run");
        fs::create_dir_all(&test_base).unwrap();
        let dir = test_base.join(format!(
            "checkpoint-unit-{}-{}-{}",
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("cursor.bin");
        let c = Checkpoint {
            volume_serial: 1,
            journal_id: 2,
            cursor: 3,
        };
        c.save(&path)
            .expect("initial checkpoint save must succeed in owned engineering test root");
        assert_eq!(Checkpoint::load(&path).unwrap(), c);
        let next = Checkpoint {
            cursor: 4,
            ..c.clone()
        };
        next.save(&path).unwrap();
        assert_eq!(Checkpoint::load(&path).unwrap(), next);
        let creation_failure = atomic_create(&path, &c.encode()).unwrap_err();
        assert!(creation_failure.raw_os_error().is_some());
        assert_eq!(Checkpoint::load(&path).unwrap(), next);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            let locked = OpenOptions::new()
                .read(true)
                .share_mode(0)
                .open(&path)
                .unwrap();
            let failure = c.save(&path).unwrap_err();
            assert!(failure.raw_os_error().is_some());
            drop(locked);
            assert_eq!(Checkpoint::load(&path).unwrap(), next);
        }
        let mut bytes = fs::read(&path).unwrap();
        bytes[24] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert!(Checkpoint::load(&path).is_err());
        for len in [0, 47, 49] {
            fs::write(&path, vec![0; len]).unwrap();
            assert!(Checkpoint::load(&path).is_err());
        }
        let destination_dir = dir.join("occupied");
        fs::create_dir(&destination_dir).unwrap();
        assert!(next.save(&destination_dir).is_err());
        assert!(destination_dir.is_dir());
        fs::remove_dir_all(&dir).unwrap();
    }
}
