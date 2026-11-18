use crate::{Args, view::View};
use eframe::{egui, egui_wgpu};
use egui_dock::{DockArea, DockState, NodeIndex, Style, TabViewer};
use engine::{Cmd, Engine, Handle, Snapshot};
use ldraw::{Document, Library};
use render::Gpu;
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tab {
    Checker,
    View(usize),
    Parts,
    Inspector,
    Drafts,
    History,
    Suppliers,
    Instructions,
    Studio,
}
/// A model read off disk, waiting to replace the engine's.
pub enum Loaded {
    Ldr(Document, PathBuf),
    Project(Box<crate::project::Opened>),
}
#[derive(Clone, Copy, PartialEq)]
pub enum Tool {
    Hide,
    Lasso,
    Hinge,
    Connect,
    Select,
    Delete,
    Paint,
    Place,
}
pub struct State {
    pub gpu: Arc<Gpu>,
    pub rs: egui_wgpu::RenderState,
    pub lib: Arc<Library>,
    pub handle: Handle,
    pub snapshot: Arc<Snapshot>,
    pub views: Vec<View>,
    pub active: usize,
    pub tool: Tool,
    pub color: i32,
    pub all_colors: bool,
    /// Colour picker order: false = LEGO/LDraw number, true = by how many parts come in the colour.
    pub colour_by_commonality: bool,
    pub colour_commonality: std::collections::HashMap<i32, usize>,
    pub show_hidden: bool,
    pub place: Option<String>,
    pub place_rot: glam::DMat3,
    pub place_assembly: Vec<engine::PartSpec>,
    pub assembly_clipboard: Vec<engine::PartSpec>,
    pub mirror_job: Option<mpsc::Receiver<Result<Option<Vec<engine::PartSpec>>, String>>>,
    pub message: String,
    pub search: Option<Arc<search::Searcher>>,
    pub search_rx: mpsc::Receiver<Result<search::Searcher, String>>,
    pub query: String,
    pub filters: search::Filters,
    pub hits: Vec<search::Hit>,
    pub similar_to: Option<String>,
    pub similar_hits: Vec<search::Hit>,
    pub search_key: String,
    pub palette: bool,
    pub command_name: String,
    pub command_json: String,
    pub sub_name: String,
    pub load_rx: Option<mpsc::Receiver<Result<Loaded, String>>>,
    /// The open `.sb` project, if any.
    pub link: Option<crate::project::Link>,
    /// Preferences as last written, to notice changes.
    pub saved_prefs: serde_json::Value,
    /// Show the start page instead of the workspace.
    pub home: bool,
    pub home_ui: crate::home::Home,
    pub pending_open: Option<PathBuf>,
    pub reject_note: String,
    pub branch_name: String,
    pub list_mode: bool,
    pub recent: Vec<String>,
    pub used: Vec<(String, usize)>,
    pub used_revision: u64,
    pub draft_selection: BTreeSet<u64>,
    pub thumbs: HashMap<String, egui::TextureHandle>,
    pub thumb_pending: BTreeSet<String>,
    pub thumb_tx: mpsc::Sender<(String, String, i32)>,
    pub thumb_rx: mpsc::Receiver<(String, Result<Vec<u8>, String>)>,
    pub report: Option<engine::checker::Report>,
    pub report_rx: Option<
        mpsc::Receiver<Result<(engine::checker::Report, Vec<ldraw::FlatPart>, u64), String>>,
    >,
    pub report_file: usize,
    pub report_revision: u64,
    pub report_flat: Vec<ldraw::FlatPart>,
    pub issue: Option<(usize, Vec<usize>, u64, u64)>,
    pub issue_serial: u64,
    /// Replaces the checker caption on the X-ray overlay (e.g. why an operation refused).
    pub issue_note: Option<String>,
    pub inspect_delta: [f64; 3],
    pub angle: f64,
    pub suppliers: crate::suppliers::Panel,
    pub instructions: crate::publication::Panel,
    pub studio: crate::publication::Panel,
}
pub struct App {
    pub dock: DockState<Tab>,
    pub st: State,
    args: Args,
    started: Instant,
    first_frame: bool,
    bench_frames: u32,
    bench_times: Vec<f64>,
    last_frame: Instant,
    closing: bool,
    recovery: crate::autosave::Recovery,
    python: crate::setup::PythonOffer,
}
#[cfg(test)]
mod copy_tests {
    use super::*;
    #[test]
    fn copy_events_ctrl_c_and_cmd_c_but_not_cut_or_plain_c_activate_copy() {
        for (mut events, modifiers, expected) in [
            (vec![egui::Event::Copy], egui::Modifiers::NONE, true),
            (vec![egui::Event::Cut], egui::Modifiers::CTRL, false),
            (
                vec![key(egui::Key::C, egui::Modifiers::CTRL)],
                egui::Modifiers::CTRL,
                true,
            ),
            (
                vec![key(egui::Key::C, egui::Modifiers::COMMAND)],
                egui::Modifiers::COMMAND,
                true,
            ),
            (
                vec![key(egui::Key::X, egui::Modifiers::CTRL)],
                egui::Modifiers::CTRL,
                false,
            ),
            (
                vec![key(egui::Key::C, egui::Modifiers::NONE)],
                egui::Modifiers::NONE,
                false,
            ),
        ] {
            let ctx = egui::Context::default();
            let mut got = false;
            events.insert(0, egui::Event::ModifiersChanged(modifiers));
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ui| {
                    got = ui.input(copy_requested);
                },
            );
            output.textures_delta.clear();
            assert_eq!(got, expected);
        }
    }
    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
}

fn copy_requested(input: &egui::InputState) -> bool {
    input
        .events
        .iter()
        .any(|event| matches!(event, egui::Event::Copy))
        || ((input.modifiers.command || input.modifiers.ctrl) && input.key_pressed(egui::Key::C))
}

#[cfg(test)]
pub(crate) fn copied_piece(snapshot: &Snapshot) -> Result<engine::PartSpec, String> {
    if snapshot.model.selection.len() != 1 {
        return Err("Select one piece to copy into the Place tool".into());
    }
    let id = snapshot.model.selection.first().unwrap();
    let piece = snapshot
        .model
        .parts
        .get(id)
        .ok_or("Selected piece no longer exists")?;
    Ok(engine::PartSpec {
        name: piece.name.clone(),
        color: piece.color,
        rot: piece.rot,
        pos: glam::DVec3::ZERO,
        id: None,
    })
}

