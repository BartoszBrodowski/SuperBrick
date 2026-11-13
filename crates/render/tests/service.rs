//! The warm `RenderService` against the one-shot `Scene::build` + `Renderer::new` path.

use std::time::Instant;

use ldraw::{Document, FlatPart, Library};
use render::{
    COLOR_FORMAT, Camera, FLAG_HIDDEN, FLAG_SELECTED, Gpu, Offscreen, Panel, Rect, Renderer,
    RenderService, RgbaImage, Scene, SceneOptions,
};

/// Bricks, plates, a slope and a round brick (conditional lines), a transparent brick, studs.
const MODEL: &str = "0 Service test\r\n\
1 4 0 0 0 1 0 0 0 1 0 0 0 1 3001.dat\r\n\
1 1 0 -24 0 0 0 1 0 1 0 -1 0 0 3001.dat\r\n\
1 14 40 -8 20 1 0 0 0 1 0 0 0 1 3020.dat\r\n\
1 2 -40 0 0 1 0 0 0 1 0 0 0 1 3040b.dat\r\n\
1 15 0 -48 0 1 0 0 0 1 0 0 0 1 3062b.dat\r\n\
1 36 60 0 -30 1 0 0 0 1 0 0 0 1 3003.dat\r\n\
1 71 -20 -32 40 1 0 0 0 1 0 0 0 1 3068b.dat\r\n";

const OTHER: &str = "0 Other model\r\n\
1 0 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\r\n\
1 25 0 -24 0 1 0 0 0 1 0 0 0 1 3039.dat\r\n\
1 72 30 0 0 1 0 0 0 1 0 0 0 1 3710.dat\r\n";

fn flat(text: &str) -> Vec<FlatPart> {
    Document::parse(text.as_bytes(), "test.ldr").flatten(None)
}

fn gpu() -> Option<Gpu> {
    match Gpu::headless() {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("no GPU adapter ({e}); skipping");
            None
        }
    }
}

/// The reference: build a scene, make a renderer, render into a fresh offscreen target.
fn old_path(gpu: &Gpu, lib: &Library, flat: &[FlatPart], flags: &[(u32, u32)], cam: &Camera, size: (u32, u32)) -> RgbaImage {
    let scene = Scene::build(lib, flat, &SceneOptions::default());
    let mut r = Renderer::new(gpu, COLOR_FORMAT, 4, &scene);
    r.set_part_flags(gpu, flags.iter().copied());
    let target = Offscreen::new(gpu, 4, size.0, size.1);
    target.render(gpu, &r, cam);
    RgbaImage { width: size.0, height: size.1, pixels: target.read_rgba(gpu) }
}

/// Fraction of pixels whose largest channel difference exceeds `tol`.
fn diff_fraction(a: &RgbaImage, b: &RgbaImage, tol: u8) -> f64 {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let bad = a
        .pixels
        .chunks_exact(4)
        .zip(b.pixels.chunks_exact(4))
        .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > tol))
        .count();
    bad as f64 / (a.width * a.height) as f64
}

/// Pixels that are not the background, so a blank render cannot pass as "equal".
fn coverage(img: &RgbaImage) -> f64 {
    let bg = img.pixel(0, 0);
    img.pixels.chunks_exact(4).filter(|p| *p != bg).count() as f64 / (img.width * img.height) as f64
}

#[test]
fn service_matches_the_one_shot_renderer() {
    let Some(gpu) = gpu() else { return };
    let lib = Library::open_default().unwrap();
    let model = flat(MODEL);
    let size = (320, 240);
    let scene = Scene::build(&lib, &model, &SceneOptions::default());
    let cam = Camera::framing(scene.bounds.0, scene.bounds.1, size.0 as f32 / size.1 as f32);
    let flags = [(1, FLAG_SELECTED), (6, FLAG_HIDDEN)];

    let svc = RenderService::new().unwrap();
    assert_eq!(svc.bounds(&lib, &model, &SceneOptions::default()), scene.bounds);
    let panel = [Panel { camera: cam, rect: Rect::full(size) }];

    for flags in [&[][..], &flags[..]] {
        let want = old_path(&gpu, &lib, &model, flags, &cam, size);
        assert!(coverage(&want) > 0.1, "reference render is nearly empty");
        let got = svc.render_panels(&lib, &model, flags, &panel, size).unwrap();
        let d = diff_fraction(&want, &got, 2);
        eprintln!("flags {flags:?}: {:.4}% differ, coverage {:.1}%", d * 100.0, coverage(&want) * 100.0);
        assert!(d <= 0.005, "flags {flags:?}: {:.3}% of pixels differ", d * 100.0);
    }

    // With another model packed into the atlas first, offsets shift; the picture must not.
    svc.render_panels(&lib, &flat(OTHER), &[], &panel, size).unwrap();
    let got = svc.render_panels(&lib, &model, &[], &panel, size).unwrap();
    let want = old_path(&gpu, &lib, &model, &[], &cam, size);
    let d = diff_fraction(&want, &got, 2);
    assert!(d <= 0.005, "after another model: {:.3}% of pixels differ", d * 100.0);
}

