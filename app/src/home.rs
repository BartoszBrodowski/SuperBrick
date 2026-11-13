//! The start page (logo, new/open, recent projects with thumbnails), the File-menu
//! dialogs shared with the toolbar, and the painted toolbar icons.
use crate::app::State;
use crate::project::prefs::{self, Recent};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use ldraw::Library;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What the start page asks the app to do.
pub enum Action {
    New,
    Open,
    OpenPath(PathBuf),
    Recover,
    Back,
}

type Key = (PathBuf, u64);

#[derive(Default)]
pub struct Home {
    recents: Vec<Recent>,
    loaded: Option<Instant>,
    thumbs: HashMap<Key, egui::TextureHandle>,
    requested: HashSet<Key>,
    rx: Option<Receiver<(Key, Option<egui::ColorImage>)>>,
}

impl Home {
    /// Re-read the recent list (after opening or forgetting a file).
    pub fn refresh(&mut self) {
        self.loaded = None;
    }
    fn recents(&mut self) -> &[Recent] {
        if self.loaded.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
            self.recents = prefs::recent_files();
            self.loaded = Some(Instant::now());
        }
        &self.recents
    }
    fn poll(&mut self, ctx: &egui::Context, lib: &Arc<Library>) {
        if let Some(rx) = &self.rx {
            let mut done = false;
            loop {
                match rx.try_recv() {
                    Ok((key, Some(img))) => {
                        let tex = ctx.load_texture(format!("thumb:{}", key.0.display()), img, egui::TextureOptions::LINEAR);
                        self.thumbs.insert(key, tex);
                    }
                    Ok((_, None)) => {}
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
            if done {
                self.rx = None;
            }
        }
        if self.rx.is_none() {
            let want: Vec<Key> = self
                .recents
                .iter()
                .filter_map(|r| key(&r.path))
                .filter(|k| !self.requested.contains(k))
                .collect();
            if !want.is_empty() {
                self.requested.extend(want.iter().cloned());
                self.rx = Some(spawn_thumbs(lib.clone(), want, ctx.clone()));
            }
        }
    }
}

fn key(path: &Path) -> Option<Key> {
    let m = std::fs::metadata(path).ok()?.modified().ok()?;
    Some((path.to_path_buf(), m.duration_since(UNIX_EPOCH).ok()?.as_secs()))
}

fn thumbs_dir() -> PathBuf {
    crate::autosave::directory().parent().map(Path::to_path_buf).unwrap_or_default().join("thumbs")
}

fn spawn_thumbs(lib: Arc<Library>, keys: Vec<Key>, ctx: egui::Context) -> Receiver<(Key, Option<egui::ColorImage>)> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new().name("sb-thumbs".into()).spawn(move || {
        for k in keys {
            let img = thumbnail(&lib, &k).map_err(|e| eprintln!("thumbnail {}: {e}", k.0.display())).ok();
            if tx.send((k, img)).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    });
    rx
}

/// A cached (or freshly rendered) picture of a model file.
fn thumbnail(lib: &Library, (path, mtime): &Key) -> Result<egui::ColorImage, String> {
    let dir = thumbs_dir();
    let stem = format!("{:016x}", ldraw::num::fnv1a64(path.to_string_lossy().as_bytes()));
    let cache = dir.join(format!("{stem}-{mtime}.png"));
    let png = match std::fs::read(&cache) {
        Ok(b) => b,
        Err(_) => {
            let model = load_model(path)?;
            let file = model.doc.main_index().unwrap_or(0);
            let mut shot = ops::shots::Shot::new(file, "");
            shot.whole = true;
            shot.bare = true;
            shot.intent = ops::planner::Intent::Whole;
            shot.views = ops::shots::Views::Auto(1);
            shot.size = (440, 330);
            let pic = ops::shots::render(lib, &model, &shot, None)?;
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    if e.file_name().to_string_lossy().starts_with(&stem) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
            let _ = std::fs::write(&cache, &pic.png);
            pic.png
        }
    };
    decode(&png)
}

fn load_model(path: &Path) -> Result<engine::Model, String> {
    if crate::project::is_project(path) {
        let store = store::Store::open(path)?;
        Ok(store.load(crate::project::BRANCH)?.1)
    } else {
        let doc = ldraw::Document::load(path).map_err(|e| e.to_string())?;
        Ok(engine::Model::from_document(doc, Some(path.to_path_buf())))
    }
}

pub(crate) fn decode(png_bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png_bytes)).read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("png too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..w * h * 4].to_vec(),
        png::ColorType::Rgb => buf[..w * h * 3].chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        other => return Err(format!("unexpected png colour type {other:?}")),
    };
    Ok(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba))
}

