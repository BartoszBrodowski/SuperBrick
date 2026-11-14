//! SuperBrick development commands, run as `sb-dev <cmd>` or `sb <cmd>`.

use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};

use glam::DVec3;
use ldraw::{Color, Document, Library, Lod};
use rayon::prelude::*;

mod product;
mod relbench;
mod search_bench;
mod symmetry;

pub type Res<T> = Result<T, Box<dyn Error>>;

const USAGE: &str = "usage:
  sb-dev stats                         open the LDraw library and print counts and timings
  sb-dev validate MODEL                conservative native validator (partial connector port)
  sb-dev parity MODEL                  compare native statuses/groups/link types with Python; fail on differences
  sb-dev search QUERY...               local part search
  sb-dev search-eval                    evaluate the unchanged search golden corpus
  sb-dev search-sets [REBRICKABLE_DIR]  build out/sets.bin (element IDs, set inventories)
  sb-dev commands                      shared UI/MCP command registry
  sb-dev apply MODEL --ops JSON --out NEWFILE  explicit user transactions; refuses existing output
  sb-dev roundtrip <files|dirs>...     check that .ldr/.mpd files re-save byte-identical, and re-parse after reformatting
  sb-dev flatten <model> [--low]       flatten a model and mesh its parts; print counts and timings
  sb-dev mesh <part> [--low]           print one part's flattened geometry summary
  sb-dev relbench [golden.json]...       time physics::relation against the python bench, pair by pair
  sb-dev symmetry <model> [--axis x|y|z] [--plane P] [--min LDU3] [--probe K] [--inside] [--json]
                                       solid regions with no mirror image across the plane (default: z, model middle);
                                       --inside compares whole filled part volumes, the inner structure included
  sb-dev synth --parts N --out FILE [--seed S]   write a synthetic bonded-brick city of N parts
  sb-dev render <model> [--out FILE.png] [--size 1600x1000] [--studs baked|instanced|culled|all]
             [--no-edges] [--low] [--frames N]
             [--zoom F] [--yaw DEG] [--pitch DEG] [--target X,Y,Z] [--ortho]   offscreen GPU render: PNG, timings, orbit frame times";

/// Development subcommands `sb` hands to [`run`].
pub const SUBCOMMANDS: &[&str] = &[
    "stats", "roundtrip", "flatten", "mesh", "synth", "render", "relbench", "validate", "parity",
    "symmetry", "search", "search-eval", "search-bench", "search-sets", "commands", "apply", "mcp-stdio",
];

/// Whether `sb` should hand these arguments to [`run`] rather than open the editor.
pub fn handles(args: &[String]) -> bool {
    args.first().is_some_and(|a| SUBCOMMANDS.contains(&a.as_str()))
}

