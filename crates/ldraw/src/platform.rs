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

/// Whether a process with this id is running; true when that cannot be checked (see [`pid_probe`]).
pub fn pid_alive(pid: u32) -> bool {
    pid_probe(pid).unwrap_or(true)
}

/// Whether a process with this id is running, or `None` when that cannot be checked.
/// Unix: `kill -0` (`None` if `kill` cannot be run). Windows: `OpenProcess`, then
/// `WaitForSingleObject(…, 0)` (an exit code of 259 cannot be mistaken for "still running");
/// access denied counts as alive.
pub fn pid_probe(pid: u32) -> Option<bool> {
    if pid == std::process::id() {
        return Some(true);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, GetLastError, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid);
            if h.is_null() {
                return Some(GetLastError() == ERROR_ACCESS_DENIED);
            }
            let state = WaitForSingleObject(h, 0);
            CloseHandle(h);
            match state {
                WAIT_TIMEOUT => Some(true),
                WAIT_OBJECT_0 => Some(false),
                _ => None,
            }
        }
    }
    #[cfg(not(windows))]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .ok()
            .map(|s| s.success())
    }
}

/// `std::fs::canonicalize`, without the `\\?\` verbatim prefix Windows adds when the plain form
/// means the same (`\\?\C:\x` → `C:\x`, `\\?\UNC\srv\share` → `\\srv\share`), so paths shown to
/// people and handed to Explorer or other programs look as usual. Unchanged elsewhere.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path).map(|p| strip_verbatim(&p))
}

/// Removes a `\\?\` / `\\?\UNC\` prefix from a drive or UNC path; other paths are returned as is.
pub fn strip_verbatim(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else { return path.to_path_buf() };
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') && rest.as_bytes()[0].is_ascii_alphabetic() => {
            PathBuf::from(rest)
        }
        _ => path.to_path_buf(),
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
        assert_eq!(pid_probe(std::process::id()), Some(true));
    }

    #[test]
    fn verbatim_prefix_is_stripped() {
        assert_eq!(strip_verbatim(Path::new(r"\\?\C:\Users\me\x.sb")), PathBuf::from(r"C:\Users\me\x.sb"));
        assert_eq!(strip_verbatim(Path::new(r"\\?\UNC\srv\share\x.sb")), PathBuf::from(r"\\srv\share\x.sb"));
        assert_eq!(strip_verbatim(Path::new(r"\\?\Volume{1}\x")), PathBuf::from(r"\\?\Volume{1}\x"));
        assert_eq!(strip_verbatim(Path::new("/Users/me/x.sb")), PathBuf::from("/Users/me/x.sb"));
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