fn ago(secs: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(secs);
    let d = now.saturating_sub(secs);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=172_799 => "yesterday".into(),
        172_800..=604_799 => format!("{} days ago", d / 86_400),
        604_800..=1_209_599 => "1 week ago".into(),
        _ => format!("{} weeks ago", d / 604_800),
    }
}

fn tilde(path: &Path) -> String {
    let s = path.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

/// The start page. `current` names the model that is open behind it, if any.
pub fn ui(home: &mut Home, lib: &Arc<Library>, current: Option<String>, notice: &str, ui: &mut egui::Ui) -> Option<Action> {
    let mut action = None;
    let recents = home.recents().to_vec();
    home.poll(ui.ctx(), lib);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let width = ui.available_width().min(1180.0);
        let margin = ((ui.available_width() - width) * 0.5).max(16.0);
        ui.add_space(36.0);
        ui.horizontal(|ui| {
            ui.add_space(margin);
            ui.vertical(|ui| {
                ui.set_width(width);
                crate::brand::wordmark(ui, 72.0);
                ui.add_space(4.0);
                ui.label(egui::RichText::new("Build, check and share brick models — with AI agents that propose, and you decide.").weak());
                ui.add_space(22.0);
                ui.horizontal(|ui| {
                    let big = |t: &str| egui::Button::new(egui::RichText::new(t).size(15.0)).min_size(Vec2::new(150.0, 36.0));
                    if ui.add(big("New project…")).on_hover_text("Create a .sb project file").clicked() {
                        action = Some(Action::New);
                    }
                    if ui.add(big("Open…")).on_hover_text("Open a .sb project or an LDraw model").clicked() {
                        action = Some(Action::Open);
                    }
                    let back = match &current {
                        Some(name) => format!("Back to {name}"),
                        None => "Empty model".into(),
                    };
                    if ui.add(big(&back)).clicked() {
                        action = Some(Action::Back);
                    }
                    if ui.add(big("Recover…")).on_hover_text("Open an autosaved recovery copy").clicked() {
                        action = Some(Action::Recover);
                    }
                });
                if !notice.is_empty() {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(notice).color(ui.visuals().warn_fg_color));
                }
                ui.add_space(30.0);
                ui.label(egui::RichText::new("Recent").size(18.0).strong());
                ui.add_space(8.0);
                if recents.is_empty() {
                    ui.label(egui::RichText::new("Projects and models you open will show up here.").weak());
                }
                let card = Vec2::new(236.0, 244.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(14.0, 14.0);
                    for r in &recents {
                        if let Some(a) = recent_card(ui, home, r, card) {
                            action = Some(a);
                        }
                    }
                });
                ui.add_space(24.0);
            });
        });
    });
    action
}

