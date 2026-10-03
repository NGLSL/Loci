//! Bounded native namespace enumeration for the Windows performance probe.
//!
//! `FileIdBothDirectoryInfo` is the directory-level fast path.  It returns
//! the name, attributes, and the complete legacy 64-bit file reference in one
//! directory query; callers do not need to open every child.  The optional MFT
//! helper below is deliberately a *projection*: a USN/MFT record name is one
//! directory entry, not proof that all hard-link names have been discovered.

use crate::{
    model::EntryId,
    win::{self, Volume},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    io,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

type RawHandle = *mut c_void;
const INVALID_HANDLE_VALUE: RawHandle = -1isize as RawHandle;

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const FILE_SHARE_READ: u32 = 0x00000001;
const FILE_SHARE_WRITE: u32 = 0x00000002;
const FILE_SHARE_DELETE: u32 = 0x00000004;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x02000000;
const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x00200000;

// FILE_INFO_BY_HANDLE_CLASS values from winbase.h.
const FILE_ID_BOTH_DIR_INFO: u32 = 10;
const FILE_ID_BOTH_DIR_RESTART_INFO: u32 = 11;

const DIRECTORY_BUFFER_BYTES: usize = 64 * 1024;
const DIRECTORY_NAME_UNITS: usize = 255;
const DIRECTORY_ENTRY_BUDGET: usize = 32 * 1024;
const WORK_DEADLINE: Duration = Duration::from_secs(20);

const MFT_RECORD_BUDGET: usize = 2_000_000;
const MFT_PARENT_BUDGET: usize = 32 * 1024;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        creation: u32,
        flags: u32,
        template: RawHandle,
    ) -> RawHandle;
    fn CloseHandle(handle: RawHandle) -> i32;
    fn GetFileInformationByHandle(handle: RawHandle, info: *mut NativeFileInfo) -> i32;
    fn GetFileInformationByHandleEx(
        handle: RawHandle,
        class: u32,
        info: *mut c_void,
        size: u32,
    ) -> i32;
}

/// The fixed prefix of `FILE_ID_BOTH_DIR_INFO`, excluding its variable
/// `FileName` member.  The Windows layout is 104 bytes on the supported
/// targets; the explicit layout assertion below prevents silently using a
/// wrong FileId or FileName offset.
#[cfg(test)]
#[repr(C)]
struct FileIdBothDirInfoFixed {
    next_entry_offset: u32,
    file_index: u32,
    creation_time: i64,
    last_access_time: i64,
    last_write_time: i64,
    change_time: i64,
    end_of_file: i64,
    allocation_size: i64,
    file_attributes: u32,
    file_name_length: u32,
    ea_size: u32,
    short_name_length: u8,
    short_name: [u16; 12],
    file_id: i64,
}

const FILE_ID_BOTH_HEADER_BYTES: usize = 104;
const FILE_ID_OFFSET: usize = 96;
const FILE_NAME_OFFSET: usize = 104;

#[repr(C)]
struct NativeFileInfo {
    attributes: u32,
    creation: [u32; 2],
    access: [u32; 2],
    write: [u32; 2],
    serial: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

struct Handle(RawHandle);

impl Drop for Handle {
    fn drop(&mut self) {
        // CloseHandle is deliberately best-effort in Drop.  The owning call
        // has already returned its actual enumeration error, if any.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// A directory listing obtained from one or more 64 KiB native batches.
///
/// Each tuple is `(EntryId, FILE_ATTRIBUTE_*)`; the parent in every EntryId
/// is the caller-supplied complete legacy FRN.  `batches` counts successful
/// `FileIdBothDirectoryInfo` calls that returned at least one record.
#[derive(Debug)]
pub struct DirectoryListing {
    pub entries: Vec<(EntryId, u32)>,
    pub batches: usize,
}

/// The result of the optional whole-volume MFT projection.
///
/// `entries` contains only records whose parent was present in the supplied
/// set.  `partial_name_projection` is always true: an MFT/USN name record does
/// not enumerate every hard-link name for an object and this result cannot be
/// published as a complete namespace inventory.
#[derive(Debug)]
pub struct MftProjection {
    pub entries: BTreeMap<EntryId, u32>,
    pub total_records: usize,
    pub batches: usize,
    pub elapsed_ms: u128,
    pub final_cursor: u64,
    pub partial_name_projection: bool,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn unsupported(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message.into())
}

fn check_budget(cancel: &AtomicBool, began: Instant) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "native namespace enumeration cancelled; no partial result is publishable",
        ));
    }
    if began.elapsed() > WORK_DEADLINE {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "native namespace enumeration exceeded its 20 second deadline",
        ));
    }
    Ok(())
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn open_directory(path: &Path) -> io::Result<Handle> {
    let path = wide(path);
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(Handle(handle))
    }
}

