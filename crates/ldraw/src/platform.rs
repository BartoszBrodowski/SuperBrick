//! The only place with operating-system specific code. Other crates call these functions and
//! add no `cfg` blocks of their own.
//!
//! On every non-Windows target the paths and commands are exactly the macOS ones.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The user's home directory: `$HOME` on Unix, `%USERPROFILE%` (else `%HOMEDRIVE%%HOMEPATH%`) on Windows.
/// Empty when nothing is set.
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
            return PathBuf::from(p);
        }
        let mut p = std::ffi::OsString::new();
        p.push(std::env::var_os("HOMEDRIVE").unwrap_or_default());
        p.push(std::env::var_os("HOMEPATH").unwrap_or_default());
        PathBuf::from(p)
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
    }
}

/// Parent of the per-app support directory: `~/Library/Application Support` (all non-Windows
/// targets) or `%APPDATA%` on Windows.
pub fn support_root() -> PathBuf {
    #[cfg(windows)]
    {
        windows_dir("APPDATA", "AppData/Roaming")
    }
    #[cfg(not(windows))]
    {
        home_dir().join("Library/Application Support")
    }
}

/// Parent of the per-app cache directory: `~/Library/Caches` (all non-Windows targets) or
/// `%LOCALAPPDATA%` on Windows.
pub fn cache_root() -> PathBuf {
    #[cfg(windows)]
    {
        windows_dir("LOCALAPPDATA", "AppData/Local")
    }
    #[cfg(not(windows))]
    {
        home_dir().join("Library/Caches")
    }
}

#[cfg(windows)]
fn windows_dir(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => home_dir().join(fallback),
    }
}

/// Opens a file or folder with the default application (`open` on macOS, `ShellExecuteW` on Windows).
pub fn open_path(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let file: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: both strings are NUL-terminated and outlive the call; other pointers may be null.
        let r = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if (r as usize) <= 32 {
            return Err(io::Error::other(format!("ShellExecuteW failed with code {}", r as usize)));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        Command::new("open").arg(path).spawn().map(|_| ())
    }
}

/// Shows a file selected in the file manager (`open -R` on macOS, `explorer.exe /select,` on Windows).
pub fn reveal_path(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut arg = std::ffi::OsString::from("/select,\"");
        arg.push(path.as_os_str());
        arg.push("\"");
        Command::new("explorer.exe").raw_arg(arg).spawn().map(|_| ())
    }
    #[cfg(not(windows))]
    {
        Command::new("open").arg("-R").arg(path).spawn().map(|_| ())
    }
}

/// Keeps a child process from opening a console window on Windows; a no-op elsewhere.
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW)
    }
    #[cfg(not(windows))]
    {
        cmd
    }
}

/// Whether a process with this id is running. Unix: `kill -0` (true if `kill` cannot be run).
/// Windows: `OpenProcess` plus `GetExitCodeProcess == STILL_ACTIVE`; access denied counts as alive.
pub fn pid_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        const STILL_ACTIVE: u32 = 259;
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return GetLastError() == ERROR_ACCESS_DENIED;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok == 0 || code == STILL_ACTIVE
        }
    }
    #[cfg(not(windows))]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    }
}

/// An executable's file name: `stem` plus the platform suffix (`sb` / `sb.exe`).
pub fn exe_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

/// A shared library's file name with no `lib` prefix: `.dylib` on macOS, `.dll` on Windows, else `.so`.
pub fn shared_lib_name(stem: &str) -> String {
    let ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(windows) {
        "dll"
    } else {
        "so"
    };
    format!("{stem}.{ext}")
}

/// What the platform's file manager is called in UI text.
pub fn file_manager_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Finder"
    } else if cfg!(windows) {
        "Explorer"
    } else {
        "file manager"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(exe_name("sb"), format!("sb{}", std::env::consts::EXE_SUFFIX));
        let lib = shared_lib_name("studio_rays");
        assert!(lib.starts_with("studio_rays."));
        if cfg!(target_os = "macos") {
            assert_eq!(lib, "studio_rays.dylib");
        }
        if cfg!(windows) {
            assert_eq!(lib, "studio_rays.dll");
        }
    }

    #[test]
    fn own_pid_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[cfg(not(windows))]
    #[test]
    fn roots_match_legacy_paths() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        assert_eq!(support_root(), home.join("Library/Application Support"));
        assert_eq!(cache_root(), home.join("Library/Caches"));
        assert_eq!(home_dir(), home);
    }
}
