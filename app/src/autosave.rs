//! Timed recovery copies. Never overwrites the original model or marks it saved.
use std::{
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};
pub fn directory() -> PathBuf {
    ldraw::app_env("RECOVERY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let old = ldraw::platform::support_root().join("cad/recovery");
            let new = ldraw::runtime::support_dir().join("recovery");
            if !new.exists() && old.exists() { old } else { new }
        })
}
type Job = (PathBuf, Vec<u8>, serde_json::Value);
pub struct Recovery {
    tx: mpsc::SyncSender<Job>,
    rx: mpsc::Receiver<Result<PathBuf, String>>,
    last: Instant,
    revision: Option<u64>,
}
impl Default for Recovery {
    fn default() -> Self {
        let (tx, jobs) = mpsc::sync_channel::<Job>(1);
        let (results, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for (path, bytes, meta) in jobs {
                let result = write(&path, &bytes, &meta)
                    .map(|()| path)
                    .map_err(|e| e.to_string());
                let _ = results.send(result);
            }
        });
        Self {
            tx,
            rx,
            last: Instant::now() - Duration::from_secs(15),
            revision: None,
        }
    }
}
impl Recovery {
    pub fn tick(&mut self, s: &engine::Snapshot) -> Option<String> {
        let mut notice = None;
        while let Ok(r) = self.rx.try_recv() {
            notice = Some(match r {
                Ok(p) => format!("Recovery autosaved · {}", p.display()),
                Err(e) => {
                    self.revision = None;
                    format!("Recovery autosave failed (will retry): {e}")
                }
            });
        }
        if s.revision == s.saved_revision
            || self.revision == Some(s.revision)
            || self.last.elapsed() < Duration::from_secs(15)
        {
            return notice;
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.model.path.hash(&mut h);
        let path = directory().join(format!("build-{:x}-{}.mpd", h.finish(), std::process::id()));
        let meta = serde_json::json!({"original_path":s.model.path,"revision":s.revision,"unix_seconds":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()});
        if self
            .tx
            .try_send((path, s.model.doc.to_bytes(), meta))
            .is_ok()
        {
            self.last = Instant::now();
            self.revision = Some(s.revision);
        }
        notice
    }
}
fn write(path: &Path, bytes: &[u8], meta: &serde_json::Value) -> std::io::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = path.with_extension(format!("{suffix}.tmp"));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut file = opts.open(&tmp)?;
    let result: std::io::Result<()> = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    std::fs::write(
        path.with_extension("json"),
        serde_json::to_vec_pretty(meta)?,
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_roundtrips_without_touching_original() {
        let dir = std::env::temp_dir().join(format!(
            "cad-autosave-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let original = dir.join("original.ldr");
        let recovery = dir.join("recovery.mpd");
        std::fs::write(&original, b"original").unwrap();
        let bytes = b"0 Recovered\r\n1 4 0 -24 0 1 0 0 0 1 0 0 0 1 3005.dat\r\n";
        write(
            &recovery,
            bytes,
            &serde_json::json!({"original_path":original}),
        )
        .unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), b"original");
        assert_eq!(ldraw::Document::load(&recovery).unwrap().to_bytes(), bytes);
        for p in [&original, &recovery, &recovery.with_extension("json")] {
            std::fs::remove_file(p).unwrap();
        }
        std::fs::remove_dir(dir).unwrap();
    }
}