/// Run a development subcommand (`args` excludes the program name).
pub fn run(args: &[String]) -> ExitCode {
    let rest = args.get(1..).unwrap_or(&[]);
    let r = match args.first().map(String::as_str) {
        Some("stats") => cmd_stats(),
        Some("roundtrip") => cmd_roundtrip(rest),
        Some("flatten") => cmd_flatten(rest),
        Some("mesh") => cmd_mesh(rest),
        Some("synth") => cmd_synth(rest),
        Some("render") => cmd_render(rest),
        Some("symmetry") => symmetry::cmd_symmetry(rest),
        Some("relbench") => relbench::cmd_relbench(rest),
        Some("validate") => product::validate(rest),
        Some("parity") => product::parity(rest),
        Some("search") => product::search(rest),
        Some("search-eval") => product::search_eval(),
        Some("search-bench") => search_bench::run(rest),
        Some("search-sets") => search_sets(rest),
        Some("commands") => product::commands(),
        Some("apply") => product::apply(rest),
        Some("mcp-stdio") => Err("mcp-stdio was removed: use `sb mcp --user NAME [PROJECT.sb]`".into()),
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn ms(t: Instant) -> String {
    format!("{:.1} ms", t.elapsed().as_secs_f64() * 1e3)
}

fn flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

pub fn positional(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if matches!(
            a.as_str(),
            "--parts" | "--out" | "--seed" | "--size" | "--studs" | "--frames"
        ) {
            skip = true;
        } else if !a.starts_with("--") {
            out.push(a.as_str());
        }
    }
    out
}

fn cmd_stats() -> Res<bool> {
    let t = Instant::now();
    let lib = Library::open_default()?;
    println!(
        "library   {}  ({} files, opened in {})",
        lib.zip_path().display(),
        lib.len(),
        ms(t)
    );
    println!("parts     {}", lib.part_names().count());
    println!("colours   {}", lib.colors.by_code.len());
    println!("stamp     {:016x}", lib.fingerprint());
    let t = Instant::now();
    let names: Vec<&str> = lib.part_names().collect();
    let descs: usize = names
        .par_iter()
        .map(|n| lib.first_line(&format!("parts/{n}")).map_or(0, |l| l.len()))
        .sum();
    println!("first lines of all parts: {} bytes in {}", descs, ms(t));
    Ok(true)
}

fn collect_models(paths: &[&str]) -> Vec<PathBuf> {
    fn walk(p: &Path, out: &mut Vec<PathBuf>) {
        if p.is_dir() {
            if let Ok(rd) = std::fs::read_dir(p) {
                let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
                v.sort();
                for c in v {
                    walk(&c, out);
                }
            }
        } else if p
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "ldr" | "mpd"))
        {
            out.push(p.to_path_buf());
        }
    }
    let mut out = Vec::new();
    for p in paths {
        walk(Path::new(p), &mut out);
    }
    out
}

fn cmd_roundtrip(args: &[String]) -> Res<bool> {
    let files = collect_models(&positional(args));
    if files.is_empty() {
        return Err("no .ldr/.mpd files given".into());
    }
    let (mut ok, mut parts) = (true, 0usize);
    for f in &files {
        let bytes = std::fs::read(f)?;
        let name = f.file_name().unwrap().to_string_lossy();
        let doc = Document::parse(&bytes, &name);
        if doc.to_bytes() != bytes {
            ok = false;
            println!(
                "FAIL  {}: untouched save is not byte-identical",
                f.display()
            );
            continue;
        }
        let mut edited = doc.clone();
        edited.touch_all_parts();
        let again = Document::parse(&edited.to_bytes(), &name);
        let a: Vec<_> = doc
            .files
            .iter()
            .flat_map(|m| {
                m.parts()
                    .map(|(_, p, s)| (p.clone(), s))
                    .collect::<Vec<_>>()
            })
            .collect();
        let b: Vec<_> = again
            .files
            .iter()
            .flat_map(|m| {
                m.parts()
                    .map(|(_, p, s)| (p.clone(), s))
                    .collect::<Vec<_>>()
            })
            .collect();
        let same = a.len() == b.len()
            && a.iter().zip(&b).all(|((p, s), (q, t))| {
                p.name == q.name
                    && p.color == q.color
                    && s == t
                    && p.pos.abs_diff_eq(q.pos, 1e-5)
                    && p.rot.abs_diff_eq(q.rot, 1e-5)
            });
        if !same {
            ok = false;
            println!(
                "FAIL  {}: reformatted part lines do not parse back to the same parts",
                f.display()
            );
            continue;
        }
        parts += a.len();
    }
    println!(
        "{} {} files, {} part lines",
        if ok { "ok  " } else { "FAIL" },
        files.len(),
        parts
    );
    Ok(ok)
}

