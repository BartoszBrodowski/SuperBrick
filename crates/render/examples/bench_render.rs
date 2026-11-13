//! Warm `RenderService` timings, per stage.
//!
//!     cargo run --release -p render --example bench_render [-- MODEL.ldr ... ] [--iters N]
//!
//! With no model given it renders `/Users/lev/projects/np-claude/np.ldr` (if present) and a
//! synthetic 5000-part city. Each model is rendered once cold, then `N` (default 20) times warm in
//! two layouts (one 1024x768 view; four 512x384 views in a 2x2 composite), moving one part and
//! selecting it before every call the way an agent edit would. For comparison it also times the
//! old one-shot path (`Scene::build` + `Renderer::new` + a fresh target + default PNG) on the same
//! device.

use std::path::Path;
use std::time::Instant;

use glam::DVec3;
use ldraw::{Document, FlatPart, Library};
use render::{
    COLOR_FORMAT, Camera, FLAG_SELECTED, Offscreen, Panel, Rect, RenderService, Renderer, Scene,
    SceneFrame, SceneOptions,
};

const SIZE: (u32, u32) = (1024, 768);

fn main() {
    let mut paths = Vec::new();
    let mut iters = 20usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--iters" {
            iters = args.next().and_then(|n| n.parse().ok()).expect("--iters N");
        } else {
            paths.push(a);
        }
    }
    let mut models: Vec<(String, Vec<FlatPart>)> = Vec::new();
    let defaults = paths.is_empty();
    if defaults {
        let np = "/Users/lev/projects/np-claude/np.ldr";
        if Path::new(np).exists() {
            paths.push(np.into());
        } else {
            eprintln!("{np} not found; skipping it");
        }
    }
    for p in &paths {
        let doc = Document::load(Path::new(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
        let name = Path::new(p).file_name().unwrap().to_string_lossy().into_owned();
        models.push((name, doc.flatten(None)));
    }
    if defaults {
        let text = synth(5000, 1);
        models.push(("synth5000".into(), Document::parse(text.as_bytes(), "synth.ldr").flatten(None)));
    }

    let t = Instant::now();
    let lib = Library::open_default().expect("LDraw library");
    println!("library open        {:8.1} ms", ms(t));
    let t = Instant::now();
    let svc = RenderService::new().expect("GPU");
    println!("device (once)       {:8.1} ms  [{}]", ms(t), svc.gpu().info.name);
    let t = Instant::now();
    svc.warm_up(&lib);
    println!("warm-up (once)      {:8.1} ms  pipelines + first frame", ms(t));

    for (name, flat) in &mut models {
        println!("\n== {name}: {} parts ==", flat.len());
        let t = Instant::now();
        svc.render_framed(&lib, flat, &[], SIZE, &Default::default(), single).unwrap();
        let st = svc.last_stats();
        println!(
            "cold first call     {:8.1} ms  (mesh loading + atlas packing; {} unique parts, {} tris, atlas {:.1} MB)",
            ms(t),
            st.unique_parts,
            st.tris,
            st.atlas_bytes as f64 / 1e6
        );
        old_path(&svc, &lib, flat);
        for (layout, panels) in [
            ("1 view 1024x768", single as fn(&SceneFrame) -> Vec<Panel>),
            ("2x2 views 1024x768", grid),
        ] {
            let mut rows: Vec<[f64; 6]> = Vec::new();
            let mut png_bytes = 0;
            for i in 0..iters {
                // An edit: nudge one part up or down a plate and select it.
                let k = (i * 7919) % flat.len();
                flat[k].pos += DVec3::new(0.0, if i % 2 == 0 { -8.0 } else { 8.0 }, 0.0);
                let flags = [(k as u32, FLAG_SELECTED)];
                let t = Instant::now();
                let img = svc.render_framed(&lib, flat, &flags, SIZE, &Default::default(), panels).unwrap();
                let tm = svc.last_timings();
                let te = Instant::now();
                let png = render::encode_png_fast(&img);
                let encode = ms(te);
                png_bytes = png.len();
                rows.push([tm.sync_ms, tm.upload_ms, tm.render_ms, tm.readback_ms, encode, ms(t)]);
            }
            println!("-- {layout}, {iters} warm calls, png {:.0} KB --", png_bytes as f64 / 1024.0);
            println!("    stage        p50 ms   p95 ms");
            for (c, stage) in ["sync", "upload", "render", "readback", "encode", "total"].iter().enumerate() {
                let mut v: Vec<f64> = rows.iter().map(|r| r[c]).collect();
                v.sort_by(f64::total_cmp);
                println!("    {stage:<10} {:8.2} {:8.2}", pct(&v, 0.50), pct(&v, 0.95));
            }
        }
    }
}

fn single(f: &SceneFrame) -> Vec<Panel> {
    let aspect = SIZE.0 as f32 / SIZE.1 as f32;
    vec![Panel { camera: Camera::framing(f.bounds.0, f.bounds.1, aspect), rect: Rect::full(SIZE) }]
}

fn grid(f: &SceneFrame) -> Vec<Panel> {
    let (lo, hi) = f.bounds;
    let radius = (hi - lo).length() * 0.5;
    ["front", "right", "top", "iso"]
        .iter()
        .zip(Rect::grid(SIZE, 2, 2, 0))
        .map(|(v, rect)| {
            let (yaw, pitch) = Camera::preset(v).unwrap();
            Panel { camera: Camera::framing_subset(lo, hi, rect.aspect(), yaw, pitch, radius), rect }
        })
        .collect()
}

/// The pre-service recipe (what `agent` and `cli` did per call), minus device creation.
fn old_path(svc: &RenderService, lib: &Library, flat: &[FlatPart]) {
    let gpu = &**svc.gpu();
    let t = Instant::now();
    let scene = Scene::build(lib, flat, &SceneOptions::default());
    let t_scene = ms(t);
    let t1 = Instant::now();
    let r = Renderer::new(gpu, COLOR_FORMAT, 4, &scene);
    let t_pipes = ms(t1);
    let t2 = Instant::now();
    let cam = Camera::framing(scene.bounds.0, scene.bounds.1, SIZE.0 as f32 / SIZE.1 as f32);
    let target = Offscreen::new(gpu, 4, SIZE.0, SIZE.1);
    target.render(gpu, &r, &cam);
    let rgba = target.read_rgba(gpu);
    let t_draw = ms(t2);
    let t3 = Instant::now();
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, SIZE.0, SIZE.1);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&rgba).unwrap();
    }
    let t_png = ms(t3);
    println!(
        "old one-shot path   {:8.1} ms  (scene {t_scene:.1} + renderer {t_pipes:.1} + target/draw/read {t_draw:.1} + png {t_png:.1}; plus ~0.5 s device per call)",
        ms(t)
    );
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn pct(sorted: &[f64], q: f64) -> f64 {
    let i = ((sorted.len() as f64 * q).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[i]
}

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

/// The same city as `sb synth N --seed S` (a copy of the CLI generator, kept here so the
/// benchmark does not depend on the CLI crate).
fn synth(n: usize, seed: u64) -> String {
    let mut rng = Rng(seed.max(1).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let palette = [4, 1, 14, 15, 71, 72, 0, 2, 19, 70, 25, 28];
    let side = ((n as f64 / 30.0).sqrt().ceil() as i64).max(1);
    let ident = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let yaw90 = [0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0];
    let mut out = format!("0 Synthetic city, {n} parts\r\n0 Name: synth.ldr\r\n0 Author: cad synth\r\n");
    let mut count = 0;
    let put = |out: &mut String, count: &mut usize, col: i32, p: DVec3, r: &[f64; 9], part: &str| {
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
            let mut bottom = 0.0;
            let base = palette[rng.below(palette.len() as u64) as usize];
            for k in 0..layers {
                let col = if rng.below(4) == 0 { palette[rng.below(palette.len() as u64) as usize] } else { base };
                let turned = k % 2 == 1;
                let (part, h, cells): (&str, f64, &[(f64, f64)]) = match rng.below(5) {
                    0 => ("3020.dat", 8.0, &[(0.0, -20.0), (0.0, 20.0)]),
                    1 => ("3003.dat", 24.0, &[(-20.0, -20.0), (20.0, -20.0), (-20.0, 20.0), (20.0, 20.0)]),
                    _ => ("3001.dat", 24.0, &[(0.0, -20.0), (0.0, 20.0)]),
                };
                for &(dx, dz) in cells {
                    // Paired plates/bricks lie along x when the layer is turned.
                    let four = part == "3003.dat";
                    let (p, r) = if turned && !four {
                        (DVec3::new(cx + dz, bottom - h, cz), &yaw90)
                    } else {
                        (DVec3::new(cx + dx, bottom - h, cz + dz), &ident)
                    };
                    put(&mut out, &mut count, col, p, r, part);
                }
                bottom -= h;
            }
            let top = palette[rng.below(palette.len() as u64) as usize];
            for (dx, dz) in [(-20.0, -20.0), (20.0, -20.0), (-20.0, 20.0), (20.0, 20.0)] {
                put(&mut out, &mut count, top, DVec3::new(cx + dx, bottom - 8.0, cz + dz), &ident, "3068b.dat");
            }
            out.push_str("0 STEP\r\n");
        }
    }
    out
}
