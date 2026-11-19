//! First-run setup (the LDraw parts library) and the optional Python components.
//!
//! Without a parts library nothing else can start, so `main` runs [`Setup`] instead of the
//! editor; once the library is in place SuperBrick relaunches itself into the editor. Python is
//! only needed by a few tools: [`PythonOffer`] appears when one of them reports it is missing.

use ldraw::runtime::{self, Progress};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

type Task = Box<dyn FnOnce(&mut dyn FnMut(Progress)) -> Result<PathBuf, String> + Send>;

enum JobState {
    Running(Progress),
    Done(Result<PathBuf, String>),
}

/// A download/install running on its own thread.
pub struct Job(Arc<Mutex<JobState>>);

impl Job {
    pub fn start(ctx: &egui::Context, tasks: Vec<Task>) -> Job {
        let state = Arc::new(Mutex::new(JobState::Running(Progress::default())));
        let shared = state.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("sb-setup".into())
            .spawn(move || {
                let mut last = Ok(PathBuf::new());
                for task in tasks {
                    let mut report = |p: Progress| {
                        *shared.lock().unwrap() = JobState::Running(p);
                        ctx.request_repaint();
                    };
                    last = task(&mut report);
                    if last.is_err() {
                        break;
                    }
                }
                *shared.lock().unwrap() = JobState::Done(last);
                ctx.request_repaint();
            })
            .expect("setup thread");
        Job(state)
    }
    /// `None` while running (drawing its progress), else the outcome.
    pub fn show(&self, ui: &mut egui::Ui) -> Option<Result<PathBuf, String>> {
        match &*self.0.lock().unwrap() {
            JobState::Running(p) => {
                ui.label(if p.step.is_empty() { "Starting…" } else { &p.step });
                let text = if p.total > 0 {
                    format!("{:.0} of {:.0} MB", p.done as f64 / 1e6, p.total as f64 / 1e6)
                } else {
                    "working…".into()
                };
                let bar = match p.fraction() {
                    Some(f) => egui::ProgressBar::new(f).text(text),
                    None => egui::ProgressBar::new(0.0).animate(true).text(text),
                };
                ui.add(bar.desired_width(420.0));
                None
            }
            JobState::Done(r) => Some(r.clone()),
        }
    }
}

fn library_task() -> Task {
    Box::new(|r| runtime::install_library(r))
}
fn python_task() -> Task {
    Box::new(|r| runtime::install_python(r))
}
fn copy_library_task(from: PathBuf) -> Task {
    Box::new(move |r| {
        r(Progress { step: format!("Copying {}", from.display()), ..Default::default() });
        ldraw::zipfs::ZipFs::open(&from).map_err(|e| format!("{} is not an LDraw library zip ({e})", from.display()))?;
        let dest = runtime::downloaded_library();
        std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(&from, &dest).map_err(|e| e.to_string())?;
        Ok(dest)
    })
}

/// The first-run window: get the parts library (and optionally Python), then relaunch.
pub struct Setup {
    argv: Vec<String>,
    job: Option<Job>,
    error: Option<String>,
    python_needed: Arc<Mutex<Option<bool>>>,
    want_python: bool,
    /// `--window-shot PATH` (testing): screenshot after a few frames, then quit.
    shot: Option<(PathBuf, u32)>,
}

impl Setup {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Setup {
        crate::open_files::set_context(&cc.egui_ctx);
        crate::open_files::reinstall();
        let python_needed = Arc::new(Mutex::new(None));
        let found = python_needed.clone();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            *found.lock().unwrap() = Some(runtime::python().is_none());
            ctx.request_repaint();
        });
        let argv: Vec<String> = std::env::args().skip(1).collect();
        let shot = argv.iter().position(|a| a == "--window-shot").and_then(|i| argv.get(i + 1)).map(|p| (PathBuf::from(p), 0));
        Setup { argv, job: None, error: None, python_needed, want_python: true, shot }
    }

    fn start(&mut self, ctx: &egui::Context, first: Task) {
        let mut tasks = vec![first];
        if self.want_python && *self.python_needed.lock().unwrap() == Some(true) {
            tasks.push(python_task());
        }
        self.error = None;
        self.job = Some(Job::start(ctx, tasks));
    }

    /// Start the editor in a fresh process (with any file Finder asked us to open) and quit.
    fn relaunch(&mut self, ctx: &egui::Context) {
        let mut argv = self.argv.clone();
        argv.extend(crate::open_files::take().iter().map(|p| p.display().to_string()));
        // The restarted app must open the file itself: with SB_SHELL_LAUNCH it would hand the file
        // to this setup window (which owns the single-instance pipe) just as it closes.
        match std::env::current_exe()
            .and_then(|exe| std::process::Command::new(exe).args(&argv).env_remove("SB_SHELL_LAUNCH").spawn())
        {
            Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => self.error = Some(format!("could not restart SuperBrick: {e}")),
        }
    }
}

impl Setup {
    fn screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, frames)) = &mut self.shot else { return };
        *frames += 1;
        let ready = self.python_needed.lock().unwrap().is_some();
        if *frames == 20 || (*frames > 20 && *frames % 60 == 0 && ready) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let image = ctx.input(|i| i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        }));
        if let Some(image) = image.filter(|_| ready) {
            let pixels: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            let _ = render::save_png(path, image.size[0] as u32, image.size[1] as u32, &pixels);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}

