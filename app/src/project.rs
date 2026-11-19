//! A `.sb` project open in the window: a background thread owns its own SQLite connection,
//! publishes the window's edits (debounced), writes checkpoints, keeps the window's lease and
//! watches for agent drafts and external changes. Nothing here blocks the UI thread.
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use engine::{
    drafts::{Draft, Touched},
    Id, Model,
};
use serde_json::{json, Value};
use store::{
    checkpoint::{Burst, Due, Edit},
    Commit, Store,
};

pub const BRANCH: &str = "main";
pub const AUTHOR: &str = "human";
const PUBLISH_DEBOUNCE: Duration = Duration::from_millis(300);
const LEASE_RENEW: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(250);

pub fn is_project(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("sb"))
}

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// What the project thread tells the window.
pub enum Event {
    /// Another process changed the shared model (an accept from the CLI, a restore…).
    Model { model: Model, author: String },
    /// The live drafts changed (an agent proposed, withdrew…).
    Drafts(Vec<Draft>),
    Lease { leader: bool, holder: Option<String> },
    Log(Vec<Commit>),
    Published { revision: u64 },
    Message(String),
}

enum Req {
    Publish { model: Model, revision: u64 },
    SaveDrafts(Vec<Draft>),
    Commit { kind: String, message: String },
    Pref { scope: String, key: String, value: Value },
    TakeOver,
    Restore(i64),
    Branch(String),
    Log,
    Close { unused: Option<(Id, Id)>, done: Sender<()> },
}

/// Everything needed to put a freshly opened project into the engine.
pub struct Opened {
    pub path: PathBuf,
    pub model: Model,
    pub drafts: Vec<Draft>,
    pub prefs: Vec<(String, Value)>,
}

pub fn load(path: &Path) -> Result<Opened, String> {
    let store = Store::open(path)?;
    let (_, mut model) = store.load(BRANCH)?;
    model.path = Some(path.to_path_buf());
    let drafts = store.load_drafts(BRANCH, true)?;
    let scope = format!("user:{AUTHOR}");
    let prefs = store
        .prefs(&scope)
        .unwrap_or_default()
        .into_iter()
        .collect();
    Ok(Opened { path: path.to_path_buf(), model, drafts, prefs })
}

pub struct Link {
    tx: Sender<Req>,
    rx: Receiver<Event>,
    pub leader: bool,
    pub holder: Option<String>,
    pub log: Vec<Commit>,
    /// Last engine revision handed to the publisher, and the last one it confirmed.
    pub sent: u64,
    pub published: u64,
    changed_at: Option<Instant>,
    /// Draft states as last synchronised, so window decisions are written back once.
    pub drafts_seen: Vec<(u64, String)>,
}

pub fn draft_signature(drafts: &[Draft]) -> Vec<(u64, String)> {
    drafts.iter().map(|d| (d.id, d.state.clone())).collect()
}

