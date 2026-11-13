//! SuperBrick operations: one implementation behind `sb <project.sb> <tool> ...`, the MCP
//! server (`sb mcp`) and, in time, the GUI's own panels.
//!
//! A [`Session`] is one user working on one `.sb` project. Every call syncs from the store
//! (other processes may have published), runs, persists what it changed (drafts, observations,
//! selections, candidates) and returns a [`Reply`]: short text, structured data and pictures.
//! Edits by any user other than `human` are drafts (proposals); `human` edits the model.
pub mod args;
pub mod cost;
pub mod front;
pub mod overlay;
pub mod planner;
pub mod shots;
mod tools;

use args::{IdRef, Result, Units};
use engine::{Engine, Id, Model};
use ldraw::Library;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use store::Store;

pub use tools::{describe, TOOLS};

/// A picture in a reply.
#[derive(Clone, Debug)]
pub struct Image {
    pub png: Vec<u8>,
    pub caption: String,
    pub width: u32,
    pub height: u32,
}

impl From<shots::Picture> for Image {
    fn from(p: shots::Picture) -> Image {
        Image { png: p.png, caption: p.caption, width: p.width, height: p.height }
    }
}

/// What a tool call returns.
#[derive(Clone, Debug, Default)]
pub struct Reply {
    pub ok: bool,
    pub text: String,
    pub data: Value,
    pub images: Vec<Image>,
}

impl Reply {
    pub fn ok(text: impl Into<String>, data: Value) -> Reply {
        Reply { ok: true, text: text.into(), data, images: Vec::new() }
    }
    pub fn fail(text: impl Into<String>, data: Value) -> Reply {
        Reply { ok: false, text: text.into(), data, images: Vec::new() }
    }
    pub fn image(mut self, img: Option<Image>) -> Reply {
        self.images.extend(img);
        self
    }
    /// The reply as JSON (images as file paths when `dir` is given, else omitted).
    pub fn to_json(&self, image_paths: &[PathBuf]) -> Value {
        json!({
            "ok": self.ok,
            "text": self.text,
            "data": self.data,
            "images": image_paths.iter().zip(&self.images).map(|(p, i)| json!({
                "path": p, "caption": i.caption, "width": i.width, "height": i.height,
            })).collect::<Vec<_>>(),
        })
    }
}

/// Who may change the model directly.
pub const HUMAN: &str = "human";