fn recent_card(ui: &mut egui::Ui, home: &mut Home, r: &Recent, size: Vec2) -> Option<Action> {
    let k = key(&r.path);
    let exists = k.is_some();
    let (rect, resp) = ui.allocate_exact_size(size, if exists { Sense::click() } else { Sense::hover() });
    let visuals = ui.visuals().clone();
    let painter = ui.painter_at(rect);
    let hovered = resp.hovered() && exists;
    let fill = if hovered { visuals.widgets.hovered.weak_bg_fill } else { visuals.faint_bg_color };
    let stroke = if hovered { visuals.widgets.hovered.bg_stroke } else { Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color) };
    painter.rect(rect, 8.0, fill, stroke, StrokeKind::Inside);
    let img = Rect::from_min_size(rect.min + Vec2::splat(8.0), Vec2::new(size.x - 16.0, (size.x - 16.0) * 0.75));
    let ext = r.path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match k.as_ref().and_then(|k| home.thumbs.get(k)) {
        Some(tex) => {
            painter.rect_filled(img, 6.0, Color32::from_rgb(232, 235, 240));
            painter.image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            painter.rect_filled(img, 6.0, visuals.extreme_bg_color);
            let text = if exists { format!(".{ext}") } else { "missing".to_string() };
            painter.text(img.center(), Align2::CENTER_CENTER, text, FontId::proportional(18.0), visuals.weak_text_color());
        }
    }
    let name = r.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let text_col = if exists { visuals.strong_text_color() } else { visuals.weak_text_color() };
    let x = rect.min.x + 10.0;
    painter.text(Pos2::new(x, img.max.y + 10.0), Align2::LEFT_TOP, &name, FontId::proportional(15.0), text_col);
    let kind = if ext == "sb" { "project" } else { "LDraw" };
    painter.text(
        Pos2::new(x, img.max.y + 31.0),
        Align2::LEFT_TOP,
        format!("{} · {kind}", ago(r.opened)),
        FontId::proportional(12.0),
        visuals.weak_text_color(),
    );
    let mut action = None;
    if resp.clicked() {
        action = Some(Action::OpenPath(r.path.clone()));
    }
    resp.context_menu(|ui| {
        if ui.button("Show in Finder").clicked() {
            let _ = std::process::Command::new("open").arg("-R").arg(&r.path).spawn();
            ui.close();
        }
        if ui.button("Remove from list").clicked() {
            prefs::forget_recent(&r.path);
            home.refresh();
            ui.close();
        }
    })
    .map(|_| ())
    .unwrap_or(());
    let _ = resp.on_hover_text(tilde(&r.path));
    action
}

/// File-menu dialogs, shared by the toolbar and the start page.
impl State {
    pub fn new_project_dialog(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().add_filter("SuperBrick project", &["sb"]).set_file_name("my-project.sb").save_file() {
            let title = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            match ops::create_project(&p, &title, None) {
                Ok(()) => self.open(p, ctx),
                Err(e) => self.message = e,
            }
        }
    }
    pub fn open_dialog(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Models", &["sb", "ldr", "mpd"]).pick_file() {
            self.open(p, ctx);
        }
    }
    pub fn recover_dialog(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .set_directory(crate::autosave::directory())
            .add_filter("Recovery model", &["ldr", "mpd"])
            .pick_file()
        {
            self.open(path, ctx);
        }
    }
}

/// Toolbar icons drawn as vector shapes (crisp at any scale, follow the theme).
#[derive(Clone, Copy)]
pub enum Icon {
    Undo,
    Redo,
}

pub fn icon_button(ui: &mut egui::Ui, icon: Icon, shortcut: &str) -> egui::Response {
    let h = ui.spacing().interact_size.y.max(22.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(h + 6.0, h), Sense::click());
    let v = ui.style().interact(&resp);
    if resp.hovered() || resp.is_pointer_button_down_on() {
        ui.painter().rect(rect, v.corner_radius, v.weak_bg_fill, v.bg_stroke, StrokeKind::Inside);
    }
    let col = v.fg_stroke.color;
    let flip = match icon {
        Icon::Undo => 1.0,
        Icon::Redo => -1.0,
    };
    let s = h / 22.0;
    let r = 4.6 * s;
    let c = rect.center() + Vec2::new(2.2 * flip * s, 0.8 * s);
    let at = |dx: f32, dy: f32| Pos2::new(c.x + dx * flip, c.y + dy);
    // A U-turn: tail at the bottom, round the far side, arrowhead pointing back at the top.
    let mut pts = vec![at(-4.0 * s, r)];
    for k in 0..=14 {
        let a = std::f32::consts::FRAC_PI_2 - std::f32::consts::PI * k as f32 / 14.0;
        pts.push(at(r * a.cos(), r * a.sin()));
    }
    let point = at(-7.4 * s, -r);
    pts.push(at(-4.6 * s, -r));
    ui.painter().add(egui::Shape::line(pts, Stroke::new(1.9 * s, col)));
    let head = vec![point, at(-4.2 * s, -r - 3.3 * s), at(-4.2 * s, -r + 3.3 * s)];
    ui.painter().add(egui::Shape::convex_polygon(head, col, Stroke::NONE));
    resp.on_hover_text(tip_text(tip_label(icon), shortcut))
}

fn tip_label(icon: Icon) -> &'static str {
    match icon {
        Icon::Undo => "Undo",
        Icon::Redo => "Redo",
    }
}

fn tip_text(label: &str, shortcut: &str) -> String {
    if shortcut.is_empty() {
        label.to_string()
    } else {
        format!("{label}  {shortcut}")
    }
}