fn handle_identity(handle: RawHandle) -> io::Result<(u64, u64)> {
    let mut info: NativeFileInfo = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let object = ((info.index_high as u64) << 32) | info.index_low as u64;
    if object == 0 {
        return Err(invalid(
            "directory identity returned a zero legacy file reference",
        ));
    }
    Ok((info.serial as u64, object))
}

fn u32_at(buffer: &[u8], offset: usize) -> io::Result<u32> {
    let bytes = buffer
        .get(offset..offset + 4)
        .ok_or_else(|| invalid("FILE_ID_BOTH_DIR_INFO field exceeds the 64 KiB buffer"))?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}

fn u64_at(buffer: &[u8], offset: usize) -> io::Result<u64> {
    let bytes = buffer
        .get(offset..offset + 8)
        .ok_or_else(|| invalid("FILE_ID_BOTH_DIR_INFO identity exceeds the 64 KiB buffer"))?;
    Ok(u64::from_le_bytes(bytes.try_into().unwrap()))
}

fn name_at(buffer: &[u8], offset: usize, length: usize) -> io::Result<Vec<u16>> {
    if length == 0 || length % 2 != 0 || length / 2 > DIRECTORY_NAME_UNITS {
        return Err(invalid(
            "FILE_ID_BOTH_DIR_INFO has an invalid or over-budget UTF-16 name",
        ));
    }
    let bytes = buffer
        .get(offset..offset + length)
        .ok_or_else(|| invalid("FILE_ID_BOTH_DIR_INFO name exceeds the 64 KiB buffer"))?;
    let mut name = Vec::with_capacity(length / 2);
    for chunk in bytes.chunks_exact(2) {
        let unit = u16::from_le_bytes([chunk[0], chunk[1]]);
        if unit == 0 {
            return Err(invalid(
                "FILE_ID_BOTH_DIR_INFO name contains an embedded NUL",
            ));
        }
        name.push(unit);
    }
    Ok(name)
}

fn is_dot_name(name: &[u16]) -> bool {
    name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16]
}

