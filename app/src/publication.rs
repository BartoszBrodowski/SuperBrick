//! Offline, revision-stamped instruction and studio render jobs. No model writes.
use crate::app::State;
use eframe::egui;
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        Arc, Mutex, mpsc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Instructions,
    Studio,
}
enum Event {
    Image(String),
    Started(Arc<Mutex<Child>>),
    Progress(String),
    Finished(Result<serde_json::Value, String>),
}
pub struct Panel {
    rx: Option<mpsc::Receiver<Event>>,
    child: Option<Arc<Mutex<Child>>>,
    cancelled: Arc<AtomicBool>,
    message: String,
    output: Option<PathBuf>,
    result: Option<serde_json::Value>,
    textures: Vec<egui::TextureHandle>,
    revision: u64,
    size: u32,
    samples: u32,
    high_quality: bool,
    page: usize,
    step_texture: Option<(usize, egui::TextureHandle)>,
    tray_texture: Option<(usize, egui::TextureHandle)>,
}
impl Default for Panel {
    fn default() -> Self {
        Self {
            rx: None,
            child: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            message: String::new(),
            output: None,
            result: None,
            textures: vec![],
            revision: 0,
            size: 1024,
            samples: 4,
            high_quality: false,
            page: 0,
            step_texture: None,
            tray_texture: None,
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(child) = &self.child {
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        }
    }
}

fn resources() -> (PathBuf, PathBuf) {
    let installed = ldraw::installed_share("connectors");
    let root = ldraw::app_env("CONNECTOR_DATA")
        .map(PathBuf::from)
        .or_else(|| installed.filter(|p| p.join("bench/render.py").is_file()));
    if let Some(root) = root {
        (root.join("bench"), root.join("shadow"))
    } else {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        (
            root.join("bench-next"),
            root.join("data-next/ldcad_shadow/LDCadShadowLibrary-main"),
        )
    }
}

/// The end of a worker log: the last traceback, or else its last lines.
fn log_tail(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.iter().rposition(|l| l.starts_with("Traceback")).unwrap_or(lines.len().saturating_sub(8));
    let tail = &lines[start..];
    // Keep the exception and the frames nearest to it.
    let tail = if tail.len() > 12 { [&tail[..1], &tail[tail.len() - 11..]].concat() } else { tail.to_vec() };
    (!tail.is_empty()).then(|| tail.join("\n"))
}

/// Explain a worker that failed, with what it wrote to its log.
fn worker_failed(what: &str, log: &Path) -> String {
    match log_tail(log) {
        Some(tail) => format!("{what}\n{tail}"),
        None => format!("{what}. See {}.", log.display()),
    }
}

/// Hand the job to the worker. A worker that dies before reading it (a Python that cannot
/// import its packages, say) closes the pipe; report its error, not EPIPE.
fn send_job(child: &mut Child, job: &serde_json::Value, log: &Path) -> Result<(), String> {
    let bytes = serde_json::to_vec(job).map_err(|e| e.to_string())?;
    let sent = child.stdin.take().ok_or("Missing worker stdin")?.write_all(&bytes);
    if let Err(e) = sent {
        let status = child.wait().ok().and_then(|s| s.code()).map(|c| format!(" (exit {c})")).unwrap_or_default();
        return Err(worker_failed(&format!("The instruction worker stopped before reading its job{status}: {e}"), log));
    }
    Ok(())
}

fn cache_directory() -> PathBuf {
    ldraw::app_env("PUBLICATION_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| ldraw::runtime::cache_dir().join("publications"))
}

pub fn generate_cli(args: &[String]) -> Result<(), String> {
    let mut file = None;
    let mut output = None;
    let mut size = 512u32;
    let mut plan_only = false;
    let mut studio = false;
    let mut high_quality = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--generate-instructions" => {}
            "--output" => {
                output = Some(PathBuf::from(
                    it.next().ok_or("--output needs a directory")?,
                ))
            }
            "--image-size" => {
                size = it
                    .next()
                    .ok_or("--image-size needs a number")?
                    .parse()
                    .map_err(|_| "Invalid image size")?
            }
            "--plan-only" => plan_only = true,
            "--studio-render" => studio = true,
            "--high-quality" => high_quality = true,
            _ if arg.starts_with('-') => return Err(format!("Unknown instruction option: {arg}")),
            _ if file.is_none() => file = Some(PathBuf::from(arg)),
            _ => return Err(format!("Unexpected argument: {arg}")),
        }
    }
    if !(128..=1600).contains(&size) {
        return Err("Image size must be 128–1600".into());
    }
    let source = file
        .ok_or_else(|| "Usage: sb MODEL --generate-instructions [--studio-render] [--output NEW_DIRECTORY]".to_string())
        .and_then(|f| ldraw::platform::canonicalize(&f).map_err(|e| e.to_string()))?;
    let bytes = std::fs::read(&source).map_err(|e| e.to_string())?;
    let doc = ldraw::Document::load(&source).map_err(|e| e.to_string())?;
    let flat = doc.flatten(source.parent());
    if flat.is_empty() {
        return Err("Model has no pieces".into());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let out = output.unwrap_or_else(|| {
        PathBuf::from(format!(
            "{}-instructions-{stamp}",
            source.file_stem().unwrap_or_default().to_string_lossy()
        ))
    });
    std::fs::create_dir(&out).map_err(|e| format!("Output must be a new directory: {e}"))?;
    let out = ldraw::platform::canonicalize(&out).map_err(|e| e.to_string())?;
    let (bench, shadow) = resources();
    let parts: Vec<_> = flat.iter().map(|p| serde_json::json!({"name":p.name,"color":p.color.0,"pos":p.pos.to_array(),"rot":p.rot.to_cols_array()})).collect();
    let kind = if studio { "studio" } else { "instructions" };
    let job = serde_json::json!({"kind":kind,"out":out,"bench":bench,"shadow":shadow,"parts":parts,"size":size,"samples":if high_quality {64} else {16},"plan_only":plan_only,"high_quality":high_quality});
    std::fs::write(
        out.join("job.json"),
        serde_json::to_vec_pretty(&job).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let log = out.join("worker.log");
    let script = ldraw::runtime::helper_script("publication", include_str!("publication.py"))?;
    let mut command = ldraw::runtime::python_command()?;
    ldraw::platform::no_window(&mut command);
    let mut child =
        command
            .arg("-u")
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(&log).map_err(|e| e.to_string())?))
            .spawn()
            .map_err(|e| e.to_string())?;
    send_job(&mut child, &job, &log)?;
    let mut result = None;
    let mut error = None;
    for line in BufReader::new(child.stdout.take().ok_or("Missing worker stdout")?).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
            if let Some(p) = v["progress"].as_str() {
                eprintln!("{p}");
            }
            if let Some(e) = v["error"].as_str() {
                error = Some(e.to_owned());
            }
            if v.get("result").is_some() {
                result = Some(v["result"].clone());
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if std::fs::read(&source).map_err(|e| e.to_string())? != bytes {
        return Err(
            "Source changed during generation; output belongs to the initial snapshot".into(),
        );
    }
    if !status.success() {
        return Err(format!(
            "{}\nDiagnostics: {}",
            error.unwrap_or_else(|| worker_failed("Instruction worker failed", &log)),
            out.display()
        ));
    }
    let mut result = result.ok_or("No instruction result")?;
    // Page callouts are used by the GUI; keep the headless terminal summary
    // compact even for hundreds of illustrated steps.
    if let Some(object) = result.as_object_mut() {
        object.remove("pages");
    }
    println!("{}", serde_json::json!({"output":out,"result":result}));
    Ok(())
}

fn export_bundle(source: &Path, parent: &Path, kind: Kind) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let destination = parent.join(format!(
        "cad-{}-{stamp}",
        if kind == Kind::Studio {
            "studio"
        } else {
            "instructions"
        }
    ));
    std::fs::create_dir(&destination).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("png")
            || name == "instructions.html"
            || name == "plan.json"
        {
            std::fs::copy(&path, destination.join(&name)).map_err(|e| e.to_string())?;
        }
    }
    Ok(destination)
}

