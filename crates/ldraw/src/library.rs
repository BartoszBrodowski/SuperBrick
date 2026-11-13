//! Read-only access to the LDraw library: LeoCAD's `library.bin` zip plus optional extra folders.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::RwLock;

use crate::{
    color::Colors,
    mesh::Mesh,
    num::{decode, fnv1a64, split_lines},
    zipfs::ZipFs,
};

pub const DEFAULT_ZIP: &str = "/Applications/LeoCAD.app/Contents/Resources/library.bin";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("LDraw parts library not found at {0}: open SuperBrick once to download it, install LeoCAD, or set SB_LDRAW_ZIP")]
    NoZip(PathBuf),
    #[error("LDraw extra folder not found: {0}")]
    NoExtra(PathBuf),
    #[error("LDraw file not found in library: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Level of detail: `Low` prefers the `p/8/` low-resolution primitives.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Lod {
    #[default]
    Std,
    Low,
}

#[derive(Clone, Debug)]
pub(crate) enum Src {
    Zip(u32),
    File(PathBuf),
}

/// Normalise a referenced file name: trim, `\` to `/`, lower case (`Library.norm`).
pub fn norm(name: &str) -> String {
    name.trim().replace('\\', "/").to_lowercase()
}

/// `norm`, plus `.dat` when there is no LDraw extension (`canonical_name`).
pub fn canonical_name(name: &str) -> String {
    let n = norm(name);
    if n.ends_with(".dat") || n.ends_with(".ldr") || n.ends_with(".mpd") { n } else { n + ".dat" }
}

pub struct Library {
    zip_path: PathBuf,
    zip: ZipFs,
    pub(crate) index: HashMap<String, Src>,
    extra_dirs: Vec<PathBuf>,
    fingerprint: u64,
    pub colors: Colors,
    pub(crate) meshes: RwLock<HashMap<(String, Lod), Arc<Mesh>>>,
    descs: RwLock<HashMap<String, Arc<str>>>,
}

fn expand_home(p: &Path) -> PathBuf {
    match (p.strip_prefix("~"), std::env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => p.to_path_buf(),
    }
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let p = e?.path();
        if p.is_dir() {
            walk(&p, out)?;
        } else if p.is_file() {
            out.push(p);
        }
    }
    Ok(())
}

impl Library {
    /// Open the library the way `get_library()` does: `$BENCH_LDRAW_ZIP` (else LeoCAD's library.bin)
    /// plus the folders in `$BENCH_LDRAW_EXTRA` (separated by `:`).
    pub fn open_default() -> Result<Library> {
        let zip = crate::runtime::library_zip().unwrap_or_else(crate::runtime::downloaded_library);
        let extra: Vec<PathBuf> = std::env::var("BENCH_LDRAW_EXTRA")
            .unwrap_or_default()
            .split(':')
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .collect();
        Library::open(zip, &extra)
    }

    /// Files already in the zip win, so an extra folder can only add parts, never change official ones.
    /// Each extra folder may be an LDraw root (with `parts/` and `p/`, or an `ldraw/` folder holding them)
    /// or a flat folder of part files (with an `s/` subfolder for subparts).
    pub fn open(zip_path: impl AsRef<Path>, extra_dirs: &[PathBuf]) -> Result<Library> {
        let zip_path = zip_path.as_ref().to_path_buf();
        if !zip_path.exists() {
            return Err(Error::NoZip(zip_path));
        }
        let zip = ZipFs::open(&zip_path)?;
        let mut index = HashMap::with_capacity(zip.len() + 64);
        for (i, e) in zip.entries().iter().enumerate() {
            let ln = e.name.to_lowercase();
            if let Some(k) = ln.strip_prefix("ldraw/") {
                index.insert(k.to_string(), Src::Zip(i as u32));
            }
        }
        let mut fingerprint = zip.fingerprint();
        let extra_dirs: Vec<PathBuf> = extra_dirs.iter().map(|d| expand_home(d)).collect();
        for d in &extra_dirs {
            if !d.is_dir() {
                return Err(Error::NoExtra(d.clone()));
            }
            let mut files = Vec::new();
            walk(d, &mut files)?;
            files.sort();
            for f in files {
                let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                if !matches!(ext.as_str(), "dat" | "ldr" | "mpd") {
                    continue;
                }
                let rel = f.strip_prefix(d).unwrap_or(&f);
                let mut key = rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/").to_lowercase();
                if let Some(k) = key.strip_prefix("ldraw/") {
                    key = k.to_string();
                }
                if !index.contains_key(&key) {
                    if let Ok(md) = f.metadata() {
                        let stamp = format!("{}|{}|{:?}", f.display(), md.len(), md.modified().ok());
                        fingerprint = fingerprint.rotate_left(7) ^ fnv1a64(stamp.as_bytes());
                    }
                    index.insert(key, Src::File(f));
                }
            }
        }
        let mut lib = Library {
            zip_path,
            zip,
            index,
            extra_dirs,
            fingerprint,
            colors: Colors::default(),
            meshes: RwLock::new(HashMap::new()),
            descs: RwLock::new(HashMap::new()),
        };
        let ldconfig = lib.read_text("LDConfig.ldr")?;
        lib.colors = Colors::parse(&ldconfig);
        Ok(lib)
    }

