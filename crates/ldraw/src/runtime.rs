//! Where SuperBrick finds what it needs at run time, and how it fetches what is missing.
//!
//! * The LDraw parts library: `$SB_LDRAW_ZIP` / `$BENCH_LDRAW_ZIP`, else the copy SuperBrick
//!   downloaded into Application Support, else LeoCAD's `library.bin`.
//! * Python (for connectors, hinges, instructions and studio renders): `$SB_PYTHON`, else the
//!   copy SuperBrick installed, else a Python already on the Mac that has NumPy and Pillow.
//!
//! Both downloads use the system `curl` and `tar` (macOS: `/usr/bin`, Windows: `System32`), so no
//! network code ships in the app. Checksums use the `sha2` crate.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The official LDraw library (CC BY 2.0 / 4.0, ldraw.org).
pub const LIBRARY_URL: &str = "https://library.ldraw.org/library/updates/complete.zip";
/// Approximate download size, for progress before the server reports one.
pub const LIBRARY_BYTES: u64 = 146_853_687;

/// A relocatable CPython build (python-build-standalone), pinned by checksum.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const PYTHON_URL: &str = "https://github.com/astral-sh/python-build-standalone/releases/download/20261110/cpython-3.12.16%2B20261110-aarch64-apple-darwin-install_only_stripped.tar.gz";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const PYTHON_BYTES: u64 = 25_237_581;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
pub const PYTHON_SHA256: &str = "2f11b8aa3726bdbd87dd9eb76568412faddf4f47e26913fa45d6a2e424972acd";
#[cfg(all(windows, target_arch = "x86_64"))]
pub const PYTHON_URL: &str = "https://github.com/astral-sh/python-build-standalone/releases/download/20261110/cpython-3.12.16%2B20261110-x86_64-pc-windows-msvc-install_only_stripped.tar.gz";
#[cfg(all(windows, target_arch = "x86_64"))]
pub const PYTHON_BYTES: u64 = 22_365_213;
#[cfg(all(windows, target_arch = "x86_64"))]
pub const PYTHON_SHA256: &str = "355ee61b8207b91574491b1c6dc9b0ebb4408fc56bfafe498f3ef439314b52dc";
/// No pinned build for this target: `install_python` reports an error instead of downloading.
#[cfg(not(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64"))))]
pub const PYTHON_URL: &str = "";
#[cfg(not(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64"))))]
pub const PYTHON_BYTES: u64 = 0;
#[cfg(not(any(all(target_os = "macos", target_arch = "aarch64"), all(windows, target_arch = "x86_64"))))]
pub const PYTHON_SHA256: &str = "";
/// Packages the helpers import (SciPy only for one optional render step).
pub const PYTHON_PACKAGES: &[&str] = &["numpy", "pillow", "scipy"];
/// What a Python must import to be used.
const PYTHON_PROBE: &str = "import numpy, PIL";

/// Text the app looks for in an error to offer installing Python.
pub const NEEDS_PYTHON: &str = "needs Python with NumPy and Pillow";

fn home() -> PathBuf {
    crate::platform::home_dir()
}

/// `~/Library/Application Support/SuperBrick`, `%APPDATA%\SuperBrick` on Windows (or `$SB_SUPPORT_DIR`).
pub fn support_dir() -> PathBuf {
    crate::app_env("SUPPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::platform::support_root().join(crate::APP_NAME))
}

/// `~/Library/Caches/SuperBrick`, `%LOCALAPPDATA%\SuperBrick\Cache` on Windows (or `$SB_CACHE_DIR`).
pub fn cache_dir() -> PathBuf {
    crate::app_env("CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let root = crate::platform::cache_root().join(crate::APP_NAME);
            if cfg!(windows) { root.join("Cache") } else { root }
        })
}

/// Where a downloaded library lives.
pub fn downloaded_library() -> PathBuf {
    support_dir().join("ldraw/complete.zip")
}

/// The library zip to open, if there is one.
pub fn library_zip() -> Option<PathBuf> {
    if let Some(p) = crate::app_env("LDRAW_ZIP").or_else(|| std::env::var_os("BENCH_LDRAW_ZIP")) {
        return Some(PathBuf::from(p));
    }
    let mut places = vec![downloaded_library()];
    // SB_NO_LEOCAD=1 ignores LeoCAD's copy (to try the first-run setup on a Mac that has it).
    if crate::app_env("NO_LEOCAD").is_none() {
        places.extend(crate::library::default_zips());
    }
    places.into_iter().find(|p| p.is_file())
}