impl Link {
    /// Start the project thread. `revision` is the engine revision the opened model sits at.
    pub fn start(opened: &Opened, revision: u64, ctx: egui::Context) -> Result<Link, String> {
        let store = Store::open(&opened.path)?;
        let (w, model) = store.load(BRANCH)?;
        static WINDOWS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = WINDOWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let holder = format!("gui:{}:{n}", std::process::id());
        let (tx, jobs) = mpsc::channel();
        let (events, rx) = mpsc::channel();
        let h = holder.clone();
        std::thread::Builder::new()
            .name("sb-project".into())
            .spawn(move || Worker::new(store, h, w.seq, model, events, ctx).run(jobs))
            .map_err(|e| e.to_string())?;
        let link = Link {
            tx,
            rx,
            leader: true,
            holder: None,
            log: vec![],
            sent: revision,
            published: revision,
            changed_at: None,
            drafts_seen: draft_signature(&opened.drafts),
        };
        link.refresh_log();
        Ok(link)
    }
    pub fn events(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }
    /// Called every frame with the engine's state: publishes edits after a short pause.
    pub fn tick(&mut self, snapshot: &engine::Snapshot) {
        if snapshot.revision == self.sent {
            self.changed_at = None;
            return;
        }
        let at = *self.changed_at.get_or_insert_with(Instant::now);
        if at.elapsed() >= PUBLISH_DEBOUNCE {
            self.publish(snapshot);
        }
    }
    pub fn publish(&mut self, snapshot: &engine::Snapshot) {
        if !self.leader || snapshot.revision == self.sent {
            return;
        }
        self.sent = snapshot.revision;
        self.changed_at = None;
        let _ = self.tx.send(Req::Publish { model: snapshot.model.clone(), revision: snapshot.revision });
    }
    pub fn dirty(&self, snapshot: &engine::Snapshot) -> bool {
        snapshot.revision != self.published
    }
    pub fn save_drafts(&mut self, drafts: Vec<Draft>) {
        if !drafts.is_empty() {
            let _ = self.tx.send(Req::SaveDrafts(drafts));
        }
    }
    pub fn commit(&self, kind: &str, message: &str) {
        let _ = self.tx.send(Req::Commit { kind: kind.into(), message: message.into() });
        self.refresh_log();
    }
    pub fn set_pref(&self, key: &str, value: Value) {
        let _ = self.tx.send(Req::Pref { scope: format!("user:{AUTHOR}"), key: key.into(), value });
    }
    pub fn take_over(&self) {
        let _ = self.tx.send(Req::TakeOver);
    }
    pub fn restore(&self, commit: i64) {
        let _ = self.tx.send(Req::Restore(commit));
    }
    pub fn branch(&self, name: &str) {
        let _ = self.tx.send(Req::Branch(name.into()));
    }
    pub fn refresh_log(&self) {
        let _ = self.tx.send(Req::Log);
    }
    /// Flush, checkpoint, give back the lease and unused ids; waits briefly for the thread.
    pub fn close(&mut self, snapshot: &engine::Snapshot, unused: Option<(Id, Id)>) {
        self.publish(snapshot);
        let (done, wait) = mpsc::channel();
        if self.tx.send(Req::Close { unused, done }).is_ok() {
            let _ = wait.recv_timeout(Duration::from_secs(3));
        }
    }
}

struct Worker {
    store: Store,
    holder: String,
    seq: i64,
    shared: Model,
    events: Sender<Event>,
    ctx: egui::Context,
    leader: bool,
    announced: bool,
    burst: Burst,
    version: i64,
    drafts: Vec<(u64, String, usize)>,
}

