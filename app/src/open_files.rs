//! Files the operating system asks us to open: double-clicking a `.sb` in Finder,
//! "Open With", dropping on the Dock icon, or `open -a SuperBrick file`.
//!
//! macOS delivers these as an "open documents" Apple event, not as command-line
//! arguments, and winit does not handle it. We install our own handler when the
//! application is about to finish launching (AppKit installs its default handlers
//! during launch, so an earlier one would be replaced) and queue the paths for the
//! app, which drains them at startup and on every frame.
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static QUEUE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static WAKE: OnceLock<egui::Context> = OnceLock::new();

/// Paths received since the last call, oldest first.
pub fn take() -> Vec<PathBuf> {
    std::mem::take(&mut *QUEUE.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Lets the handler wake the UI when a file arrives while the app is idle.
pub fn set_context(ctx: &egui::Context) {
    let _ = WAKE.set(ctx.clone());
}

#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn push(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    QUEUE.lock().unwrap_or_else(|e| e.into_inner()).extend(paths);
    if let Some(ctx) = WAKE.get() {
        ctx.request_repaint();
    }
}

/// Windows: a second launch forwards its file here; open it and bring the window forward.
#[cfg(windows)]
fn on_forwarded(paths: Vec<PathBuf>) {
    push(paths);
    if let Some(ctx) = WAKE.get() {
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
/// Turns a `file://` URL into a path (percent-decoding, optional `localhost` host).
fn url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()))
            .flatten();
        if let Some(b) = hex {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let mut s = String::from_utf8(out).ok()?;
    if s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    Some(PathBuf::from(s))
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{push, url_to_path};
    use std::ffi::{c_char, c_void};

    type OSErr = i16;
    #[repr(C)]
    struct AEDesc {
        descriptor_type: u32,
        data_handle: *mut c_void,
    }
    type Handler = extern "C" fn(*const AEDesc, *mut AEDesc, *mut c_void) -> OSErr;
    type Observer = extern "C" fn(*mut c_void, *mut c_void, *const c_void, *const c_void, *const c_void);

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn AEInstallEventHandler(class: u32, id: u32, handler: Handler, refcon: *mut c_void, system: u8) -> OSErr;
        fn AEGetParamDesc(event: *const AEDesc, keyword: u32, desired: u32, result: *mut AEDesc) -> OSErr;
        fn AECountItems(list: *const AEDesc, count: *mut i32) -> OSErr;
        fn AEGetNthPtr(
            list: *const AEDesc,
            index: i32,
            desired: u32,
            keyword: *mut u32,
            actual_type: *mut u32,
            data: *mut c_void,
            max: isize,
            actual_size: *mut isize,
        ) -> OSErr;
        fn AEDisposeDesc(desc: *mut AEDesc) -> OSErr;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFNotificationCenterGetLocalCenter() -> *mut c_void;
        fn CFNotificationCenterAddObserver(
            center: *mut c_void,
            observer: *const c_void,
            callback: Observer,
            name: *const c_void,
            object: *const c_void,
            behavior: isize,
        );
        fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, encoding: u32) -> *const c_void;
    }

    const fn code(c: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*c)
    }

    extern "C" fn open_documents(event: *const AEDesc, _reply: *mut AEDesc, _refcon: *mut c_void) -> OSErr {
        let mut paths = Vec::new();
        unsafe {
            let mut list = AEDesc { descriptor_type: 0, data_handle: std::ptr::null_mut() };
            if AEGetParamDesc(event, code(b"----"), code(b"list"), &mut list) != 0 {
                return -1;
            }
            let mut n = 0i32;
            AECountItems(&list, &mut n);
            for i in 1..=n {
                let mut buf = vec![0u8; 8192];
                let (mut keyword, mut kind, mut size) = (0u32, 0u32, 0isize);
                let err = AEGetNthPtr(
                    &list,
                    i,
                    code(b"furl"),
                    &mut keyword,
                    &mut kind,
                    buf.as_mut_ptr().cast(),
                    buf.len() as isize,
                    &mut size,
                );
                if err == 0 && size > 0 && (size as usize) <= buf.len() {
                    if let Some(p) = std::str::from_utf8(&buf[..size as usize]).ok().and_then(url_to_path) {
                        paths.push(p);
                    }
                }
            }
            AEDisposeDesc(&mut list);
        }
        push(paths);
        0
    }

    fn install_handler() {
        unsafe {
            AEInstallEventHandler(code(b"aevt"), code(b"odoc"), open_documents, std::ptr::null_mut(), 0);
        }
    }

    extern "C" fn will_finish_launching(
        _center: *mut c_void,
        _observer: *mut c_void,
        _name: *const c_void,
        _object: *const c_void,
        _info: *const c_void,
    ) {
        install_handler();
    }

    pub fn install() {
        install_handler();
        unsafe {
            let name = CFStringCreateWithCString(
                std::ptr::null(),
                c"NSApplicationWillFinishLaunchingNotification".as_ptr(),
                0x0800_0100, // kCFStringEncodingUTF8
            );
            // Deliver immediately: the notification fires once, during launch.
            CFNotificationCenterAddObserver(
                CFNotificationCenterGetLocalCenter(),
                c"superbrick-open-files".as_ptr().cast(),
                will_finish_launching,
                name,
                std::ptr::null(),
                4,
            );
        }
    }

    /// Re-installs the handler once the app is running, in case AppKit replaced it
    /// after the launch notification.
    pub fn reinstall() {
        install_handler();
    }
}

/// Call before the event loop starts.
pub fn install() {
    #[cfg(target_os = "macos")]
    mac::install();
    #[cfg(windows)]
    ldraw::single_instance::serve(on_forwarded);
}

/// Call once the app is up (keeps the handler ours for later open requests).
pub fn reinstall() {
    #[cfg(target_os = "macos")]
    mac::reinstall();
}

#[cfg(test)]
mod tests {
    use super::url_to_path;
    use std::path::PathBuf;

    #[test]
    fn file_urls_become_paths() {
        assert_eq!(url_to_path("file:///Users/me/My%20Build.sb"), Some(PathBuf::from("/Users/me/My Build.sb")));
        assert_eq!(url_to_path("file://localhost/tmp/a.ldr"), Some(PathBuf::from("/tmp/a.ldr")));
        assert_eq!(url_to_path("file:///tmp/dir/"), Some(PathBuf::from("/tmp/dir")));
        assert_eq!(url_to_path("https://x"), None);
        assert_eq!(url_to_path("file:///bad%2"), Some(PathBuf::from("/bad%2")));
    }
}
