//! M0 spike S5: does the planned shell hold together and stay smooth?
//!
//! - egui_dock layout: a toolbar at the top, then a parts panel, the build viewport, and an AI panel.
//!   Every panel can be closed and reopened from View. "+ Subbuild" opens a second CAD viewport.
//! - Each viewport draws with `render::Renderer` on eframe's own wgpu device into an offscreen
//!   texture that egui shows. A viewport redraws only when its camera or size changes.
//! - The parts panel filters about 24k library parts by substring as you type, as a placeholder for S6.
//! - The AI panel streams a canned markdown reply token by token, to test chat beside heavy viewports.
//!
//! usage: ui-spike [MODEL_A] [MODEL_B] [--bench FRAMES] [--shot FILE.png]
//!   --bench runs a scripted session: idle while the chat streams, then both viewports orbiting while
//!   it streams. It prints frame-time stats, saves a screenshot of the window, and exits.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use eframe::egui_wgpu;
use egui::{Color32, Key, Rect, Sense, Vec2, pos2};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use egui_dock::{DockArea, DockState, NodeIndex, Style, TabViewer};
use ldraw::{Document, Library};
use render::{COLOR_FORMAT, Camera, DEPTH_FORMAT, Gpu, Renderer, Scene, SceneOptions};

const SAMPLES: u32 = 4;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut models: Vec<PathBuf> = Vec::new();
    let mut skip = false;
    for a in &args {
        if skip {
            skip = false;
        } else if a == "--bench" || a == "--shot" {
            skip = true;
        } else {
            models.push(PathBuf::from(a));
        }
    }
    if models.is_empty() {
        models.push(root.join("runs/design/tortoise/studio/model.ldr"));
    }
    if models.len() < 2 {
        models.push(root.join("runs/design/skilltest_helmet/studio/model.ldr"));
    }
    let bench = opt("--bench").and_then(|s| s.parse::<u32>().ok());
    let shot = opt("--shot").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("out/screenshot.png"));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1500.0, 900.0]).with_title("cad — UI spike"),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_config(),
        ..Default::default()
    };
    eframe::run_native("cad-ui-spike", options, Box::new(move |cc| Ok(Box::new(App::new(cc, models, bench, shot)))))
}

