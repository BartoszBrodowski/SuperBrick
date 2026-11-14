//! `compare`: a booklet picture beside the render, in one image, from the same camera.

use crate::args::Result;
use crate::overlay::{Canvas, BLACK, GREY};
use crate::Image;
use render::RgbaImage;
use serde_json::{json, Value};
use std::path::Path;

pub const DEFAULT_VIEW: (f32, f32) = (45.0, 30.0);
const HEIGHT: u32 = 640;
const LABEL: u32 = 30;
const GAP: u32 = 10;

pub fn schema() -> Value {
    json!({
        "type": "object",
        "description": "Put a picture (a booklet page) beside the render, as one image from one camera: {page:\"/abs/p15.png\", box:[x0,y0,x1,y1] (fractions of the page, or pixels), view:[yaw,pitch] (default [45,30], the usual booklet camera: front-left, above)}. The render shows the whole current model with this edit's parts outlined.",
        "properties": {
            "page": {"type": "string"},
            "box": {"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4},
            "view": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}
        },
        "required": ["page"]
    })
}

/// The camera to force when a comparison is asked for.
pub fn view(args: &Value) -> Result<Option<(f32, f32)>> {
    let Some(c) = args.get("compare") else { return Ok(None) };
    let Some(page) = c.get("page").and_then(Value::as_str) else {
        return Err("compare needs \"page\": the path of a page image".into());
    };
    if !Path::new(page).is_file() {
        return Err(format!("compare: no page image at {page}"));
    }
    Ok(Some(match c.get("view").and_then(Value::as_array) {
        Some(v) if v.len() == 2 => (v[0].as_f64().map_or(DEFAULT_VIEW.0, |x| x as f32), v[1].as_f64().map_or(DEFAULT_VIEW.1, |x| x as f32)),
        _ => DEFAULT_VIEW,
    }))
}

/// Replace the render with [page crop | render], labelled. Returns the render unchanged without `compare`.
pub fn attach(args: &Value, img: Option<Image>) -> Result<Option<Image>> {
    let Some(c) = args.get("compare") else { return Ok(img) };
    let Some(img) = img else { return Ok(None) };
    let (yaw, pitch) = view(args)?.unwrap_or(DEFAULT_VIEW);
    let page = c.get("page").and_then(Value::as_str).unwrap_or_default();
    let mut left = decode(Path::new(page)).map_err(|e| format!("compare: {page}: {e}"))?;
    if let Some(b) = c.get("box").and_then(Value::as_array) {
        let v: Vec<f64> = b.iter().filter_map(Value::as_f64).collect();
        if v.len() != 4 {
            return Err("compare box is [x0, y0, x1, y1]".into());
        }
        left = crop(&left, &v)?;
    }
    let right = decode_bytes(&img.png)?;
    let a = scale(&left, HEIGHT);
    let b = scale(&right, HEIGHT);
    let w = a.width + GAP + b.width;
    let h = HEIGHT + LABEL;
    let mut out = RgbaImage { width: w, height: h, pixels: vec![255; (w * h * 4) as usize] };
    blit(&mut out, &a, 0, LABEL);
    blit(&mut out, &b, a.width + GAP, LABEL);
    {
        let mut cv = Canvas::new(&mut out);
        cv.fill_rect(a.width as i32, 0, GAP as i32, h as i32, GREY, 1.0);
        cv.text(8.0, 6.0, "booklet", 18.0, BLACK);
        cv.text((a.width + GAP + 8) as f32, 6.0, &format!("model now (yaw {yaw}, pitch {pitch}); this edit outlined"), 18.0, BLACK);
    }
    Ok(Some(Image {
        png: render::encode_png_fast(&out),
        caption: format!("booklet | model, yaw {yaw} pitch {pitch}"),
        width: w,
        height: h,
    }))
}

fn decode(path: &Path) -> std::result::Result<RgbaImage, String> {
    decode_bytes(&std::fs::read(path).map_err(|e| e.to_string())?)
}

