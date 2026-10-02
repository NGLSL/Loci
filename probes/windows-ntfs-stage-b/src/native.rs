//! Open a journal object by its complete NTFS reference, then enumerate ALL links.
//! This observes names but returns only engineering-scope relative parent paths.
use std::{
    collections::BTreeSet,
    ffi::c_void,
    io, mem,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr,
};
type RawHandle = *mut c_void;
const INVALID: RawHandle = -1isize as RawHandle;
#[repr(C)]
struct Descriptor {
    size: u32,
    kind: u32,
    identity: [u64; 2],
}
#[repr(C)]
struct FileInfo {
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
    fn OpenFileById(
        volume: RawHandle,
        id: *const Descriptor,
        access: u32,
        share: u32,
        security: *const c_void,
        flags: u32,
    ) -> RawHandle;
    fn CloseHandle(handle: RawHandle) -> i32;
    fn GetFileInformationByHandle(handle: RawHandle, info: *mut FileInfo) -> i32;
    fn GetFinalPathNameByHandleW(handle: RawHandle, path: *mut u16, length: u32, flags: u32)
        -> u32;
    fn FindFirstFileNameW(
        name: *const u16,
        flags: u32,
        length: *mut u32,
        output: *mut u16,
    ) -> RawHandle;
    fn FindNextFileNameW(handle: RawHandle, length: *mut u32, output: *mut u16) -> i32;
    fn FindClose(handle: RawHandle) -> i32;
}
struct Handle(RawHandle);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Find(RawHandle);
impl Drop for Find {
    fn drop(&mut self) {
        unsafe {
            FindClose(self.0);
        }
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn information(handle: RawHandle) -> io::Result<(u64, u64, u32)> {
    let mut info: FileInfo = unsafe { mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        ((info.index_high as u64) << 32) | info.index_low as u64,
        info.serial as u64,
        info.links,
    ))
}
fn volume_relative(path: &[u16]) -> io::Result<&[u16]> {
    let at = if path.starts_with(&[92, 92, 63, 92]) {
        4
    } else {
        0
    };
    if path.len() < at + 3 || path[at + 1] != 58 || path[at + 2] != 92 {
        return Err(invalid("drive-letter source path required"));
    }
    Ok(&path[at + 2..])
}
fn scoped_parent(scope: &[u16], name: &[u16]) -> io::Result<Option<PathBuf>> {
    if name.first() != Some(&92) || name.iter().any(|n| matches!(*n, 0 | 47 | 58)) {
        return Err(invalid("invalid volume-relative hardlink name"));
    }
    let components: Vec<_> = name[1..].split(|n| *n == 92).collect();
    if components
        .iter()
        .any(|part| part.is_empty() || *part == [46] || *part == [46, 46])
    {
        return Err(invalid("invalid hardlink name component"));
    }
    if !name.starts_with(scope) || name.get(scope.len()) != Some(&92) {
        return Ok(None);
    }
    let relative = &name[scope.len() + 1..];
    let split = relative.iter().rposition(|n| *n == 92);
    Ok(Some(PathBuf::from(std::ffi::OsString::from_wide(
        split.map(|index| &relative[..index]).unwrap_or(&[]),
    ))))
}
pub fn hardlink_parents(spec: &str, object: u128, root: &Path) -> io::Result<Vec<PathBuf>> {
    let object=u64::try_from(object).map_err(|_|io::Error::new(io::ErrorKind::Unsupported,"128-bit file reference requires FileId128 OpenFileById support; rebuild/downgrade required"))?;
    let mut volume: Vec<u16> = format!(r"\\.\{spec}").encode_utf16().collect();
    volume.push(0);
    let handle = unsafe {
        CreateFileW(
            volume.as_ptr(),
            0x80000000,
            7,
            ptr::null(),
            3,
            0,
            ptr::null_mut(),
        )
    };
    if handle == INVALID {
        return Err(io::Error::last_os_error());
    }
    let volume = Handle(handle);
    let descriptor = Descriptor {
        size: mem::size_of::<Descriptor>() as u32,
        kind: 0,
        identity: [object, 0],
    };
    let handle = unsafe {
        OpenFileById(
            volume.0,
            &descriptor,
            0,
            7,
            ptr::null(),
            0x02000000 | 0x00200000,
        )
    };
    if handle == INVALID {
        let error = io::Error::last_os_error();
        // Only failure to open the complete reference itself proves that the
        // object disappeared. A later pathname disappearance is a race and
        // must propagate so replay discards and retries its candidate.
        if matches!(error.raw_os_error(), Some(2 | 3)) {
            return Ok(Vec::new());
        }
        return Err(error);
    }
    let object_handle = Handle(handle);
    let before = information(object_handle.0)?;
    if before.0 != object {
        return Err(invalid(
            "OpenFileById returned a different complete file reference",
        ));
    }
    let mut path = vec![0u16; 32768];
    let length = unsafe {
        GetFinalPathNameByHandleW(object_handle.0, path.as_mut_ptr(), path.len() as u32, 0)
    };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize >= path.len() {
        return Err(invalid("resolved object path exceeds native buffer budget"));
    }
    path[length as usize] = 0;
    let resolved = PathBuf::from(std::ffi::OsString::from_wide(&path[..length as usize]));
    let path_before = crate::win::identity(&resolved)?;
    if path_before.object != object as u128 || path_before.volume_serial != before.1 {
        return Err(invalid("resolved hardlink path changed object identity"));
    }
    let mut names = vec![0u16; 32768];
    let mut length = names.len() as u32;
    let handle = unsafe { FindFirstFileNameW(path.as_ptr(), 0, &mut length, names.as_mut_ptr()) };
    if handle == INVALID {
        return Err(io::Error::last_os_error());
    }
    let find = Find(handle);
    let raw_scope: Vec<u16> = root.as_os_str().encode_wide().collect();
    let scope = volume_relative(&raw_scope)?;
    let mut parents = BTreeSet::new();
    let mut count = 0usize;
    loop {
        let end = names
            .iter()
            .position(|n| *n == 0)
            .ok_or_else(|| invalid("hardlink name lacks terminator"))?;
        // Names outside scope are transient API output, never retained or logged.
        if let Some(parent) = scoped_parent(scope, &names[..end])? {
            parents.insert(parent);
        }
        count += 1;
        if count > 4096 {
            return Err(invalid("hardlink namespace exceeds 4096-name budget"));
        }
        names.fill(0);
        length = names.len() as u32;
        if unsafe { FindNextFileNameW(find.0, &mut length, names.as_mut_ptr()) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(38) {
                break;
            }
            return Err(error);
        }
    }
    let after = information(object_handle.0)?;
    let path_after = crate::win::identity(&resolved)?;
    if before != after
        || after.2 as usize != count
        || path_after.object != object as u128
        || path_after.volume_serial != before.1
    {
        return Err(invalid(
            "hardlink identity/count changed during native enumeration; retry required",
        ));
    }
    Ok(parents.into_iter().collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().collect()
    }
    #[test]
    fn hardlink_scope_is_component_bounded_and_preserves_raw_names() {
        let scope = wide("\\Project\\run\\fixture");
        assert_eq!(
            scoped_parent(&scope, &wide("\\Project\\run\\fixture\\bucket\\file")).unwrap(),
            Some(PathBuf::from("bucket"))
        );
        assert_eq!(
            scoped_parent(&scope, &wide("\\Project\\run\\fixture-other\\file")).unwrap(),
            None
        );
        assert_eq!(scoped_parent(&scope, &wide("\\other\\file")).unwrap(), None);
        assert_eq!(
            scoped_parent(&scope, &wide("\\Project\\run\\fixture\\file")).unwrap(),
            Some(PathBuf::new())
        );
        let mut name = wide("\\Project\\run\\fixture\\");
        name.extend([0xd800, 92, 97]);
        assert_eq!(
            scoped_parent(&scope, &name)
                .unwrap()
                .unwrap()
                .as_os_str()
                .encode_wide()
                .collect::<Vec<_>>(),
            vec![0xd800]
        );
    }
    #[test]
    fn malformed_hardlink_names_are_explicit_errors() {
        for name in [
            "relative",
            "\\Project\\run\\fixture\\..\\file",
            "\\Project\\run\\fixture\\file:ads",
        ] {
            assert!(scoped_parent(&wide("\\Project\\run\\fixture"), &wide(name)).is_err());
        }
    }
}
