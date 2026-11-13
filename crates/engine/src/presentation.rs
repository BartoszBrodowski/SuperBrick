//! Read-only effective agent views and ghost geometry. No preview can mutate the base model.
use super::{Engine, Model, Result, Snapshot};
use ldraw::{Document, FlatPart, Library};
use std::{
    collections::{BTreeSet, HashSet},
    sync::Arc,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GhostKind {
    Added,
    Removed,
    Stale,
}
pub struct Ghost {
    pub part: FlatPart,
    pub kind: GhostKind,
}
impl Snapshot {
    pub fn presentation_revision(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.revision.hash(&mut h);
        for op in &self.drafts {
            op.id.hash(&mut h);
            op.state.hash(&mut h);
        }
        h.finish()
    }
    pub(crate) fn fork(&self, lib: Arc<Library>) -> Engine {
        let mut e = Engine::new(lib, Document::new("preview.ldr", "preview"), None);
        e.model = self.model.clone();
        e.next_id = self
            .next_id
            .max(e.model.parts.keys().next_back().copied().unwrap_or(0) + 1);
        e.revision = self.revision;
        e
    }
    pub fn effective(&self, lib: Arc<Library>, session: &str) -> Result<Model> {
        let mut e = self.fork(lib);
        for op in self
            .drafts
            .iter()
            .filter(|o| o.session == session && o.state == "pending")
        {
            e.apply_draft(op)?;
        }
        Ok(e.model)
    }
    pub fn ghosts(&self, lib: Arc<Library>, file: usize) -> Vec<Ghost> {
        let base = self.model.flat(file);
        let keys: HashSet<_> = base.iter().map(key).collect();
        let mut ghosts = Vec::new();
        let sessions: BTreeSet<_> = self
            .drafts
            .iter()
            .filter(|o| o.state == "pending")
            .map(|o| o.session.as_str())
            .collect();
        for session in sessions {
            if let Ok(effective) = self.effective(lib.clone(), session) {
                let flat = effective.flat(file);
                let new_keys: HashSet<_> = flat.iter().map(key).collect();
                ghosts.extend(
                    flat.into_iter()
                        .filter(|p| !keys.contains(&key(p)))
                        .map(|part| Ghost {
                            part,
                            kind: GhostKind::Added,
                        }),
                );
                ghosts.extend(
                    base.iter()
                        .filter(|p| !new_keys.contains(&key(p)))
                        .cloned()
                        .map(|part| Ghost {
                            part,
                            kind: GhostKind::Removed,
                        }),
                );
            }
        }
        for op in self
            .drafts
            .iter()
            .filter(|o| o.state == "stale" || o.state == "blocked")
        {
            let mut e = self.fork(lib.clone());
            let commands: Option<Vec<_>> = op
                .commands
                .iter()
                .map(|c| {
                    let mut cmd = serde_json::to_value(c).ok()?;
                    if let Some(free) = cmd.get_mut("free") {
                        *free = serde_json::Value::Bool(true);
                    }
                    serde_json::from_value(cmd).ok()
                })
                .collect();
            if let Some(commands) = commands {
                let mut preview = op.clone();
                preview.commands = commands;
                if e.apply_draft(&preview).is_ok() {
                    let effective = e.model.flat(file);
                    let new_keys: HashSet<_> = effective.iter().map(key).collect();
                    ghosts.extend(
                        base.iter()
                            .filter(|p| !new_keys.contains(&key(p)))
                            .cloned()
                            .map(|part| Ghost {
                                part,
                                kind: GhostKind::Stale,
                            }),
                    );
                    ghosts.extend(
                        effective
                            .into_iter()
                            .filter(|p| !keys.contains(&key(p)))
                            .map(|part| Ghost {
                                part,
                                kind: GhostKind::Stale,
                            }),
                    );
                }
            }
        }
        ghosts
    }
}
fn key(p: &FlatPart) -> String {
    let values = p.pos.to_array().into_iter().chain(p.rot.to_cols_array());
    format!(
        "{}:{}:{:?}",
        p.name,
        p.color.0,
        values.map(|v| (v * 1e6).round() as i64).collect::<Vec<_>>()
    )
}