/// The renderer wants the adapter's own limits (big vertex buffers, storage buffers), not wgpu's defaults.
fn wgpu_config() -> egui_wgpu::WgpuConfiguration {
    let mut cfg = egui_wgpu::WgpuConfiguration::default();
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut cfg.wgpu_setup {
        setup.power_preference = wgpu::PowerPreference::HighPerformance;
        setup.device_descriptor = Arc::new(|adapter: &wgpu::Adapter| wgpu::DeviceDescriptor {
            label: Some("cad-ui"),
            required_limits: adapter.limits(),
            ..Default::default()
        });
    }
    cfg
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Tab {
    Viewport(usize),
    Parts,
    Ai,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Select,
    Delete,
    Paint,
    Group,
    Connect,
}

impl Tool {
    const ALL: [(Tool, &'static str, Key); 5] = [
        (Tool::Select, "Select", Key::S),
        (Tool::Delete, "Delete", Key::D),
        (Tool::Paint, "Paint", Key::P),
        (Tool::Group, "Group", Key::G),
        (Tool::Connect, "Connect", Key::C),
    ];
}

/// The offscreen images one viewport draws into.
struct Target {
    size: (u32, u32),
    color: wgpu::TextureView,
    resolve: wgpu::TextureView,
    depth: wgpu::TextureView,
    tex: egui::TextureId,
}

struct Loaded {
    renderer: Renderer,
    cam: Camera,
    parts: usize,
    tris: usize,
}

struct Viewport {
    title: String,
    path: PathBuf,
    rx: Option<mpsc::Receiver<Result<(Scene, usize, f64), String>>>,
    loaded: Option<Loaded>,
    error: Option<String>,
    target: Option<Target>,
    dirty: bool,
    load_ms: f64,
    /// CPU time to encode and submit the last redraw.
    encode_ms: f64,
    redraws: u64,
}

impl Viewport {
    fn start(title: &str, path: PathBuf, lib: Arc<Library>, ctx: egui::Context) -> Viewport {
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        thread::spawn(move || {
            let t = Instant::now();
            let r = Document::load(&p).map_err(|e| e.to_string()).map(|doc| {
                let flat = doc.flatten(p.parent());
                let scene = Scene::build(&lib, &flat, &SceneOptions::default());
                (scene, flat.len(), t.elapsed().as_secs_f64() * 1e3)
            });
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        Viewport {
            title: title.into(),
            path,
            rx: Some(rx),
            loaded: None,
            error: None,
            target: None,
            dirty: true,
            load_ms: 0.0,
            encode_ms: 0.0,
            redraws: 0,
        }
    }
}

struct Msg {
    user: bool,
    text: String,
}

struct Chat {
    msgs: Vec<Msg>,
    input: String,
    rx: Option<mpsc::Receiver<String>>,
    cache: CommonMarkCache,
    tokens: usize,
}

const REPLY: &str = "Sure. Here's a plan for the **tower**:\n\n1. Lay a `2 x 4` plate ring on the baseplate (8 plates).\n2. Build up four courses of `1 x 2` bricks, offsetting each course by one stud.\n3. Cap the top with `2 x 2` tiles, leaving the studs on the corners for the battlements.\n\n| step | parts | colour |\n|---|---|---|\n| base | 3020 x8 | Dark Bluish Gray |\n| walls | 3004 x64 | Light Bluish Gray |\n| top | 3068b x12 | Dark Tan |\n\nI'll put these in as a draft so you can accept them step by step. Each step gets checked by the bench before you see it, and I'll stop and ask if anything I place would collide with parts you've added in the meantime. ";

fn stream_reply(ctx: egui::Context) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // About 60 tokens per second, like a fast model.
        let text = REPLY.repeat(3);
        let mut tok = String::new();
        for ch in text.chars() {
            tok.push(ch);
            if ch == ' ' || ch == '\n' {
                if tx.send(std::mem::take(&mut tok)).is_err() {
                    return;
                }
                ctx.request_repaint();
                thread::sleep(Duration::from_millis(16));
            }
        }
        let _ = tx.send(tok);
        ctx.request_repaint();
    });
    rx
}

struct Parts {
    all: Vec<(String, String, String)>,
    rx: Option<mpsc::Receiver<Vec<(String, String, String)>>>,
    query: String,
    hits: Vec<usize>,
    filter_ms: f64,
    last_query: Option<String>,
}

#[derive(Default)]
struct Bench {
    frames: u32,
    phase: u32,
    done: u32,
    last: Option<Instant>,
    intervals: [Vec<f64>; 2],
    ui_ms: [Vec<f64>; 2],
    shot_requested: bool,
}

struct State {
    lib: Arc<Library>,
    gpu: Gpu,
    rs: egui_wgpu::RenderState,
    views: Vec<Viewport>,
    chat: Chat,
    parts: Parts,
    tool: Tool,
    frame_ms: f64,
    last_frame: Instant,
    bench: Option<Bench>,
    shot: PathBuf,
    extra_model: PathBuf,
}

struct App {
    dock: DockState<Tab>,
    st: State,
}

impl App {
    fn new(cc: &eframe::CreationContext, models: Vec<PathBuf>, bench: Option<u32>, shot: PathBuf) -> App {
        let rs = cc.wgpu_render_state.clone().expect("eframe runs on wgpu");
        let gpu = Gpu { device: rs.device.clone(), queue: rs.queue.clone(), info: rs.adapter.get_info() };
        eprintln!("GPU: {} ({:?}), surface {:?}", gpu.info.name, gpu.info.backend, rs.target_format);
        let t = Instant::now();
        let lib = Arc::new(Library::open_default().expect("LDraw library"));
        eprintln!("library opened in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
        let ctx = cc.egui_ctx.clone();
        let views = vec![Viewport::start("Build", models[0].clone(), lib.clone(), ctx.clone())];

        let (ptx, prx) = mpsc::channel();
        {
            let lib = lib.clone();
            let ctx = ctx.clone();
            thread::spawn(move || {
                let mut v: Vec<(String, String, String)> = lib
                    .part_names()
                    .map(|n| {
                        let d = lib.description(n).to_string();
                        let low = format!("{} {}", n.to_ascii_lowercase(), d.to_ascii_lowercase());
                        (n.to_string(), d, low)
                    })
                    .filter(|(_, d, _)| !d.starts_with('~'))
                    .collect();
                v.sort_by(|a, b| a.0.cmp(&b.0));
                let _ = ptx.send(v);
                ctx.request_repaint();
            });
        }

        let mut dock = DockState::new(vec![Tab::Viewport(0)]);
        let surface = dock.main_surface_mut();
        let [center, _] = surface.split_left(NodeIndex::root(), 0.2, vec![Tab::Parts]);
        surface.split_right(center, 0.72, vec![Tab::Ai]);

        let mut chat = Chat { msgs: Vec::new(), input: String::new(), rx: None, cache: CommonMarkCache::default(), tokens: 0 };
        if bench.is_some() {
            chat.msgs.push(Msg { user: true, text: "Build me a small castle tower on the baseplate.".into() });
            chat.msgs.push(Msg { user: false, text: String::new() });
            chat.rx = Some(stream_reply(ctx.clone()));
        }
        App {
            dock,
            st: State {
                lib,
                gpu,
                rs,
                views,
                chat,
                parts: Parts { all: Vec::new(), rx: Some(prx), query: String::new(), hits: Vec::new(), filter_ms: 0.0, last_query: None },
                tool: Tool::Select,
                frame_ms: 0.0,
                last_frame: Instant::now(),
                bench: bench.map(|frames| Bench { frames, ..Default::default() }),
                shot,
                extra_model: models[1].clone(),
            },
        }
    }

    fn open_subbuild(&mut self, ctx: &egui::Context) {
        let i = self.st.views.len();
        let title = format!("Subbuild {i}");
        self.st.views.push(Viewport::start(&title, self.st.extra_model.clone(), self.st.lib.clone(), ctx.clone()));
        // Beside the build viewport: split its leaf to the bottom.
        if let Some(path) = self.dock.find_tab(&Tab::Viewport(0)) {
            let node = path.node;
            self.dock.main_surface_mut().split_below(node, 0.5, vec![Tab::Viewport(i)]);
        } else {
            self.dock.push_to_focused_leaf(Tab::Viewport(i));
        }
    }

    fn toggle(&mut self, tab: Tab) {
        if let Some(path) = self.dock.find_tab(&tab) {
            self.dock.remove_tab(path);
        } else {
            match tab {
                Tab::Parts => {
                    self.dock.main_surface_mut().split_left(NodeIndex::root(), 0.2, vec![tab]);
                }
                Tab::Ai => {
                    self.dock.main_surface_mut().split_right(NodeIndex::root(), 0.75, vec![tab]);
                }
                Tab::Viewport(_) => self.dock.push_to_focused_leaf(tab),
            }
        }
    }

    fn bench_tick(&mut self, ctx: &egui::Context, ui_ms: f64) {
        let Some(b) = self.st.bench.as_mut() else { return };
        let ready = self.st.views.iter().all(|v| v.loaded.is_some() || v.error.is_some()) && self.st.views.len() >= 2;
        if !ready {
            if self.st.views.len() < 2 && self.st.views[0].loaded.is_some() {
                self.open_subbuild(ctx);
            }
            ctx.request_repaint();
            return;
        }
        let now = Instant::now();
        if b.phase < 2 {
            if let Some(last) = b.last {
                b.intervals[b.phase as usize].push((now - last).as_secs_f64() * 1e3);
                b.ui_ms[b.phase as usize].push(ui_ms);
                b.done += 1;
            }
            b.last = Some(now);
            if b.phase == 1 {
                for v in &mut self.st.views {
                    if let Some(l) = v.loaded.as_mut() {
                        l.cam.yaw += 0.015;
                        v.dirty = true;
                    }
                }
            }
            if b.done >= b.frames {
                b.phase += 1;
                b.done = 0;
                b.last = None;
            }
            ctx.request_repaint();
        } else if b.shot_requested {
            // Keep frames coming until the screenshot event arrives.
            ctx.request_repaint();
        } else if !b.shot_requested {
            b.shot_requested = true;
            let pct = |v: &mut Vec<f64>, p: f64| {
                v.sort_by(|a, b| a.total_cmp(b));
                v.get(((v.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(f64::NAN)
            };
            for (i, name) in ["idle viewports, chat streaming", "both viewports orbiting, chat streaming"].iter().enumerate() {
                let n = b.intervals[i].len();
                let over = b.intervals[i].iter().filter(|&&x| x > 25.0).count();
                let (p50, p95, max) = (pct(&mut b.intervals[i], 0.5), pct(&mut b.intervals[i], 0.95), pct(&mut b.intervals[i], 1.0));
                let (u50, u95) = (pct(&mut b.ui_ms[i], 0.5), pct(&mut b.ui_ms[i], 0.95));
                println!(
                    "{name:42} {n} frames: interval p50 {p50:.1} ms, p95 {p95:.1} ms, max {max:.1} ms, {over} over 25 ms; ui() CPU p50 {u50:.2} ms, p95 {u95:.2} ms"
                );
            }
            for v in &self.st.views {
                if let Some(l) = &v.loaded {
                    println!(
                        "  {:10} {} parts, {:.1}M tris, loaded in {:.0} ms, last redraw encode {:.2} ms, {} redraws",
                        v.title,
                        l.parts,
                        l.tris as f64 / 1e6,
                        v.load_ms,
                        v.encode_ms,
                        v.redraws
                    );
                }
            }
            println!("  chat streamed {} tokens", self.st.chat.tokens);
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            ctx.request_repaint();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        let ctx = ui.ctx().clone();
        self.st.frame_ms = self.st.last_frame.elapsed().as_secs_f64() * 1e3;
        self.st.last_frame = Instant::now();

        // Screenshot from the bench: save it and quit.
        let shots: Vec<Arc<egui::ColorImage>> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None })
                .collect()
        });
        if let Some(img) = shots.first() {
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(dir) = self.st.shot.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match render::save_png(&self.st.shot, img.size[0] as u32, img.size[1] as u32, &rgba) {
                Ok(()) => println!("  screenshot {}", self.st.shot.display()),
                Err(e) => println!("  screenshot failed: {e}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Background results.
        for v in &mut self.st.views {
            if let Some(rx) = &v.rx {
                if let Ok(r) = rx.try_recv() {
                    v.rx = None;
                    match r {
                        Ok((scene, parts, ms)) => {
                            let t = Instant::now();
                            let renderer = Renderer::new(&self.st.gpu, COLOR_FORMAT, SAMPLES, &scene);
                            let (lo, hi) = scene.bounds;
                            v.load_ms = ms + t.elapsed().as_secs_f64() * 1e3;
                            let tris = scene.stats.tris as usize;
                            v.loaded = Some(Loaded { renderer, cam: Camera::framing(lo, hi, 1.5), parts, tris });
                            v.dirty = true;
                        }
                        Err(e) => v.error = Some(e),
                    }
                }
            }
        }
        if let Some(rx) = &self.st.parts.rx {
            if let Ok(all) = rx.try_recv() {
                self.st.parts.all = all;
                self.st.parts.rx = None;
                self.st.parts.last_query = None;
            }
        }
        if let Some(rx) = &self.st.chat.rx {
            let mut done = false;
            loop {
                match rx.try_recv() {
                    Ok(tok) => {
                        self.st.chat.tokens += 1;
                        if let Some(m) = self.st.chat.msgs.last_mut() {
                            m.text.push_str(&tok);
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        done = true;
                        break;
                    }
                }
            }
            if done {
                self.st.chat.rx = None;
            }
        }

        // Tool keybinds, unless a text field has focus.
        if !ctx.egui_wants_keyboard_input() {
            for (tool, _, key) in Tool::ALL {
                if ctx.input(|i| i.key_pressed(key) && i.modifiers.is_none()) {
                    self.st.tool = tool;
                }
            }
        }

        let mut open_sub = false;
        let mut toggles = Vec::new();
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.menu_button("View", |ui| {
                    for (tab, name) in [(Tab::Parts, "Parts"), (Tab::Ai, "AI"), (Tab::Viewport(0), "Build")] {
                        let mut open = self.dock.find_tab(&tab).is_some();
                        if ui.checkbox(&mut open, name).changed() {
                            toggles.push(tab);
                        }
                    }
                });
                ui.separator();
                for (tool, name, key) in Tool::ALL {
                    let label = format!("{name} ({})", key.name().to_lowercase());
                    if ui.selectable_label(self.st.tool == tool, label).clicked() {
                        self.st.tool = tool;
                    }
                }
                ui.separator();
                if ui.button("+ Subbuild").clicked() {
                    open_sub = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{:.1} ms/frame", self.st.frame_ms));
                });
            });
        });
        for t in toggles {
            self.toggle(t);
        }
        if open_sub {
            self.open_subbuild(&ctx);
        }

        egui::CentralPanel::default().show(ui, |ui| {
            let style = Style::from_egui(ui.style());
            DockArea::new(&mut self.dock)
                .style(style)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut Viewer { st: &mut self.st });
        });

        let ui_ms = t0.elapsed().as_secs_f64() * 1e3;
        self.bench_tick(&ctx, ui_ms);
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
        match tab {
            Tab::Viewport(i) => self.st.views[*i].title.clone().into(),
            Tab::Parts => "Parts".into(),
            Tab::Ai => "AI".into(),
        }
    }

    fn is_closeable(&self, tab: &Tab) -> bool {
        *tab != Tab::Viewport(0)
    }

    fn clear_background(&self, tab: &Tab) -> bool {
        !matches!(tab, Tab::Viewport(_))
    }

    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        match tab {
            Tab::Viewport(_) | Tab::Ai | Tab::Parts => [false, false],
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match *tab {
            Tab::Viewport(i) => viewport_ui(self.st, i, ui),
            Tab::Parts => parts_ui(&mut self.st.parts, ui),
            Tab::Ai => chat_ui(&mut self.st.chat, ui),
        }
    }
}