/// Decode one successful `FileIdBothDirectoryInfo` output buffer.
///
/// The caller owns `seen` across calls, so a duplicate EntryId is rejected
/// even if the API were to repeat an entry in a later batch.
fn decode_directory_batch(
    buffer: &[u8],
    parent: u128,
    seen: &mut BTreeSet<EntryId>,
) -> io::Result<Vec<(EntryId, u32)>> {
    if buffer.len() != DIRECTORY_BUFFER_BYTES {
        return Err(invalid("directory decoder requires a 64 KiB buffer"));
    }
    if parent == 0 || parent > u64::MAX as u128 {
        return Err(unsupported(
            "directory parent must be a complete legacy 64-bit file reference",
        ));
    }
    let mut entries = Vec::new();
    let mut offset = 0usize;
    loop {
        if offset % 8 != 0 || offset + FILE_ID_BOTH_HEADER_BYTES > buffer.len() {
            return Err(invalid(
                "FILE_ID_BOTH_DIR_INFO record has an invalid offset",
            ));
        }
        let next = u32_at(buffer, offset)? as usize;
        let attributes = u32_at(buffer, offset + 56)?;
        let name_length = u32_at(buffer, offset + 60)? as usize;
        let name = name_at(buffer, offset + FILE_NAME_OFFSET, name_length)?;

        // NTFS may legally expose dot entries with a zero FileId.  They are
        // the two and only two names deliberately ignored by this decoder.
        if !is_dot_name(&name) {
            let object = u64_at(buffer, offset + FILE_ID_OFFSET)?;
            if object == 0 {
                return Err(invalid(
                    "FILE_ID_BOTH_DIR_INFO returned a zero file reference",
                ));
            }
            let entry = EntryId {
                parent,
                object: object as u128,
                name,
            };
            if !seen.insert(entry.clone()) {
                return Err(invalid("duplicate FILE_ID_BOTH_DIR_INFO EntryId"));
            }
            entries.push((entry, attributes));
            if seen.len() > DIRECTORY_ENTRY_BUDGET {
                return Err(invalid(
                    "directory listing exceeds the bounded 32768-entry budget",
                ));
            }
        }

        if next == 0 {
            break;
        }
        if next % 8 != 0 || next < FILE_ID_BOTH_HEADER_BYTES || next > buffer.len() - offset {
            return Err(invalid(
                "FILE_ID_BOTH_DIR_INFO NextEntryOffset does not make progress",
            ));
        }
        offset = offset
            .checked_add(next)
            .ok_or_else(|| invalid("FILE_ID_BOTH_DIR_INFO offset overflow"))?;
    }
    Ok(entries)
}

/// Enumerate a directory through `GetFileInformationByHandleEx`.
///
/// The supplied `parent` and `serial` are checked against both the opened
/// handle and a before/after path identity.  A failed check returns an error;
/// callers must retry and cannot publish the partial list.
pub fn list_directory(
    path: &Path,
    parent: u128,
    serial: u64,
    cancel: &AtomicBool,
    began: Instant,
) -> io::Result<DirectoryListing> {
    check_budget(cancel, began)?;
    if parent == 0 || parent > u64::MAX as u128 {
        return Err(unsupported(
            "directory parent must be a complete legacy 64-bit file reference",
        ));
    }
    let before_path = win::identity(path)?;
    if before_path.volume_serial != serial || before_path.object != parent {
        return Err(invalid(
            "directory path identity changed before native enumeration",
        ));
    }
    let handle = open_directory(path)?;
    let before_handle = handle_identity(handle.0)?;
    if before_handle != (serial, parent as u64) {
        return Err(invalid(
            "directory handle identity does not match the requested source",
        ));
    }
    let mut buffer = vec![0u64; DIRECTORY_BUFFER_BYTES / std::mem::size_of::<u64>()];
    let buffer_bytes = unsafe {
        std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), DIRECTORY_BUFFER_BYTES)
    };
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    let mut batches = 0usize;
    // The restart class both resets the directory cursor and returns the
    // first FILE_ID_BOTH_DIR_INFO batch.  Subsequent calls use class 10.
    let mut information_class = FILE_ID_BOTH_DIR_RESTART_INFO;

    loop {
        check_budget(cancel, began)?;
        buffer_bytes.fill(0);
        let ok = unsafe {
            GetFileInformationByHandleEx(
                handle.0,
                information_class,
                buffer_bytes.as_mut_ptr().cast(),
                DIRECTORY_BUFFER_BYTES as u32,
            )
        };
        if ok == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(18) {
                break;
            }
            return Err(error);
        }
        let batch = decode_directory_batch(buffer_bytes, parent, &mut seen)?;
        batches = batches
            .checked_add(1)
            .ok_or_else(|| invalid("directory batch counter overflow"))?;
        entries.extend(batch);
        information_class = FILE_ID_BOTH_DIR_INFO;
        check_budget(cancel, began)?;
    }

    check_budget(cancel, began)?;
    let after_handle = handle_identity(handle.0)?;
    if after_handle != before_handle {
        return Err(invalid(
            "directory identity changed during native enumeration",
        ));
    }
    let after_path = win::identity(path)?;
    if after_path.volume_serial != serial || after_path.object != parent {
        return Err(invalid(
            "directory path identity changed after native enumeration",
        ));
    }
    Ok(DirectoryListing { entries, batches })
}