/// Where SuperBrick's own Python lives once installed.
pub fn managed_python() -> PathBuf {
    support_dir().join("python").join(python_rel())
}

/// The interpreter's path inside an unpacked `python` folder.
fn python_rel() -> &'static str {
    if cfg!(windows) { "python.exe" } else { "bin/python3" }
}

/// Whether `p` has a `WindowsApps` component (the Microsoft Store `python.exe` stub), any case.
pub fn is_windows_apps_stub(p: &Path) -> bool {
    p.components().any(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case("WindowsApps"))
        || p.to_string_lossy().to_ascii_lowercase().contains("\\windowsapps\\")
}

static PYTHON: Mutex<Option<Option<PathBuf>>> = Mutex::new(None);

/// Forget the cached Python lookup (after installing one).
pub fn reset_python() {
    *PYTHON.lock().unwrap() = None;
}

/// The Python to run helpers with: explicit, managed, or an existing one that has NumPy and Pillow.
pub fn python() -> Option<PathBuf> {
    let mut cached = PYTHON.lock().unwrap();
    if let Some(found) = cached.as_ref() {
        return found.clone();
    }
    let found = find_python();
    *cached = Some(found.clone());
    found
}

fn find_python() -> Option<PathBuf> {
    if let Some(p) = crate::app_env("PYTHON") {
        return Some(PathBuf::from(p));
    }
    let managed = managed_python();
    if managed.is_file() {
        return Some(managed);
    }
    #[cfg(windows)]
    {
        return windows_candidates().into_iter().filter(|p| !is_windows_apps_stub(p) && p.is_file()).find(|p| usable(p));
    }
    #[cfg(not(windows))]
    macos_python()
}

#[cfg(windows)]
fn windows_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(p) = py_launcher() {
        out.push(p);
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(local).join("Programs").join("Python");
        let mut found: Vec<(u32, PathBuf)> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let minor = python_dir_minor(&name)?;
                Some((minor, e.path().join("python.exe")))
            })
            .collect();
        found.sort_by(|a, b| b.0.cmp(&a.0));
        out.extend(found.into_iter().map(|(_, p)| p));
    }
    let h = home();
    for d in ["miniforge3", "miniconda3", "anaconda3"] {
        out.push(h.join(d).join("python.exe"));
    }
    out.push(h.join("scoop").join("shims").join("python.exe"));
    if let Some(path) = std::env::var_os("PATH") {
        out.extend(std::env::split_paths(&path).map(|d| d.join("python.exe")));
    }
    out
}

/// The minor version of a `Python312`-style folder name (`Python3*` only), if it is one.
#[cfg_attr(not(windows), allow(dead_code))]
fn python_dir_minor(name: &str) -> Option<u32> {
    name.get(..7).filter(|p| p.eq_ignore_ascii_case("python3"))?;
    let digits = name[7..].split(|c: char| !c.is_ascii_digit()).next().unwrap_or("");
    Some(digits.parse().unwrap_or(0))
}