fn make_target(gpu: &Gpu, rs: &egui_wgpu::RenderState, size: (u32, u32), old: Option<egui::TextureId>) -> Target {
    let ext = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
    let tex = |label: &str, format, samples, usage| {
        gpu.device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: ext,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    let ra = wgpu::TextureUsages::RENDER_ATTACHMENT;
    let color = tex("vp-color", COLOR_FORMAT, SAMPLES, ra);
    // The scene renders through an sRGB view (blending in linear light), but egui expects
    // gamma-encoded texels and does its own blending on a non-sRGB surface, so egui samples the
    // same texture through a plain Rgba8Unorm view. Sampling the sRGB view would darken the image.
    let resolve_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("vp-resolve"),
        size: ext,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: ra | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
    });
    let resolve = resolve_tex.create_view(&Default::default());
    let for_egui = resolve_tex.create_view(&wgpu::TextureViewDescriptor {
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    let depth = tex("vp-depth", DEPTH_FORMAT, SAMPLES, ra);
    let mut r = rs.renderer.write();
    let tex = match old {
        Some(id) => {
            r.update_egui_texture_from_wgpu_texture(&gpu.device, &for_egui, wgpu::FilterMode::Linear, id);
            id
        }
        None => r.register_native_texture(&gpu.device, &for_egui, wgpu::FilterMode::Linear),
    };
    Target { size, color, resolve, depth, tex }
}

fn viewport_ui(st: &mut State, i: usize, ui: &mut egui::Ui) {
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let v = &mut st.views[i];
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, Color32::from_rgb(243, 245, 247));
    let Some(l) = v.loaded.as_mut() else {
        let msg = v.error.as_deref().map_or_else(|| format!("loading {} …", v.path.display()), |e| format!("error: {e}"));
        painter.text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(14.0), Color32::DARK_GRAY);
        return;
    };

    // Camera: left-drag orbits, right/middle-drag pans, scroll zooms.
    let d = resp.drag_delta();
    if resp.dragged_by(egui::PointerButton::Primary) && d != Vec2::ZERO {
        l.cam.yaw -= d.x * 0.008;
        l.cam.pitch = (l.cam.pitch + d.y * 0.008).clamp(-1.5, 1.5);
        v.dirty = true;
    }
    if (resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle)) && d != Vec2::ZERO {
        let eye = l.cam.eye();
        let fwd = (l.cam.target - eye).normalize();
        let right = fwd.cross(glam::Vec3::NEG_Y).normalize();
        let up = right.cross(fwd);
        let k = l.cam.dist * (l.cam.fov_y * 0.5).tan() * 2.0 / rect.height().max(1.0);
        l.cam.target += (-right * d.x + up * d.y) * k;
        v.dirty = true;
    }
    if resp.hovered() {
        let s = ui.input(|i| i.smooth_scroll_delta.y);
        if s != 0.0 {
            l.cam.dist = (l.cam.dist * (-s * 0.002).exp()).max(1.0);
            v.dirty = true;
        }
    }

    let ppp = ui.ctx().pixels_per_point();
    let size = ((rect.width() * ppp).round().max(1.0) as u32, (rect.height() * ppp).round().max(1.0) as u32);
    if v.target.as_ref().is_none_or(|t| t.size != size) {
        let old = v.target.as_ref().map(|t| t.tex);
        v.target = Some(make_target(&st.gpu, &st.rs, size, old));
        v.dirty = true;
    }
    let t = v.target.as_ref().unwrap();
    if v.dirty {
        let t0 = Instant::now();
        let mut enc = st.gpu.device.create_command_encoder(&Default::default());
        l.renderer.encode(&st.gpu, &mut enc, &t.color, Some(&t.resolve), &t.depth, &l.cam, t.size);
        st.gpu.queue.submit([enc.finish()]);
        v.encode_ms = t0.elapsed().as_secs_f64() * 1e3;
        v.redraws += 1;
        v.dirty = false;
    }
    painter.image(t.tex, rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);

    // Overlays: piece count bottom-right, redraw cost top-left.
    let small = egui::FontId::proportional(13.0);
    let count = format!("{} pieces", l.parts);
    let galley = painter.layout_no_wrap(count, small.clone(), Color32::from_gray(40));
    let at = rect.right_bottom() - galley.size() - Vec2::splat(10.0);
    painter.rect_filled(Rect::from_min_size(at - Vec2::splat(5.0), galley.size() + Vec2::splat(10.0)), 4.0, Color32::from_white_alpha(200));
    painter.galley(at, galley, Color32::from_gray(40));
    painter.text(
        rect.left_top() + Vec2::splat(8.0),
        egui::Align2::LEFT_TOP,
        format!("{:.1}M tris · {}×{} · encode {:.2} ms · {} redraws", l.tris as f64 / 1e6, size.0, size.1, v.encode_ms, v.redraws),
        small,
        Color32::from_gray(110),
    );
}

