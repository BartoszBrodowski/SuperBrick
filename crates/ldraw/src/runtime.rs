//! Where SuperBrick finds what it needs at run time, and how it fetches what is missing.
//!
//! * The LDraw parts library: `$SB_LDRAW_ZIP` / `$BENCH_LDRAW_ZIP`, else the copy SuperBrick
//!   downloaded into Application Support, else LeoCAD's `library.bin`.
//! * Python (for connectors, hinges, instructions and studio renders): `$SB_PYTHON`, else the
//!   copy SuperBrick installed, else a Python already on the Mac that has NumPy and Pillow.
//!
//! Both downloads use the macOS `curl`, `shasum` and `tar`, so no network code ships in the app.

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
pub const PYTHON_URL: &str = "https://github.com/astral-sh/python-build-standalone/releases/download/20261110/cpython-3.12.16%2B20261110-aarch64-apple-darwin-install_only_stripped.tar.gz";
pub const PYTHON_BYTES: u64 = 25_237_581;
pub const PYTHON_SHA256: &str = "2f11b8aa3726bdbd87dd9eb76568412faddf4f47e26913fa45d6a2e424972acd";
/// Packages the helpers import (SciPy only for one optional render step).
pub const PYTHON_PACKAGES: &[&str] = &["numpy", "pillow", "scipy"];
/// What a Python must import to be used.
const PYTHON_PROBE: &str = "import numpy, PIL";

/// Text the app looks for in an error to offer installing Python.
pub const NEEDS_PYTHON: &str = "needs Python with NumPy and Pillow";

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// `~/Library/Application Support/SuperBrick` (or `$SB_SUPPORT_DIR`).
pub fn support_dir() -> PathBuf {
    crate::app_env("SUPPORT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Library/Application Support").join(crate::APP_NAME))
}

/// `~/Library/Caches/SuperBrick` (or `$SB_CACHE_DIR`).
pub fn cache_dir() -> PathBuf {
    crate::app_env("CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("Library/Caches").join(crate::APP_NAME))
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
        places.push(PathBuf::from(crate::library::DEFAULT_ZIP));
    }
    places.into_iter().find(|p| p.is_file())
}

/// Where SuperBrick's own Python lives once installed.
pub fn managed_python() -> PathBuf {
    support_dir().join("python/bin/python3")
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
    let tools = Command::new("/usr/bin/xcode-select").arg("-p").stdout(Stdio::null()).stderr(Stdio::null()).status();
    if tools.map(|s| s.success()).unwrap_or(false) {
        candidates.push("/usr/bin/python3".into());
    }
    candidates.into_iter().filter(|p| p.is_file()).find(|p| usable(p))
}

/// Whether this Python can import what the helpers need (checked with a timeout).
pub fn usable(python: &Path) -> bool {
    let Ok(mut child) = Command::new(python)
        .args(["-c", PYTHON_PROBE])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
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
    let mut cmd = Command::new(python);
    cmd.env("PYTHONDONTWRITEBYTECODE", "1");
    cmd.env("PYTHONNOUSERSITE", "1");
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
    let mut child = Command::new("/usr/bin/curl")
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

fn sha256(path: &Path) -> Result<String, String> {
    let out = Command::new("/usr/bin/shasum").args(["-a", "256"]).arg(path).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().next().map(str::to_owned).ok_or_else(|| "shasum failed".into())
}

/// Download SuperBrick's own Python and install the helper packages into it.
pub fn install_python(report: &mut dyn FnMut(Progress)) -> Result<PathBuf, String> {
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
    let ok = Command::new("/usr/bin/tar")
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
    let python = staging.join("python/bin/python3");
    report(Progress { step: format!("Installing {}", PYTHON_PACKAGES.join(", ")), ..Default::default() });
    let out = Command::new(&python)
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
    std::fs::rename(staging.join("python"), &target).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&staging);
    reset_python();
    Ok(managed_python())
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
    fn missing_python_is_not_usable() {
        assert!(!usable(Path::new("/nonexistent/python3")));
    }
}