/// The interpreter the `py -3` launcher picks.
#[cfg(windows)]
fn py_launcher() -> Option<PathBuf> {
    let mut cmd = Command::new("py");
    crate::platform::no_window(&mut cmd)
        .args(["-3", "-c", "import sys;print(sys.executable)"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

#[cfg(not(windows))]
fn macos_python() -> Option<PathBuf> {
    let h = home();
    let mut candidates: Vec<PathBuf> = vec![
        "/opt/homebrew/bin/python3".into(),
        "/usr/local/bin/python3".into(),
        "/Library/Frameworks/Python.framework/Versions/Current/bin/python3".into(),
        h.join("miniforge3/bin/python3"),
        h.join("miniconda3/bin/python3"),
        h.join("anaconda3/bin/python3"),
        h.join("opt/anaconda3/bin/python3"),
        h.join(".pyenv/shims/python3"),
    ];
    // /usr/bin/python3 is a stub that pops up an installer unless the developer tools exist.
    let mut probe = Command::new("/usr/bin/xcode-select");
    probe.arg("-p").stdout(Stdio::null()).stderr(Stdio::null());
    let tools = crate::platform::no_window(&mut probe).status();
    if tools.map(|s| s.success()).unwrap_or(false) {
        candidates.push("/usr/bin/python3".into());
    }
    candidates.into_iter().filter(|p| p.is_file()).find(|p| usable(p))
}

/// Whether `python` is the copy SuperBrick installed into Application Support.
/// (Or the one it is unpacking into `python-staging` while installing.)
fn is_managed(python: &Path) -> bool {
    let support = support_dir();
    [support.join("python"), support.join("python-staging")].iter().any(|root| {
        python.starts_with(root)
            || python.canonicalize().ok().zip(root.canonicalize().ok()).is_some_and(|(p, r)| p.starts_with(r))
    })
}

/// A bare command for `python` with the environment every helper runs under. Only SuperBrick's
/// own Python ignores the user site-packages: a Homebrew or framework Python often has Pillow or
/// NumPy installed there (`pip install --user`), and hiding them would break the helpers.
fn python_base(python: &Path) -> Command {
    let mut cmd = Command::new(python);
    cmd.env("PYTHONDONTWRITEBYTECODE", "1");
    if is_managed(python) {
        cmd.env("PYTHONNOUSERSITE", "1");
    }
    cmd
}

/// Whether this Python can import what the helpers need (checked with a timeout), under the same
/// environment `python_command` gives the helpers.
pub fn usable(python: &Path) -> bool {
    let mut cmd = python_base(python);
    crate::platform::no_window(&mut cmd)
        .args(["-c", PYTHON_PROBE])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < Duration::from_secs(20) => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// A command for a helper script: the right Python, with the library and a writable cache.
pub fn python_command() -> Result<Command, String> {
    let python = python().ok_or_else(|| {
        format!("This tool {NEEDS_PYTHON}. Open SuperBrick and choose File → Install Python components, or set SB_PYTHON.")
    })?;
    let mut cmd = python_base(&python);
    if let Some(zip) = library_zip() {
        cmd.env("BENCH_LDRAW_ZIP", zip);
    }
    if std::env::var_os("BENCH_CACHE").is_none() {
        let cache = cache_dir().join("bench");
        let _ = std::fs::create_dir_all(&cache);
        cmd.env("BENCH_CACHE", cache);
    }
    Ok(cmd)
}

/// Progress of a download or install.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub step: String,
    pub done: u64,
    pub total: u64,
}

impl Progress {
    pub fn fraction(&self) -> Option<f32> {
        (self.total > 0).then(|| (self.done as f32 / self.total as f32).min(1.0))
    }
}

/// Download `url` to `dest` (through a `.part` file) with `curl`, reporting bytes as they arrive.
pub fn download(url: &str, dest: &Path, expect: u64, step: &str, report: &mut dyn FnMut(Progress)) -> Result<(), String> {
    let dir = dest.parent().ok_or("bad download path")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let part = dest.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let mut curl = Command::new(system_tool("curl"));
    let mut child = crate::platform::no_window(&mut curl)
        .args(["--fail", "--location", "--silent", "--show-error", "--retry", "3", "--output"])
        .arg(&part)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start curl: {e}"))?;
    loop {
        let done = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        report(Progress { step: step.into(), done, total: expect.max(done) });
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            if !status.success() {
                let mut err = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut err);
                }
                let _ = std::fs::remove_file(&part);
                return Err(format!("download failed: {}", err.trim()));
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    std::fs::rename(&part, dest).map_err(|e| e.to_string())
}

/// Download the official LDraw library into Application Support.
pub fn install_library(report: &mut dyn FnMut(Progress)) -> Result<PathBuf, String> {
    let dest = downloaded_library();
    download(LIBRARY_URL, &dest, LIBRARY_BYTES, "Downloading the LDraw parts library", report)?;
    if let Err(e) = crate::zipfs::ZipFs::open(&dest) {
        let _ = std::fs::remove_file(&dest);
        return Err(format!("the downloaded library is damaged ({e}); try again"));
    }
    Ok(dest)
}

/// The OS's own `curl` / `tar`: `/usr/bin` on macOS, `%SystemRoot%\System32` on Windows.
fn system_tool(name: &str) -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        root.join("System32").join(format!("{name}.exe"))
    } else {
        PathBuf::from("/usr/bin").join(name)
    }
}

/// Lower-case hex SHA-256 of a file, streamed.
fn sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Download SuperBrick's own Python and install the helper packages into it.
pub fn install_python(report: &mut dyn FnMut(Progress)) -> Result<PathBuf, String> {
    if PYTHON_URL.is_empty() {
        return Err("there is no built-in Python download for this platform; install Python with NumPy and Pillow and set SB_PYTHON".into());
    }
    let support = support_dir();
    let tarball = support.join("python-download.tar.gz");
    download(PYTHON_URL, &tarball, PYTHON_BYTES, "Downloading Python", report)?;
    if sha256(&tarball)? != PYTHON_SHA256 {
        let _ = std::fs::remove_file(&tarball);
        return Err("the Python download did not match its checksum; try again".into());
    }
    let staging = support.join("python-staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    report(Progress { step: "Unpacking Python".into(), ..Default::default() });
    let mut tar = Command::new(system_tool("tar"));
    let ok = crate::platform::no_window(&mut tar)
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&staging)
        .status()
        .map_err(|e| e.to_string())?
        .success();
    let _ = std::fs::remove_file(&tarball);
    if !ok {
        return Err("could not unpack Python".into());
    }
    let python = staging.join("python").join(python_rel());
    report(Progress { step: format!("Installing {}", PYTHON_PACKAGES.join(", ")), ..Default::default() });
    let mut pip = Command::new(&python);
    let out = crate::platform::no_window(&mut pip)
        .args(["-m", "pip", "install", "--disable-pip-version-check", "--no-warn-script-location", "--only-binary=:all:"])
        .args(PYTHON_PACKAGES)
        .env("PYTHONNOUSERSITE", "1")
        .output()
        .map_err(|e| format!("could not run the new Python: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(4).collect();
        return Err(format!("installing packages failed: {}", tail.into_iter().rev().collect::<Vec<_>>().join(" ")));
    }
    if !usable(&python) {
        return Err("the new Python cannot import NumPy and Pillow".into());
    }
    let target = support.join("python");
    let _ = std::fs::remove_dir_all(&target);
    rename_retry(&staging.join("python"), &target).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&staging);
    reset_python();
    Ok(managed_python())
}

/// Rename a folder; on Windows retry briefly, since antivirus scanners hold freshly written files.
fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let tries = if cfg!(windows) { 10 } else { 1 };
    let mut last = Ok(());
    for i in 0..tries {
        last = std::fs::rename(from, to);
        if last.is_ok() {
            break;
        }
        if i + 1 < tries {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_fraction() {
        assert_eq!(Progress::default().fraction(), None);
        assert_eq!(Progress { done: 5, total: 10, ..Default::default() }.fraction(), Some(0.5));
        assert_eq!(Progress { done: 20, total: 10, ..Default::default() }.fraction(), Some(1.0));
    }

    #[test]
    fn only_the_managed_python_ignores_user_site() {
        let has = |cmd: &Command| cmd.get_envs().any(|(k, v)| k == "PYTHONNOUSERSITE" && v.is_some());
        assert!(has(&python_base(&managed_python())));
        assert!(!has(&python_base(Path::new("/opt/homebrew/bin/python3"))));
    }

    #[test]
    fn store_stub_is_rejected() {
        assert!(is_windows_apps_stub(Path::new(r"C:\Users\a\AppData\Local\Microsoft\WindowsApps\python.exe")));
        assert!(is_windows_apps_stub(Path::new(r"c:\users\a\appdata\local\microsoft\windowsapps\python3.exe")));
        assert!(is_windows_apps_stub(Path::new("C:/Program Files/WINDOWSAPPS/x/python.exe")));
        assert!(!is_windows_apps_stub(Path::new(r"C:\Users\a\AppData\Local\Programs\Python\Python312\python.exe")));
        assert!(!is_windows_apps_stub(Path::new("/usr/bin/python3")));
    }

    #[test]
    fn python_dir_versions() {
        assert_eq!(python_dir_minor("Python312"), Some(12));
        assert_eq!(python_dir_minor("python39"), Some(9));
        assert_eq!(python_dir_minor("Python2"), None);
        assert_eq!(python_dir_minor("Foo"), None);
    }

    #[test]
    fn sha256_known_vector() {
        let p = std::env::temp_dir().join(format!("sb-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        let got = sha256(&p);
        let _ = std::fs::remove_file(&p);
        assert_eq!(got.unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn missing_python_is_not_usable() {
        assert!(!usable(Path::new("/nonexistent/python3")));
    }
}
