use super::*;
use std::ffi::c_void;
use std::sync::{atomic::Ordering, OnceLock};
use std::time::{Duration, Instant};

type Handle = *mut c_void;
const INVALID: Handle = -1isize as Handle;
// Interactive users can transfer messages, but cannot create a pipe instance.
const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12019b;;;IU)";
#[repr(C)]
struct SecurityAttributes {
    size: u32,
    descriptor: Handle,
    inherit: i32,
}
#[repr(C)]
struct ServiceStatus {
    service_type: u32,
    state: u32,
    controls: u32,
    win32_exit: u32,
    service_exit: u32,
    checkpoint: u32,
    wait_hint: u32,
}
#[repr(C)]
struct ServiceEntry {
    name: *mut u16,
    main: Option<unsafe extern "system" fn(u32, *mut *mut u16)>,
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        mode: u32,
        pipe_mode: u32,
        instances: u32,
        out_size: u32,
        in_size: u32,
        timeout: u32,
        attributes: *const SecurityAttributes,
    ) -> Handle;
    fn ConnectNamedPipe(handle: Handle, overlapped: Handle) -> i32;
    fn DisconnectNamedPipe(handle: Handle) -> i32;
    fn ReadFile(
        handle: Handle,
        buffer: *mut c_void,
        size: u32,
        read: *mut u32,
        overlapped: Handle,
    ) -> i32;
    fn WriteFile(
        handle: Handle,
        buffer: *const c_void,
        size: u32,
        written: *mut u32,
        overlapped: Handle,
    ) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn LocalFree(memory: Handle) -> Handle;
    fn GetLastError() -> u32;
    fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        sharing: u32,
        attributes: *const c_void,
        creation: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn SetNamedPipeHandleState(
        handle: Handle,
        mode: *const u32,
        max: *const u32,
        timeout: *const u32,
    ) -> i32;
    fn CreateDirectoryW(name: *const u16, attributes: *const SecurityAttributes) -> i32;
    fn GetFileAttributesW(name: *const u16) -> u32;
    fn GetLogicalDrives() -> u32;
    fn GetCurrentProcess() -> Handle;
    fn SetLastError(error: u32);
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn GetVolumeInformationW(
        root: *const u16,
        name: *mut u16,
        name_size: u32,
        serial: *mut u32,
        max_component: *mut u32,
        flags: *mut u32,
        fs: *mut u16,
        fs_size: u32,
    ) -> i32;
    fn GetNamedPipeServerProcessId(handle: Handle, pid: *mut u32) -> i32;
}
#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        result: *mut Handle,
        size: *mut u32,
    ) -> i32;
    fn StartServiceCtrlDispatcherW(entries: *const ServiceEntry) -> i32;
    fn RegisterServiceCtrlHandlerExW(
        name: *const u16,
        handler: Option<unsafe extern "system" fn(u32, u32, Handle, Handle) -> u32>,
        context: Handle,
    ) -> Handle;
    fn SetServiceStatus(handle: Handle, status: *const ServiceStatus) -> i32;
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
    fn OpenServiceW(manager: Handle, name: *const u16, access: u32) -> Handle;
    fn CloseServiceHandle(handle: Handle) -> i32;
    fn QueryServiceStatusEx(
        service: Handle,
        level: u32,
        data: *mut u8,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn QueryServiceConfigW(service: Handle, data: *mut u8, size: u32, needed: *mut u32) -> i32;
    fn GetNamedSecurityInfoW(
        name: *const u16,
        kind: u32,
        info: u32,
        owner: *mut Handle,
        group: *mut Handle,
        dacl: *mut Handle,
        sacl: *mut Handle,
        descriptor: *mut Handle,
    ) -> u32;
    fn IsWellKnownSid(sid: Handle, kind: u32) -> i32;
    fn GetAce(acl: Handle, index: u32, ace: *mut Handle) -> i32;
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
    fn AdjustTokenPrivileges(
        token: Handle,
        disable: i32,
        state: *const TokenPrivileges,
        size: u32,
        previous: Handle,
        returned: *mut u32,
    ) -> i32;
}
#[repr(C)]
struct Luid {
    low: u32,
    high: i32,
}
#[repr(C)]
struct TokenPrivileges {
    count: u32,
    luid: Luid,
    attributes: u32,
}
pub fn enable_backup_privilege() -> io::Result<()> {
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), 0x20 | 8, &mut token) } == 0 {
        return Err(os_error());
    }
    let token = Owned(token);
    let mut luid = Luid { low: 0, high: 0 };
    if unsafe {
        LookupPrivilegeValueW(
            std::ptr::null(),
            wide("SeBackupPrivilege").as_ptr(),
            &mut luid,
        )
    } == 0
    {
        return Err(os_error());
    }
    let state = TokenPrivileges {
        count: 1,
        luid,
        attributes: 2,
    };
    unsafe {
        SetLastError(0);
    }
    if unsafe {
        AdjustTokenPrivileges(
            token.0,
            0,
            &state,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(os_error());
    }
    if unsafe { GetLastError() } != 0 {
        return Err(os_error());
    }
    Ok(())
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn os_error() -> io::Error {
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}
struct Owned(Handle);
unsafe impl Send for Owned {}
impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}
struct Pipe {
    handle: Owned,
    stop: Arc<AtomicBool>,
    deadline: Instant,
}
impl Pipe {
    fn check(&self) -> io::Result<()> {
        if self.stop.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Service stopping",
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Loci IPC timed out",
            ));
        }
        Ok(())
    }
    fn retry(&self) -> io::Result<()> {
        self.check()?;
        std::thread::sleep(Duration::from_millis(10));
        Ok(())
    }
}
impl Read for Pipe {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            let mut count = 0;
            if unsafe {
                ReadFile(
                    self.handle.0,
                    bytes.as_mut_ptr().cast(),
                    bytes.len().min(u32::MAX as usize) as u32,
                    &mut count,
                    std::ptr::null_mut(),
                )
            } != 0
            {
                if count > 0 {
                    return Ok(count as usize);
                }
                self.retry()?;
                continue;
            }
            let error = unsafe { GetLastError() };
            if ![0, 232, 536].contains(&error) {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            self.retry()?;
        }
    }
}
impl Write for Pipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            self.check()?;
            let mut count = 0;
            if unsafe {
                WriteFile(
                    self.handle.0,
                    bytes.as_ptr().cast(),
                    bytes.len().min(u32::MAX as usize) as u32,
                    &mut count,
                    std::ptr::null_mut(),
                )
            } != 0
            {
                if count > 0 {
                    return Ok(count as usize);
                }
                self.retry()?;
                continue;
            }
            let error = unsafe { GetLastError() };
            if ![0, 232, 536].contains(&error) {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            self.retry()?;
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn serve(backend: Arc<dyn Backend>, stop: Arc<AtomicBool>) -> io::Result<()> {
    serve_at(backend, stop, PIPE_NAME)
}
fn serve_at(backend: Arc<dyn Backend>, stop: Arc<AtomicBool>, name: &str) -> io::Result<()> {
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(PIPE_SDDL).as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(os_error());
    }
    struct Descriptor(Handle);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    let descriptor = Descriptor(descriptor);
    let attributes = SecurityAttributes {
        size: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor: descriptor.0,
        inherit: 0,
    };
    // FIRST_PIPE_INSTANCE prevents accepting connections to a pre-existing spoofed pipe.
    let handle = unsafe {
        CreateNamedPipeW(
            wide(name).as_ptr(),
            3 | 0x80000,
            1 | 8,
            1,
            MAX_FRAME as u32,
            MAX_FRAME as u32,
            1000,
            &attributes,
        )
    };
    if handle == INVALID {
        return Err(os_error());
    }
    let mut pipe = Pipe {
        handle: Owned(handle),
        stop: stop.clone(),
        deadline: Instant::now() + Duration::from_secs(5),
    };
    while !stop.load(Ordering::Acquire) {
        let connected = unsafe { ConnectNamedPipe(pipe.handle.0, std::ptr::null_mut()) };
        let error = unsafe { GetLastError() };
        if connected == 0 && error != 535 {
            if ![232, 536].contains(&error) {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            if error == 232 {
                unsafe {
                    DisconnectNamedPipe(pipe.handle.0);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        pipe.deadline = Instant::now() + Duration::from_secs(5);
        let response = match read_frame::<Request>(&mut pipe) {
            Ok(request) => dispatch(backend.as_ref(), request),
            Err(error) => Response::error(error.to_string()),
        };
        if write_frame(&mut pipe, &response).is_ok() {
            // DisconnectNamedPipe discards unread bytes. A client acknowledgment
            // proves the complete frame was consumed without blocking flush.
            let mut acknowledgment = [0];
            let _ = pipe.read_exact(&mut acknowledgment);
        }
        // Nonblocking mode avoids a malicious client holding SCM shutdown indefinitely.
        unsafe {
            DisconnectNamedPipe(pipe.handle.0);
        }
    }
    Ok(())
}
fn verify_system_server(pipe: Handle) -> io::Result<()> {
    let mut pid = 0;
    if unsafe { GetNamedPipeServerProcessId(pipe, &mut pid) } == 0 {
        return Err(os_error());
    }
    // SCM is the authority for the service PID and account. Unlike reading a
    // SYSTEM process token, querying these fields works for ordinary users.
    struct ServiceHandle(Handle);
    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            unsafe {
                CloseServiceHandle(self.0);
            }
        }
    }
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), 1) };
    if manager.is_null() {
        return Err(os_error());
    }
    let manager = ServiceHandle(manager);
    let service = unsafe { OpenServiceW(manager.0, wide(SERVICE_NAME).as_ptr(), 1 | 4) };
    if service.is_null() {
        return Err(os_error());
    }
    let service = ServiceHandle(service);
    let mut status = [0u32; 9];
    let mut needed = 0;
    if unsafe { QueryServiceStatusEx(service.0, 0, status.as_mut_ptr().cast(), 36, &mut needed) }
        == 0
    {
        return Err(os_error());
    }
    if status[1] != 4 || status[7] != pid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Pipe server is not the running Loci service",
        ));
    }
    unsafe {
        QueryServiceConfigW(service.0, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > 8192 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid service configuration",
        ));
    }
    let mut config = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        QueryServiceConfigW(
            service.0,
            config.as_mut_ptr().cast(),
            (config.len() * std::mem::size_of::<usize>()) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(os_error());
    }
    #[repr(C)]
    struct Config {
        service_type: u32,
        start_type: u32,
        error_control: u32,
        binary: *const u16,
        group: *const u16,
        tag: u32,
        dependencies: *const u16,
        account: *const u16,
        display: *const u16,
    }
    let account = unsafe { (*(config.as_ptr().cast::<Config>())).account };
    if account.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Missing service account",
        ));
    }
    let mut length = 0;
    while length < 256 && unsafe { *account.add(length) } != 0 {
        length += 1;
    }
    let account = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(account, length) });
    if !account.eq_ignore_ascii_case("LocalSystem") {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Loci server must run as LocalSystem",
        ));
    }
    Ok(())
}
pub fn request(request: &Request) -> io::Result<Response> {
    request_with_timeout(request, Duration::from_secs(6))
}
pub fn request_with_timeout(request: &Request, timeout: Duration) -> io::Result<Response> {
    let deadline = Instant::now() + timeout;
    let name = wide(PIPE_NAME);
    if timeout.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Loci IPC timed out",
        ));
    }
    if unsafe {
        WaitNamedPipeW(
            name.as_ptr(),
            timeout.as_millis().clamp(1, u32::MAX as u128) as u32,
        )
    } == 0
    {
        return Err(os_error());
    }
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            0x12019b,
            0,
            std::ptr::null(),
            3,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID {
        return Err(os_error());
    }
    let handle = Owned(handle);
    verify_system_server(handle.0)?;
    let mode = 1;
    if unsafe { SetNamedPipeHandleState(handle.0, &mode, std::ptr::null(), std::ptr::null()) } == 0
    {
        return Err(os_error());
    }
    let mut pipe = Pipe {
        handle,
        stop: Arc::new(AtomicBool::new(false)),
        deadline,
    };
    write_frame(&mut pipe, request)?;
    let response = read_frame(&mut pipe)?;
    pipe.write_all(&[0])?;
    Ok(response)
}
pub fn ntfs_volumes() -> Vec<String> {
    let drives = unsafe { GetLogicalDrives() };
    let mut volumes = Vec::new();
    for letter in 0..26 {
        if drives & (1 << letter) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + letter) as char);
        let name = wide(&root);
        if unsafe { GetDriveTypeW(name.as_ptr()) } != 3 {
            continue;
        }
        let mut fs = [0u16; 32];
        if unsafe {
            GetVolumeInformationW(
                name.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                32,
            )
        } == 0
        {
            continue;
        }
        let end = fs.iter().position(|v| *v == 0).unwrap_or(fs.len());
        if String::from_utf16_lossy(&fs[..end]).eq_ignore_ascii_case("NTFS") {
            volumes.push(root.trim_end_matches('\\').to_owned());
        }
    }
    volumes
}
pub fn secure_data_directory() -> io::Result<std::path::PathBuf> {
    let base = std::env::var_os("ProgramData")
        .ok_or_else(|| io::Error::other("ProgramData is unavailable"))?;
    let base = std::path::PathBuf::from(base);
    if !base.is_absolute() {
        return Err(io::Error::other("ProgramData must be absolute"));
    }
    let base_name = wide(&base.to_string_lossy());
    let attributes = unsafe { GetFileAttributesW(base_name.as_ptr()) };
    if attributes == u32::MAX || attributes & 0x400 != 0 {
        return Err(io::Error::other(
            "ProgramData is unavailable or a reparse point",
        ));
    }
    let path = base.join("Loci");
    let name = wide(&path.to_string_lossy());
    let mut descriptor = std::ptr::null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide("O:SYG:SYD:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)").as_ptr(),
            1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(os_error());
    }
    let sa = SecurityAttributes {
        size: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor,
        inherit: 0,
    };
    let created = unsafe { CreateDirectoryW(name.as_ptr(), &sa) };
    let error = unsafe { GetLastError() };
    unsafe {
        LocalFree(descriptor);
    }
    if created == 0 && error != 183 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    verify_data_directory(&path)?;
    Ok(path)
}
pub fn verify_data_directory(path: &std::path::Path) -> io::Result<()> {
    let name = wide(&path.to_string_lossy());
    let attrs = unsafe { GetFileAttributesW(name.as_ptr()) };
    if attrs == u32::MAX || attrs & 16 == 0 || attrs & 0x400 != 0 {
        return Err(io::Error::other(
            "Loci data directory must be a normal directory, not a reparse point",
        ));
    }
    let mut owner = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    let result = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            1,
            1 | 4,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    struct Descriptor(Handle);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
    let _descriptor = Descriptor(descriptor);
    let trusted = |sid: Handle| {
        !sid.is_null()
            && (unsafe { IsWellKnownSid(sid, 22) } != 0 || unsafe { IsWellKnownSid(sid, 26) } != 0)
    };
    if !trusted(owner) || dacl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Loci data directory has an untrusted owner or unrestricted ACL",
        ));
    }
    #[repr(C)]
    struct Acl {
        revision: u8,
        unused: u8,
        size: u16,
        count: u16,
        unused2: u16,
    }
    let count = unsafe { (*(dacl.cast::<Acl>())).count };
    for index in 0..count as u32 {
        let mut ace = std::ptr::null_mut();
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(os_error());
        }
        // Only ordinary allow ACEs for SYSTEM/Admin are accepted. Fail closed
        // for object/callback ACLs instead of trying to reinterpret them.
        let bytes = ace.cast::<u8>();
        let kind = unsafe { *bytes };
        if kind == 1 {
            continue;
        }
        if kind != 0 || !trusted(unsafe { bytes.add(8).cast() }) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Loci data directory grants access to an untrusted principal",
            ));
        }
    }
    Ok(())
}
struct Context {
    backend: Arc<dyn Backend>,
    stop: Arc<AtomicBool>,
    status: std::sync::Mutex<usize>,
    result: std::sync::Mutex<Option<io::Result<()>>>,
}
static CONTEXT: OnceLock<Context> = OnceLock::new();
fn publish(state: u32, exit: u32) {
    let context = CONTEXT.get().unwrap();
    let handle = *context.status.lock().unwrap() as Handle;
    if !handle.is_null() {
        unsafe {
            SetServiceStatus(
                handle,
                &ServiceStatus {
                    service_type: 16,
                    state,
                    controls: if state == 4 { 1 | 4 } else { 0 },
                    win32_exit: exit,
                    service_exit: 0,
                    checkpoint: if state == 2 || state == 3 { 1 } else { 0 },
                    wait_hint: if state == 2 || state == 3 { 10000 } else { 0 },
                },
            );
        }
    }
}
unsafe extern "system" fn handler(control: u32, _: u32, _: Handle, _: Handle) -> u32 {
    if control == 1 || control == 5 {
        let context = CONTEXT.get().unwrap();
        context.stop.store(true, Ordering::Release);
        publish(3, 0);
    }
    0
}
unsafe extern "system" fn main(_: u32, _: *mut *mut u16) {
    let context = CONTEXT.get().unwrap();
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(
            wide(SERVICE_NAME).as_ptr(),
            Some(handler),
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() {
        *context.result.lock().unwrap() = Some(Err(os_error()));
        return;
    }
    *context.status.lock().unwrap() = handle as usize;
    publish(2, 0);
    let owner = context.backend.clone().start(context.stop.clone());
    publish(4, 0);
    let result = serve(context.backend.clone(), context.stop.clone());
    context.stop.store(true, Ordering::Release);
    if let Some(owner) = owner {
        let _ = owner.join();
    }
    publish(1, if result.is_ok() { 0 } else { 1 });
    *context.result.lock().unwrap() = Some(result);
}
pub fn run(backend: Arc<dyn Backend>, stop: Arc<AtomicBool>) -> io::Result<()> {
    CONTEXT
        .set(Context {
            backend,
            stop,
            status: std::sync::Mutex::new(0),
            result: std::sync::Mutex::new(None),
        })
        .map_err(|_| io::Error::other("Service already started"))?;
    let mut name = wide(SERVICE_NAME);
    let entries = [
        ServiceEntry {
            name: name.as_mut_ptr(),
            main: Some(main),
        },
        ServiceEntry {
            name: std::ptr::null_mut(),
            main: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(entries.as_ptr()) } == 0 {
        return Err(os_error());
    }
    CONTEXT
        .get()
        .unwrap()
        .result
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(|| Err(io::Error::other("Service did not start")))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acl_excludes_instance_creation_and_pipe_is_local() {
        let rights = 0x12019bu32;
        assert_eq!(rights & 4, 0);
        assert_eq!(rights & 3, 3);
        let mut descriptor = std::ptr::null_mut();
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide(PIPE_SDDL).as_ptr(),
                    1,
                    &mut descriptor,
                    std::ptr::null_mut(),
                )
            },
            0
        );
        unsafe {
            LocalFree(descriptor);
        }
        assert_eq!(PIPE_NAME, r"\\.\pipe\Loci.Search.v1");
    }
    #[test]
    fn cancellation_is_observed_before_waiting() {
        let pipe = Pipe {
            handle: Owned(std::ptr::null_mut()),
            stop: Arc::new(AtomicBool::new(true)),
            deadline: Instant::now() + Duration::from_secs(5),
        };
        assert_eq!(
            pipe.retry().unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        let mut pipe = pipe;
        let mut byte = [0];
        // read_exact retries Interrupted. Cancellation must terminate instead.
        assert_eq!(
            pipe.read_exact(&mut byte).unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
    #[test]
    fn data_directory_rejects_user_owned_existing_directory() {
        let path = std::env::temp_dir().join(format!("loci-service-acl-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let result = verify_data_directory(&path);
        std::fs::remove_dir(&path).unwrap();
        assert!(
            result.is_err(),
            "The ordinary-user fixture must not be trusted as a privileged data directory"
        );
    }
    #[test]
    fn volume_enumeration_uses_ntfs_backend_drive_spec() {
        for volume in ntfs_volumes() {
            let bytes = volume.as_bytes();
            assert_eq!(bytes.len(), 2);
            assert!(bytes[0].is_ascii_uppercase());
            assert_eq!(bytes[1], b':');
        }
    }
    #[test]
    fn real_pipe_response_survives_disconnect_and_client_can_arrive_first() {
        struct Fixture;
        impl Backend for Fixture {
            fn status(&self) -> Response {
                Response::pending()
            }
            fn query(&self, _: &str, _: Filter, _: usize) -> Response {
                Response {
                    ready: true,
                    pending: false,
                    error: None,
                    version: 7,
                    total: 1,
                    total_exact: true,
                    skipped_invalid_names: 0,
                    files: vec![FileResult {
                        path: r"C:\R&D 100%\报告.txt".into(),
                        name: "报告.txt".into(),
                        is_directory: false,
                    }],
                }
            }
        }
        let name = format!(r"\\.\pipe\Loci.Transport.Test.{}", std::process::id());
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_name = name.clone();
        let worker =
            std::thread::spawn(move || serve_at(Arc::new(Fixture), worker_stop, &worker_name));
        let mut client = None;
        for _ in 0..100 {
            let handle = unsafe {
                CreateFileW(
                    wide(&name).as_ptr(),
                    0x12019b,
                    0,
                    std::ptr::null(),
                    3,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if handle != INVALID {
                let handle = Owned(handle);
                let mode = 1;
                assert_ne!(
                    unsafe {
                        SetNamedPipeHandleState(handle.0, &mode, std::ptr::null(), std::ptr::null())
                    },
                    0
                );
                client = Some(handle);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut client = Pipe {
            handle: client.expect("Test pipe should accept ordinary user"),
            stop: Arc::new(AtomicBool::new(false)),
            deadline: Instant::now() + Duration::from_secs(2),
        };
        write_frame(
            &mut client,
            &Request::Query {
                query: "报告".into(),
                filter: Filter::All,
                limit: 20,
            },
        )
        .unwrap();
        let response: Response = read_frame(&mut client).unwrap();
        assert!(response.ready);
        assert_eq!(response.files[0].name, "报告.txt");
        assert_eq!(response.version, 7);
        client.write_all(&[0]).unwrap();
        drop(client);
        stop.store(true, Ordering::Release);
        assert!(worker.join().unwrap().is_ok());
        // Connecting before the server invokes ConnectNamedPipe yields 535;
        // transport must recognize that as a successful connection.
        let early = format!(r"\\.\pipe\Loci.Transport.Early.{}", std::process::id());
        let handle = unsafe {
            CreateNamedPipeW(
                wide(&early).as_ptr(),
                3 | 0x80000,
                1 | 8,
                1,
                1024,
                1024,
                1000,
                std::ptr::null(),
            )
        };
        assert_ne!(handle, INVALID);
        let server = Owned(handle);
        let handle = unsafe {
            CreateFileW(
                wide(&early).as_ptr(),
                0x80000000 | 0x40000000,
                0,
                std::ptr::null(),
                3,
                0,
                std::ptr::null_mut(),
            )
        };
        assert_ne!(handle, INVALID);
        let _client = Owned(handle);
        assert_eq!(
            unsafe { ConnectNamedPipe(server.0, std::ptr::null_mut()) },
            0
        );
        assert_eq!(unsafe { GetLastError() }, 535);
    }
}
