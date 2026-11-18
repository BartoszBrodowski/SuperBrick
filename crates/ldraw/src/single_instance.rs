//! One running SuperBrick window per user on Windows: a second GUI launch with a file hands the
//! file to the first one over a named pipe and exits (the Windows counterpart of macOS opening
//! documents in the running app).
//!
//! Pipe: `\\.\pipe\SuperBrick-<user SID>` (falls back to the user name). The pipe is created with
//! the process's default security descriptor, so only the same user, administrators and SYSTEM can
//! connect, and remote clients are rejected. Protocol, one connection per message:
//! the client writes `SuperBrick-open 1\n`, then one absolute path per line (UTF-8), then an empty
//! line; the server answers one byte (`'+'`) once the paths are queued.
//!
//! On other platforms [`forward`] returns false and [`serve`] does nothing.
use std::path::PathBuf;

const HEADER: &str = "SuperBrick-open 1";
/// Upper bound on one message, so a confused client cannot make the server buffer without limit.
#[cfg(windows)]
const MAX_MESSAGE: usize = 1 << 20;

/// Wire form of a list of paths.
pub fn encode_message(paths: &[PathBuf]) -> String {
    let mut s = String::from(HEADER);
    s.push('\n');
    for p in paths {
        let line = p.to_string_lossy();
        // A line break inside a path would split it; Windows file names cannot contain one.
        if !line.is_empty() && !line.contains(['\n', '\r']) {
            s.push_str(&line);
            s.push('\n');
        }
    }
    s.push('\n');
    s
}

/// Parses a message; `None` when the header is wrong. Stops at the first empty line.
pub fn decode_message(text: &str) -> Option<Vec<PathBuf>> {
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    if lines.next()? != HEADER {
        return None;
    }
    Some(lines.take_while(|l| !l.is_empty()).map(PathBuf::from).collect())
}