    pub fn zip_path(&self) -> &Path {
        &self.zip_path
    }

    pub fn extra_dirs(&self) -> &[PathBuf] {
        &self.extra_dirs
    }

    /// Content stamp of the whole library, for disk-cache keys.
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    /// Number of indexed files.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn has(&self, key: &str) -> bool {
        self.index.contains_key(key)
    }

    /// The library key (path relative to `ldraw/`, lower case) for a referenced file name.
    pub fn resolve(&self, name: &str, lod: Lod) -> Option<String> {
        let n = norm(name);
        if lod == Lod::Low {
            let base = n.rsplit('/').next().unwrap_or(&n);
            if n.starts_with("48/") && self.has(&format!("p/8/{base}")) {
                return Some(format!("p/8/{base}"));
            }
            if !n.contains('/') && self.has(&format!("p/8/{n}")) {
                return Some(format!("p/8/{n}"));
            }
        }
        for pre in ["parts/", "p/", "models/", ""] {
            let k = format!("{pre}{n}");
            if self.has(&k) {
                return Some(k);
            }
        }
        if !n.ends_with(".dat") && !n.ends_with(".ldr") {
            return self.resolve(&format!("{n}.dat"), lod);
        }
        None
    }

    pub fn exists(&self, name: &str) -> bool {
        self.resolve(name, Lod::Std).is_some()
    }

    fn key_for(&self, key_or_name: &str) -> Result<String> {
        if self.has(key_or_name) {
            return Ok(key_or_name.to_string());
        }
        self.resolve(key_or_name, Lod::Std).ok_or_else(|| Error::NotFound(key_or_name.to_string()))
    }

    pub fn read_bytes(&self, key_or_name: &str) -> Result<Vec<u8>> {
        let key = self.key_for(key_or_name)?;
        Ok(match &self.index[&key] {
            Src::Zip(i) => self.zip.read(*i as usize)?,
            Src::File(p) => std::fs::read(p)?,
        })
    }

    /// File text, decoded as UTF-8, else Latin-1.
    pub fn read_text(&self, key_or_name: &str) -> Result<String> {
        Ok(decode(&self.read_bytes(key_or_name)?).0)
    }

    /// The first line of a file, read cheaply (for indexing descriptions).
    pub fn first_line(&self, key_or_name: &str) -> Result<String> {
        let key = self.key_for(key_or_name)?;
        let bytes = match &self.index[&key] {
            Src::Zip(i) => self.zip.read_prefix(*i as usize, 512)?,
            Src::File(p) => std::fs::read(p)?,
        };
        let cut = bytes.iter().position(|&b| b == b'\n' || b == b'\r').unwrap_or(bytes.len());
        Ok(decode(&bytes[..cut]).0)
    }

    /// The part's title line ("Brick  2 x  4"), or `(unknown part x.dat)`.
    pub fn description(&self, name: &str) -> Arc<str> {
        let n = canonical_name(name);
        if let Some(d) = self.descs.read().get(&n) {
            return d.clone();
        }
        let d: Arc<str> = match self.read_text(&n) {
            Ok(text) => match split_lines(&text).next() {
                Some(first) if first.starts_with('0') => first[1..].trim().into(),
                _ => n.as_str().into(),
            },
            Err(_) => format!("(unknown part {n})").into(),
        };
        self.descs.write().insert(n, d.clone());
        d
    }

    /// Target of a `~Moved to xxx` redirection part.
    pub fn moved_to(&self, name: &str) -> Option<String> {
        let d = self.description(name);
        let rest = d.strip_prefix("~Moved to")?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        rest.split_whitespace().next().map(canonical_name)
    }

    /// Follow `~Moved to` redirections; return the canonical `xxxx.dat`.
    pub fn canonical_part(&self, name: &str) -> String {
        let mut n = canonical_name(name);
        let mut seen = std::collections::HashSet::new();
        while seen.insert(n.clone()) {
            match self.moved_to(&n) {
                Some(t) => n = t,
                None => break,
            }
        }
        n
    }

    /// File names of all top-level parts (`parts/xxx.dat` → `xxx.dat`), unsorted.
    pub fn part_names(&self) -> impl Iterator<Item = &str> {
        self.index.keys().filter_map(|k| {
            let rest = k.strip_prefix("parts/")?;
            (!rest.contains('/') && rest.ends_with(".dat")).then_some(rest)
        })
    }

    /// Whether a key comes from an extra folder rather than the zip.
    pub fn is_extra(&self, key: &str) -> bool {
        matches!(self.index.get(key), Some(Src::File(_)))
    }
}