fn cmd_flatten(args: &[String]) -> Res<bool> {
    let pos = positional(args);
    let path = Path::new(pos.first().ok_or("give a model file")?);
    let lod = if flag(args, "--low") {
        Lod::Low
    } else {
        Lod::Std
    };
    let t = Instant::now();
    let lib = Library::open_default()?;
    println!("library opened        {}", ms(t));
    let t = Instant::now();
    let doc = Document::load(path)?;
    println!(
        "parsed                {}  ({} files, {} part lines)",
        ms(t),
        doc.files.len(),
        doc.line_part_count()
    );
    let t = Instant::now();
    let flat = doc.flatten(path.parent());
    println!("flattened             {}  ({} parts)", ms(t), flat.len());
    let unique: Vec<String> = {
        let mut seen = HashSet::new();
        flat.iter()
            .map(|p| ldraw::canonical_name(&p.name))
            .filter(|n| seen.insert(n.clone()))
            .collect()
    };
    let t = Instant::now();
    let meshes: Vec<(String, Option<std::sync::Arc<ldraw::Mesh>>)> = unique
        .par_iter()
        .map(|n| (n.clone(), lib.mesh(n, lod).ok()))
        .collect();
    println!("meshed unique parts   {}  ({} unique)", ms(t), unique.len());
    let missing: Vec<&str> = meshes
        .iter()
        .filter(|(_, m)| m.is_none())
        .map(|(n, _)| n.as_str())
        .collect();
    let by: std::collections::HashMap<&str, &ldraw::Mesh> = meshes
        .iter()
        .filter_map(|(n, m)| m.as_deref().map(|m| (n.as_str(), m)))
        .collect();
    let (mut tris, mut stud_tris, mut studs, mut lines) = (0usize, 0usize, 0usize, 0usize);
    for p in &flat {
        if let Some(m) = by.get(ldraw::canonical_name(&p.name).as_str()) {
            tris += m.tris.len();
            stud_tris += m.tstud.iter().filter(|&&s| s).count();
            studs += m.studs.iter().filter(|s| s.outer).count();
            lines += m.lines.len();
        }
    }
    let uniq_tris: usize = by.values().map(|m| m.tris.len()).sum();
    println!(
        "instanced totals      {tris} triangles ({stud_tris} in studs), {studs} studs, {lines} edge lines"
    );
    println!("unique-part triangles {uniq_tris}");
    if !missing.is_empty() {
        println!("missing parts ({}): {}", missing.len(), missing.join(", "));
    }
    Ok(true)
}

fn cmd_mesh(args: &[String]) -> Res<bool> {
    let pos = positional(args);
    let name = pos.first().ok_or("give a part name")?;
    let lod = if flag(args, "--low") {
        Lod::Low
    } else {
        Lod::Std
    };
    let lib = Library::open_default()?;
    let t = Instant::now();
    let m = lib.mesh(name, lod)?;
    let (lo, hi) = m.bbox(true);
    println!("{}  {}", ldraw::canonical_name(name), lib.description(name));
    println!("meshed in {}", ms(t));
    println!(
        "triangles {} ({} in studs), lines {}, conditional {}",
        m.tris.len(),
        m.tstud.iter().filter(|&&s| s).count(),
        m.lines.len(),
        m.cond.len()
    );
    println!("bbox (no studs) {:?} .. {:?}", lo.to_array(), hi.to_array());
    let mut hist: BTreeMap<i32, usize> = BTreeMap::new();
    for c in &m.tcol {
        *hist.entry(c.0).or_default() += 1;
    }
    println!("triangle colours {hist:?}");
    for s in &m.studs {
        println!(
            "stud {:<16} at {:?} dir {:?}{}{}",
            ldraw::MALE_STUDS[s.prim as usize],
            s.pos.to_array(),
            s.dir.to_array(),
            if s.hollow { " hollow" } else { "" },
            if s.outer { "" } else { " (nested)" }
        );
    }
    Ok(true)
}

