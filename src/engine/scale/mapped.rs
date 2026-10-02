//! Private anonymous storage for large immutable scale buffers. A mapping stays
//! alive through the containing Arc and is returned directly on its last drop.
use std::io;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::OnceLock;

unsafe extern "C" {
    fn mmap(
        address: *mut std::ffi::c_void,
        length: usize,
        protection: i32,
        flags: i32,
        fd: i32,
        offset: isize,
    ) -> *mut std::ffi::c_void;
    fn munmap(address: *mut std::ffi::c_void, length: usize) -> i32;
    fn sysconf(name: i32) -> isize;
}
fn page_size() -> usize {
    static SIZE: OnceLock<usize> = OnceLock::new();
    *SIZE.get_or_init(|| {
        // Linux _SC_PAGESIZE. All types stored here have alignment <= a page.
        let size = unsafe { sysconf(30) };
        assert!(size > 0);
        size as usize
    })
}
pub(super) fn allocation_bytes<T>(capacity: usize) -> io::Result<usize> {
    let bytes = capacity
        .checked_mul(std::mem::size_of::<T>())
        .ok_or_else(|| io::Error::other("mapped capacity overflow"))?;
    let page = page_size();
    bytes
        .checked_add(page - 1)
        .map(|n| n / page * page)
        .ok_or_else(|| io::Error::other("mapped capacity overflow"))
}
pub(super) struct Buffer<T> {
    pointer: NonNull<T>,
    length: usize,
    bytes: usize,
}
impl<T> Default for Buffer<T> {
    fn default() -> Self {
        Self {
            pointer: NonNull::dangling(),
            length: 0,
            bytes: 0,
        }
    }
}
// Exclusive mutation requires &mut Buffer; shared access exposes only &[T].
unsafe impl<T: Send> Send for Buffer<T> {}
unsafe impl<T: Sync> Sync for Buffer<T> {}
impl<T> Buffer<T> {
    pub fn with_capacity(capacity: usize) -> io::Result<Self> {
        let bytes = allocation_bytes::<T>(capacity)?;
        if bytes == 0 {
            return Ok(Self::default());
        }
        assert!(std::mem::align_of::<T>() <= page_size());
        assert!(std::mem::size_of::<T>() != 0);
        // Linux MAP_PRIVATE | MAP_ANONYMOUS, PROT_READ | PROT_WRITE.
        let pointer = unsafe { mmap(std::ptr::null_mut(), bytes, 3, 0x22, -1, 0) };
        if pointer as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            pointer: NonNull::new(pointer.cast()).expect("non-null mmap"),
            length: 0,
            bytes,
        })
    }
    pub fn allocated_bytes(&self) -> usize {
        self.bytes
    }
    pub fn capacity(&self) -> usize {
        self.bytes / std::mem::size_of::<T>()
    }
    pub fn push(&mut self, value: T) {
        assert!(
            self.length < self.capacity(),
            "mapped buffer capacity admitted before growth"
        );
        unsafe {
            self.pointer.as_ptr().add(self.length).write(value);
        }
        self.length += 1;
    }
}
impl<T: Clone> Buffer<T> {
    pub fn try_clone(&self) -> io::Result<Self> {
        let mut copy = Self::with_capacity(self.capacity())?;
        for value in self.iter() {
            copy.push(value.clone());
        }
        Ok(copy)
    }
    pub fn reserve_capacity(&mut self, capacity: usize) -> io::Result<()> {
        if capacity <= self.capacity() {
            return Ok(());
        }
        let mut replacement = Self::with_capacity(capacity)?;
        for value in self.iter() {
            replacement.push(value.clone());
        }
        *self = replacement;
        Ok(())
    }
    pub fn extend_from_slice(&mut self, values: &[T]) {
        for value in values {
            self.push(value.clone());
        }
    }
}
impl<T> Deref for Buffer<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.length) }
    }
}
impl<T> DerefMut for Buffer<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        unsafe { std::slice::from_raw_parts_mut(self.pointer.as_ptr(), self.length) }
    }
}
impl<T> Drop for Buffer<T> {
    fn drop(&mut self) {
        unsafe {
            std::ptr::drop_in_place(std::ptr::slice_from_raw_parts_mut(
                self.pointer.as_ptr(),
                self.length,
            ));
        }
        if self.bytes != 0 {
            unsafe {
                munmap(self.pointer.as_ptr().cast(), self.bytes);
            }
        }
    }
}