fn parts_ui(p: &mut Parts, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.add(egui::TextEdit::singleline(&mut p.query).hint_text("search parts…").desired_width(f32::INFINITY));
    if p.last_query.as_deref() != Some(p.query.as_str()) {
        let t = Instant::now();
        let words: Vec<String> = p.query.to_ascii_lowercase().split_whitespace().map(String::from).collect();
        p.hits = p.all.iter().enumerate().filter(|(_, (_, _, low))| words.iter().all(|w| low.contains(w.as_str()))).map(|(i, _)| i).collect();
        p.filter_ms = t.elapsed().as_secs_f64() * 1e3;
        p.last_query = Some(p.query.clone());
    }
    ui.label(egui::RichText::new(format!("{} of {} parts · {:.1} ms", p.hits.len(), p.all.len(), p.filter_ms)).small().weak());
    ui.separator();
    let row = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row, p.hits.len(), |ui, range| {
        for &i in &p.hits[range] {
            let (name, desc, _) = &p.all[i];
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(name.trim_end_matches(".dat")).monospace().weak());
                ui.label(desc);
            });
        }
    });
}

fn chat_ui(c: &mut Chat, ui: &mut egui::Ui) {
    let input_h = 64.0;
    let avail = ui.available_height();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height((avail - input_h).max(50.0))
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for (i, m) in c.msgs.iter().enumerate() {
                let fill = if m.user { Color32::from_rgb(232, 238, 250) } else { Color32::TRANSPARENT };
                egui::Frame::new().fill(fill).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if m.user {
                        ui.label(&m.text);
                    } else {
                        ui.push_id(i, |ui| {
                            CommonMarkViewer::new().show(ui, &mut c.cache, &m.text);
                        });
                    }
                });
                ui.add_space(6.0);
            }
            if c.rx.is_some() {
                ui.spinner();
            }
        });
    ui.separator();
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut c.input).hint_text("ask the builder…").desired_width(ui.available_width() - 60.0));
        let send = ui.button("Send").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
        if send && !c.input.trim().is_empty() && c.rx.is_none() {
            c.msgs.push(Msg { user: true, text: std::mem::take(&mut c.input) });
            c.msgs.push(Msg { user: false, text: String::new() });
            c.rx = Some(stream_reply(ui.ctx().clone()));
        }
    });
}