fn start(
    panel: &mut Panel,
    st: &State,
    ctx: &egui::Context,
    kind: Kind,
    parent: PathBuf,
) -> Result<(), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let out = parent.join(format!(
        "cad-{}-{stamp}",
        if kind == Kind::Studio {
            "studio"
        } else {
            "instructions"
        }
    ));
    std::fs::create_dir(&out).map_err(|e| e.to_string())?;
    panel.output = Some(out.clone());
    panel.revision = st.snapshot.revision;
    panel.result = None;
    panel.textures.clear();
    panel.page = 0;
    panel.step_texture = None;
    panel.tray_texture = None;
    let snapshot = st.snapshot.clone();
    let size = panel.size;
    let samples = panel.samples;
    let high_quality = panel.high_quality;
    let ctx = ctx.clone();
    panel.cancelled = Arc::new(AtomicBool::new(false));
    let cancelled = panel.cancelled.clone();
    let (tx, rx) = mpsc::channel();
    panel.rx = Some(rx);
    panel.message = "Preparing complete build geometry…".into();
    std::thread::spawn(move || {
        let run = || -> Result<serde_json::Value, String> {
            let file = snapshot.model.doc.main_index().ok_or("No main model")?;
            let flat = snapshot.model.flat(file);
            let (bench, shadow) = resources();
            let parts:Vec<_>=flat.iter().map(|p|serde_json::json!({"name":p.name,"color":p.color.0,"pos":p.pos.to_array(),"rot":p.rot.to_cols_array()})).collect();
            let job = serde_json::json!({"kind":if kind==Kind::Studio {"studio"}else{"instructions"},"out":out,"bench":bench,"shadow":shadow,"parts":parts,"size":size,"samples":samples,"high_quality":high_quality});
            // Persist a reproducible, local-only snapshot manifest; never the source path.
            std::fs::write(
                out.join("job.json"),
                serde_json::to_vec_pretty(&job).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let log = out.join("worker.log");
            let script = ldraw::runtime::helper_script("publication", include_str!("publication.py"))?;
            let mut command = ldraw::runtime::python_command()?;
            ldraw::platform::no_window(&mut command);
            let mut child =
                command
                    .arg("-u")
                    .arg(&script)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::from(std::fs::File::create(&log).map_err(|e| e.to_string())?))
                    .spawn()
                    .map_err(|e| e.to_string())?;
            send_job(&mut child, &job, &log)?;
            let stdout = child.stdout.take().ok_or("Missing worker stdout")?;
            let child = Arc::new(Mutex::new(child));
            let _ = tx.send(Event::Started(child.clone()));
            ctx.request_repaint();
            let mut result = None;
            let mut error = None;
            for line in BufReader::new(stdout).lines() {
                let line = line.map_err(|e| e.to_string())?;
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(text) = value["progress"].as_str() {
                        let _ = tx.send(Event::Progress(text.into()));
                        ctx.request_repaint();
                    }
                    if let Some(text) = value["error"].as_str() {
                        error = Some(text.to_string());
                    }
                    if let Some(image) = value["image"].as_str() {
                        let _ = tx.send(Event::Image(image.into()));
                        ctx.request_repaint();
                    }
                    if value.get("result").is_some() {
                        result = Some(value["result"].clone());
                    }
                }
            }
            let status = child
                .lock()
                .map_err(|_| "Worker lock failed")?
                .wait()
                .map_err(|e| e.to_string())?;
            if !status.success() {
                // Stop/close sets `cancelled` before killing; a bare signal (unix) is also a cancel.
                return Err(error.unwrap_or_else(|| match status.code() {
                    _ if cancelled.load(Ordering::SeqCst) => "Job stopped/cancelled.".into(),
                    None => "Job stopped/cancelled.".into(),
                    Some(code) => worker_failed(&format!("The instruction worker failed (exit {code})"), &log),
                }));
            }
            result.ok_or_else(|| {
                error.unwrap_or_else(|| worker_failed("The instruction worker returned no output", &log))
            })
        };
        let _ = tx.send(Event::Finished(run()));
        ctx.request_repaint();
    });
    Ok(())
}