fn decode_bytes(bytes: &[u8]) -> std::result::Result<RgbaImage, String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().map_err(|e| e.to_string())?;
    let (w, h) = (reader.info().width, reader.info().height);
    if (w as u64) * (h as u64) > 80_000_000 {
        return Err(format!("{w}x{h} is too large"));
    }
    let mut buf = vec![0; reader.output_buffer_size().ok_or("bad size")?];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let n = (info.width * info.height) as usize;
    let mut px = Vec::with_capacity(n * 4);
    match info.color_type {
        png::ColorType::Rgba => px.extend_from_slice(&buf[..n * 4]),
        png::ColorType::Rgb => buf[..n * 3].chunks(3).for_each(|c| px.extend_from_slice(&[c[0], c[1], c[2], 255])),
        png::ColorType::Grayscale => buf[..n].iter().for_each(|&g| px.extend_from_slice(&[g, g, g, 255])),
        png::ColorType::GrayscaleAlpha => buf[..n * 2].chunks(2).for_each(|c| px.extend_from_slice(&[c[0], c[0], c[0], c[1]])),
        other => return Err(format!("unsupported PNG colour type {other:?}")),
    }
    // Composite transparency over white, as on paper.
    for p in px.chunks_mut(4) {
        let a = p[3] as u32;
        for c in &mut p[..3] {
            *c = ((*c as u32 * a + 255 * (255 - a)) / 255) as u8;
        }
        p[3] = 255;
    }
    Ok(RgbaImage { width: info.width, height: info.height, pixels: px })
}

fn crop(img: &RgbaImage, b: &[f64]) -> Result<RgbaImage> {
    let (w, h) = (img.width as f64, img.height as f64);
    let frac = b.iter().all(|v| (0.0..=1.0).contains(v));
    let (x0, y0, x1, y1) = if frac { (b[0] * w, b[1] * h, b[2] * w, b[3] * h) } else { (b[0], b[1], b[2], b[3]) };
    let (x0, x1) = (x0.min(x1).clamp(0.0, w) as u32, x0.max(x1).clamp(0.0, w) as u32);
    let (y0, y1) = (y0.min(y1).clamp(0.0, h) as u32, y0.max(y1).clamp(0.0, h) as u32);
    if x1 <= x0 + 4 || y1 <= y0 + 4 {
        return Err(format!("compare box is empty on a {}x{} page", img.width, img.height));
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut px = Vec::with_capacity((cw * ch * 4) as usize);
    for y in y0..y1 {
        let s = ((y * img.width + x0) * 4) as usize;
        px.extend_from_slice(&img.pixels[s..s + (cw * 4) as usize]);
    }
    Ok(RgbaImage { width: cw, height: ch, pixels: px })
}

/// Resize to a height (area average when shrinking, bilinear when growing).
fn scale(img: &RgbaImage, height: u32) -> RgbaImage {
    let f = height as f64 / img.height as f64;
    let width = ((img.width as f64 * f).round() as u32).max(1);
    let mut px = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let (sx0, sx1) = (x as f64 / f, (x + 1) as f64 / f);
            let (sy0, sy1) = (y as f64 / f, (y + 1) as f64 / f);
            let mut acc = [0f64; 4];
            let mut n = 0f64;
            if f < 1.0 {
                for yy in sy0.floor() as u32..(sy1.ceil() as u32).min(img.height) {
                    for xx in sx0.floor() as u32..(sx1.ceil() as u32).min(img.width) {
                        let i = ((yy * img.width + xx) * 4) as usize;
                        for k in 0..4 {
                            acc[k] += img.pixels[i + k] as f64;
                        }
                        n += 1.0;
                    }
                }
            } else {
                let (cx, cy) = (((sx0 + sx1) / 2.0 - 0.5).max(0.0), ((sy0 + sy1) / 2.0 - 0.5).max(0.0));
                let (ix, iy) = (cx.floor() as u32, cy.floor() as u32);
                let (tx, ty) = (cx - ix as f64, cy - iy as f64);
                for (dy, wy) in [(0, 1.0 - ty), (1, ty)] {
                    for (dx, wx) in [(0, 1.0 - tx), (1, tx)] {
                        let xx = (ix + dx).min(img.width - 1);
                        let yy = (iy + dy).min(img.height - 1);
                        let i = ((yy * img.width + xx) * 4) as usize;
                        for k in 0..4 {
                            acc[k] += img.pixels[i + k] as f64 * wx * wy;
                        }
                        n += wx * wy;
                    }
                }
            }
            let o = ((y * width + x) * 4) as usize;
            for k in 0..4 {
                px[o + k] = (acc[k] / n.max(1e-9)).round() as u8;
            }
        }
    }
    RgbaImage { width, height, pixels: px }
}

fn blit(dst: &mut RgbaImage, src: &RgbaImage, x: u32, y: u32) {
    for row in 0..src.height.min(dst.height.saturating_sub(y)) {
        let s = (row * src.width * 4) as usize;
        let d = (((y + row) * dst.width + x) * 4) as usize;
        let n = (src.width.min(dst.width - x) * 4) as usize;
        dst.pixels[d..d + n].copy_from_slice(&src.pixels[s..s + n]);
    }
}
