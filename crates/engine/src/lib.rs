//! Single-owner document engine. UI and MCP use exactly the same commands.
mod assemblies;
pub mod availability;
pub mod checker;
pub mod connectors;
pub mod drafts;
pub mod inventory;
pub mod joints;
pub mod placement;
pub use placement::{PlacementSolution, PreparedPlacement};
pub mod presentation;
mod rotation_fit;
pub mod strength;
pub mod validation;

use glam::{DMat3, DVec3};
use ldraw::{
    Color, Document, Library,
    model::{FileKind, Line, LineKind, ModelFile, PartLine},
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
};

pub type Id = u64;
pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PartSpec {
    pub name: String,
    #[serde(default = "default_color")]
    pub color: i32,
    #[serde(default)]
    pub pos: DVec3,
    #[serde(default = "identity")]
    pub rot: DMat3,
    #[serde(default)]
    pub id: Option<Id>,
}
fn identity() -> DMat3 {
    DMat3::IDENTITY
}
fn default_color() -> i32 {
    16
}
fn default_one() -> usize {
    1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Cmd {
    Mirror {
        ids: Vec<Id>,
        axis: usize,
        pivot: DVec3,
    },
    Snap {
        ids: Vec<Id>,
        file: usize,
        part: PartSpec,
        near: DVec3,
    },
    Connect {
        ids: Vec<Id>,
        file: usize,
        source: connectors::Endpoint,
        target: connectors::Endpoint,
        #[serde(default)]
        pose: usize,
    },
    Articulate {
        ids: Vec<Id>,
        file: usize,
        clicked: Id,
        joint: u64,
        axis: usize,
        angle: f64,
        #[serde(default)]
        detach: Vec<u64>,
    },
    Add {
        file: usize,
        parts: Vec<PartSpec>,
        #[serde(default)]
        free: bool,
    },
    Move {
        ids: Vec<Id>,
        delta: DVec3,
        #[serde(default)]
        free: bool,
    },
    Nudge {
        ids: Vec<Id>,
        direction: DVec3,
        #[serde(default)]
        normal: bool,
        /// Used instead of `direction` when the engaged surface is not
        /// horizontal (e.g. a sideways connection): lets "up" slide up the wall.
        #[serde(default)]
        alt: Option<DVec3>,
    },
    Rotate {
        ids: Vec<Id>,
        axis: DVec3,
        angle: f64,
        pivot: DVec3,
        #[serde(default)]
        free: bool,
    },
    Remove {
        ids: Vec<Id>,
    },
    Paint {
        ids: Vec<Id>,
        color: i32,
    },
    Group {
        ids: Vec<Id>,
        name: String,
    },
    Copy {
        ids: Vec<Id>,
        delta: DVec3,
        #[serde(default = "default_one")]
        count: usize,
    },
    MakeSubbuild {
        ids: Vec<Id>,
        name: String,
    },
    Select {
        ids: Vec<Id>,
    },
    Hide {
        ids: Vec<Id>,
        value: bool,
    },
    Lock {
        ids: Vec<Id>,
        value: bool,
    },
    /// End the current construction step: appends `0 STEP` to the file, after an optional
    /// `0 // label` comment.
    Step {
        file: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Move parts' lines to just after another part's line in the same file, so they join its
    /// construction step (the order of `ids` is kept).
    Restep { ids: Vec<Id>, after: Id },
}
impl Cmd {
    pub fn ids(&self) -> &[Id] {
        match self {
            Cmd::Add { .. } | Cmd::Step { .. } => &[],
            Cmd::Snap { ids, .. }
            | Cmd::Mirror { ids, .. }
            | Cmd::Connect { ids, .. }
            | Cmd::Articulate { ids, .. }
            | Cmd::Move { ids, .. }
            | Cmd::Nudge { ids, .. }
            | Cmd::Rotate { ids, .. }
            | Cmd::Remove { ids }
            | Cmd::Paint { ids, .. }
            | Cmd::Group { ids, .. }
            | Cmd::Copy { ids, .. }
            | Cmd::MakeSubbuild { ids, .. }
            | Cmd::Select { ids }
            | Cmd::Hide { ids, .. }
            | Cmd::Lock { ids, .. }
            | Cmd::Restep { ids, .. } => ids,
        }
    }
    /// Every existing part a command names: moving ids, connector endpoints, pivots.
    pub fn refs(&self) -> BTreeSet<Id> {
        let mut out: BTreeSet<Id> = self.ids().iter().copied().collect();
        match self {
            Cmd::Connect { source, target, .. } => {
                for e in [source, target] {
                    if let Some(id) = serde_json::to_value(e)
                        .ok()
                        .and_then(|v| v.get("part").and_then(Value::as_u64))
                    {
                        out.insert(id);
                    }
                }
            }
            Cmd::Articulate { clicked, .. } => {
                out.insert(*clicked);
            }
            Cmd::Restep { after, .. } => {
                out.insert(*after);
            }
            _ => {}
        }
        out
    }
}

pub struct CommandDef {
    pub name: &'static str,
    pub key: &'static str,
    pub schema: Value,
}
/// Every entry is exposed in the command palette and by MCP; there is no second write API.
pub fn registry() -> Vec<CommandDef> {
    let ids = json!({"type":"array","items":{"type":"integer","minimum":1},"uniqueItems":true});
    let vec3 = json!({"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3});
    let entries = [
        (
            "connect",
            "c",
            json!({"ids":ids,"file":{"type":"integer"},"source":{"type":"object"},"target":{"type":"object"},"pose":{"type":"integer","minimum":0}}),
            vec!["ids", "file", "source", "target"],
        ),
        (
            "articulate",
            "h",
            json!({"ids":ids,"file":{"type":"integer"},"clicked":{"type":"integer"},"joint":{"type":"integer"},"axis":{"type":"integer","minimum":0},"angle":{"type":"number"},"detach":{"type":"array","items":{"type":"integer"}}}),
            vec!["ids", "file", "clicked", "joint", "axis", "angle"],
        ),
        (
            "mirror",
            "",
            json!({"ids":ids,"axis":{"type":"integer","minimum":0,"maximum":2},"pivot":vec3}),
            vec!["ids", "axis", "pivot"],
        ),
        (
            "add",
            "drag",
            json!({"file":{"type":"integer","minimum":0},"parts":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"color":{"type":"integer"},"pos":vec3,"rot":{"type":"array","items":{"type":"number"},"minItems":9,"maxItems":9}},"required":["name"]}},"free":{"type":"boolean"}}),
            vec!["file", "parts"],
        ),
        (
            "snap",
            "drag",
            json!({"ids":ids,"file":{"type":"integer","minimum":0},"near":vec3,"part":{"type":"object","properties":{"name":{"type":"string"},"color":{"type":"integer"},"pos":vec3,"rot":{"type":"array","items":{"type":"number"},"minItems":9,"maxItems":9}},"required":["name"]}}),
            vec!["ids", "file", "part", "near"],
        ),
        (
            "move",
            "arrows",
            json!({"ids":ids,"delta":vec3,"free":{"type":"boolean"}}),
            vec!["ids", "delta"],
        ),
        (
            "nudge",
            "shift+arrows",
            json!({"ids":ids,"direction":vec3,"normal":{"type":"boolean"}}),
            vec!["ids", "direction"],
        ),
        (
            "rotate",
            "arrows / r",
            json!({"ids":ids,"axis":vec3,"angle":{"type":"number"},"pivot":vec3,"free":{"type":"boolean"}}),
            vec!["ids", "axis", "angle", "pivot"],
        ),
        ("remove", "delete", json!({"ids":ids}), vec!["ids"]),
        (
            "paint",
            "p",
            json!({"ids":ids,"color":{"type":"integer"}}),
            vec!["ids", "color"],
        ),
        (
            "group",
            "g",
            json!({"ids":ids,"name":{"type":"string"}}),
            vec!["ids", "name"],
        ),
        (
            "copy",
            "cmd+d",
            json!({"ids":ids,"delta":vec3,"count":{"type":"integer","minimum":1,"maximum":10000}}),
            vec!["ids", "delta"],
        ),
        (
            "make_subbuild",
            "",
            json!({"ids":ids,"name":{"type":"string"}}),
            vec!["ids", "name"],
        ),
        ("select", "s", json!({"ids":ids}), vec!["ids"]),
        (
            "hide",
            "",
            json!({"ids":ids,"value":{"type":"boolean"}}),
            vec!["ids", "value"],
        ),
        (
            "lock",
            "",
            json!({"ids":ids,"value":{"type":"boolean"}}),
            vec!["ids", "value"],
        ),
    ];
    entries.into_iter().map(|(name,key,properties,required)| CommandDef { name,key,schema:json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}) }).collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct Part {
    pub id: Id,
    pub file: usize,
    pub line: usize,
    pub revision: u64,
    pub name: String,
    pub color: i32,
    pub pos: DVec3,
    pub rot: DMat3,
    pub step: u32,
    pub hidden: bool,
    pub locked: bool,
}
impl Part {
    pub fn line(&self) -> PartLine {
        PartLine {
            name: self.name.clone(),
            color: Color(self.color),
            pos: self.pos,
            rot: self.rot,
        }
    }
    /// Process-independent identity of this part's content: file index plus the exact
    /// LDraw line text. Draft bases use it, so unrelated undo/redo never stales a draft.
    pub fn content_hash(&self) -> u64 {
        let text = format!("{}|{}", self.file, self.line().to_text());
        ldraw::num::fnv1a64(text.as_bytes())
    }
    pub fn flags(&self) -> u8 {
        self.hidden as u8 | (self.locked as u8) << 1
    }
}
/// Stable part ids per file, in part-line order, with view flags (bit 0 hidden, bit 1 locked).
/// Ids never appear in exported LDraw; a project file stores them beside each file.
pub type IdMap = Vec<Vec<(Id, u8)>>;
#[derive(Clone)]
pub struct Model {
    pub doc: Document,
    pub parts: BTreeMap<Id, Part>,
    pub selection: BTreeSet<Id>,
    pub path: Option<PathBuf>,
}
impl Model {
    pub fn from_document(doc: Document, path: Option<PathBuf>) -> Self {
        Self::from_document_with_ids(doc, path, &Vec::new())
    }
    /// Rebuild with persisted ids. Missing or duplicate ids get fresh ones above the maximum.
    pub fn from_document_with_ids(doc: Document, path: Option<PathBuf>, ids: &IdMap) -> Self {
        let mut wanted: Vec<Vec<Option<(Id, u8)>>> = Vec::new();
        let mut used = BTreeSet::new();
        for (file, f) in doc.files.iter().enumerate() {
            let given = ids.get(file);
            wanted.push(
                f.parts()
                    .enumerate()
                    .map(|(k, _)| {
                        given
                            .and_then(|g| g.get(k).copied())
                            .filter(|(id, _)| *id > 0 && used.insert(*id))
                    })
                    .collect(),
            );
        }
        let mut next = used.iter().next_back().copied().unwrap_or(0) + 1;
        let mut parts = BTreeMap::new();
        for (file, f) in doc.files.iter().enumerate() {
            for (k, (line, p, step)) in f.parts().enumerate() {
                let (id, flags) = wanted[file][k].unwrap_or_else(|| {
                    next += 1;
                    (next - 1, 0)
                });
                parts.insert(
                    id,
                    Part {
                        id,
                        file,
                        line,
                        revision: 0,
                        name: p.name.clone(),
                        color: p.color.0,
                        pos: p.pos,
                        rot: p.rot,
                        step,
                        hidden: flags & 1 != 0,
                        locked: flags & 2 != 0,
                    },
                );
            }
        }
        Self {
            doc,
            parts,
            selection: BTreeSet::new(),
            path,
        }
    }
    /// Current ids in part-line order per file (what a project file persists).
    pub fn id_map(&self) -> IdMap {
        let mut out: IdMap = vec![Vec::new(); self.doc.files.len()];
        let mut sorted: Vec<_> = self.parts.values().collect();
        sorted.sort_by_key(|p| (p.file, p.line));
        for p in sorted {
            if let Some(f) = out.get_mut(p.file) {
                f.push((p.id, p.flags()));
            }
        }
        out
    }
    pub fn id_at(&self, file: usize, line: usize) -> Option<Id> {
        self.parts
            .values()
            .find(|p| p.file == file && p.line == line)
            .map(|p| p.id)
    }
    pub fn flat(&self, file: usize) -> Vec<ldraw::FlatPart> {
        if file >= self.doc.files.len() {
            return Vec::new();
        }
        self.doc
            .flatten_file(file, self.path.as_deref().and_then(Path::parent))
    }
    pub fn write_part(&mut self, id: Id) {
        let p = &self.parts[&id];
        self.doc.set_part(
            p.file,
            p.line,
            PartLine {
                name: p.name.clone(),
                color: Color(p.color),
                pos: p.pos,
                rot: p.rot,
            },
        );
    }
    fn append(&mut self, file: usize, line: Line) -> Result<usize> {
        let f = self.doc.files.get_mut(file).ok_or("unknown model")?;
        if f.kind != FileKind::Model {
            return Err("cannot edit embedded data".into());
        }
        let at = f
            .lines
            .iter()
            .position(|l| matches!(l.kind, LineKind::NoFile))
            .unwrap_or(f.lines.len());
        if at > 0 && f.lines[at - 1].eol == ldraw::model::Eol::None {
            f.lines[at - 1].eol = self.doc.eol;
        }
        f.lines.insert(at, line);
        for p in self
            .parts
            .values_mut()
            .filter(|p| p.file == file && p.line >= at)
        {
            p.line += 1;
        }
        Ok(at)
    }
    /// Insert a line at `at` in a model file, shifting the parts after it.
    fn insert_line(&mut self, file: usize, at: usize, line: Line) {
        self.doc.files[file].lines.insert(at, line);
        for p in self.parts.values_mut().filter(|p| p.file == file && p.line >= at) {
            p.line += 1;
        }
    }
    /// Remove the line at `at` (which holds no tracked part), shifting the parts after it.
    fn remove_line(&mut self, file: usize, at: usize) -> Line {
        let line = self.doc.files[file].lines.remove(at);
        for p in self.parts.values_mut().filter(|p| p.file == file && p.line > at) {
            p.line -= 1;
        }
        line
    }
    /// Recompute every part's construction step in a file from its `0 STEP` lines.
    fn restep(&mut self, file: usize) {
        let steps: HashMap<usize, u32> = self.doc.files[file].parts().map(|(l, _, s)| (l, s)).collect();
        for p in self.parts.values_mut().filter(|p| p.file == file) {
            if let Some(s) = steps.get(&p.line) {
                p.step = *s;
            }
        }
    }
}

