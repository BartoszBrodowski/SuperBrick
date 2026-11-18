//! SuperBrick.exe: the double-click and file-association entry point on Windows.
//!
//! It is a GUI-subsystem program (no console flash) that starts `sb.exe` from its own folder
//! with the same arguments, without waiting, and exits. If `sb.exe` cannot be started it shows
//! a message box. It is never installed on other platforms.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let result = std::env::current_exe().and_then(|me| {
        let dir = me
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| std::io::Error::other("cannot find the folder of SuperBrick.exe"))?;
        let sb = dir.join("sb.exe");
        std::process::Command::new(&sb)
            .args(std::env::args_os().skip(1))
            // Keep the caller's working directory: relative paths on the command line mean
            // what they mean to the caller (Explorer passes absolute paths anyway).
            .env("SB_SHELL_LAUNCH", "1")
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", sb.display())))
    });
    if let Err(e) = result {
        message_box(&format!("SuperBrick could not start.\n\n{e}"));
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn message_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(text), wide("SuperBrick"));
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

#[cfg(not(windows))]
fn main() {
    eprintln!("SuperBrick.exe is the Windows launcher; on this platform run `sb`.");
    std::process::exit(1);
}