/// User names: letters, digits, `-`, `_`, `.`; at most 32 characters.
pub fn valid_user(user: &str) -> bool {
    !user.is_empty()
        && user.len() <= 32
        && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub struct Session {
    pub lib: Arc<Library>,
    pub store: Store,
    pub user: String,
    pub branch: String,
    pub engine: Engine,
    /// Working-tree sequence the engine's model came from.
    seq: i64,
    /// Store change counter at the last sync.
    change: i64,
    /// What the previous edit changed (the `"last"` id reference).
    pub last: Vec<Id>,
    /// Camera direction of the last picture (yaw, pitch): later pictures start from it.
    pub memory: Option<(f32, f32)>,
    /// Make pictures at all (`--no-images`).
    pub images: bool,
    pub size: (u32, u32),
    pub turn: u64,
    search: Option<search::Searcher>,
}

impl Drop for Session {
    /// Hand back unused ids so short CLI calls do not burn through the id space.
    fn drop(&mut self) {
        if let Some((next, end)) = self.engine.unused_ids() {
            let _ = self.store.release_ids(next, end);
        }
    }
}

impl Session {
    /// Open `path` as `user`. The project must exist (see [`create_project`]).
    pub fn open(path: &Path, user: &str, lib: Arc<Library>) -> Result<Session> {
        if !valid_user(user) {
            return Err(format!("invalid user name {user:?}: use letters, digits, '-', '_' or '.'"));
        }
        let mut store = Store::open(path)?;
        store.set_origin(format!("{}:{}", user, std::process::id()));
        let (w, model) = store.load("main")?;
        let mut engine = Engine::from_model(lib.clone(), model);
        engine.set_id_reserver(Store::id_reserver_sized(path.to_path_buf(), 64));
        let scope = format!("user:{user}");
        let last = store
            .pref(&scope, "last")?
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let memory = store.pref(&scope, "camera")?.and_then(|v| serde_json::from_value(v).ok());
        let turn = store.pref(&scope, "turn")?.and_then(|v| v.as_u64()).unwrap_or(0);
        let mut s = Session {
            lib,
            store,
            user: user.to_string(),
            branch: "main".into(),
            engine,
            seq: w.seq,
            change: -1,
            last,
            memory,
            images: true,
            size: (960, 720),
            turn,
            search: None,
        };
        s.sync()?;
        Ok(s)
    }

    pub fn is_human(&self) -> bool {
        self.user == HUMAN
    }

    /// Bring the engine up to date with the store: working tree, drafts, observations.
    pub fn sync(&mut self) -> Result<()> {
        let change = self.store.last_change()?;
        if change == self.change {
            return Ok(());
        }
        let w = self.store.working(&self.branch)?;
        if w.seq != self.seq {
            let model = self.store.load_tree(&w.tree)?;
            self.engine.replace_model(model);
            self.seq = w.seq;
        }
        self.engine.drafts.ops = self.store.load_drafts(&self.branch, true)?;
        self.engine.refresh_drafts();
        let seen = self.store.load_observations(&self.user, &self.branch)?;
        if seen.is_empty() {
            // First visit: the user is shown the model in its first picture.
            self.engine.observe(&self.user.clone());
            self.save_observations()?;
        } else {
            self.engine.drafts.seen.insert(self.user.clone(), seen);
        }
        self.change = self.store.last_change()?;
        Ok(())
    }

    pub fn save_observations(&mut self) -> Result<()> {
        let seen = self.engine.drafts.seen.get(&self.user).cloned().unwrap_or_default();
        self.store.save_observations(&self.user, &self.branch, &seen)
    }

    /// Persist drafts changed by this session (all live ones of the branch are upserted).
    pub fn save_drafts(&mut self, ids: &[u64]) -> Result<()> {
        let ops: Vec<_> = self.engine.drafts.ops.iter().filter(|d| ids.contains(&d.id)).cloned().collect();
        self.store.save_drafts(&self.branch, &ops)
    }

    /// The model as this user sees it: the shared model plus the user's pending drafts.
    pub fn view(&mut self) -> Model {
        if self.is_human() {
            self.engine.model.clone()
        } else {
            let u = self.user.clone();
            self.engine.effective_model(&u)
        }
    }

    pub fn set_last(&mut self, ids: Vec<Id>) -> Result<()> {
        self.last = ids;
        let scope = format!("user:{}", self.user);
        self.store.set_pref(&scope, "last", &json!(self.last))
    }

    pub fn remember_camera(&mut self, views: &[(f32, f32)]) {
        if let Some(&v) = views.first() {
            self.memory = Some(v);
            let scope = format!("user:{}", self.user);
            let _ = self.store.set_pref(&scope, "camera", &json!(v));
        }
    }

    pub fn next_turn(&mut self) -> u64 {
        self.turn += 1;
        let scope = format!("user:{}", self.user);
        let _ = self.store.set_pref(&scope, "turn", &json!(self.turn));
        self.turn
    }

    pub fn ctx(&self) -> Ctx<'_> {
        Ctx { lib: &self.lib, store: &self.store, user: &self.user, last: &self.last }
    }
    pub fn resolve(&self, model: &Model, refs: &[IdRef], file: usize) -> Result<Vec<Id>> {
        self.ctx().resolve(model, refs, file)
    }
    pub fn ids_arg(&self, model: &Model, args: &Value, key: &str, file: usize) -> Result<Vec<Id>> {
        self.ctx().ids_arg(model, args, key, file)
    }
    pub fn seq_value(&self) -> i64 {
        self.seq
    }
    pub fn set_seq(&mut self, seq: i64) {
        self.seq = seq;
    }
    /// Publish the engine's model as the branch's working tree (compare-and-swap against the
    /// version this session last read) and commit it. Errors starting "conflict" mean someone
    /// else published first: `force_sync` and retry.
    pub fn publish_shared(&mut self, kind: &str, message: &str, drafts: &[u64]) -> Result<i64> {
        let seq = self.store.publish(&self.branch, &self.engine.model, Some(self.seq), &self.user)?;
        self.seq = seq;
        if !drafts.is_empty() {
            self.save_drafts(drafts)?;
        }
        let commit = self.store.commit(&self.branch, kind, &self.user, message, drafts)?;
        self.change = self.store.last_change()?;
        Ok(commit)
    }
    /// Re-read everything from the store.
    pub fn force_sync(&mut self) -> Result<()> {
        self.change = -1;
        self.sync()
    }
}

/// The parts of a session that tools read while the engine is busy in a transaction.
pub struct Ctx<'a> {
    pub lib: &'a Arc<Library>,
    pub store: &'a Store,
    pub user: &'a str,
    pub last: &'a [Id],
}