impl Worker {
    fn new(store: Store, holder: String, seq: i64, shared: Model, events: Sender<Event>, ctx: egui::Context) -> Worker {
        let version = store.data_version().unwrap_or(0);
        Worker { store, holder, seq, shared, events, ctx, leader: false, announced: false, burst: Burst::default(), version, drafts: vec![] }
    }
    fn send(&self, e: Event) {
        let _ = self.events.send(e);
        self.ctx.request_repaint();
    }
    fn lease(&mut self, force: bool) {
        if force {
            if let Ok(Some(l)) = self.store.lease(BRANCH) {
                let _ = self.store.release_lease(BRANCH, &l.holder);
            }
        }
        let leader = self.store.acquire_lease(BRANCH, &self.holder).unwrap_or(false);
        if leader != self.leader || force || !self.announced {
            self.leader = leader;
            self.announced = true;
            let holder = if leader { None } else { self.store.lease(BRANCH).ok().flatten().map(|l| l.holder) };
            self.send(Event::Lease { leader, holder });
        }
    }
    fn run(mut self, jobs: Receiver<Req>) {
        self.lease(false);
        self.poll(true);
        let mut renewed = Instant::now();
        loop {
            match jobs.recv_timeout(POLL) {
                Ok(Req::Close { unused, done }) => {
                    // Drain anything queued before the close (a final publish).
                    for req in jobs.try_iter() {
                        self.handle(req);
                    }
                    if !self.burst.is_empty() {
                        self.checkpoint("close");
                    }
                    if let Some((next, end)) = unused {
                        let _ = self.store.release_ids(next, end);
                    }
                    let _ = self.store.release_lease(BRANCH, &self.holder);
                    let _ = self.store.maintain(BRANCH);
                    let _ = done.send(());
                    return;
                }
                Ok(req) => self.handle(req),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.store.release_lease(BRANCH, &self.holder);
                    return;
                }
            }
            if renewed.elapsed() >= LEASE_RENEW {
                renewed = Instant::now();
                self.lease(false);
            }
            self.poll(false);
            if let Due::Now(why) = self.burst.due(now()) {
                self.checkpoint(why);
            }
        }
    }
    fn checkpoint(&mut self, why: &str) {
        let message = format!("{} ({why})", self.burst.describe());
        match self.store.commit(BRANCH, "checkpoint", AUTHOR, &message, &[]) {
            Ok(_) => self.burst.clear(),
            Err(e) => self.send(Event::Message(format!("checkpoint failed: {e}"))),
        }
        self.log();
    }
    fn log(&self) {
        if let Ok(log) = self.store.log(BRANCH, 200) {
            self.send(Event::Log(log));
        }
    }
    fn handle(&mut self, req: Req) {
        match req {
            Req::Publish { model, revision } => {
                if !self.leader {
                    self.send(Event::Message("Read-only: another window holds this project; take over to edit".into()));
                    return;
                }
                let t = Touched::between(&self.shared, &model);
                let edit = Edit {
                    added: t.created.clone(),
                    removed: t.removed.clone(),
                    changed: t.modified.clone(),
                    files: t.new_files.len(),
                    time: now(),
                };
                if let Due::Before(why) = self.burst.check(&edit) {
                    self.checkpoint(why);
                }
                let r = match self.store.publish(BRANCH, &model, Some(self.seq), AUTHOR) {
                    Err(e) if e.starts_with("conflict") => {
                        self.send(Event::Message("An external change to the shared model was overwritten by this window's edits".into()));
                        self.store.publish(BRANCH, &model, None, AUTHOR)
                    }
                    r => r,
                };
                match r {
                    Ok(seq) => {
                        self.seq = seq;
                        self.shared = model;
                        if !edit.is_empty() {
                            self.burst.push(&edit);
                        }
                        self.send(Event::Published { revision });
                    }
                    Err(e) => self.send(Event::Message(format!("could not save to the project: {e}"))),
                }
            }
            Req::SaveDrafts(drafts) => {
                if let Err(e) = self.store.save_drafts(BRANCH, &drafts) {
                    self.send(Event::Message(format!("could not save draft decisions: {e}")));
                }
                self.drafts = self.signature();
            }
            Req::Commit { kind, message } => {
                if !self.burst.is_empty() && kind != "checkpoint" {
                    self.burst.clear();
                }
                if let Err(e) = self.store.commit(BRANCH, &kind, AUTHOR, &message, &[]) {
                    self.send(Event::Message(format!("commit failed: {e}")));
                }
                self.log();
            }
            Req::Pref { scope, key, value } => {
                let _ = self.store.set_pref(&scope, &key, &value);
            }
            Req::TakeOver => self.lease(true),
            Req::Restore(id) => match self.store.restore(BRANCH, id, AUTHOR) {
                Ok(_) => {
                    self.burst.clear();
                    self.poll(true);
                    self.log();
                }
                Err(e) => self.send(Event::Message(format!("restore failed: {e}"))),
            },
            Req::Branch(name) => match self.store.create_branch(&name, None, AUTHOR) {
                Ok(()) => self.send(Event::Message(format!("Created branch {name} from the current version"))),
                Err(e) => self.send(Event::Message(format!("branch failed: {e}"))),
            },
            Req::Log => self.log(),
            Req::Close { .. } => {}
        }
    }
    fn signature(&self) -> Vec<(u64, String, usize)> {
        self.store
            .load_drafts(BRANCH, true)
            .unwrap_or_default()
            .iter()
            .map(|d| (d.id, d.state.clone(), d.commands.len()))
            .collect()
    }
    /// Look for other processes' writes: a new shared model or changed drafts.
    fn poll(&mut self, force: bool) {
        let v = self.store.data_version().unwrap_or(self.version);
        if v == self.version && !force {
            return;
        }
        self.version = v;
        if let Ok(w) = self.store.working(BRANCH) {
            if w.seq != self.seq {
                if let Ok((w, model)) = self.store.load(BRANCH) {
                    self.seq = w.seq;
                    self.shared = model.clone();
                    self.send(Event::Model { model, author: w.author });
                }
            }
        }
        let sig = self.signature();
        if sig != self.drafts || force {
            self.drafts = sig;
            if let Ok(d) = self.store.load_drafts(BRANCH, true) {
                self.send(Event::Drafts(d));
            }
        }
    }
}