/// Small deterministic generator (xorshift64*).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// A city of bonded 4x4-stud towers on one stud lattice. Each layer is two 2x4 bricks, two 2x4 plates or
/// four 2x2 bricks, turning 90° every layer so joints never line up; towers end in 2x2 tiles.
/// Neighbouring towers touch side to side. One step per tower.
fn synth(n: usize, seed: u64) -> String {
    let mut rng = Rng(seed.max(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let palette = [4, 1, 14, 15, 71, 72, 0, 2, 19, 70, 25, 28];
    let side = ((n as f64 / 30.0).sqrt().ceil() as i64).max(1);
    let ident = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let yaw90 = [0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0];
    let mut out =
        format!("0 Synthetic city, {n} parts\r\n0 Name: synth.ldr\r\n0 Author: cad synth\r\n");
    let mut count = 0;
    let put =
        |out: &mut String, count: &mut usize, col: i32, p: DVec3, r: &[f64; 9], part: &str| {
            if *count >= n {
                return;
            }
            let mut s = format!("1 {col}");
            for x in p.to_array().iter().chain(r.iter()) {
                s.push(' ');
                s.push_str(&ldraw::num::fmt_num(*x));
            }
            out.push_str(&format!("{s} {part}\r\n"));
            *count += 1;
        };
    'towers: for tz in 0..side * 4 {
        for tx in 0..side {
            if count >= n {
                break 'towers;
            }
            let (cx, cz) = (tx as f64 * 80.0 + 40.0, tz as f64 * 80.0 + 40.0);
            let layers = 6 + rng.below(14) as usize;
            let mut bottom = 0.0; // LDraw y of the layer's underside (up is -y)
            let base = palette[rng.below(palette.len() as u64) as usize];
            for k in 0..layers {
                let col = if rng.below(4) == 0 {
                    palette[rng.below(palette.len() as u64) as usize]
                } else {
                    base
                };
                let turned = k % 2 == 1;
                match rng.below(5) {
                    0 => {
                        // two 2x4 plates
                        for d in [-20.0, 20.0] {
                            let (p, r) = if turned {
                                (DVec3::new(cx + d, bottom - 8.0, cz), &yaw90)
                            } else {
                                (DVec3::new(cx, bottom - 8.0, cz + d), &ident)
                            };
                            put(&mut out, &mut count, col, p, r, "3020.dat");
                        }
                        bottom -= 8.0;
                    }
                    1 => {
                        // four 2x2 bricks
                        for (dx, dz) in [(-20.0, -20.0), (20.0, -20.0), (-20.0, 20.0), (20.0, 20.0)]
                        {
                            put(
                                &mut out,
                                &mut count,
                                col,
                                DVec3::new(cx + dx, bottom - 24.0, cz + dz),
                                &ident,
                                "3003.dat",
                            );
                        }
                        bottom -= 24.0;
                    }
                    _ => {
                        // two 2x4 bricks
                        for d in [-20.0, 20.0] {
                            let (p, r) = if turned {
                                (DVec3::new(cx + d, bottom - 24.0, cz), &yaw90)
                            } else {
                                (DVec3::new(cx, bottom - 24.0, cz + d), &ident)
                            };
                            put(&mut out, &mut count, col, p, r, "3001.dat");
                        }
                        bottom -= 24.0;
                    }
                }
            }
            let top = palette[rng.below(palette.len() as u64) as usize];
            for (dx, dz) in [(-20.0, -20.0), (20.0, -20.0), (-20.0, 20.0), (20.0, 20.0)] {
                put(
                    &mut out,
                    &mut count,
                    top,
                    DVec3::new(cx + dx, bottom - 8.0, cz + dz),
                    &ident,
                    "3068b.dat",
                );
            }
            out.push_str("0 STEP\r\n");
        }
    }
    out
}

fn cmd_synth(args: &[String]) -> Res<bool> {
    let n: usize = opt(args, "--parts")
        .ok_or("--parts N is required")?
        .parse()?;
    let out = PathBuf::from(opt(args, "--out").ok_or("--out FILE is required")?);
    let seed: u64 = opt(args, "--seed").map_or(Ok(1), str::parse)?;
    let text = synth(n, seed);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&out, &text)?;
    let doc = Document::parse(text.as_bytes(), "synth.ldr");
    let colours: HashSet<Color> = doc.flatten(None).iter().map(|p| p.color).collect();
    println!(
        "wrote {} ({} parts, {} colours)",
        out.display(),
        doc.line_part_count(),
        colours.len()
    );
    Ok(true)
}

