//! SuperBrick (`sb`) — the LDraw desktop editor, its headless operations and the agent server.
//!
//! `sb [PROJECT.sb|MODEL.ldr] [--size WxH] [--bench FRAMES] [--shot FILE.png] [--select ID,ID…]`
//! `sb <dev-command> ...` runs a development command (see `sb-dev`).

mod app;
mod autosave;
mod brand;
mod home;
mod inspector;
mod interaction;
mod lasso;
mod mechanical;
mod open_files;
mod panels;
mod placement_worker;
mod project;
mod setup;
mod publication;
mod suppliers;
mod view;
mod visibility;

use std::{path::PathBuf, sync::Arc, time::Instant};

pub struct Args {
    pub file: Option<PathBuf>,
    pub size: [f32; 2],
    /// Orbit for this many UI frames, print UI-loop intervals, optionally capture, then quit.
    pub bench: Option<u32>,
    pub shot: Option<PathBuf>,
    pub window_shot: Option<PathBuf>,
    pub submodel: Option<String>,
    /// Part ids to select once the file has opened (for screenshots and smoke tests).
    pub select: Vec<u64>,
    pub suppliers: bool,
    pub instructions: bool,
    pub studio: bool,
    pub check: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        file: None,
        size: [1500.0, 900.0],
        bench: None,
        shot: None,
        window_shot: None,
        submodel: None,
        select: Vec::new(),
        suppliers: false,
        instructions: false,
        studio: false,
        check: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--bench" => a.bench = it.next().and_then(|v| v.parse().ok()),
            "--shot" => a.shot = it.next().map(PathBuf::from),
            "--window-shot" => a.window_shot = it.next().map(PathBuf::from),
            "--open-submodel" => a.submodel = it.next(),
            "--select" => {
                a.select = it
                    .next()
                    .map(|v| v.split(',').filter_map(|id| id.trim().trim_start_matches('#').parse().ok()).collect())
                    .unwrap_or_default()
            }
            "--suppliers" => a.suppliers = true,
            "--instructions" => a.instructions = true,
            "--studio" => a.studio = true,
            "--check" => a.check = true,
            "--size" => {
                if let Some((w, h)) = it.next().as_deref().and_then(|v| v.split_once('x')) {
                    a.size = [w.parse().unwrap_or(1500.0), h.parse().unwrap_or(900.0)];
                }
            }
            _ => a.file = Some(PathBuf::from(s)),
        }
    }
    a
}

fn main() -> eframe::Result {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if ops::front::handles(&argv) {
        std::process::exit(ops::front::run(&argv));
    }
    if cli::handles(&argv) {
        std::process::exit(match cli::run(&argv) {
            c if c == std::process::ExitCode::SUCCESS => 0,
            _ => 1,
        });
    }
    if std::env::args().any(|a| a == "--help" || a == "-h") {
        println!(
            "Panels: --instructions (draft build planner), --studio (ray-cast previews), --check (read-only diagnostic on open)"
        );
        println!(
            "Headless: sb MODEL --generate-instructions [--output NEW_DIRECTORY] [--image-size 512] [--high-quality] [--plan-only]"
        );
        println!(
            "sb [FILE.sb|FILE.ldr|FILE.mpd]\n\nOpen a LEGO model in SuperBrick, or start a new model.\nRelative paths are resolved from your current directory.\n\nProjects (.sb) and agents:\n  sb --new NAME [--import MODEL.ldr]           Create NAME.sb\n  sb P.sb --user NAME --op '{{\"tool\":...}}'      Run a tool (or: sb P.sb --user NAME TOOL 'JSON')\n  sb P.sb --user NAME --ops @ops.json          Run several tools in order\n  sb P.sb export [-o OUT.ldr]                  Write the model as LDraw\n  sb mcp --user NAME [P.sb]                    MCP server for AI agents (stdio)\n  sb tools [TOOL]                              Describe the tools\n  sb setup [--library] [--python]              Check or download the parts library / Python helpers\n  (--user human changes the model directly; any other name proposes drafts)\n\n  --suppliers    Open the full-build parts list and supplier exports\n  --help, -h     Show this help\n  --version      Show version\n\nDevelopment commands: sb {{stats|validate|search|render|apply|...}} (same as sb-dev)"
        );
        return Ok(());
    }
    if std::env::args().any(|a| a == "--version") {
        println!("sb (SuperBrick) {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if std::env::args().any(|a| a == "--generate-instructions") {
        if let Err(error) = publication::generate_cli(&std::env::args().skip(1).collect::<Vec<_>>())
        {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return Ok(());
    }
    if std::env::args().any(|a| a == "--mcp-stdio") {
        eprintln!("--mcp-stdio was removed: agents now work on project files. Use `sb mcp --user NAME [PROJECT.sb]`.");
        std::process::exit(2);
    }
    let started = Instant::now();
    let args = parse_args();
    // Windows: opening a file from Explorer or a shortcut while SuperBrick runs hands it to that
    // window instead of starting another (macOS does the same through Apple events). Those
    // launches go through SuperBrick.exe, which sets SB_SHELL_LAUNCH; `sb FILE` typed in a
    // terminal opens a new window, as on macOS. Headless runs never forward.
    if let Some(file) = &args.file {
        let headless = args.bench.is_some() || args.shot.is_some() || args.window_shot.is_some();
        let shell = std::env::var_os("SB_SHELL_LAUNCH").is_some_and(|v| v == "1");
        if cfg!(windows) && shell && !headless && ldraw::single_instance::forward(std::slice::from_ref(file)) {
            return Ok(());
        }
    }
    let mut viewport = egui::ViewportBuilder::default();
    if let Some(icon) = brand::icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_inner_size(args.size)
            .with_title(ldraw::APP_NAME)
            .with_app_id("superbrick")
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: wgpu_config(),
        ..Default::default()
    };
    open_files::install();
    if ldraw::runtime::library_zip().is_none() {
        let mut options = options;
        options.viewport = options.viewport.with_inner_size([760.0, 520.0]);
        return eframe::run_native(
            ldraw::APP_NAME,
            options,
            Box::new(|cc| Ok(Box::new(setup::Setup::new(cc)))),
        );
    }
    eframe::run_native(
        ldraw::APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, args, started)))),
    )
}

/// The renderer wants the adapter's own limits (big vertex and storage buffers), not wgpu's defaults.
fn wgpu_config() -> egui_wgpu::WgpuConfiguration {
    let mut cfg = egui_wgpu::WgpuConfiguration::default();
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut cfg.wgpu_setup {
        setup.power_preference = wgpu::PowerPreference::HighPerformance;
        setup.device_descriptor = Arc::new(|adapter: &wgpu::Adapter| wgpu::DeviceDescriptor {
            label: Some("superbrick"),
            required_limits: adapter.limits(),
            ..Default::default()
        });
    }
    cfg
}

use eframe::egui_wgpu;