/// Keeps ASCII letters, digits, `-` and `_`; everything else (including `\`) becomes `_`.
pub fn sanitize_pipe_part(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Full pipe path for a per-user id.
pub fn pipe_name(user_id: &str) -> String {
    format!(r"\\.\pipe\SuperBrick-{}", sanitize_pipe_part(user_id))
}

/// Sends `paths` to a running SuperBrick of this user; true once it acknowledged them.
pub fn forward(paths: &[PathBuf]) -> bool {
    #[cfg(windows)]
    {
        win::forward(paths)
    }
    #[cfg(not(windows))]
    {
        let _ = paths;
        false
    }
}

/// Starts the pipe server thread; `on_paths` runs on that thread for every message received.
/// Does nothing when another instance already owns the pipe.
pub fn serve(on_paths: impl Fn(Vec<PathBuf>) + Send + Sync + 'static) {
    #[cfg(windows)]
    win::serve(on_paths);
    #[cfg(not(windows))]
    let _ = on_paths;
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use std::sync::Arc;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GetLastError, HANDLE,
        INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_DUPLEX};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};

    /// The user's SID string, or the sanitised user name when the SID is unavailable.
    fn user_id() -> String {
        sid_string().unwrap_or_else(|| {
            let user = std::env::var("USERNAME").unwrap_or_default();
            let domain = std::env::var("USERDOMAIN").unwrap_or_default();
            format!("{domain}-{user}")
        })
    }

    fn sid_string() -> Option<String> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut out = None;
            if len > 0 {
                // u64 elements keep the buffer aligned for TOKEN_USER.
                let mut buf = vec![0u64; (len as usize).div_ceil(8)];
                if GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) != 0 {
                    let user = &*(buf.as_ptr() as *const TOKEN_USER);
                    let mut wide: *mut u16 = std::ptr::null_mut();
                    if ConvertSidToStringSidW(user.User.Sid, &mut wide) != 0 && !wide.is_null() {
                        let n = (0..).take_while(|&i| *wide.add(i) != 0).count();
                        out = String::from_utf16(std::slice::from_raw_parts(wide, n)).ok();
                        LocalFree(wide.cast());
                    }
                }
            }
            CloseHandle(token);
            out
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }

    fn name() -> String {
        pipe_name(&user_id())
    }

    fn open_pipe(name: &str) -> Option<File> {
        for _ in 0..20 {
            match OpenOptions::new().read(true).write(true).open(name) {
                Ok(f) => return Some(f),
                // Every instance is momentarily busy with another client: try again shortly.
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(_) => return None,
            }
        }
        None
    }

    pub fn forward(paths: &[PathBuf]) -> bool {
        // Paths are resolved here: the running instance has a different working directory.
        let paths: Vec<PathBuf> = paths.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())).collect();
        let message = encode_message(&paths);
        let name = name();
        // The running window may take the foreground from us only if we allow it.
        unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        let (tx, rx) = std::sync::mpsc::channel();
        // A std File cannot time out, so the exchange runs on its own thread.
        std::thread::spawn(move || {
            let ok = (|| {
                let mut pipe = open_pipe(&name)?;
                pipe.write_all(message.as_bytes()).ok()?;
                let mut ack = [0u8; 1];
                pipe.read_exact(&mut ack).ok()?;
                (ack[0] == b'+').then_some(())
            })();
            let _ = tx.send(ok.is_some());
        });
        rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
    }

    fn create_instance(name: &[u16], first: bool) -> Result<File, u32> {
        let open_mode = PIPE_ACCESS_DUPLEX | if first { FILE_FLAG_FIRST_PIPE_INSTANCE } else { 0 };
        // Null security attributes: the default DACL of this process's token (this user,
        // administrators, SYSTEM).
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                4096,
                4096,
                0,
                std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(unsafe { GetLastError() });
        }
        Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
    }

    fn handle_client(mut pipe: File, on_paths: &dyn Fn(Vec<PathBuf>)) {
        let mut data = Vec::new();
        let mut chunk = [0u8; 4096];
        // Messages end with an empty line ("\n\n"); the header-only message is the shortest.
        while !data.windows(2).any(|w| w == b"\n\n") && data.len() < MAX_MESSAGE {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => data.extend_from_slice(&chunk[..n]),
            }
        }
        if let Some(paths) = std::str::from_utf8(&data).ok().and_then(decode_message) {
            if !paths.is_empty() {
                on_paths(paths);
            }
            let _ = pipe.write_all(b"+");
            unsafe { windows_sys::Win32::Storage::FileSystem::FlushFileBuffers(std::os::windows::io::AsRawHandle::as_raw_handle(&pipe) as HANDLE) };
        }
        unsafe { DisconnectNamedPipe(std::os::windows::io::AsRawHandle::as_raw_handle(&pipe) as HANDLE) };
    }

    pub fn serve(on_paths: impl Fn(Vec<PathBuf>) + Send + Sync + 'static) {
        let name = wide(&name());
        // The first instance proves we are the only server; if it fails, another SuperBrick
        // already serves (or the pipe name is taken by something else) and we stay quiet.
        let first = match create_instance(&name, true) {
            Ok(f) => f,
            Err(_) => return,
        };
        let on_paths = Arc::new(on_paths);
        let _ = std::thread::Builder::new().name("single-instance".into()).spawn(move || {
            let mut pipe = first;
            loop {
                let raw = std::os::windows::io::AsRawHandle::as_raw_handle(&pipe) as HANDLE;
                let connected = unsafe { ConnectNamedPipe(raw, std::ptr::null_mut()) } != 0
                    || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
                // Open the next instance before serving this one so clients never see a gap.
                let next = create_instance(&name, false);
                if connected {
                    let cb = on_paths.clone();
                    let _ = std::thread::spawn(move || handle_client(pipe, &*cb));
                }
                match next {
                    Ok(f) => pipe = f,
                    Err(_) => return,
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip() {
        let paths = vec![PathBuf::from(r"C:\Users\me\My Build.sb"), PathBuf::from("/tmp/ünï.ldr")];
        let text = encode_message(&paths);
        assert!(text.ends_with("\n\n"));
        assert_eq!(decode_message(&text), Some(paths));
    }

    #[test]
    fn message_edge_cases() {
        assert_eq!(decode_message("nope\n\n"), None);
        assert_eq!(decode_message(&encode_message(&[])), Some(vec![]));
        // CRLF and trailing data after the blank line are tolerated / ignored.
        assert_eq!(decode_message("SuperBrick-open 1\r\na.sb\r\n\r\nignored\n"), Some(vec![PathBuf::from("a.sb")]));
        // A path with a line break is dropped, not split.
        assert_eq!(decode_message(&encode_message(&[PathBuf::from("a\nb")])), Some(vec![]));
    }

    #[test]
    fn pipe_names_are_safe() {
        assert_eq!(pipe_name("S-1-5-21-1-2-3-1001"), r"\\.\pipe\SuperBrick-S-1-5-21-1-2-3-1001");
        assert_eq!(pipe_name(r"DOM\us er/x"), r"\\.\pipe\SuperBrick-DOM_us_er_x");
    }

    #[cfg(not(windows))]
    #[test]
    fn inert_off_windows() {
        assert!(!forward(&[PathBuf::from("a.sb")]));
        serve(|_| {});
    }
}