impl Ctx<'_> {
    /// Resolve id references against `model` (numbers, `"last"`, `"all"`, selections).
    pub fn resolve(&self, model: &Model, refs: &[IdRef], file: usize) -> Result<Vec<Id>> {
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for r in refs {
            let ids: Vec<Id> = match r {
                IdRef::Id(id) => vec![*id],
                IdRef::Named(n) if n == "last" => {
                    if self.last.is_empty() {
                        return Err("\"last\" is empty: no edit yet in this project by this user".into());
                    }
                    self.last.to_vec()
                }
                IdRef::Named(n) if n == "all" => model.parts.values().filter(|p| p.file == file).map(|p| p.id).collect(),
                IdRef::Named(n) => self.store.selection(self.user, n)?.parts,
            };
            for id in ids {
                if !model.parts.contains_key(&id) {
                    return Err(format!("no part #{id} in the model (it may have been removed)"));
                }
                if seen.insert(id) {
                    out.push(id);
                }
            }
        }
        Ok(out)
    }

    pub fn ids_arg(&self, model: &Model, args: &Value, key: &str, file: usize) -> Result<Vec<Id>> {
        let refs = args::id_refs(args.get(key).unwrap_or(&Value::Null))?;
        self.resolve(model, &refs, file)
    }
}

impl Session {
    pub fn units(&self, args: &Value) -> Result<Units> {
        Units::from_args(args)
    }

    pub fn searcher(&mut self) -> Result<&search::Searcher> {
        if self.search.is_none() {
            let dir = search::data_dir();
            self.search = Some(search::load(&dir, false).map_err(|e| format!("part search data: {e}"))?);
        }
        Ok(self.search.as_ref().unwrap())
    }

    /// Render a shot of `model` unless pictures are off; errors become a note in the text.
    pub fn picture(&mut self, model: &Model, mut shot: shots::Shot) -> (Option<Image>, Option<String>) {
        if !self.images {
            return (None, None);
        }
        if shot.size == (960, 720) {
            shot.size = self.size;
        }
        match shots::render(&self.lib, model, &shot, self.memory) {
            Ok(p) => {
                self.remember_camera(&p.views);
                let notes = (!p.notes.is_empty()).then(|| p.notes.join(" "));
                (Some(p.into()), notes)
            }
            Err(e) => (None, Some(format!("(no picture: {e})"))),
        }
    }

    /// Run one tool.
    pub fn call(&mut self, tool: &str, args: &Value) -> Reply {
        if let Err(e) = self.sync() {
            return Reply::fail(format!("could not read the project: {e}"), json!({}));
        }
        let args = if args.is_null() { json!({}) } else { args.clone() };
        let news = self.human_news();
        let mut reply = match tools::run(self, tool, &args) {
            Ok(r) => r,
            Err(e) => Reply::fail(e, json!({})),
        };
        if let Some((text, list)) = news {
            reply.text = format!("{text}\n{}", reply.text);
            if let Some(obj) = reply.data.as_object_mut() {
                obj.insert("from_human".into(), list);
            }
        }
        reply
    }

    /// What the human decided about this user's drafts since the user last heard: accepted,
    /// rejected (with the note), or blocked/stale because the model changed underneath.
    /// Each decision is reported once, on the user's next call.
    fn human_news(&mut self) -> Option<(String, Value)> {
        if self.user == HUMAN {
            return None;
        }
        let scope = format!("user:{}", self.user);
        let told: BTreeMap<String, String> = self
            .store
            .pref(&scope, "told")
            .ok()
            .flatten()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let all = self.store.load_drafts(&self.branch, false).ok()?;
        let mut lines = Vec::new();
        let mut list = Vec::new();
        let mut now = told.clone();
        for d in all.iter().filter(|d| d.session == self.user) {
            let key = d.id.to_string();
            if told.get(&key) == Some(&d.state) {
                continue;
            }
            let said = match d.state.as_str() {
                "accepted" => format!("draft {} accepted", d.id),
                "rejected" if !d.note.is_empty() => format!("draft {} rejected: \"{}\"", d.id, d.note),
                "rejected" => format!("draft {} rejected", d.id),
                "stale" | "blocked" => format!("draft {} is {}: {}", d.id, d.state, d.reason),
                _ => {
                    now.insert(key, d.state.clone());
                    continue;
                }
            };
            now.insert(key, d.state.clone());
            lines.push(said);
            list.push(json!({"draft": d.id, "state": d.state, "note": d.note, "reason": d.reason, "label": d.label}));
        }
        if now != told {
            let _ = self.store.set_pref(&scope, "told", &json!(now));
        }
        (!lines.is_empty()).then(|| (format!("From the human: {}.", lines.join("; ")), json!(list)))
    }
}

/// Create a new project: a blank `main.ldr` (or an imported LDraw file).
pub fn create_project(path: &Path, title: &str, import: Option<&Path>) -> Result<()> {
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    match import {
        Some(src) => {
            let bytes = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
            let name = src.file_name().and_then(|n| n.to_str()).unwrap_or("main.ldr");
            let doc = ldraw::Document::parse(&bytes, name);
            Store::import(path, doc, HUMAN)?;
        }
        None => {
            Store::create(path, title)?;
        }
    }
    Ok(())
}