/// Stream the documented MFT enumeration and retain records for selected
/// parent references only. No outside name is retained in the projection or
/// logged; the Stage A decoder temporarily owns a bounded batch of records.
pub fn enumerate_scope(
    volume: &Volume,
    parents: &BTreeSet<u128>,
    high: i64,
    cancel: &AtomicBool,
) -> io::Result<MftProjection> {
    if parents.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MFT projection requires at least one parent reference",
        ));
    }
    if parents.len() > MFT_PARENT_BUDGET {
        return Err(invalid("MFT projection parent scope exceeds 32768 entries"));
    }
    if parents
        .iter()
        .any(|parent| *parent == 0 || *parent > u64::MAX as u128)
    {
        return Err(unsupported(
            "MFT projection requires complete legacy 64-bit parent references",
        ));
    }
    if high < 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "FSCTL_ENUM_USN_DATA high boundary must be non-negative",
        ));
    }

    let began = Instant::now();
    let mut cursor = 0u64;
    let mut total_records = 0usize;
    let mut batches = 0usize;
    let mut entries = BTreeMap::new();

    loop {
        check_budget(cancel, began)?;
        let (next, records) = match volume.enumerate(cursor, high) {
            Ok(value) => value,
            Err(error) if error.raw_os_error() == Some(38) => break,
            Err(error) => return Err(error),
        };
        if next <= cursor {
            return Err(invalid(
                "MFT enumeration cursor made no progress; projection is incomplete",
            ));
        }
        cursor = next;
        batches = batches
            .checked_add(1)
            .ok_or_else(|| invalid("MFT projection batch counter overflow"))?;
        for record in records {
            total_records = total_records
                .checked_add(1)
                .ok_or_else(|| invalid("MFT record counter overflow"))?;
            if total_records > MFT_RECORD_BUDGET {
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "MFT projection exceeded its 2,000,000-record bounded budget",
                ));
            }
            check_budget(cancel, began)?;
            if record.object == 0 || record.parent == 0 {
                return Err(invalid(
                    "MFT projection returned a zero object or parent reference",
                ));
            }
            if record.object > u64::MAX as u128 || record.parent > u64::MAX as u128 {
                return Err(unsupported(
                    "MFT projection encountered a reference wider than the supported legacy 64-bit namespace",
                ));
            }
            if !parents.contains(&record.parent) {
                // Do not clone, format, or log names outside the selected
                // parent scope.  The record is dropped immediately.
                continue;
            }
            let entry = record.entry_id();
            if entries.insert(entry, record.attributes).is_some() {
                return Err(invalid("duplicate MFT projection EntryId"));
            }
        }
    }

    Ok(MftProjection {
        entries,
        total_records,
        batches,
        elapsed_ms: began.elapsed().as_millis(),
        final_cursor: cursor,
        partial_name_projection: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u32(buffer: &mut [u8], offset: usize, value: u32) {
        buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(buffer: &mut [u8], offset: usize, value: u64) {
        buffer[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn directory_record(next: u32, object: u64, attributes: u32, name: &[u16]) -> Vec<u8> {
        let record_bytes = (FILE_NAME_OFFSET + name.len() * 2 + 7) & !7;
        let mut record = vec![0u8; record_bytes];
        put_u32(&mut record, 0, next);
        put_u32(&mut record, 56, attributes);
        put_u32(&mut record, 60, (name.len() * 2) as u32);
        put_u64(&mut record, FILE_ID_OFFSET, object);
        for (index, unit) in name.iter().copied().enumerate() {
            record[FILE_NAME_OFFSET + index * 2..FILE_NAME_OFFSET + index * 2 + 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
        record
    }

    fn batch(records: &[Vec<u8>]) -> Vec<u8> {
        let mut output = vec![0u8; DIRECTORY_BUFFER_BYTES];
        let mut offset = 0usize;
        for (index, record) in records.iter().enumerate() {
            let next = if index + 1 == records.len() {
                0
            } else {
                record.len() as u32
            };
            let mut record = record.clone();
            put_u32(&mut record, 0, next);
            output[offset..offset + record.len()].copy_from_slice(&record);
            offset += record.len();
        }
        output
    }

    #[test]
    fn directory_decoder_keeps_raw_utf16_and_attributes() {
        let raw_name = [0xd800, b'x' as u16];
        let buffer = batch(&[directory_record(0, 0x1234, 0x10, &raw_name)]);
        let mut seen = BTreeSet::new();
        let entries = decode_directory_batch(&buffer, 0x99, &mut seen).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.parent, 0x99);
        assert_eq!(entries[0].0.object, 0x1234);
        assert_eq!(entries[0].0.name, raw_name);
        assert_eq!(entries[0].1, 0x10);
    }

    #[test]
    fn directory_decoder_ignores_only_dot_entries() {
        let buffer = batch(&[
            directory_record(0, 0, 0x10, &[b'.' as u16]),
            directory_record(0, 0, 0x10, &[b'.' as u16, b'.' as u16]),
        ]);
        // The test buffer must use a valid chain: the helper above is also
        // intentionally exercised with both legal ignored names.
        let mut first = directory_record(0, 0, 0x10, &[b'.' as u16]);
        let second = directory_record(0, 0, 0x10, &[b'.' as u16, b'.' as u16]);
        put_u32(&mut first, 0, second.len() as u32);
        let mut valid = vec![0u8; DIRECTORY_BUFFER_BYTES];
        valid[..first.len()].copy_from_slice(&first);
        valid[first.len()..first.len() + second.len()].copy_from_slice(&second);
        let mut seen = BTreeSet::new();
        assert!(decode_directory_batch(&valid, 1, &mut seen)
            .unwrap()
            .is_empty());
        // Keep `buffer` in the test so malformed construction cannot become
        // an accidental source of an unused parser path during refactoring.
        assert_eq!(buffer.len(), DIRECTORY_BUFFER_BYTES);
    }

    #[test]
    fn directory_decoder_rejects_zero_id_duplicate_bad_offset_and_long_name() {
        let mut seen = BTreeSet::new();
        let zero = batch(&[directory_record(0, 0, 0x20, &[b'x' as u16])]);
        assert!(decode_directory_batch(&zero, 1, &mut seen).is_err());

        let one = batch(&[directory_record(0, 7, 0x20, &[b'x' as u16])]);
        let mut seen = BTreeSet::new();
        assert!(decode_directory_batch(&one, 1, &mut seen).is_ok());
        assert!(decode_directory_batch(&one, 1, &mut seen).is_err());

        let mut bad = one.clone();
        put_u32(&mut bad, 0, 4);
        let mut seen = BTreeSet::new();
        assert!(decode_directory_batch(&bad, 1, &mut seen).is_err());

        let long = batch(&[directory_record(
            0,
            8,
            0x20,
            &vec![b'a' as u16; DIRECTORY_NAME_UNITS + 1],
        )]);
        let mut seen = BTreeSet::new();
        assert!(decode_directory_batch(&long, 1, &mut seen).is_err());
    }

    #[test]
    fn fixed_layout_matches_documented_file_id_both_prefix() {
        assert_eq!(
            std::mem::size_of::<FileIdBothDirInfoFixed>(),
            FILE_ID_BOTH_HEADER_BYTES
        );
        assert_eq!(std::mem::align_of::<FileIdBothDirInfoFixed>(), 8);
    }
}