fn texture(path: &Path, ctx: &egui::Context) -> Result<egui::TextureHandle, String> {
    let mut decoder = png::Decoder::new(BufReader::new(
        std::fs::File::open(path).map_err(|e| e.to_string())?,
    ));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut bytes = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader.next_frame(&mut bytes).map_err(|e| e.to_string())?;
    let bytes = &bytes[..info.buffer_size()];
    let mut rgba = Vec::new();
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(bytes),
        png::ColorType::Rgb => {
            for rgb in bytes.chunks_exact(3) {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        _ => return Err("Expected RGB/RGBA render".into()),
    }
    Ok(ctx.load_texture(
        path.display().to_string(),
        egui::ColorImage::from_rgba_unmultiplied(
            [info.width as usize, info.height as usize],
            &rgba,
        ),
        Default::default(),
    ))
}

pub fn ui(st: &mut State, ui: &mut egui::Ui, kind: Kind) {
    let mut panel = if kind == Kind::Studio {
        std::mem::take(&mut st.studio)
    } else {
        std::mem::take(&mut st.instructions)
    };
    let mut finished = None;
    if let Some(rx) = &panel.rx {
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::Started(child) => panel.child = Some(child),
                Event::Progress(text) => panel.message = text,
                Event::Image(name) => {
                    if let Some(out) = &panel.output {
                        match texture(&out.join(name), ui.ctx()) {
                            Ok(t) => panel.textures.push(t),
                            Err(e) => panel.message = e,
                        }
                    }
                }
                Event::Finished(result) => finished = Some(result),
            }
        }
    }
    if let Some(result) = finished {
        panel.rx = None;
        panel.child = None;
        match result {
            Ok(result) => {
                panel.message = result["status"].as_str().unwrap_or("Done").into();
                if let (Some(out), Some(images)) = (&panel.output, result["images"].as_array()) {
                    for name in images.iter().filter_map(|n| n.as_str()) {
                        match texture(&out.join(name), ui.ctx()) {
                            Ok(t) => panel.textures.push(t),
                            Err(e) => panel.message = e,
                        }
                    }
                }
                panel.result = Some(result);
            }
            Err(error) => panel.message = error,
        }
    }
    ui.heading(if kind == Kind::Studio {
        "Studio preview"
    } else {
        "Build instructions"
    });
    if kind == Kind::Instructions {
        ui.label("Connector-aware subbuilds at side studs / joints, then reverse-disassembly insertion checks. One piece or completed subbuild per numbered step.");
        ui.colored_label(egui::Color32::from_rgb(190,130,30),"Draft instructions: sampled straight insertion checks do not prove hand access, stability, flexible clips or every hinged assembly path. Blocked plans are reported, not invented.");
    } else {
        ui.label("Four white-background angles. Fast CAD-style by default; High quality adds glossy ray tracing and soft shadows.");
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Image size");
        ui.add(
            egui::DragValue::new(&mut panel.size)
                .range(320..=1600)
                .speed(32),
        );
        ui.checkbox(&mut panel.high_quality, "High quality").on_hover_text("Instructions: 2× supersampling at the same output size. Studio: glossy ray tracing and soft shadows. Off: faster CAD-style rendering.");
        if kind == Kind::Studio && panel.high_quality {
            ui.label("Samples");
            ui.add(egui::DragValue::new(&mut panel.samples).range(1..=8));
        }
        if ui
            .add_enabled(panel.rx.is_none(), egui::Button::new("Generate…"))
            .clicked()
        {
            let parent = cache_directory();
            if let Err(e) = std::fs::create_dir_all(&parent)
                .map_err(|e| e.to_string())
                .and_then(|_| start(&mut panel, st, ui.ctx(), kind, parent))
            {
                panel.message = e;
                panel.rx = None;
            }
        }
        if panel.rx.is_some() {
            ui.spinner();
            if ui.button("Cancel").clicked() {
                panel.cancelled.store(true, Ordering::SeqCst);
                if let Some(child) = &panel.child {
                    if let Ok(mut child) = child.lock() {
                        let _ = child.kill();
                    }
                }
            }
        }
    });
    ui.label(&panel.message);
    if let Some(warnings) = panel.result.as_ref().and_then(|r| r["warnings"].as_array()) {
        for warning in warnings {
            if let Some(text) = warning.as_str() {
                ui.colored_label(egui::Color32::YELLOW, text);
            }
        }
    }
    if panel.output.is_some() && panel.revision != st.snapshot.revision {
        ui.colored_label(egui::Color32::YELLOW,"Build changed: these outputs belong to the earlier snapshot. Generate again to update.");
    }
    if let Some(out) = &panel.output {
        if panel.result.is_some() && ui.button("Export / save a copy…").clicked() {
            if let Some(parent) = rfd::FileDialog::new()
                .set_title("Export generated images / instruction document")
                .pick_folder()
            {
                panel.message = match export_bundle(out, &parent, kind) {
                    Ok(path) => format!("Exported to {}", path.display()),
                    Err(e) => format!("Export failed: {e}"),
                };
            }
        }
        if kind == Kind::Instructions
            && panel.result.is_some()
            && ui.button("Open printable instruction document").clicked()
        {
            if let Err(e) = ldraw::platform::open_path(&out.join("instructions.html")) {
                panel.message = format!("Could not open the instruction document: {e}");
            }
        }
        ui.collapsing("Cache / diagnostics", |ui| {
            if ui.button("Open cache folder").clicked() {
                if let Err(e) = ldraw::platform::open_path(out) {
                    panel.message = format!("Could not open the cache folder: {e}");
                }
            }
            ui.small(out.display().to_string());
        });
    }
    if kind == Kind::Instructions {
        if let Some(steps) = panel.result.as_ref().and_then(|r| r["steps"].as_u64()) {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(panel.page > 0, egui::Button::new("← Previous"))
                    .clicked()
                {
                    panel.page -= 1;
                }
                ui.strong(if panel.page == 0 {
                    "Cover".into()
                } else {
                    format!("Step {} / {}", panel.page, steps)
                });
                if ui
                    .add_enabled(panel.page < steps as usize, egui::Button::new("Next →"))
                    .clicked()
                {
                    panel.page += 1;
                }
            });
            ui.add(
                egui::Slider::new(&mut panel.page, 0..=steps as usize)
                    .text("Page / step (0 = cover)"),
            );
            if panel.page > 0 && panel.step_texture.as_ref().map(|v| v.0) != Some(panel.page) {
                if let Some(out) = &panel.output {
                    match texture(&out.join(format!("step-{:04}.png", panel.page)), ui.ctx()) {
                        Ok(t) => panel.step_texture = Some((panel.page, t)),
                        Err(e) => panel.message = e,
                    }
                    panel.tray_texture =
                        texture(&out.join(format!("parts-{:04}.png", panel.page)), ui.ctx())
                            .ok()
                            .map(|t| (panel.page, t));
                }
            }
        }
    }
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if kind == Kind::Instructions && panel.page > 0 {
                if let Some(page) = panel
                    .result
                    .as_ref()
                    .and_then(|r| r["pages"].get(panel.page - 1))
                {
                    ui.heading(page["title"].as_str().unwrap_or("Assembly step"));
                    if let Some((_, tray)) = &panel.tray_texture {
                        ui.image((tray.id(), egui::vec2(130., 130.)));
                    }
                    ui.label(page["parts"].as_str().unwrap_or(""));
                    ui.label(page["action"].as_str().unwrap_or(""));
                }
                if let Some((_, image)) = &panel.step_texture {
                    let side = ui.available_width().min(1000.);
                    ui.image((image.id(), egui::vec2(side, side)));
                }
                return;
            }
            for image in &panel.textures {
                let side = ui.available_width().min(1000.);
                ui.image((image.id(), egui::vec2(side, side)));
            }
        });
    if kind == Kind::Studio {
        st.studio = panel;
    } else {
        st.instructions = panel;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    #[test]
    fn worker_errors_show_the_log_tail() {
        let dir = std::env::temp_dir().join(format!("sb-log-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("worker.log");
        std::fs::write(&log, "noise\nTraceback (most recent call last):\n  File \"<string>\", line 7\nModuleNotFoundError: No module named 'PIL'\n").unwrap();
        let text = worker_failed("The instruction worker stopped before reading its job", &log);
        assert!(text.contains("No module named 'PIL'") && !text.contains("noise"), "{text}");
        #[cfg(unix)]
        let mut command = Command::new("/bin/sh");
        #[cfg(unix)]
        command.args(["-c", "echo boom >&2; exit 3"]);
        #[cfg(windows)]
        let mut command = Command::new("cmd");
        #[cfg(windows)]
        command.args(["/C", "echo boom 1>&2 & exit 3"]);
        let mut child = command.stdin(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(&log).unwrap())).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let big = serde_json::json!({"pad": "x".repeat(1 << 20)});
        let err = send_job(&mut child, &big, &log).unwrap_err();
        assert!(err.contains("boom") && err.contains("exit 3"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn export_is_a_new_copy_without_private_job_or_log() {
        let root = std::env::temp_dir().join(format!(
            "cad-publish-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("cache");
        std::fs::create_dir_all(&source).unwrap();
        for (name, bytes) in [
            ("instructions.html", "book"),
            ("step-0001.png", "image"),
            ("plan.json", "plan"),
            ("job.json", "private snapshot"),
            ("worker.log", "log"),
        ] {
            std::fs::write(source.join(name), bytes).unwrap();
        }
        let output = export_bundle(&source, &root, Kind::Instructions).unwrap();
        assert_eq!(
            std::fs::read_to_string(output.join("instructions.html")).unwrap(),
            "book"
        );
        assert!(output.join("step-0001.png").is_file());
        assert!(!output.join("job.json").exists());
        assert!(!output.join("worker.log").exists());
        assert!(source.join("job.json").is_file());
        for folder in [&source, &output] {
            for e in std::fs::read_dir(folder).unwrap() {
                std::fs::remove_file(e.unwrap().path()).unwrap();
            }
            std::fs::remove_dir(folder).unwrap();
        }
        std::fs::remove_dir(root).unwrap();
    }
}