/// The model as LDraw for export: the placeholders removed parts leave behind (`0 // removed`,
/// kept so line numbers stay stable while editing) are dropped.
pub fn export_bytes(model: &Model) -> Vec<u8> {
    let mut doc = model.doc.clone();
    for f in &mut doc.files {
        f.lines.retain(|l| l.text.trim() != "0 // removed");
    }
    doc.to_bytes()
}

#[derive(Clone)]
pub struct Snapshot {
    pub model: Model,
    pub revision: u64,
    pub saved_revision: u64,
    pub drafts: Vec<drafts::Draft>,
    /// The live engine's next id, so previews allocate where the engine would.
    pub next_id: Id,
}

pub struct Engine {
    pub model: Model,
    pub lib: Arc<Library>,
    pub revision: u64,
    pub saved_revision: u64,
    next_id: Id,
    undo: Vec<Model>,
    redo: Vec<Model>,
    pub drafts: drafts::Drafts,
    pub validator: validation::Validator,
    disk_hash: Option<u64>,
    last_rotation: Option<rotation_fit::Record>,
    /// Draft replay: ids handed out in recorded order instead of fresh allocation.
    replay: Option<std::collections::VecDeque<Id>>,
    /// Draft recording: ids allocated while a draft command runs.
    recorded: Option<Vec<Id>>,
    /// Block id source shared between processes (a project file's reserved ranges).
    reserve: Option<Box<dyn FnMut() -> Option<(Id, Id)> + Send>>,
    block_end: Id,
    /// A follower view of a project another process writes: edits are refused.
    pub read_only: bool,
    /// Structured detail of the last refused placement (collisions with part ids, overlap and
    /// an estimated separation), so callers can draw and label it instead of parsing text.
    pub last_failure: Option<Value>,
    /// While a batch defers its collision check: the parts it changed so far.
    pub(crate) deferred: Option<Vec<Id>>,
}
impl Engine {
    pub fn new(lib: Arc<Library>, doc: Document, path: Option<PathBuf>) -> Self {
        Self::from_model(lib, Model::from_document(doc, path))
    }
    pub fn from_model(lib: Arc<Library>, model: Model) -> Self {
        let next_id = model.parts.keys().next_back().copied().unwrap_or(0) + 1;
        let disk_hash = model
            .path
            .as_ref()
            .map(|_| hash_bytes(&model.doc.to_bytes()));
        Self {
            model,
            lib,
            revision: 0,
            saved_revision: 0,
            next_id,
            undo: Vec::new(),
            redo: Vec::new(),
            drafts: Default::default(),
            validator: Default::default(),
            disk_hash,
            last_rotation: None,
            replay: None,
            recorded: None,
            reserve: None,
            block_end: Id::MAX,
            read_only: false,
            last_failure: None,
            deferred: None,
        }
    }
    /// Swap in a model read from elsewhere (another process's publish, a checkout).
    /// Keeps the library, validator caches and drafts; clears undo.
    pub fn replace_model(&mut self, model: Model) {
        let top = model.parts.keys().next_back().copied().unwrap_or(0) + 1;
        if self.reserve.is_none() {
            self.next_id = self.next_id.max(top);
        }
        self.model = model;
        self.undo.clear();
        self.redo.clear();
        self.revision += 1;
        self.saved_revision = self.revision;
        self.refresh_drafts();
    }
    /// Allocate new ids from blocks reserved by `reserve` (start, end exclusive), so
    /// several processes editing one project never hand out the same id.
    pub fn set_id_reserver(&mut self, reserve: Box<dyn FnMut() -> Option<(Id, Id)> + Send>) {
        self.reserve = Some(reserve);
        self.block_end = self.next_id;
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            model: self.model.clone(),
            revision: self.revision,
            saved_revision: self.saved_revision,
            drafts: self.drafts.ops.clone(),
            next_id: self.next_id,
        }
    }
    pub fn allocate(&mut self) -> Id {
        if let Some(id) = self.replay.as_mut().and_then(|q| q.pop_front()) {
            if let Some(r) = &mut self.recorded {
                r.push(id);
            }
            return id;
        }
        // Never hand out an id a part already has, even if the counter fell behind.
        let id = loop {
            if self.next_id >= self.block_end {
                if let Some((start, end)) = self.reserve.as_mut().and_then(|f| f()) {
                    self.next_id = start;
                    self.block_end = end;
                }
            }
            let id = self.next_id;
            self.next_id += 1;
            if !self.model.parts.contains_key(&id) {
                break id;
            }
        };
        if let Some(r) = &mut self.recorded {
            r.push(id);
        }
        id
    }
    pub fn next_id(&self) -> Id {
        self.next_id
    }
    /// The unused rest of the id block this engine holds, `[next, end)`, if any — a short-lived
    /// process hands it back (see `Store::release_ids`) so ids stay small.
    pub fn unused_ids(&self) -> Option<(Id, Id)> {
        (self.reserve.is_some() && self.next_id < self.block_end && self.block_end != Id::MAX).then_some((self.next_id, self.block_end))
    }
    pub fn apply_for_preview(&mut self, cmd: Cmd) -> Result<Value> {
        self.apply(cmd)
    }
    pub fn execute(&mut self, cmd: Cmd) -> Result<Value> {
        if self.read_only && !matches!(cmd, Cmd::Select { .. } | Cmd::Hide { .. }) {
            return Err("this view is read-only; another process owns the project".into());
        }
        self.last_failure = None;
        let old = self.model.clone();
        let rotation = self.last_rotation.clone();
        let result = self.apply(cmd.clone());
        match result {
            Ok(value) => {
                if !matches!(cmd, Cmd::Select { .. }) {
                    self.undo.push(old);
                    self.redo.clear();
                    self.revision += 1;
                    self.refresh_drafts();
                }
                Ok(value)
            }
            Err(e) => {
                self.model = old;
                self.last_rotation = rotation;
                Err(e)
            }
        }
    }
    /// Apply a transaction to this engine. Caller owns rollback and undo grouping.
    pub(crate) fn apply(&mut self, cmd: Cmd) -> Result<Value> {
        let unique: BTreeSet<_> = cmd.ids().iter().copied().collect();
        if unique.len() != cmd.ids().len() {
            return Err("duplicate part ids".into());
        }
        for id in cmd.ids() {
            let p = self
                .model
                .parts
                .get(id)
                .ok_or_else(|| format!("unknown part #{id}"))?;
            if p.locked
                && !matches!(
                    cmd,
                    Cmd::Select { .. }
                        | Cmd::Lock { .. }
                        | Cmd::Hide { .. }
                        | Cmd::Connect { .. }
                        | Cmd::Articulate { .. }
                )
            {
                return Err(format!("part #{id} is locked"));
            }
        }
        let mut changed = Vec::new();
        let mut legal = false;
        let result = match cmd {
            Cmd::Mirror { ids, axis, pivot } => {
                if axis > 2 || !pivot.is_finite() {
                    return Err("Choose mirror axis X, Y or Z and a finite pivot".into());
                }
                let first = ids
                    .first()
                    .and_then(|id| self.model.parts.get(id))
                    .ok_or("Select an assembly to mirror")?;
                let file = first.file;
                if ids.iter().any(|id| self.model.parts[id].file != file) {
                    return Err("Mirror one model panel at a time".into());
                }
                let lines: BTreeSet<_> = ids.iter().map(|id| self.model.parts[id].line).collect();
                let specs: Vec<_> = self
                    .model
                    .flat(file)
                    .into_iter()
                    .filter(|p| lines.contains(&p.root_line))
                    .map(|p| PartSpec {
                        name: p.name,
                        color: p.color.0,
                        pos: p.pos,
                        rot: p.rot,
                        id: None,
                    })
                    .collect();
                let parts = self.snapshot().mirror_specs(file, specs, axis, pivot)?;
                self.apply(Cmd::Remove { ids })?;
                let value = self.apply(Cmd::Add {
                    file,
                    parts,
                    free: false,
                })?;
                self.model.selection = serde_json::from_value::<Vec<Id>>(value["ids"].clone())
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .collect();
                return Ok(value);
            }
            Cmd::Snap {
                ids,
                file,
                part,
                near,
            } => {
                if ids.is_empty() {
                    let value = connectors::request(
                        &self.model,
                        file,
                        json!({"action":"drag","moving":ids,"part":part,"near":near}),
                    )?;
                    let pose: connectors::Pose = serde_json::from_value(value["poses"][0].clone())
                        .map_err(|e| e.to_string())?;
                    let mut part = part;
                    part.pos = pose.pos;
                    part.rot = pose.rot;
                    part.id = None;
                    return self.apply(Cmd::Add {
                        file,
                        parts: vec![part],
                        free: true,
                    });
                }
                let references: Vec<_> = self
                    .model
                    .parts
                    .values()
                    .filter(|p| p.file == file)
                    .map(|p| p.id)
                    .collect();
                return self.apply_connector(
                    file,
                    &references,
                    json!({"action":"drag","moving":ids,"source":{"part":ids[0]},"near":near}),
                );
            }
            Cmd::Connect {
                ids,
                file,
                source,
                target,
                pose,
            } => {
                return self.apply_connector(
                    file,
                    &ids,
                    json!({"action":"connect","source":source,"target":target,"pose":pose}),
                );
            }
            Cmd::Articulate {
                ids,
                file,
                clicked,
                joint,
                axis,
                angle,
                detach,
            } => {
                let analysis = connectors::analyze(&self.model, file)?;
                let plan = analysis.graph.articulate(clicked, joint)?;
                let _ = plan.rotation(axis, angle, &detach)?;
                if detach.iter().copied().collect::<BTreeSet<_>>()
                    != plan.detach.iter().copied().collect()
                {
                    return Err("unexpected loop detachment".into());
                }
                return self.apply_connector(file,&ids,json!({"action":"rotate","moving":plan.moving,"axis":plan.axes[axis],"angle":angle,"pivot":plan.pivot,"detach":detach}));
            }
            Cmd::Add { file, parts, free } => {
                if parts.is_empty() || parts.len() > 10000 {
                    return Err("add requires 1..10000 parts".into());
                }
                for spec in parts {
                    check_pose(spec.pos, spec.rot)?;
                    if spec.name.chars().any(char::is_control)
                        || Path::new(&spec.name)
                            .components()
                            .any(|c| !matches!(c, std::path::Component::Normal(_)))
                    {
                        return Err("part name must be a relative library/model name without control characters or parent paths".into());
                    }
                    if !spec.name.to_lowercase().ends_with(".dat")
                        && self.model.doc.file_index(&spec.name).is_none()
                    {
                        return Err("part must be .dat or an existing submodel".into());
                    }
                    if let Some(sub) = self.model.doc.file_index(&spec.name) {
                        self.check_cycle(file, sub)?;
                    }
                    let id = spec.id.unwrap_or_else(|| self.allocate());
                    if self.model.parts.contains_key(&id) {
                        return Err("part id already exists".into());
                    }
                    if id == 0 || id > i64::MAX as u64 {
                        return Err("part id out of range".into());
                    }
                    if self.reserve.is_none() {
                        self.next_id = self.next_id.max(id + 1);
                    }
                    let p = PartLine {
                        name: spec.name.clone(),
                        color: Color(spec.color),
                        pos: spec.pos,
                        rot: spec.rot,
                    };
                    let line = self.model.append(file, Line::part(p, self.model.doc.eol))?;
                    let step = self.model.doc.files[file]
                        .parts()
                        .find(|(li, _, _)| *li == line)
                        .unwrap()
                        .2;
                    self.model.parts.insert(
                        id,
                        Part {
                            id,
                            file,
                            line,
                            revision: self.revision + 1,
                            name: spec.name,
                            color: spec.color,
                            pos: spec.pos,
                            rot: spec.rot,
                            step,
                            hidden: false,
                            locked: false,
                        },
                    );
                    changed.push(id);
                }
                legal = !free;
                json!({"ids":changed})
            }
            Cmd::Move { ids, delta, free } => {
                if !delta.is_finite() {
                    return Err("non-finite delta".into());
                }
                for id in ids {
                    self.model.parts.get_mut(&id).unwrap().pos += delta;
                    changed.push(id);
                }
                legal = !free;
                json!({"ids":changed})
            }
            Cmd::Nudge {
                ids,
                direction,
                normal,
                alt,
            } => self.nudge(ids, direction, normal, alt)?,
            Cmd::Rotate {
                ids,
                axis,
                angle,
                pivot,
                free,
            } => {
                if !axis.is_finite()
                    || axis.length() < 1e-9
                    || !angle.is_finite()
                    || !pivot.is_finite()
                {
                    return Err("invalid rotation".into());
                }
                let r = DMat3::from_axis_angle(axis.normalize(), angle.to_radians());
                let before = ids
                    .iter()
                    .map(|id| self.model.parts[id].clone())
                    .collect::<Vec<_>>();
                for id in ids {
                    let p = self.model.parts.get_mut(&id).unwrap();
                    p.pos = pivot + r * (p.pos - pivot);
                    p.rot = r * p.rot;
                    changed.push(id);
                }
                if !free {
                    self.fit_rotation(before, axis.normalize(), angle)?;
                }
                legal = !free;
                json!({"ids":changed})
            }
            Cmd::Paint { ids, color } => {
                let mut warnings = Vec::new();
                for id in ids {
                    if let Some(warning) = availability::warning(&self.model.parts[&id].name, color)
                    {
                        warnings.push(warning);
                    }
                    self.model.parts.get_mut(&id).unwrap().color = color;
                    changed.push(id);
                }
                warnings.sort();
                warnings.dedup();
                json!({"ids":changed,"warnings":warnings})
            }
            Cmd::Remove { ids } => {
                for id in &ids {
                    let p = self.model.parts.remove(id).unwrap();
                    self.model.doc.files[p.file].lines[p.line] =
                        Line::new("0 // removed", self.model.doc.eol);
                    self.model.selection.remove(id);
                }
                json!({"removed":ids})
            }
            Cmd::Step { file, label } => {
                let f = self.model.doc.files.get(file).ok_or("unknown model")?;
                if f.kind != FileKind::Model {
                    return Err("cannot edit embedded data".into());
                }
                if let Some(label) = label.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
                    if label.chars().any(char::is_control) {
                        return Err("step label must be one line".into());
                    }
                    self.model.append(file, Line::new(&format!("0 // {label}"), self.model.doc.eol))?;
                }
                let at = self.model.append(file, Line::new("0 STEP", self.model.doc.eol))?;
                self.model.restep(file);
                json!({"step_line": at})
            }
            Cmd::Restep { ids, after } => {
                let file = self.model.parts.get(&after).ok_or_else(|| format!("unknown part #{after}"))?.file;
                if ids.contains(&after) {
                    return Err("a part cannot be moved after itself".into());
                }
                if ids.iter().any(|id| self.model.parts[id].file != file) {
                    return Err("restep: all parts must be in the same model file as the anchor".into());
                }
                for (k, id) in ids.iter().enumerate() {
                    let from = self.model.parts[id].line;
                    // Detach the line: park the part on a placeholder, then drop the placeholder.
                    let line = std::mem::replace(&mut self.model.doc.files[file].lines[from], Line::new("0", self.model.doc.eol));
                    self.model.parts.get_mut(id).unwrap().line = 0; // parked; at >= 1 below, so the shifts skip it
                    self.model.remove_line(file, from);
                    // The first part goes to the end of the anchor's step (just before its `0 STEP`,
                    // outside any group block); the rest follow it in order.
                    let at = if k == 0 {
                        let lines = &self.model.doc.files[file].lines;
                        let from = self.model.parts[&after].line + 1;
                        (from..lines.len())
                            .find(|&i| matches!(lines[i].kind, ldraw::LineKind::Step | ldraw::LineKind::RotStep))
                            .unwrap_or(lines.len())
                    } else {
                        self.model.parts[&ids[k - 1]].line + 1
                    };
                    self.model.insert_line(file, at, line);
                    self.model.parts.get_mut(id).unwrap().line = at;
                    changed.push(*id);
                }
                self.model.restep(file);
                json!({"after": after, "step": self.model.parts[&after].step})
            }
            Cmd::Select { ids } => {
                self.model.selection = ids.into_iter().collect();
                json!({"selection":self.model.selection})
            }
            Cmd::Hide { ids, value } => {
                for id in ids {
                    self.model.parts.get_mut(&id).unwrap().hidden = value;
                    changed.push(id);
                }
                json!({"hidden":value})
            }
            Cmd::Lock { ids, value } => {
                for id in ids {
                    self.model.parts.get_mut(&id).unwrap().locked = value;
                    changed.push(id);
                }
                json!({"locked":value})
            }
            Cmd::Group { ids, name } => {
                safe_name(&name)?;
                if ids.is_empty() {
                    return Err("select parts first".into());
                }
                let file = self.model.parts[&ids[0]].file;
                if ids.iter().any(|id| self.model.parts[id].file != file) {
                    return Err("group selection must be in one model".into());
                }
                // Scope the markers around each selected source line, without grouping unselected parts.
                let mut lines: Vec<usize> =
                    ids.iter().map(|id| self.model.parts[id].line).collect();
                lines.sort_unstable();
                for line in lines.into_iter().rev() {
                    for (at, text) in [
                        (line + 1, "0 !LEOCAD GROUP END".to_string()),
                        (line, format!("0 !LEOCAD GROUP BEGIN {name}")),
                    ] {
                        self.model.doc.files[file]
                            .lines
                            .insert(at, Line::new(&text, self.model.doc.eol));
                        for p in self
                            .model
                            .parts
                            .values_mut()
                            .filter(|p| p.file == file && p.line >= at)
                        {
                            p.line += 1;
                        }
                    }
                }
                changed = ids;
                json!({"group":name})
            }
            Cmd::Copy { ids, delta, count } => {
                if ids.is_empty() || count == 0 || count > 10000 || count * ids.len() > 10000 {
                    return Err("copy requires 1..10000 new parts".into());
                }
                let file = self.model.parts[&ids[0]].file;
                if ids.iter().any(|id| self.model.parts[id].file != file) {
                    return Err("copy from one model at a time".into());
                }
                let parts = (1..=count)
                    .flat_map(|k| ids.iter().map(move |id| (*id, k)))
                    .map(|(id, k)| {
                        let p = &self.model.parts[&id];
                        PartSpec {
                            name: p.name.clone(),
                            color: p.color,
                            pos: p.pos + delta * k as f64,
                            rot: p.rot,
                            id: None,
                        }
                    })
                    .collect();
                self.apply(Cmd::Add {
                    file,
                    parts,
                    free: false,
                })?
            }
            Cmd::MakeSubbuild { ids, name } => {
                let sub = self.make_subbuild(&ids, &name)?;
                changed = ids;
                json!({"file":sub,"name":self.model.doc.files[sub].name})
            }
        };
        for id in &changed {
            if let Some(p) = self.model.parts.get_mut(id) {
                p.revision = self.revision + 1;
                self.model.write_part(*id);
            }
        }
        if legal {
            match self.deferred.as_mut() {
                Some(pending) => pending.extend(changed.iter().copied()),
                None => self.validate_changes(&changed)?,
            }
        }
        Ok(result)
    }
    fn check_cycle(&self, parent: usize, sub: usize) -> Result<()> {
        let mut stack = vec![sub];
        let mut seen = BTreeSet::new();
        while let Some(i) = stack.pop() {
            if i == parent {
                return Err("recursive submodel reference".into());
            }
            if seen.insert(i) {
                stack.extend(
                    self.model.doc.files[i]
                        .parts()
                        .filter_map(|(_, p, _)| self.model.doc.file_index(&p.name)),
                );
            }
        }
        Ok(())
    }
    fn make_subbuild(&mut self, ids: &[Id], name: &str) -> Result<usize> {
        safe_name(name)?;
        if ids.is_empty() {
            return Err("select parts first".into());
        }
        let name = if name.to_lowercase().ends_with(".ldr") {
            name.to_string()
        } else {
            format!("{name}.ldr")
        };
        if self.model.doc.file_index(&name).is_some() {
            return Err("submodel name already exists".into());
        }
        let file = self.model.parts[&ids[0]].file;
        if ids.iter().any(|id| self.model.parts[id].file != file) {
            return Err("selection must come from one model".into());
        }
        let step = self.model.parts[&ids[0]].step;
        if ids.iter().any(|id| self.model.parts[id].step != step) {
            return Err("subbuild selection must be in one construction step; otherwise its insertion would lose step ordering".into());
        }
        let mut ids = ids.to_vec();
        ids.sort_by_key(|id| self.model.parts[id].line);
        let pivot = self.model.parts[&ids[0]].pos;
        let eol = self.model.doc.eol;
        if !self.model.doc.mpd {
            self.model.doc.mpd = true;
            let n = self.model.doc.files[0].name.clone();
            self.model.doc.files[0]
                .lines
                .insert(0, Line::new(&format!("0 FILE {n}"), eol));
            for p in self.model.parts.values_mut().filter(|p| p.file == 0) {
                p.line += 1;
            }
        }
        let sub = self.model.doc.files.len();
        let first_line = self.model.parts[&ids[0]].line;
        self.model.doc.files.push(ModelFile {
            name: name.clone(),
            kind: FileKind::Model,
            lines: vec![Line::new(&format!("0 FILE {name}"), eol)],
        });
        for id in &ids {
            let p = self.model.parts.get_mut(id).unwrap();
            self.model.doc.files[p.file].lines[p.line] = Line::new("0 // moved to submodel", eol);
            p.file = sub;
            p.line = self.model.doc.files[sub].lines.len();
            p.pos -= pivot;
            p.step = 1;
            self.model.doc.files[sub].lines.push(Line::part(
                PartLine {
                    name: p.name.clone(),
                    color: Color(p.color),
                    pos: p.pos,
                    rot: p.rot,
                },
                eol,
            ));
        }
        let id = self.allocate();
        let part = PartLine {
            name: name.clone(),
            color: Color(16),
            pos: pivot,
            rot: DMat3::IDENTITY,
        };
        self.model.doc.files[file].lines[first_line] = Line::part(part, eol);
        self.model.parts.insert(
            id,
            Part {
                id,
                file,
                line: first_line,
                revision: self.revision + 1,
                name,
                color: 16,
                pos: pivot,
                rot: DMat3::IDENTITY,
                step,
                hidden: false,
                locked: false,
            },
        );
        self.model.selection.clear();
        Ok(sub)
    }
    fn apply_connector(&mut self, file: usize, ids: &[Id], request: Value) -> Result<Value> {
        if file != self.model.doc.main_index().unwrap_or(0) {
            return Err("Mechanical tools currently edit the main model only; shared-subbuild instance constraints are not yet supported".into());
        }
        let referenced: BTreeSet<_> = ids.iter().copied().collect();
        if self
            .model
            .parts
            .values()
            .filter(|p| p.file == file)
            .any(|p| !referenced.contains(&p.id))
        {
            return Err("connector commands must stamp all parts in the edited file".into());
        }
        let result = connectors::request(&self.model, file, request)?;
        let poses: Vec<connectors::Pose> =
            serde_json::from_value(result["poses"].clone()).map_err(|e| e.to_string())?;
        for p in &poses {
            check_pose(p.pos, p.rot)?;
            let old = self
                .model
                .parts
                .get(&p.id)
                .ok_or("unknown connector result part")?;
            if old.file != file || old.locked {
                return Err("the moving chunk contains a locked or foreign part".into());
            }
        }
        for p in &poses {
            let old = self.model.parts.get_mut(&p.id).unwrap();
            old.pos = p.pos;
            old.rot = p.rot;
            old.revision = self.revision + 1;
            self.model.write_part(p.id);
        }
        Ok(
            json!({"changed":poses.iter().map(|p|p.id).collect::<Vec<_>>(),"validation":"full connector backend"}),
        )
    }
    /// Refuse the change when the checker finds a new COLLISION involving a changed part (in any
    /// model file that uses it). CONFLICT, FLOATING and LOOSE are reported after the edit, not refused.
    /// Without the checker service (no Python) the plain voxel collision test stands in.
    fn validate_changes(&mut self, ids: &[Id]) -> Result<()> {
        let changed: BTreeSet<_> = ids
            .iter()
            .filter_map(|id| self.model.parts.get(id).map(|p| (0, p.file, p.line)))
            .collect();
        // Check every world instance, not just the edited submodel's local geometry.
        for file in 0..self.model.doc.files.len() {
            let flat = self.model.flat(file);
            let affected: Vec<_> = flat
                .iter()
                .map(|p| {
                    changed.contains(&p.src) || p.ancestors.iter().any(|a| changed.contains(a))
                })
                .collect();
            if !affected.iter().any(|&a| a) {
                continue;
            }
            // (moving flat index, blocker flat index, overlap, box, clear_by, depth)
            type Clash = (usize, usize, f64, [[f64; 3]; 2], [f64; 3], f64);
            let clashes: Vec<Clash> = match checker::check(&self.model, file) {
                Ok(report) => report
                    .hits()
                    .filter(|(a, b, h)| h.kind == "collision" && (affected[*a] || affected[*b]))
                    .map(|(a, b, h)| {
                        let (push, depth) = h.separation();
                        let bounds = h.bounds.unwrap_or_default();
                        if affected[a] {
                            (a, b, h.overlap, bounds, push, depth)
                        } else {
                            (b, a, h.overlap, bounds, push.map(|v| -v), depth)
                        }
                    })
                    .collect(),
                Err(_) => {
                    let report = self.validator.check_focus(&self.lib, &flat, Some(&affected));
                    let centre = |i: usize| flat[i].pos.to_array();
                    report
                        .pairs
                        .iter()
                        .filter(|p| p.overlap >= physics::COLLIDE_MIN && (affected[p.a] || affected[p.b]))
                        .map(|p| {
                            let (m, b) = if affected[p.a] { (p.a, p.b) } else { (p.b, p.a) };
                            let (push, depth) = p.separation(centre(m), centre(b));
                            let push = if m == p.a { push } else { push.map(|v| -v) };
                            (m, b, p.overlap, [p.lo, p.hi], push, depth)
                        })
                        .collect()
                }
            };
            if clashes.is_empty() {
                continue;
            }
            let ids: HashMap<usize, Id> = self
                .model
                .parts
                .values()
                .filter(|p| p.file == file)
                .map(|p| (p.line, p.id))
                .collect();
            let detail: Vec<Value> = clashes
                .iter()
                .map(|&(m, b, overlap, bounds, push, depth)| {
                    // Report the edited piece as `moving` and the obstacle as `blocker`.
                    json!({
                        "moving": ids.get(&flat[m].root_line),
                        "moving_part": flat[m].name,
                        "blocker": ids.get(&flat[b].root_line),
                        "blocker_part": flat[b].name,
                        "overlap_ldu3": (overlap * 10.0).round() / 10.0,
                        "box": bounds,
                        "clear_by": push,
                        "depth_ldu": (depth * 10.0).round() / 10.0,
                    })
                })
                .collect();
            let first = &detail[0];
            let message = format!(
                "placement collides: {} #{} overlaps {} #{} by about {} LDU{}",
                first["moving_part"].as_str().unwrap_or("?"),
                first["moving"],
                first["blocker_part"].as_str().unwrap_or("?"),
                first["blocker"],
                first["depth_ldu"],
                if detail.len() > 1 {
                    format!(" (+{} more collisions)", detail.len() - 1)
                } else {
                    String::new()
                }
            );
            self.last_failure = Some(json!({
                "kind": "collision",
                "file": self.model.doc.files[file].name,
                "collisions": detail,
            }));
            return Err(message);
        }
        Ok(())
    }
    pub fn undo(&mut self, redo: bool) -> Result<()> {
        let previous = if redo {
            self.redo.pop()
        } else {
            self.undo.pop()
        }
        .ok_or("nothing to undo/redo")?;
        let old = std::mem::replace(&mut self.model, previous);
        if redo {
            self.undo.push(old);
        } else {
            self.redo.push(old);
        }
        self.revision += 1;
        for p in self.model.parts.values_mut() {
            p.revision = self.revision;
        }
        self.refresh_drafts();
        Ok(())
    }
    pub fn refresh_drafts(&mut self) {
        self.drafts.refresh(&self.model);
        let original = self.model.clone();
        let sessions: BTreeSet<_> = self
            .drafts
            .ops
            .iter()
            .filter(|o| o.state == "pending")
            .map(|o| o.session.clone())
            .collect();
        for session in sessions {
            self.model = original.clone();
            let ops: Vec<_> = self
                .drafts
                .ops
                .iter()
                .enumerate()
                .filter(|(_, o)| o.session == session && o.state == "pending")
                .map(|(i, o)| (i, o.clone()))
                .collect();
            for (i, op) in ops {
                let before = self.model.clone();
                if let Err(reason) = self.apply_draft(&op) {
                    self.model = before;
                    self.drafts.ops[i].state = "blocked".into();
                    self.drafts.ops[i].reason = reason;
                }
            }
        }
        self.model = original;
        self.drafts.refresh(&self.model);
    }
    pub fn save(&mut self, path: Option<PathBuf>) -> Result<()> {
        let path = path
            .or_else(|| self.model.path.clone())
            .ok_or("choose a save path")?;
        if self.model.path.as_ref() == Some(&path) {
            if let Some(expected) = self.disk_hash {
                let actual = std::fs::read(&path)
                    .map_err(|e| format!("cannot verify the on-disk model: {e}; use Save as"))?;
                if hash_bytes(&actual) != expected {
                    return Err("The file was changed outside CAD. Refusing to overwrite it; use Save as or reopen it.".into());
                }
            }
        }
        // Same-directory temporary + rename: a failed write must not truncate the user's model.
        let tmp = path.with_extension(format!("sb-save-{}", std::process::id()));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        let result = (|| {
            f.write_all(&self.model.doc.to_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        result.map_err(|e| e.to_string())?;
        self.model.path = Some(path);
        self.disk_hash = Some(hash_bytes(&self.model.doc.to_bytes()));
        self.saved_revision = self.revision;
        Ok(())
    }
}
fn safe_name(name: &str) -> Result<()> {
    if name.trim().is_empty()
        || name.contains(['\n', '\r', '/', '\\'])
        || name == "."
        || name == ".."
    {
        return Err("invalid name".into());
    }
    Ok(())
}
fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}
fn check_pose(pos: DVec3, r: DMat3) -> Result<()> {
    if !pos.is_finite()
        || !r.is_finite()
        || (r.transpose() * r - DMat3::IDENTITY)
            .to_cols_array()
            .iter()
            .any(|v| v.abs() > 1e-4)
        || (r.determinant().abs() - 1.0).abs() > 1e-4
    {
        return Err("pose must be finite and rigid".into());
    }
    Ok(())
}

type Job = Box<dyn FnOnce(&mut Engine) + Send>;
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<Job>,
    snapshot: Arc<RwLock<Arc<Snapshot>>>,
}
impl Handle {
    pub fn spawn(mut engine: Engine) -> Self {
        let snapshot = Arc::new(RwLock::new(Arc::new(engine.snapshot())));
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("sb-engine".into())
            .spawn(move || {
                for job in rx {
                    job(&mut engine);
                }
            })
            .expect("engine thread");
        Self { tx, snapshot }
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().clone()
    }
    pub fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Engine) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = mpsc::sync_channel(1);
        let published = self.snapshot.clone();
        self.tx
            .send(Box::new(move |e| {
                let r = f(e);
                *published.write() = Arc::new(e.snapshot());
                let _ = tx.send(r);
            }))
            .map_err(|_| "engine stopped")?;
        rx.recv().map_err(|_| "engine stopped".to_string())?
    }
    pub fn command(&self, cmd: Cmd) -> Result<Value> {
        self.call(move |e| e.execute(cmd))
    }
}