impl State {
    pub fn copy_to_place(&mut self) {
        match self.snapshot.copied_assembly() {
            Ok(parts) => {
                self.assembly_clipboard = parts;
                self.paste_assembly();
            }
            Err(reason) => self.message = reason,
        }
    }
    pub fn paste_assembly(&mut self) {
        if let Some(first) = self.assembly_clipboard.first() {
            self.place = Some(first.name.clone());
            self.place_rot = first.rot;
            self.color = first.color;
            self.place_assembly = if self.assembly_clipboard.len() > 1 {
                self.assembly_clipboard.clone()
            } else {
                vec![]
            };
            self.tool = Tool::Place;
            self.message = format!(
                "Place copied assembly · {} pieces · Ctrl/Cmd+V repeats · Esc cancels",
                self.assembly_clipboard.len()
            );
        } else {
            self.message = "Copy a selection first".into();
        }
    }
    pub fn mirror_selection(&mut self, axis: usize) {
        if self.mirror_job.is_some() {
            return;
        }
        let pending = self.tool == Tool::Place
            && self
                .assembly_clipboard
                .first()
                .is_some_and(|p| self.place.as_deref() == Some(p.name.as_str()));
        let parts = self.assembly_clipboard.clone();
        let snapshot = self.snapshot.clone();
        let handle = self.handle.clone();
        let file = self.views[self.active].file;
        let ids = self.ids();
        let pivot = ids
            .first()
            .and_then(|id| snapshot.model.parts.get(id))
            .map(|p| p.pos)
            .unwrap_or_default();
        let (tx, rx) = mpsc::channel();
        self.mirror_job = Some(rx);
        self.message = "Verifying mirrored geometry and available left/right parts…".into();
        std::thread::spawn(move || {
            let result = if pending {
                snapshot
                    .mirror_specs(file, parts, axis, glam::DVec3::ZERO)
                    .map(Some)
            } else {
                handle
                    .call(move |e| {
                        if e.revision != snapshot.revision {
                            return Err(
                                "Build changed before mirror started; select and retry".into()
                            );
                        }
                        e.execute(Cmd::Mirror { ids, axis, pivot })
                    })
                    .map(|_| None)
            };
            let _ = tx.send(result);
        });
    }
    pub fn command(&mut self, c: Cmd) {
        let result = self.handle.command(c);
        self.value_result(result);
    }
    pub fn value_result(&mut self, result: Result<serde_json::Value, String>) {
        self.message = match result {
            Ok(v) if v["warnings"].as_array().is_some_and(|w| !w.is_empty()) => format!(
                "Warning — {}",
                v["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|w| w.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ")
            ),
            Ok(v) => v.to_string(),
            Err(e) => format!("Refused: {e}"),
        };
    }
    /// Shared X-ray: highlight flat-part `indices` of `file` in red and fly the camera to them.
    /// Used by the checker and by any tool that wants to point at the parts that made it fail.
    pub fn show_xray(&mut self, file: usize, indices: Vec<usize>, note: Option<String>) {
        if indices.is_empty() {
            return;
        }
        self.issue_serial += 1;
        self.issue_note = note;
        self.issue = Some((file, indices, self.snapshot.revision, self.issue_serial));
    }
    /// X-ray the selected parts whose part file is `name` (flat indices in the active panel).
    pub fn xray_selected_named(&mut self, file: usize, name: &str, note: String) {
        let lines: std::collections::BTreeSet<_> = self
            .ids()
            .iter()
            .filter_map(|id| self.snapshot.model.parts.get(id).map(|p| p.line))
            .collect();
        let indices: Vec<usize> = self
            .snapshot
            .model
            .flat(file)
            .iter()
            .enumerate()
            .filter(|(_, p)| lines.contains(&p.root_line) && p.name.eq_ignore_ascii_case(name))
            .map(|(i, _)| i)
            .collect();
        self.show_xray(file, indices, Some(note));
    }
    pub fn ids(&self) -> Vec<u64> {
        self.snapshot.model.selection.iter().copied().collect()
    }
    pub fn result<T>(&mut self, r: Result<T, String>) {
        self.message = match r {
            Ok(_) => "Done".into(),
            Err(e) => e,
        };
    }
    pub fn open(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.snapshot.revision != self.snapshot.saved_revision {
            let answer = rfd::MessageDialog::new()
                .set_title("Unsaved model")
                .set_description("Open another model without saving the current changes?")
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            if answer != rfd::MessageDialogResult::Yes {
                return;
            }
        }
        let (tx, rx) = mpsc::channel();
        self.load_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let loaded = if crate::project::is_project(&path) {
                crate::project::load(&path).map(|o| Loaded::Project(Box::new(o)))
            } else {
                Document::load(&path)
                    .map(|d| Loaded::Ldr(d, path))
                    .map_err(|e| e.to_string())
            };
            let _ = tx.send(loaded);
            ctx.request_repaint();
        });
        self.message = "Opening model…".into();
    }
    pub fn save(&mut self, save_as: bool) {
        if let (Some(link), false) = (self.link.as_mut(), save_as) {
            link.publish(&self.snapshot);
            link.commit("save", "saved");
            self.message = "Saved a version to the project".into();
            return;
        }
        let path = if save_as || self.snapshot.model.path.is_none() {
            let stem = self
                .snapshot
                .model
                .path
                .as_deref()
                .and_then(Path::file_stem)
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "model".into());
            let Some(path) = rfd::FileDialog::new()
                .add_filter("SuperBrick project", &["sb"])
                .add_filter("LDraw", &["ldr", "mpd"])
                .set_file_name(format!("{stem}.sb"))
                .save_file()
            else {
                return;
            };
            if crate::project::is_project(&path) {
                if path.exists() {
                    self.message = format!("{} already exists; choose a new project name", path.display());
                    return;
                }
                let name = self.snapshot.model.path.as_deref().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "untitled".into());
                match store::Store::create_with(&path, &self.snapshot.model, crate::project::AUTHOR, &format!("created from {name}")) {
                    Ok(_) => {
                        // Reopen as a project so the window follows the new file.
                        self.handle.call(|e| {
                            e.saved_revision = e.revision;
                            Ok(())
                        }).ok();
                        self.snapshot = self.handle.snapshot();
                        self.pending_open = Some(path.clone());
                        self.message = format!("Saved project {}", path.display());
                    }
                    Err(e) => self.message = format!("could not create the project: {e}"),
                }
                return;
            }
            Some(path)
        } else {
            None
        };
        let r = self.handle.call(move |e| e.save(path));
        self.result(r);
    }
    /// Write the current model as plain LDraw (the project keeps working on its own copy).
    pub fn export_ldraw(&mut self) {
        let mpd = self.snapshot.model.doc.mpd || self.snapshot.model.doc.files.len() > 1;
        let Some(path) = rfd::FileDialog::new()
            .add_filter("LDraw", &["ldr", "mpd"])
            .set_file_name(if mpd { "model.mpd" } else { "model.ldr" })
            .save_file()
        else {
            return;
        };
        self.message = match std::fs::write(&path, self.snapshot.model.doc.to_bytes()) {
            Ok(()) => format!("Exported {}", path.display()),
            Err(e) => format!("export failed: {e}"),
        };
    }
    /// Save, then show the start page (the model stays loaded behind it). A project saves
    /// a version; a changed LDraw model is written back (an untitled one asks where), and
    /// cancelling that dialog keeps the editor open.
    pub fn go_home(&mut self) {
        let project = self.link.is_some();
        if project || self.snapshot.revision != self.snapshot.saved_revision {
            self.save(false);
            self.snapshot = self.handle.snapshot();
            if !project && self.snapshot.revision != self.snapshot.saved_revision {
                return;
            }
        }
        self.home = true;
        self.message = "Ready".into();
        self.home_ui.refresh();
    }
    /// Close the open project: final publish, checkpoint, lease and unused ids given back.
    pub fn close_project(&mut self) {
        if let Some(mut link) = self.link.take() {
            let unused = self.handle.call(|e| Ok(e.unused_ids())).ok().flatten();
            self.snapshot = self.handle.snapshot();
            link.close(&self.snapshot, unused);
            let _ = self.handle.call(|e| {
                e.read_only = false;
                Ok(())
            });
        }
    }
    /// Put an opened project into the engine and start following it.
    pub fn attach_project(&mut self, o: crate::project::Opened, ctx: &egui::Context) -> Result<(), String> {
        let model = o.model.clone();
        let drafts = o.drafts.clone();
        let path = o.path.clone();
        let rev = self.handle.call(move |e| {
            let rev = e.revision + 1;
            let mut n = Engine::from_model(e.lib.clone(), model);
            n.drafts.ops = drafts;
            n.set_id_reserver(store::Store::id_reserver(path));
            n.revision = rev;
            n.saved_revision = rev;
            for p in n.model.parts.values_mut() {
                p.revision = rev;
            }
            n.refresh_drafts();
            *e = n;
            Ok(rev)
        })?;
        for (k, v) in &o.prefs {
            match k.as_str() {
                "color" => {
                    if let Some(c) = v.as_i64() {
                        self.color = c as i32;
                    }
                }
                "recent" => {
                    if let Ok(r) = serde_json::from_value::<Vec<String>>(v.clone()) {
                        self.recent = r;
                    }
                }
                _ => {}
            }
        }
        self.saved_prefs = serde_json::json!({"color": self.color, "recent": self.recent});
        self.link = Some(crate::project::Link::start(&o, rev, ctx.clone())?);
        Ok(())
    }
    /// Follow the project: apply other processes' changes, write back window decisions,
    /// publish edits after a short pause.
    pub fn project_tick(&mut self) {
        use crate::project::{draft_signature, Event, AUTHOR};
        let Some(link) = self.link.as_mut() else {
            return;
        };
        for ev in link.events() {
            match ev {
                Event::Model { model, author } => {
                    if link.dirty(&self.snapshot) {
                        self.message = format!("{author} changed the shared model while you were editing; your edits were kept");
                        continue;
                    }
                    if let Ok(rev) = self.handle.call(move |e| {
                        e.replace_model(model);
                        Ok(e.revision)
                    }) {
                        link.sent = rev;
                        link.published = rev;
                    }
                    self.message = format!("Model updated by {author}");
                }
                Event::Drafts(list) => {
                    let r = self.handle.call(move |e| {
                        let terminal = |s: &str| matches!(s, "accepted" | "rejected" | "withdrawn");
                        let mut out = Vec::new();
                        for d in list {
                            match e.drafts.ops.iter().find(|o| o.id == d.id) {
                                Some(mine) if terminal(&mine.state) && !terminal(&d.state) => out.push(mine.clone()),
                                _ => out.push(d),
                            }
                        }
                        e.drafts.ops = out;
                        e.refresh_drafts();
                        // The human's own CLI edits queue as drafts while the window leads; take them.
                        let mine: Vec<u64> = e.drafts.ops.iter().filter(|d| d.session == AUTHOR && d.state == "pending").map(|d| d.id).collect();
                        if !mine.is_empty() {
                            e.accept(AUTHOR, Some(mine.clone()))?;
                        }
                        Ok(mine.len())
                    });
                    match r {
                        Ok(0) => {}
                        Ok(n) => self.message = format!("Applied {n} edit(s) made with --user human"),
                        Err(e) => self.message = e,
                    }
                }
                Event::Lease { leader, holder } => {
                    link.leader = leader;
                    link.holder = holder.clone();
                    let ro = !leader;
                    let _ = self.handle.call(move |e| {
                        e.read_only = ro;
                        Ok(())
                    });
                    self.message = if leader {
                        "This window edits the project".into()
                    } else {
                        format!("Read-only: {} has this project open", holder.unwrap_or_else(|| "another window".into()))
                    };
                }
                Event::Log(l) => link.log = l,
                Event::Published { revision } => {
                    link.published = link.published.max(revision);
                    let _ = self.handle.call(move |e| {
                        if e.revision == revision {
                            e.saved_revision = revision;
                        }
                        Ok(())
                    });
                }
                Event::Message(m) => self.message = m,
            }
        }
        self.snapshot = self.handle.snapshot();
        let sig = draft_signature(&self.snapshot.drafts);
        if sig != link.drafts_seen {
            let changed: Vec<engine::drafts::Draft> = self
                .snapshot
                .drafts
                .iter()
                .filter(|d| !link.drafts_seen.contains(&(d.id, d.state.clone())))
                .cloned()
                .collect();
            let accepted: Vec<String> = changed.iter().filter(|d| d.state == "accepted").map(|d| format!("#{} ({})", d.id, d.session)).collect();
            link.drafts_seen = sig;
            if !accepted.is_empty() {
                link.publish(&self.snapshot);
            }
            link.save_drafts(changed);
            if !accepted.is_empty() {
                link.commit("accept", &format!("accepted {}", accepted.join(", ")));
            }
        }
        link.tick(&self.snapshot);
    }
    /// Remember the paint colour and recent pieces across launches (and in the project).
    pub fn prefs_tick(&mut self) {
        let now = serde_json::json!({"color": self.color, "recent": self.recent});
        if now == self.saved_prefs {
            return;
        }
        let mut all = crate::project::prefs::load();
        all["color"] = now["color"].clone();
        all["recent"] = now["recent"].clone();
        crate::project::prefs::save(&all);
        if let Some(l) = &self.link {
            l.set_pref("color", now["color"].clone());
            l.set_pref("recent", now["recent"].clone());
        }
        self.saved_prefs = now;
    }
    pub fn refresh_search(&mut self) {
        let key = format!(
            "{}:{:?}:{:?}:{}:{}",
            self.query,
            self.filters.color,
            self.filters.tags,
            self.filters.unknown_colors,
            self.all_colors
        );
        if key == self.search_key {
            return;
        }
        if let Some(s) = &self.search {
            let limit = if self.all_colors && self.query.trim().is_empty() {
                s.cat.parts.len()
            } else {
                200
            };
            self.hits = s.filtered(&self.query, &self.filters, limit).0;
            self.search_key = key;
        }
    }
    pub fn check(&mut self) {
        let snapshot = self.snapshot.clone();
        let file = self.views[self.active].file;
        self.report_file = file;
        let (tx, rx) = mpsc::channel();
        self.report_rx = Some(rx);
        std::thread::spawn(move || {
            let flat = snapshot.model.flat(file);
            let r = engine::checker::check(&snapshot.model, file)
                .map(|report| (report, flat, snapshot.revision))
                .map_err(|e| format!("Checker unavailable: {e}"));
            let _ = tx.send(r);
        });
        self.message = "Checking connections, collisions and stability…".into();
    }
}
impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args, started: Instant) -> Self {
        cc.egui_ctx.set_theme(egui::ThemePreference::System);
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .expect("wgpu renderer")
            .clone();
        let gpu = Arc::new(Gpu {
            device: rs.device.clone(),
            queue: rs.queue.clone(),
            info: rs.adapter.get_info(),
        });
        let lib = Arc::new(
            Library::open_default().expect("LDraw library: install LeoCAD or set LDRAW_LIBRARY"),
        );
        let handle = Handle::spawn(Engine::new(
            lib.clone(),
            Document::new("model.ldr", "Untitled"),
            None,
        ));
        let snapshot = handle.snapshot();
        // Agents work on .sb project files (`sb mcp`, `sb P.sb --user NAME …`), not on this window.
        let message = String::from("Ready");
        let (tx, search_rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let r = search::load(&search::data_dir(), true)
                .or_else(|_| search::load(&search::data_dir(), false))
                .map_err(|e| e.to_string());
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        let (thumb_tx, requests) = mpsc::channel::<(String, String, i32)>();
        let (results, thumb_rx) = mpsc::channel();
        let tg = gpu.clone();
        let tl = lib.clone();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            for (key, name, color) in requests {
                let p = ldraw::FlatPart {
                    name,
                    color: ldraw::Color(color),
                    pos: glam::DVec3::ZERO,
                    rot: glam::DMat3::IDENTITY,
                    step: 1,
                    path: String::new(),
                    src: (0, 0, 0),
                    root_line: 0,
                    ancestors: Vec::new(),
                };
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let scene = render::Scene::build(&tl, &[p], &render::SceneOptions::default());
                    let cam = render::Camera::framing(scene.bounds.0, scene.bounds.1, 1.0);
                    let renderer = render::Renderer::new(&tg, render::COLOR_FORMAT, 4, &scene);
                    let target = render::Offscreen::new(&tg, 4, 96, 96);
                    target.render(&tg, &renderer, &cam);
                    target.read_rgba(&tg)
                }))
                .map_err(|_| "thumbnail renderer failed".to_string());
                if results.send((key, r)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        let prefs = crate::project::prefs::load();
        let start_color = prefs["color"].as_i64().map(|c| c as i32).unwrap_or(4);
        let start_recent: Vec<String> = serde_json::from_value(prefs["recent"].clone()).unwrap_or_default();
        let mut st = State {
            gpu,
            rs,
            lib,
            handle,
            snapshot,
            views: vec![View::new(0)],
            active: 0,
            tool: Tool::Select,
            color: start_color,
            all_colors: false,
            colour_by_commonality: false,
            colour_commonality: Default::default(),
            show_hidden: false,
            place: None,
            place_rot: glam::DMat3::IDENTITY,
            place_assembly: vec![],
            assembly_clipboard: vec![],
            mirror_job: None,
            message,
            search: None,
            search_rx,
            query: String::new(),
            filters: search::Filters {
                color: Some(start_color),
                ..Default::default()
            },
            hits: vec![],
            similar_to: None,
            similar_hits: vec![],
            search_key: String::new(),
            palette: false,
            command_name: "move".into(),
            command_json: "{\"ids\": [], \"delta\": [20, 0, 0]}".into(),
            sub_name: "subbuild".into(),
            load_rx: None,
            link: None,
            saved_prefs: serde_json::json!({"color": start_color, "recent": start_recent}),
            pending_open: None,
            reject_note: String::new(),
            branch_name: String::new(),
            list_mode: false,
            recent: start_recent,
            used: vec![],
            used_revision: u64::MAX,
            suppliers: Default::default(),
            instructions: Default::default(),
            studio: Default::default(),
            draft_selection: BTreeSet::new(),
            thumbs: HashMap::new(),
            thumb_pending: BTreeSet::new(),
            thumb_tx,
            thumb_rx,
            report: None,
            report_rx: None,
            report_file: 0,
            report_revision: 0,
            report_flat: vec![],
            issue: None,
            issue_serial: 0,
            issue_note: None,
            inspect_delta: [0.0; 3],
            angle: 90.0,
            home: args.file.is_none(),
            home_ui: Default::default(),
        };
        crate::open_files::set_context(&cc.egui_ctx);
        crate::open_files::reinstall();
        // Launched by opening a file in Finder: the request arrives as an event, not an argument.
        let requested = args.file.clone().or_else(|| crate::open_files::take().pop());
        if let Some(path) = requested {
            st.home = false;
            st.open(path, &cc.egui_ctx);
        }
        let mut dock = DockState::new(vec![Tab::View(0)]);
        let [center, _] =
            dock.main_surface_mut()
                .split_left(NodeIndex::root(), 0.22, vec![Tab::Parts]);
        dock.main_surface_mut().split_right(
            center,
            0.75,
            vec![
                Tab::Inspector,
                Tab::Checker,
                Tab::Drafts,
                Tab::Suppliers,
                Tab::Instructions,
                Tab::Studio,
            ],
        );
        if args.suppliers {
            if let Some(path) = dock.find_tab(&Tab::Suppliers) {
                let _ = dock.set_active_tab(path);
            }
        }
        for (enabled, tab) in [
            (args.instructions, Tab::Instructions),
            (args.studio, Tab::Studio),
        ] {
            if enabled {
                if let Some(path) = dock.find_tab(&tab) {
                    let _ = dock.set_active_tab(path);
                }
            }
        }
        Self {
            dock,
            st,
            args,
            started,
            first_frame: false,
            bench_frames: 0,
            bench_times: vec![],
            last_frame: Instant::now(),
            closing: false,
            recovery: Default::default(),
            python: Default::default(),
        }
    }
    fn tick(&mut self, ctx: &egui::Context) {
        if let Some(result) = self
            .st
            .mirror_job
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.st.mirror_job = None;
            match result {
                Ok(Some(parts)) => {
                    self.st.assembly_clipboard = parts;
                    self.st.paste_assembly();
                }
                Ok(None) => {
                    self.st.message =
                        "Mirror verified and applied; Undo restores the original".into()
                }
                Err(e) => {
                    // Errors read "<part>.dat: reason"; X-ray every selected copy of that part.
                    if let Some((name, _)) = e.split_once(": ") {
                        if name.ends_with(".dat") {
                            let file = self.st.views[self.st.active].file;
                            self.st.xray_selected_named(file, name, format!("Mirror refused · {e}"));
                        }
                    }
                    self.st.message = format!("Mirror refused: {e}")
                }
            }
        }
        self.st.snapshot = self.st.handle.snapshot();
        if let Some(rx) = &self.st.load_rx {
            if let Ok(r) = rx.try_recv() {
                self.st.load_rx = None;
                match r {
                    Ok(loaded) => {
                        self.st.close_project();
                        let opened_path = match &loaded {
                            Loaded::Ldr(_, p) => p.clone(),
                            Loaded::Project(o) => o.path.clone(),
                        };
                        let result = match loaded {
                            Loaded::Ldr(doc, path) => self.st.handle.call(move |e| {
                                let rev = e.revision + 1;
                                *e = Engine::new(e.lib.clone(), doc, Some(path));
                                e.revision = rev;
                                e.saved_revision = rev;
                                for p in e.model.parts.values_mut() {
                                    p.revision = rev;
                                }
                                Ok(())
                            }),
                            Loaded::Project(o) => self.st.attach_project(*o, ctx),
                        };
                        if result.is_ok() {
                            crate::project::prefs::touch_recent(&opened_path);
                            self.st.home_ui.refresh();
                            self.st.home = false;
                        }
                        self.st.result(result);
                        self.st.snapshot = self.st.handle.snapshot();
                        self.st.views = vec![View::new(
                            self.st.snapshot.model.doc.main_index().unwrap_or(0),
                        )];
                        self.st.active = 0;
                        self.st.draft_selection.clear();
                        if let Some(name) = self.args.submodel.take() {
                            if let Some(file) = self.st.snapshot.model.doc.file_index(&name) {
                                self.st.views[0].open_submodel = Some(file);
                            } else {
                                self.st.message = format!("Submodel not found: {name}");
                            }
                        }
                        if !self.args.select.is_empty() {
                            let ids = std::mem::take(&mut self.args.select);
                            self.st.command(engine::Cmd::Select { ids });
                        }
                        // Closed submodel tabs must not outlive the old document.
                        self.dock = DockState::new(vec![Tab::View(0)]);
                        let [c, _] = self.dock.main_surface_mut().split_left(
                            NodeIndex::root(),
                            0.22,
                            vec![Tab::Parts],
                        );
                        self.dock.main_surface_mut().split_right(
                            c,
                            0.75,
                            vec![
                                Tab::Inspector,
                                Tab::Checker,
                                Tab::Drafts,
                                Tab::History,
                                Tab::Suppliers,
                                Tab::Instructions,
                                Tab::Studio,
                            ],
                        );
                        if self.args.suppliers {
                            if let Some(path) = self.dock.find_tab(&Tab::Suppliers) {
                                let _ = self.dock.set_active_tab(path);
                            }
                        }
                        for (enabled, tab) in [
                            (self.args.instructions, Tab::Instructions),
                            (self.args.studio, Tab::Studio),
                        ] {
                            if enabled {
                                if let Some(path) = self.dock.find_tab(&tab) {
                                    let _ = self.dock.set_active_tab(path);
                                }
                            }
                        }
                        self.first_frame = false;
                        if self.args.check {
                            self.args.check = false;
                            self.st.check();
                        }
                    }
                    Err(e) => self.st.message = e,
                }
            }
        }
        if let Ok(r) = self.st.search_rx.try_recv() {
            match r {
                Ok(s) => {
                    self.st.search = Some(Arc::new(s));
                    self.st.refresh_search();
                }
                Err(e) => self.st.message = format!("Search unavailable: {e}"),
            }
        }
        while let Ok((key, result)) = self.st.thumb_rx.try_recv() {
            if let Ok(pixels) = result {
                self.st.thumbs.insert(
                    key.clone(),
                    ctx.load_texture(
                        &key,
                        egui::ColorImage::from_rgba_unmultiplied([96, 96], &pixels),
                        Default::default(),
                    ),
                );
            }
        }
        if let Some(rx) = &self.st.report_rx {
            if let Ok(r) = rx.try_recv() {
                self.st.report_rx = None;
                match r {
                    Ok((r, flat, revision)) => {
                        self.st.message = format!(
                            "{} — {} pieces, {} with errors, {} with warnings",
                            r.verdict_words(),
                            r.pieces,
                            r.errors,
                            r.warnings
                        );
                        // Point at the first problem: a collision/conflict pair, else a floating piece.
                        let indices = r
                            .hits()
                            .next()
                            .map(|(a, b, _)| vec![a, b])
                            .or_else(|| {
                                r.rows.iter().find(|row| row.has("FLOATING")).map(|row| vec![row.index()])
                            });
                        self.st.report = Some(r);
                        self.st.report_flat = flat;
                        self.st.report_revision = revision;
                        if let Some(indices) = indices {
                            self.st.issue_note = None;
                            self.st.issue_serial += 1;
                            self.st.issue = Some((self.st.report_file, indices, revision, self.st.issue_serial));
                        }
                        if let Some(path) = self.dock.find_tab(&Tab::Checker) {
                            let _ = self.dock.set_active_tab(path);
                        } else {
                            self.dock.push_to_focused_leaf(Tab::Checker);
                        }
                    }
                    Err(e) => self.st.message = e,
                }
            }
        }
        self.st.project_tick();
        self.st.prefs_tick();
        if let Some(p) = self.st.pending_open.take() {
            self.st.open(p, ctx);
        }
        // Snapshot polling continues when minimized; the engine itself never depends on repaint.
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal_wrapped(|ui| {
            if crate::brand::toolbar(ui).on_hover_text("Save and go to the start page").clicked() {
                self.st.go_home();
            }
            ui.add_space(6.0);
            let shortcut = |m: egui::Modifiers, k: egui::Key| ctx.format_shortcut(&egui::KeyboardShortcut::new(m, k));
            ui.menu_button("File", |ui| {
                ui.set_min_width(210.0);
                if ui.button("New project…").clicked() {
                    ui.close();
                    self.st.new_project_dialog(&ctx);
                }
                if ui.button("Open…").clicked() {
                    ui.close();
                    self.st.open_dialog(&ctx);
                }
                ui.menu_button("Open recent", |ui| {
                    let recent = crate::project::prefs::recent_files();
                    if recent.is_empty() {
                        ui.weak("Nothing yet");
                    }
                    for r in recent.iter().take(12) {
                        let name = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        let b = ui.add_enabled(r.path.exists(), egui::Button::new(name)).on_hover_text(r.path.display().to_string());
                        if b.clicked() {
                            ui.close();
                            self.st.open(r.path.clone(), &ctx);
                        }
                    }
                });
                ui.separator();
                if ui.add(egui::Button::new("Save").shortcut_text(shortcut(egui::Modifiers::COMMAND, egui::Key::S))).clicked() {
                    ui.close();
                    self.st.save(false);
                }
                if ui
                    .add(egui::Button::new("Save as…").shortcut_text(shortcut(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::S)))
                    .clicked()
                {
                    ui.close();
                    self.st.save(true);
                }
                if ui.button("Export LDraw…").clicked() {
                    ui.close();
                    self.st.export_ldraw();
                }
                if ui.button("Install Python components…").on_hover_text("Needed by Connect, Hinge, instructions and studio renders").clicked() {
                    ui.close();
                    self.python.ask();
                }
                ui.separator();
                if ui.button("Recover…").on_hover_text("Open a recovery copy; use Save as to restore your project").clicked() {
                    ui.close();
                    self.st.recover_dialog(&ctx);
                }
            });
            ui.separator();
            let undo = shortcut(egui::Modifiers::COMMAND, egui::Key::Z);
            if crate::home::icon_button(ui, crate::home::Icon::Undo, &undo).clicked() {
                let r = self.st.handle.call(|e| e.undo(false));
                self.st.result(r);
            }
            let redo = shortcut(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::Z);
            if crate::home::icon_button(ui, crate::home::Icon::Redo, &redo).clicked() {
                let r = self.st.handle.call(|e| e.undo(true));
                self.st.result(r);
            }
            ui.separator();
            for (tool, label) in [
                (Tool::Select, "Select · S"),
                (Tool::Lasso, "Lasso · L"),
                (Tool::Hinge, "Hinge · H"),
                (Tool::Connect, "Connect · C"),
                (Tool::Hide, "Hide · V"),
                (Tool::Delete, "Delete · D"),
                (Tool::Paint, "Paint · P"),
                (Tool::Place, "Place"),
            ] {
                ui.selectable_value(&mut self.st.tool, tool, label);
            }
            ui.separator();
            if ui.button("Check").clicked() {
                self.st.check();
            }
            if ui.button("Parts list / suppliers").clicked() {
                if let Some(path) = self.dock.find_tab(&Tab::Suppliers) {
                    let _ = self.dock.set_active_tab(path);
                } else {
                    self.dock.push_to_focused_leaf(Tab::Suppliers);
                }
            }
            if self.st.tool == Tool::Hide {
                ui.menu_button("Hide options", |ui| {
                    ui.checkbox(&mut self.st.show_hidden, "Show hidden pieces");
                    let hidden: Vec<_> = self
                        .st
                        .snapshot
                        .model
                        .parts
                        .values()
                        .filter(|p| p.hidden)
                        .map(|p| p.id)
                        .collect();
                    let label = format!("Show all ({} hidden)", hidden.len());
                    if ui
                        .add_enabled(!hidden.is_empty(), egui::Button::new(label))
                        .on_hover_text("Unhide every hidden piece in the project (one undo step)")
                        .clicked()
                    {
                        self.st.command(Cmd::Hide {
                            ids: hidden,
                            value: false,
                        });
                        ui.close();
                    }
                    ui.label("Click a piece to hide/unhide it. Hidden pieces still collide.");
                });
            }
            if ui.button(if cfg!(target_os = "macos") { "Commands ⌘K" } else { "Commands Ctrl+K" }).clicked() {
                self.st.palette = true;
            }
            ui.menu_button("Panels", |ui| {
                for (tab, label) in [
                    (Tab::Parts, "Parts"),
                    (Tab::Inspector, "Inspector"),
                    (Tab::Checker, "Checker"),
                    (Tab::Drafts, "Agents"),
                    (Tab::History, "History"),
                    (Tab::Suppliers, "Suppliers / parts list"),
                    (Tab::Instructions, "Instructions"),
                    (Tab::Studio, "Studio preview"),
                ] {
                    if ui.button(label).clicked() {
                        if let Some(path) = self.dock.find_tab(&tab) {
                            let _ = self.dock.set_active_tab(path);
                        } else {
                            self.dock.push_to_focused_leaf(tab);
                        }
                        ui.close();
                    }
                }
            });
            if self.st.tool == Tool::Lasso {
                ui.menu_button("Lasso settings", |ui| {
                    let key = egui::Id::new("lasso-tolerance");
                    let boundary = egui::Id::new("lasso-joints");
                    let mut tolerance = ui.data_mut(|d| d.get_persisted::<f32>(key).unwrap_or(8.));
                    let mut stop =
                        ui.data_mut(|d| d.get_persisted::<bool>(boundary).unwrap_or(true));
                    ui.add(
                        egui::Slider::new(&mut tolerance, 0.0..=40.0).text("Edge tolerance · px"),
                    );
                    ui.checkbox(&mut stop, "Stop at hinges / movable joints");
                    ui.data_mut(|d| {
                        d.insert_persisted(key, tolerance);
                        d.insert_persisted(boundary, stop);
                    });
                });
            }
            if self.st.snapshot.model.selection.len() > 1 && ui.button("Make subbuild…").clicked()
            {
                let ids = self.st.ids();
                let name = format!("subbuild-{}.ldr", self.st.snapshot.revision);
                self.st.command(Cmd::MakeSubbuild { ids, name });
            }
            ui.menu_button("Theme", |ui| {
                egui::widgets::global_theme_preference_buttons(ui);
            });
        });
    }
    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Key;
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        if ctx.input(copy_requested) {
            self.st.copy_to_place();
            return;
        }
        if ctx.input(|i| {
            ((i.modifiers.command || i.modifiers.ctrl) && i.key_pressed(Key::V))
                || i.events.iter().any(|e| matches!(e, egui::Event::Paste(_)))
        }) {
            self.st.paste_assembly();
            return;
        }
        let cmd = ctx.input(|i| i.modifiers.command);
        if cmd {
            if ctx.input(|i| i.key_pressed(Key::K)) {
                self.st.palette = true;
            }
            if ctx.input(|i| i.key_pressed(Key::S)) {
                self.st.save(ctx.input(|i| i.modifiers.shift));
            }
            if ctx.input(|i| i.key_pressed(Key::Z)) {
                let redo = ctx.input(|i| i.modifiers.shift);
                let r = self.st.handle.call(move |e| e.undo(redo));
                self.st.result(r);
            }
            if ctx.input(|i| i.key_pressed(Key::D)) {
                self.st.command(Cmd::Copy {
                    ids: self.st.ids(),
                    delta: glam::DVec3::X * 20.0,
                    count: 1,
                });
            }
            return;
        }
        for (key, tool) in [
            (Key::S, Tool::Select),
            (Key::L, Tool::Lasso),
            (Key::H, Tool::Hinge),
            (Key::C, Tool::Connect),
            (Key::D, Tool::Delete),
            (Key::P, Tool::Paint),
            (Key::V, Tool::Hide),
        ] {
            if ctx.input(|i| i.key_pressed(key)) {
                self.st.tool = tool;
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.st.place = None;
            self.st.tool = Tool::Select;
        }
        if ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
            self.st.command(Cmd::Remove { ids: self.st.ids() });
        }
        let ids = self.st.ids();
        if ids.is_empty() {
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::R)) {
            let pivot = self.st.snapshot.model.parts[&ids[0]].pos;
            self.st.command(Cmd::Rotate {
                ids: ids.clone(),
                axis: glam::DVec3::NEG_Y,
                angle: 90.0,
                pivot,
                free: false,
            });
        }
        if self.st.tool == Tool::Hinge || self.st.views.iter().any(|v| v.is_placing()) {
            return;
        }
        let camera = self
            .st
            .views
            .get(self.st.active)
            .and_then(|v| v.loaded.as_ref())
            .map(|l| l.cam);
        if let Some(cam) = camera {
            let forward = (cam.target - cam.eye()).normalize();
            let right = forward.cross(glam::Vec3::NEG_Y).normalize();
            let horizontal = if right.x.abs() > right.z.abs() {
                glam::DVec3::X * right.x.signum() as f64
            } else {
                glam::DVec3::Z * right.z.signum() as f64
            };
            let depth = glam::DVec3::NEG_Y.cross(horizontal);
            for (key, v) in [
                (Key::ArrowLeft, -horizontal),
                (Key::ArrowRight, horizontal),
                (Key::ArrowUp, depth),
                (Key::ArrowDown, -depth),
            ] {
                if ctx.input(|i| i.key_pressed(key)) {
                    if ctx.input(|i| i.modifiers.shift) {
                        self.st.command(Cmd::Nudge {
                            ids: ids.clone(),
                            direction: v * 20.0,
                            normal: false,
                            alt: match key {
                                Key::ArrowUp => Some(glam::DVec3::NEG_Y * 20.0),
                                Key::ArrowDown => Some(glam::DVec3::Y * 20.0),
                                _ => None,
                            },
                        });
                    } else {
                        let pivot = ids
                            .iter()
                            .map(|id| self.st.snapshot.model.parts[id].pos)
                            .sum::<glam::DVec3>()
                            / ids.len() as f64;
                        let axis = if matches!(key, Key::ArrowLeft | Key::ArrowRight) {
                            glam::DVec3::NEG_Y
                        } else {
                            horizontal
                        };
                        let sign = if matches!(key, Key::ArrowLeft | Key::ArrowDown) {
                            -1.0
                        } else {
                            1.0
                        };
                        self.st.command(Cmd::Rotate {
                            ids: ids.clone(),
                            axis,
                            angle: sign * 90.0,
                            pivot,
                            free: false,
                        });
                    }
                }
            }
        }
    }
}
impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);
        if let Some(message) = self.python.show(&ctx, &self.st.message) {
            self.st.message = message;
        }
        if self.st.link.is_none() {
            if let Some(message) = self.recovery.tick(&self.st.snapshot) {
                self.st.message = message;
            }
        }
        if !self.st.home {
            self.shortcuts(&ctx);
        }
        if let Some(path) = crate::open_files::take().pop() {
            self.st.open(path, &ctx);
        }
        if self.args.window_shot.is_some() {
            let image = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| {
                    if let egui::Event::Screenshot { image, .. } = e {
                        Some(image.clone())
                    } else {
                        None
                    }
                })
            });
            if let Some(image) = image {
                let path = self.args.window_shot.take().unwrap();
                let pixels: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
                match render::save_png(&path, image.size[0] as u32, image.size[1] as u32, &pixels) {
                    Ok(()) => eprintln!("window screenshot: {}", path.display()),
                    Err(e) => eprintln!("window screenshot failed: {e}"),
                };
                self.closing = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(p) = dropped {
            self.st.open(p, &ctx);
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.st.link.is_some() {
            // Projects save continuously: flush, checkpoint and let go.
            self.st.close_project();
            self.closing = true;
        }
        if ctx.input(|i| i.viewport().close_requested())
            && self.st.snapshot.revision != self.st.snapshot.saved_revision
            && !self.closing
        {
            let r = rfd::MessageDialog::new()
                .set_title("Unsaved model")
                .set_description("Close without saving changes?")
                .set_buttons(rfd::MessageButtons::YesNo)
                .show();
            if r != rfd::MessageDialogResult::Yes {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            } else {
                self.closing = true;
            }
        }
        // The start page is its own screen: no editor toolbar or status bar.
        if !self.st.home {
            egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
            if let Some(link) = &self.st.link {
                if !link.leader {
                    let holder = link.holder.clone().unwrap_or_else(|| "another window".into());
                    egui::Panel::top("readonly").show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.colored_label(egui::Color32::from_rgb(200, 120, 0), format!("Read-only: {holder} has this project open."));
                            if ui.button("Take over").clicked() {
                                link.take_over();
                            }
                        });
                    });
                }
            }
            egui::Panel::bottom("status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    let path = self
                        .st
                        .snapshot
                        .model
                        .path
                        .as_deref()
                        .and_then(Path::file_name)
                        .unwrap_or_default()
                        .to_string_lossy();
                    ui.label(format!(
                        "{path}{}",
                        if self.st.snapshot.revision != self.st.snapshot.saved_revision {
                            " *"
                        } else {
                            ""
                        }
                    ));
                    ui.separator();
                    ui.label(format!(
                        "{} source pieces · {} selected",
                        self.st.snapshot.model.parts.len(),
                        self.st.snapshot.model.selection.len()
                    ));
                    ui.separator();
                    ui.label(&self.st.message);
                });
            });
        }
        if self.st.home {
            let current = self.st.snapshot.model.path.as_deref().and_then(Path::file_stem).map(|s| s.to_string_lossy().into_owned());
            let lib = self.st.lib.clone();
            let notice = if self.st.message == "Ready" { String::new() } else { self.st.message.clone() };
            let action = egui::CentralPanel::default().show(ui, |ui| crate::home::ui(&mut self.st.home_ui, &lib, current, &notice, ui)).inner;
            match action {
                Some(crate::home::Action::New) => self.st.new_project_dialog(&ctx),
                Some(crate::home::Action::Open) => self.st.open_dialog(&ctx),
                Some(crate::home::Action::OpenPath(p)) => self.st.open(p, &ctx),
                Some(crate::home::Action::Recover) => self.st.recover_dialog(&ctx),
                Some(crate::home::Action::Back) => self.st.home = false,
                None => {}
            }
        } else {
            egui::CentralPanel::default().show(ui, |ui| {
                DockArea::new(&mut self.dock)
                    .style(Style::from_egui(ui.style()))
                    .show_leaf_collapse_buttons(false)
                    .show_inside(ui, &mut Viewer { st: &mut self.st });
            });
        }
        crate::panels::palette(&mut self.st, &ctx);
        if let Some(file) = self
            .st
            .views
            .get(self.st.active)
            .and_then(|v| v.open_submodel)
        {
            self.st.views[self.st.active].open_submodel = None;
            let i = if let Some(i) = self.st.views.iter().position(|v| v.file == file) {
                i
            } else {
                let i = self.st.views.len();
                self.st.views.push(View::new(file));
                i
            };
            if self.dock.find_tab(&Tab::View(i)).is_none() {
                if let Some(path) = self.dock.find_tab(&Tab::View(0)) {
                    self.dock
                        .main_surface_mut()
                        .split_below(path.node, 0.5, vec![Tab::View(i)]);
                } else {
                    self.dock.push_to_focused_leaf(Tab::View(i));
                }
            }
        }
        if self.st.load_rx.is_none() && (self.st.views[0].loaded.is_some() || self.st.home) {
            if !self.first_frame {
                eprintln!(
                    "viewer first scene: {:.1} ms",
                    self.started.elapsed().as_secs_f64() * 1000.0
                );
                self.first_frame = true;
            }
            if let Some(frames) = self.args.bench {
                self.bench_frames += 1;
                self.bench_times
                    .push(self.last_frame.elapsed().as_secs_f64() * 1000.0);
                if let Some(l) = self.st.views[0].loaded.as_mut() {
                    l.cam.yaw += 0.015;
                    self.st.views[0].dirty = true;
                }
                ctx.request_repaint();
                if self.bench_frames >= frames && !self.closing {
                    self.bench_times.sort_by(f64::total_cmp);
                    eprintln!(
                        "viewer orbit: p50 {:.2} ms p95 {:.2} ms ({} frames)",
                        self.bench_times[self.bench_times.len() / 2],
                        self.bench_times[(self.bench_times.len() - 1) * 95 / 100],
                        self.bench_times.len()
                    );
                    if let Some(path) = self.args.shot.take() {
                        self.st.views[0].screenshot(&self.st.gpu, &path);
                    }
                    self.closing = true;
                    if self.args.window_shot.is_some() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                            egui::UserData::default(),
                        ));
                    } else {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
        self.last_frame = Instant::now();
    }
}
struct Viewer<'a> {
    st: &'a mut State,
}
impl TabViewer for Viewer<'_> {
    type Tab = Tab;
    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }
    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match *tab {
            Tab::View(i) => self
                .st
                .snapshot
                .model
                .doc
                .files
                .get(self.st.views[i].file)
                .map(|f| f.name.clone())
                .unwrap_or("Build".into())
                .into(),
            Tab::Parts => "Parts".into(),
            Tab::Inspector => "Inspector".into(),
            Tab::Checker => "Checker".into(),
            Tab::Drafts => {
                let n = self.st.snapshot.drafts.iter().filter(|d| d.state == "pending").count();
                if n > 0 { format!("Agents ({n})").into() } else { "Agents".into() }
            }
            Tab::History => "History".into(),
            Tab::Suppliers => "Suppliers".into(),
            Tab::Instructions => "Instructions".into(),
            Tab::Studio => "Studio preview".into(),
        }
    }
    fn is_closeable(&self, tab: &Tab) -> bool {
        *tab != Tab::View(0)
    }
    fn scroll_bars(&self, _: &Tab) -> [bool; 2] {
        [false, false]
    }
    fn clear_background(&self, tab: &Tab) -> bool {
        !matches!(tab, Tab::View(_))
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match *tab {
            Tab::View(i) => crate::view::ui(self.st, i, ui),
            Tab::Parts => crate::panels::parts(self.st, ui),
            Tab::Inspector => crate::panels::inspector(self.st, ui),
            Tab::Checker => crate::panels::checker(self.st, ui),
            Tab::Drafts => crate::panels::drafts(self.st, ui),
            Tab::History => crate::panels::history(self.st, ui),
            Tab::Suppliers => crate::suppliers::ui(self.st, ui),
            Tab::Instructions => {
                crate::publication::ui(self.st, ui, crate::publication::Kind::Instructions)
            }
            Tab::Studio => crate::publication::ui(self.st, ui, crate::publication::Kind::Studio),
        }
    }
}
