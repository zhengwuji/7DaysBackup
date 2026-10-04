//! 开机自启：HKCU\...\CurrentVersion\Run 注册表键（即时生效）
//! 兼容清理旧版 Python 版创建的启动文件夹 .lnk 快捷方式

use std::path::PathBuf;

use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE};
use winreg::RegKey;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "7DaysBackup";
const SHORTCUT_NAME: &str = "7DaysBackup.lnk";

pub fn legacy_shortcut_path() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(base)
        .join(r"Microsoft\Windows\Start Menu\Programs\Startup")
        .join(SHORTCUT_NAME)
}

/// 生成启动命令："exe" --hidden
pub fn get_command() -> String {
    match std::env::current_exe() {
        Ok(exe) => format!("\"{}\" --hidden", exe.display()),
        Err(_) => "7DaysBackup.exe --hidden".into(),
    }
}

pub fn is_enabled() -> bool {
    if let Ok(hkcu) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(RUN_KEY) {
        if hkcu.get_value::<String, _>(RUN_VALUE).is_ok() {
            return true;
        }
    }
    legacy_shortcut_path().exists() // 兼容旧版 .lnk
}

pub fn enable() -> Result<(), String> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
        .map_err(|e| format!("打开注册表失败: {e}"))?;
    hkcu.set_value(RUN_VALUE, &get_command())
        .map_err(|e| format!("写入注册表失败: {e}"))?;
    disable_legacy_shortcut();
    crate::logger::info("开机自启已启用（注册表 Run 键）");
    Ok(())
}

pub fn disable() -> Result<(), String> {
    if let Ok(hkcu) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
    {
        match hkcu.delete_value(RUN_VALUE) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("删除注册表值失败: {e}")),
        }
    }
    disable_legacy_shortcut();
    crate::logger::info("开机自启已禁用");
    Ok(())
}

fn disable_legacy_shortcut() {
    let path = legacy_shortcut_path();
    if path.exists() {
        let _ = std::fs::remove_file(&path);
        crate::logger::info("已清理旧版启动快捷方式");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_quoted_and_hidden() {
        let cmd = get_command();
        assert!(cmd.starts_with('"'), "cmd: {cmd}");
        assert!(cmd.ends_with("\" --hidden"), "cmd: {cmd}");
    }

    #[test]
    fn shortcut_path_shape() {
        let p = legacy_shortcut_path();
        assert!(p.to_string_lossy().ends_with(r"Startup\7DaysBackup.lnk"));
    }

    #[test]
    fn is_enabled_does_not_panic() {
        let _ = is_enabled();
    }
}
