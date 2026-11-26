use dambo_core::DamboRuntime;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn spawn_with_stdin(cmd: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    child.wait().is_ok_and(|s| s.success())
}

pub(crate) fn copy_to_clipboard(text: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = spawn_with_stdin("pbcopy", &[], text);
    }
    #[cfg(target_os = "windows")]
    {
        let _ = spawn_with_stdin("cmd", &["/C", "clip"], text);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let candidates: [(&str, &[&str]); 3] = [
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ];
        for (cmd, args) in candidates {
            if spawn_with_stdin(cmd, args, text) {
                break;
            }
        }
    }
}

pub(crate) fn normalize_os_path_str(path: &Path, is_windows: bool) -> String {
    let raw_in = path.to_string_lossy();
    let is_win_abs = is_windows
        && (raw_in.starts_with("\\\\")
            || (raw_in.len() >= 2
                && raw_in.as_bytes()[0].is_ascii_alphabetic()
                && raw_in.as_bytes()[1] == b':'));

    let resolved = if path.as_os_str().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else if path.is_relative() && !is_win_abs {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |cwd| cwd.join(path))
    } else {
        path.to_path_buf()
    };

    let normalized = std::fs::canonicalize(&resolved).unwrap_or(resolved);
    let raw = normalized.to_string_lossy().into_owned();

    if is_windows {
        let s = raw.replace('/', "\\");
        if let Some(stripped) = s.strip_prefix("\\\\?\\UNC\\") {
            format!("\\\\{}", stripped)
        } else if let Some(stripped) = s.strip_prefix("\\\\?\\") {
            stripped.to_string()
        } else {
            s
        }
    } else {
        raw
    }
}

pub(crate) fn open_in_os(target: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("open").arg(target).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("cmd")
            .args(["/C", "start", "", target])
            .spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = Command::new("xdg-open").arg(target).spawn();
    }
}

pub(crate) fn open_dir_in_os(dir: &Path) {
    let base = if dir.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        let _ = std::fs::create_dir_all(dir);
        dir.to_path_buf()
    };
    let target = normalize_os_path_str(&base, cfg!(target_os = "windows"));

    #[cfg(target_os = "windows")]
    {
        let _ = Command::new("explorer").arg(&target).spawn();
    }
    #[cfg(not(target_os = "windows"))]
    {
        open_in_os(&target);
    }
}

pub(crate) fn open_file_in_os(path: &Path) {
    let target = normalize_os_path_str(path, cfg!(target_os = "windows"));

    #[cfg(target_os = "macos")]
    {
        let opened = Command::new("open")
            .arg(&target)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !opened {
            let _ = Command::new("open").args(["-t", &target]).spawn();
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        open_in_os(&target);
    }
}

pub(crate) fn open_config_file_in_os(runtime: &DamboRuntime) {
    if runtime.config_manager.path.exists() {
        open_file_in_os(&runtime.config_manager.path);
    } else {
        open_dir_in_os(&runtime.base_dir);
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn show_fatal_startup_dialog(title: &str, message: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            hwnd: *mut std::ffi::c_void,
            lp_text: *const u16,
            lp_caption: *const u16,
            u_type: u32,
        ) -> i32;
    }

    const MB_OK: u32 = 0x0000_0000;
    const MB_ICONERROR: u32 = 0x0000_0010;
    const MB_SYSTEMMODAL: u32 = 0x0000_1000;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;

    let wide_text: Vec<u16> = OsStr::new(message).encode_wide().chain(Some(0)).collect();
    let wide_caption: Vec<u16> = OsStr::new(title).encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide_text` and `wide_caption` are null-terminated UTF-16 buffers that stay alive
    // for the synchronous duration of `MessageBoxW`.
    unsafe {
        let _ = MessageBoxW(
            std::ptr::null_mut(),
            wide_text.as_ptr(),
            wide_caption.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SYSTEMMODAL | MB_SETFOREGROUND,
        );
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn show_fatal_startup_dialog(title: &str, message: &str) {
    let _ = Command::new("osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display alert (item 1 of argv) message (item 2 of argv) as critical",
            "-e",
            "end run",
            title,
            message,
        ])
        .status();
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn show_fatal_startup_dialog(title: &str, message: &str) {
    if Command::new("zenity")
        .args(["--error", "--title", title, "--text", message])
        .status()
        .is_ok_and(|s| s.success())
    {
        return;
    }
    let _ = Command::new("kdialog")
        .args(["--error", message, "--title", title])
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_os_path_str_windows_separators_and_unc_prefix() {
        assert_eq!(
            normalize_os_path_str(Path::new(r"C:\Users\demo\dambo\var/records"), true),
            r"C:\Users\demo\dambo\var\records"
        );
        assert_eq!(
            normalize_os_path_str(Path::new(r"\\?\C:\Users\demo\dambo\var/logs"), true),
            r"C:\Users\demo\dambo\var\logs"
        );
        assert_eq!(
            normalize_os_path_str(Path::new(r"\\?\UNC\server\share\var/records"), true),
            r"\\server\share\var\records"
        );
    }
}