fn cmd_render(args: &[String]) -> Res<bool> {
    use render::{Camera, Gpu, Offscreen, Renderer, Scene, SceneOptions, StudMode};
    let t_start = Instant::now();
    let pos = positional(args);
    let path = Path::new(pos.first().ok_or("give a model file")?);
    let modes: Vec<StudMode> = match opt(args, "--studs").unwrap_or("all") {
        "all" => StudMode::ALL.to_vec(),
        s => vec![StudMode::parse(s).ok_or("--studs is baked, instanced, culled or all")?],
    };
    let (w, h) = match opt(args, "--size") {
        Some(s) => {
            let (a, b) = s.split_once('x').ok_or("--size is WxH")?;
            (a.parse::<u32>()?, b.parse::<u32>()?)
        }
        None => (1600, 1000),
    };
    let frames: u32 = opt(args, "--frames").map_or(Ok(60), str::parse)?;
    let lod = if flag(args, "--low") {
        Lod::Low
    } else {
        Lod::Std
    };
    let out = opt(args, "--out").map(PathBuf::from);

    let t = Instant::now();
    let lib = Library::open_default()?;
    let lib_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let doc = Document::load(path)?;
    let flat = doc.flatten(path.parent());
    let load_ms = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let gpu = Gpu::headless()?;
    let gpu_ms = t.elapsed().as_secs_f64() * 1e3;
    println!("{}  ({} parts)", path.display(), flat.len());
    println!(
        "gpu {} ({:?})  |  library {lib_ms:.0} ms, parse+flatten {load_ms:.0} ms, device {gpu_ms:.0} ms",
        gpu.info.name, gpu.info.backend
    );
    println!(
        "{:<10} {:>9} {:>9} {:>9} {:>8} {:>7} {:>6} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "studs",
        "tris",
        "lines",
        "cond",
        "studs",
        "culled",
        "draws",
        "atlasMB",
        "mesh",
        "build",
        "upload",
        "first",
        "p50",
        "p95"
    );
    let mut first_total = None;
    let mut first_px: Option<(&str, Vec<u8>)> = None;
    for mode in &modes {
        let opts = SceneOptions {
            lod,
            stud_lod: lod,
            studs: *mode,
            edges: !flag(args, "--no-edges"),
            ..Default::default()
        };
        let scene = Scene::build(&lib, &flat, &opts);
        let t = Instant::now();
        let r = Renderer::new(&gpu, render::COLOR_FORMAT, 4, &scene);
        let off = Offscreen::new(&gpu, 4, w, h);
        gpu.wait();
        let upload_ms = t.elapsed().as_secs_f64() * 1e3;
        let mut cam = Camera::framing(scene.bounds.0, scene.bounds.1, w as f32 / h as f32);
        if let Some(z) = opt(args, "--zoom") {
            cam.dist *= z.parse::<f32>()?;
        }
        if let Some(v) = opt(args, "--yaw") {
            cam.yaw = v.parse::<f32>()?.to_radians();
        }
        if let Some(v) = opt(args, "--pitch") {
            cam.pitch = v.parse::<f32>()?.to_radians();
        }
        if let Some(v) = opt(args, "--target") {
            let c: Vec<f32> = v.split(',').map(str::parse).collect::<Result<_, _>>()?;
            cam.target = glam::Vec3::new(c[0], c[1], c[2]);
        }
        cam.ortho = flag(args, "--ortho");
        let t = Instant::now();
        off.render(&gpu, &r, &cam);
        let px = off.read_rgba(&gpu);
        let first_ms = t.elapsed().as_secs_f64() * 1e3;
        first_total.get_or_insert(t_start.elapsed().as_secs_f64() * 1e3);
        if let Some(o) = &out {
            let p = if modes.len() > 1 {
                o.with_file_name(format!(
                    "{}-{}.png",
                    o.file_stem().unwrap().to_string_lossy(),
                    mode.name()
                ))
            } else {
                o.clone()
            };
            render::save_png(&p, w, h, &px)?;
        }
        let times = render::bench_orbit(&gpu, &r, &off, &cam, frames);
        let mut l = times.latency_ms.clone();
        l.sort_by(f64::total_cmp);
        let pct = |q: f64| {
            l.get(((l.len() as f64 - 1.0) * q).round() as usize)
                .copied()
                .unwrap_or(0.0)
        };
        let s = &scene.stats;
        println!(
            "{:<10} {:>9} {:>9} {:>9} {:>8} {:>7} {:>6} {:>7.1} {:>7.1} {:>7.1} {:>7.1} {:>7.1} {:>7.2} {:>7.2}   back-to-back {:.2} ms",
            mode.name(),
            s.tris,
            s.lines,
            s.cond,
            s.stud_instances,
            s.studs_culled,
            s.draw_calls,
            s.atlas_bytes as f64 / 1e6,
            s.mesh_ms,
            s.build_ms,
            upload_ms,
            first_ms,
            pct(0.5),
            pct(0.95),
            times.throughput_ms
        );
        if !s.missing.is_empty() {
            println!("  missing ({}): {}", s.missing.len(), s.missing.join(", "));
        }
        // Instancing and culling must not change the picture: count pixels that differ from the first mode.
        match &first_px {
            None => first_px = Some((mode.name(), px)),
            Some((name, a)) => {
                let differ = a
                    .chunks_exact(4)
                    .zip(px.chunks_exact(4))
                    .filter(|(p, q)| (0..3).any(|i| p[i].abs_diff(q[i]) > 8))
                    .count();
                println!(
                    "  vs {name}: {differ} of {} pixels differ ({:.3}%)",
                    w * h,
                    differ as f64 * 100.0 / (w * h) as f64
                );
                if let (Some(o), true) = (&out, differ > 0) {
                    // Dimmed first-mode image with differing pixels in magenta.
                    let mut img = a.clone();
                    for (p, q) in img.chunks_exact_mut(4).zip(px.chunks_exact(4)) {
                        let hit = (0..3).any(|i| p[i].abs_diff(q[i]) > 8);
                        for i in 0..3 {
                            p[i] = if hit {
                                [255, 0, 255][i]
                            } else {
                                128 + p[i] / 2
                            };
                        }
                    }
                    let d = o.with_file_name(format!(
                        "{}-diff-{}.png",
                        o.file_stem().unwrap().to_string_lossy(),
                        mode.name()
                    ));
                    render::save_png(&d, w, h, &img)?;
                    println!("  wrote {}", d.display());
                }
            }
        }
    }
    println!(
        "process start to first frame on screen-ready pixels: {:.0} ms",
        first_total.unwrap_or(0.0)
    );
    Ok(true)
}

/// Build out/sets.bin (element IDs and set inventories) from Rebrickable's CSV dumps.
fn search_sets(args: &[String]) -> Result<bool, Box<dyn std::error::Error>> {
    let data = search::data_dir();
    let rb = args
        .first()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| data.join("data/rebrickable"));
    let t = std::time::Instant::now();
    let lib = ldraw::Library::open_default().map_err(|e| e.to_string())?;
    let cat: search::catalog::Catalog =
        serde_json::from_slice(&std::fs::read(data.join("out/catalog.json"))?)?;
    let (bytes, stats) = search::sets::build(&lib, &rb, &cat)?;
    let out = data.join("out/sets.bin");
    std::fs::write(&out, &bytes)?;
    println!("{}", serde_json::to_string_pretty(&stats)?);
    println!(
        "{}: {:.1} MB in {:.1} s",
        out.display(),
        bytes.len() as f64 / 1e6,
        t.elapsed().as_secs_f64()
    );
    Ok(true)
}