impl eframe::App for Setup {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.screenshot(&ctx);
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(40.0);
                crate::brand::wordmark(ui, 64.0);
                ui.add_space(24.0);
                ui.heading("Welcome to SuperBrick");
                ui.add_space(8.0);
                ui.label("SuperBrick builds with the LDraw parts library: a free catalogue of over 20,000 brick parts from ldraw.org.");
                ui.label(format!("It is a one-time download of about {:.0} MB.", runtime::LIBRARY_BYTES as f64 / 1e6));
                ui.add_space(16.0);
                if let Some(job) = &self.job {
                    match job.show(ui) {
                        None => {}
                        Some(Ok(_)) if runtime::library_zip().is_some() => {
                            self.job = None;
                            self.relaunch(&ctx);
                        }
                        Some(Ok(_)) => {
                            self.job = None;
                            self.error = Some("the library is still missing".into());
                        }
                        Some(Err(e)) => {
                            self.job = None;
                            self.error = Some(e);
                        }
                    }
                    return;
                }
                let python = *self.python_needed.lock().unwrap();
                if python == Some(true) {
                    ui.checkbox(
                        &mut self.want_python,
                        "Also install Python components (about 50 MB) for Connect, Hinge, instructions and studio renders",
                    );
                    ui.add_space(8.0);
                }
                ui.horizontal(|ui| {
                    let width = 420.0;
                    ui.add_space((ui.available_width() - width).max(0.0) / 2.0);
                    let ready = python.is_some();
                    if ui.add_enabled(ready, egui::Button::new("Download the parts library").min_size(egui::vec2(220.0, 32.0))).clicked() {
                        self.start(&ctx, library_task());
                    }
                    if ui.add_enabled(ready, egui::Button::new("Use a library file…").min_size(egui::vec2(160.0, 32.0))).on_hover_text("An LDraw complete.zip, or LeoCAD's library.bin").clicked() {
                        if let Some(file) = rfd::FileDialog::new().add_filter("LDraw library", &["zip", "bin"]).pick_file() {
                            self.start(&ctx, copy_library_task(file));
                        }
                    }
                });
                if python.is_none() {
                    ui.small("Checking for Python…");
                }
                if let Some(e) = &self.error {
                    ui.add_space(8.0);
                    ui.colored_label(ui.visuals().warn_fg_color, e);
                }
                ui.add_space(24.0);
                ui.small(format!("Saved in {}", runtime::support_dir().display()));
            });
        });
    }
}

/// Offers to install Python when a tool reports it is missing, and shows the install.
#[derive(Default)]
pub struct PythonOffer {
    open: bool,
    seen: String,
    job: Option<Job>,
}

impl PythonOffer {
    /// Open the offer now (File menu).
    pub fn ask(&mut self) {
        self.open = true;
    }
    /// Draw it; returns a status message when something finished.
    pub fn show(&mut self, ctx: &egui::Context, message: &str) -> Option<String> {
        if message.contains(runtime::NEEDS_PYTHON) && message != self.seen {
            self.seen = message.to_owned();
            self.open = true;
        }
        if !self.open {
            return None;
        }
        let mut result = None;
        let mut open = true;
        egui::Window::new("Python components")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_max_width(460.0);
                if let Some(job) = &self.job {
                    match job.show(ui) {
                        None => {}
                        Some(Ok(path)) => {
                            result = Some(format!("Python is ready ({}); try the tool again", path.display()));
                            self.job = None;
                            self.open = false;
                        }
                        Some(Err(e)) => {
                            result = Some(format!("Python install failed: {e}"));
                            self.job = None;
                        }
                    }
                    return;
                }
                if let Some(found) = runtime::python() {
                    ui.label(format!("SuperBrick is using {}.", found.display()));
                    if ui.button("Close").clicked() {
                        self.open = false;
                    }
                    return;
                }
                ui.label(if cfg!(target_os = "macos") {
                    "Connect, Hinge, instruction booklets and studio renders run small Python helpers that need NumPy and Pillow. No such Python was found on this Mac."
                } else {
                    "Connect, Hinge, instruction booklets and studio renders run small Python helpers that need NumPy and Pillow. No such Python was found on this computer."
                });
                ui.add_space(6.0);
                ui.label(if cfg!(target_os = "macos") {
                    "SuperBrick can install its own copy into Application Support (about 50 MB to download, 220 MB on disk, a minute or two). Nothing else on your Mac changes.".to_string()
                } else {
                    format!(
                        "SuperBrick can install its own copy into {} (about 50 MB to download, 220 MB on disk, a minute or two). Nothing else on your computer changes.",
                        ldraw::runtime::support_dir().display()
                    )
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Install Python components").clicked() {
                        self.job = Some(Job::start(ctx, vec![python_task()]));
                    }
                    if ui.button("Not now").clicked() {
                        self.open = false;
                    }
                });
            });
        if !open && self.job.is_none() {
            self.open = false;
        }
        result
    }
}
