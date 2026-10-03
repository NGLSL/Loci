//! Explicit installation of the fixed installer bundled beside this plugin.
use serde_json::{json, Value};
use std::ffi::c_void;
use std::os::windows::{ffi::OsStrExt, fs::MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Default)]
struct State {
    active: bool,
    message: String,
}
fn state() -> &'static Arc<Mutex<State>> {
    static STATE: OnceLock<Arc<Mutex<State>>> = OnceLock::new();
    STATE.get_or_init(|| Arc::new(Mutex::new(State::default())))
}
pub fn status() -> String {
    state().lock().unwrap().message.clone()
}
pub fn validate_action(action: &str, payload: &Value) -> Result<(), String> {
    if !matches!(action, "install_service" | "show_status") {
        return Err("未知操作；插件只允许安装内置 Loci 服务或查看状态。".into());
    }
    if !payload.is_null() && !payload.as_object().is_some_and(|object| object.is_empty()) {
        return Err("此操作不接受路径、命令或其他参数。".into());
    }
    Ok(())
}
fn installer_path(executable: &Path) -> Result<PathBuf, String> {
    if !executable.is_absolute() {
        return Err("插件程序路径必须是绝对路径。".into());
    }
    let directory = executable.parent().ok_or("插件程序目录不存在。")?;
    // Reject links/junctions at every level, including a redirected plugin root.
    for parent in directory.ancestors() {
        let metadata = std::fs::symlink_metadata(parent)
            .map_err(|error| format!("无法检查插件目录：{error}"))?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err("插件目录不能是链接或重解析点。".into());
        }
    }
    let installer = directory.join("loci-setup.exe");
    let metadata = std::fs::symlink_metadata(&installer).map_err(|error| {
        format!("插件包缺少 loci-setup.exe，请重新导入完整 Loci 插件包：{error}")
    })?;
    if !metadata.is_file() || metadata.file_attributes() & 0x400 != 0 {
        return Err("内置安装程序必须是普通文件，不能是链接或重解析点。".into());
    }
    Ok(installer)
}
pub fn start() -> Result<Value, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let installer = installer_path(&executable)?;
    let shared = Arc::clone(state());
    {
        let mut state = shared.lock().unwrap();
        if state.active {
            return Err("已在等待管理员授权，请完成或取消当前授权。".into());
        }
        state.active = true;
        state.message = "等待管理员授权；完成安装后点击“查看状态”。".into();
    }
    // The RPC response must not wait on a human answering a UAC prompt.
    let worker = Arc::clone(&shared);
    if let Err(error) = std::thread::Builder::new()
        .name("loci-install".into())
        .spawn(move || {
            let result = installer_path(&executable).and_then(|checked| {
                if checked != installer {
                    return Err("安装程序路径已变化。".into());
                }
                launch(&checked)
            });
            let mut state = worker.lock().unwrap();
            state.active = false;
            state.message = match result {
                Ok(()) => "安装程序已启动；完成安装后点击“查看状态”检查 Loci 服务。".into(),
                Err(error) => error,
            };
        })
    {
        let mut state = shared.lock().unwrap();
        state.active = false;
        state.message = format!("无法启动安装任务：{error}");
        return Err(state.message.clone());
    }
    Ok(json!({"ok":true,"message":"正在请求管理员授权；完成安装后查看服务状态。"}))
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteW(
        window: *mut c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> *mut c_void;
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
    fn SetLastError(error: u32);
}
fn launch(installer: &Path) -> Result<(), String> {
    let file: Vec<_> = installer.as_os_str().encode_wide().chain(Some(0)).collect();
    let operation: Vec<_> = "runas".encode_utf16().chain(Some(0)).collect();
    unsafe {
        SetLastError(0);
    }
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    } as isize;
    let error = unsafe { GetLastError() };
    launch_result(result, error)
}
fn launch_result(result: isize, error: u32) -> Result<(), String> {
    if result > 32 {
        return Ok(());
    }
    if error == 1223 {
        Err("已取消管理员授权，Loci 服务尚未安装。".into())
    } else {
        Err(format!(
            "安装程序启动失败（ShellExecute {result}，Windows {error}）；请重新查看服务状态。"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_actions_reject_command_path_and_payload_injection() {
        for action in ["install_service", "show_status"] {
            assert!(validate_action(action, &Value::Null).is_ok());
            assert!(validate_action(action, &json!({})).is_ok());
            for payload in [
                json!({"path":"C:\\evil.exe"}),
                json!({"args":"/S"}),
                json!([]),
                json!("cmd.exe"),
            ] {
                assert!(validate_action(action, &payload).is_err());
            }
        }
        for action in ["configure", "run", "", "install_service --silent"] {
            assert!(validate_action(action, &json!({})).is_err());
        }
    }
    #[test]
    fn uac_cancellation_and_launch_failure_are_not_reported_as_installed() {
        assert!(launch_result(42, 0).is_ok());
        assert!(launch_result(5, 1223)
            .unwrap_err()
            .contains("已取消管理员授权"));
        assert!(launch_result(2, 2)
            .unwrap_err()
            .contains("安装程序启动失败"));
    }
    #[test]
    fn installer_is_fixed_and_requires_a_regular_packaged_file() {
        let root = std::env::temp_dir().join(format!("loci-installer-path-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let executable = root.join("kite-plugin-loci.exe");
        assert!(installer_path(&executable).is_err());
        let installer = root.join("loci-setup.exe");
        std::fs::create_dir(&installer).unwrap();
        assert!(installer_path(&executable).is_err());
        std::fs::remove_dir(&installer).unwrap();
        std::fs::write(&installer, b"fixture only; never executed").unwrap();
        assert_eq!(installer_path(&executable).unwrap(), installer);
        assert!(installer_path(Path::new("kite-plugin-loci.exe")).is_err());
        std::fs::remove_file(installer).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