/// Window preferences that outlive the window, for plain LDraw files too.
pub mod prefs {
    use super::*;
    pub fn path() -> PathBuf {
        crate::autosave::directory().parent().map(Path::to_path_buf).unwrap_or_default().join("prefs.json")
    }
    pub fn load() -> Value {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| json!({}))
    }
    /// A model file opened recently (newest first in the list).
    #[derive(Clone, Debug)]
    pub struct Recent {
        pub path: PathBuf,
        pub opened: u64,
    }
    pub fn recent_files() -> Vec<Recent> {
        load()["recent_files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| Some(Recent { path: PathBuf::from(r["path"].as_str()?), opened: r["opened"].as_u64().unwrap_or(0) }))
            .collect()
    }
    fn store_recent(list: &[Recent]) {
        let mut all = load();
        all["recent_files"] = Value::Array(list.iter().map(|r| json!({"path": r.path, "opened": r.opened})).collect());
        save(&all);
    }
    /// Put `path` at the top of the recent list (recovery copies are not remembered).
    pub fn touch_recent(path: &Path) {
        let path = ldraw::platform::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if ldraw::platform::canonicalize(&crate::autosave::directory()).is_ok_and(|d| path.starts_with(d)) {
            return;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut list = recent_files();
        list.retain(|r| r.path != path);
        list.insert(0, Recent { path, opened: now });
        list.truncate(24);
        store_recent(&list);
    }
    pub fn forget_recent(path: &Path) {
        let mut list = recent_files();
        list.retain(|r| r.path != path);
        store_recent(&list);
    }
    pub fn save(v: &Value) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec_pretty(v).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(tmp, p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn wait_for(link: &mut Link, mut f: impl FnMut(&Event) -> bool) -> bool {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(5) {
            for e in link.events() {
                if f(&e) {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        false
    }

    #[test]
    fn window_publishes_follows_agents_and_shares_the_lease() {
        let dir = std::env::temp_dir().join(format!("sb-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.sb");
        ops::create_project(&path, "p", None).unwrap();
        let lib = Arc::new(ldraw::Library::open_default().expect("LDraw library"));

        // Window opens the project and leads.
        let opened = load(&path).unwrap();
        let mut e = engine::Engine::from_model(lib.clone(), opened.model.clone());
        e.set_id_reserver(Store::id_reserver(path.clone()));
        let mut link = Link::start(&opened, e.revision, egui::Context::default()).unwrap();
        assert!(wait_for(&mut link, |ev| matches!(ev, Event::Lease { leader: true, .. })));
        link.leader = true;

        // A window edit is published after the debounce.
        e.execute(engine::Cmd::Add {
            file: 0,
            parts: vec![engine::PartSpec { name: "3001.dat".into(), color: 4, pos: glam::DVec3::ZERO, rot: glam::DMat3::IDENTITY, id: None }],
            free: true,
        })
        .unwrap();
        let snap = e.snapshot();
        link.tick(&snap);
        std::thread::sleep(PUBLISH_DEBOUNCE + Duration::from_millis(50));
        link.tick(&snap);
        assert!(wait_for(&mut link, |ev| matches!(ev, Event::Published { .. })));
        let (_, shared) = Store::open(&path).unwrap().load(BRANCH).unwrap();
        assert_eq!(shared.parts.len(), 1);

        // An agent proposes; the window hears about it.
        let mut agent = ops::Session::open(&path, "claude", lib.clone()).unwrap();
        let r = agent.call("edit", &json!({"ops": [{"op": "add", "part": "3001", "at": [0, -24, 0], "color": 1}], "images": false}));
        assert!(r.ok, "{}", r.text);
        let mut got = vec![];
        assert!(wait_for(&mut link, |ev| match ev {
            Event::Drafts(d) => {
                got = d.clone();
                !d.is_empty()
            }
            _ => false,
        }));
        assert_eq!(got[0].session, "claude");

        // A second window follows read-only until it takes over.
        let mut second = Link::start(&load(&path).unwrap(), 0, egui::Context::default()).unwrap();
        assert!(wait_for(&mut second, |ev| matches!(ev, Event::Lease { leader: false, .. })));
        second.take_over();
        assert!(wait_for(&mut second, |ev| matches!(ev, Event::Lease { leader: true, .. })));
        assert!(wait_for(&mut link, |ev| matches!(ev, Event::Lease { leader: false, .. })));

        // Closing writes a checkpoint and gives the lease back.
        second.close(&snap, None);
        link.close(&snap, e.unused_ids());
        let st = Store::open(&path).unwrap();
        assert!(st.lease(BRANCH).unwrap().is_none());
        assert!(st.log(BRANCH, 10).unwrap().iter().any(|c| c.kind == "checkpoint"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