#[test]
fn panels_composite_into_one_image() {
    if gpu().is_none() {
        return;
    }
    let lib = Library::open_default().unwrap();
    let model = flat(MODEL);
    let svc = RenderService::new().unwrap();
    let (lo, hi) = svc.bounds(&lib, &model, &SceneOptions::default());
    let radius = (hi - lo).length() * 0.5;
    let size = (400, 300);
    let rects = Rect::grid(size, 2, 2, 0);
    let views = ["front", "top", "right", "iso"];
    let panels: Vec<Panel> = views
        .iter()
        .zip(&rects)
        .map(|(v, r)| {
            let (yaw, pitch) = Camera::preset(v).unwrap();
            Panel { camera: Camera::framing_subset(lo, hi, r.aspect(), yaw, pitch, radius), rect: *r }
        })
        .collect();
    let all = svc.render_panels(&lib, &model, &[], &panels, size).unwrap();
    assert_eq!((all.width, all.height, all.pixels.len()), (400, 300, 400 * 300 * 4));

    let mut crops = Vec::new();
    for p in &panels {
        let alone = svc
            .render_panels(&lib, &model, &[], &[Panel { rect: Rect::full((p.rect.w, p.rect.h)), ..*p }], (p.rect.w, p.rect.h))
            .unwrap();
        let crop = all.crop(p.rect);
        assert!(coverage(&alone) > 0.05, "panel {:?} is nearly empty", p.rect);
        let d = diff_fraction(&alone, &crop, 2);
        assert!(d <= 0.005, "panel {:?}: {:.3}% of pixels differ from a lone render", p.rect, d * 100.0);
        crops.push(crop);
    }
    // Different views really are different pictures.
    for i in 0..crops.len() {
        for j in i + 1..crops.len() {
            assert!(diff_fraction(&crops[i], &crops[j], 8) > 0.02, "views {} and {} look the same", views[i], views[j]);
        }
    }
    // Encodes to an RGB PNG that decodes back to the same pixels.
    let png = render::encode_png_fast(&all);
    if let Ok(dir) = std::env::var("RENDER_TEST_OUT") {
        std::fs::write(std::path::Path::new(&dir).join("composite.png"), &png).unwrap();
    }
    let mut dec = png::Decoder::new(std::io::Cursor::new(png)).read_info().unwrap();
    let mut buf = vec![0; dec.output_buffer_size().unwrap()];
    let info = dec.next_frame(&mut buf).unwrap();
    assert_eq!((info.width, info.height, info.color_type), (400, 300, png::ColorType::Rgb));
    assert_eq!(&buf[..info.buffer_size()], &all.to_rgb()[..]);
}

#[test]
fn second_render_is_warm() {
    if gpu().is_none() {
        return;
    }
    let lib = Library::open_default().unwrap();
    let model = flat(MODEL);
    // Meshes are cached in the library, not the service: load them first so the comparison is
    // about the service's own warm state (device, pipelines, atlas, targets).
    Scene::build(&lib, &model, &SceneOptions::default());
    let size = (512, 384);
    let t = Instant::now();
    let svc = RenderService::new().unwrap();
    let render = |svc: &RenderService| {
        svc.render_framed(&lib, &model, &[], size, &Default::default(), |f| {
            vec![Panel { camera: Camera::framing(f.bounds.0, f.bounds.1, 4.0 / 3.0), rect: Rect::full(size) }]
        })
        .unwrap()
    };
    let first_img = render(&svc);
    let first = t.elapsed();
    let mut warm = std::time::Duration::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        let img = render(&svc);
        warm = warm.min(t.elapsed());
        assert_eq!(img, first_img, "warm render differs from the cold one");
    }
    eprintln!("cold {first:?} (incl. device), warm {warm:?}, last {:?}", svc.last_timings());
    assert!(warm < first, "warm {warm:?} is not faster than cold {first:?}");
    assert!(warm.as_millis() < 500, "warm render took {warm:?}");
}

#[test]
fn thumbnail_shows_the_part_in_its_colour() {
    if gpu().is_none() {
        return;
    }
    let lib = Library::open_default().unwrap();
    let svc = RenderService::new().unwrap();
    let img = svc.thumbnail(&lib, "3001.dat", ldraw::Color(4), 96).unwrap();
    assert_eq!((img.width, img.height), (96, 96));
    assert!(coverage(&img) > 0.2, "thumbnail is nearly empty");
    let reddish = img.pixels.chunks_exact(4).filter(|p| p[0] > 120 && p[1] < 80 && p[2] < 80).count();
    assert!(reddish > 96 * 96 / 10, "only {reddish} red pixels in a red brick thumbnail");
}
