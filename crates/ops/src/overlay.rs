//! CPU annotation over rendered images: text, numbered badges with collision-free placement and
//! leader lines, boxes, lines, captions and legends. Pure pixels, so it is testable without a GPU.
use ab_glyph::{Font, FontRef, PxScale, ScaleFont, point};
use render::RgbaImage;
use std::sync::OnceLock;

pub type Rgb = [u8; 3];
pub const WHITE: Rgb = [255, 255, 255];
pub const BLACK: Rgb = [20, 20, 24];
pub const RED: Rgb = [226, 32, 28];
pub const BLUE: Rgb = [26, 110, 245];
pub const ORANGE: Rgb = [255, 150, 20];
pub const GREEN: Rgb = [30, 170, 70];
pub const GREY: Rgb = [110, 115, 125];

/// Distinct badge colours for numbered candidates (cycled).
pub const PALETTE: [Rgb; 8] = [
    [26, 110, 245],
    [230, 120, 0],
    [20, 160, 80],
    [200, 40, 160],
    [0, 150, 170],
    [150, 90, 30],
    [110, 70, 220],
    [190, 160, 0],
];

fn font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR).expect("bundled font"))
}

pub struct Canvas<'a> {
    pub img: &'a mut RgbaImage,
    /// Occupied cells (8 px) so labels do not overlap each other.
    taken: Vec<bool>,
}

const CELL: u32 = 8;

impl<'a> Canvas<'a> {
    pub fn new(img: &'a mut RgbaImage) -> Canvas<'a> {
        let n = (img.width.div_ceil(CELL) * img.height.div_ceil(CELL)) as usize;
        Canvas { img, taken: vec![false; n] }
    }
    pub fn width(&self) -> u32 {
        self.img.width
    }
    pub fn height(&self) -> u32 {
        self.img.height
    }

    pub fn blend(&mut self, x: i32, y: i32, c: Rgb, a: f32) {
        if x < 0 || y < 0 || x >= self.img.width as i32 || y >= self.img.height as i32 || a <= 0.0 {
            return;
        }
        let i = ((y as u32 * self.img.width + x as u32) * 4) as usize;
        let p = &mut self.img.pixels[i..i + 4];
        let a = a.min(1.0);
        for k in 0..3 {
            p[k] = (p[k] as f32 * (1.0 - a) + c[k] as f32 * a).round() as u8;
        }
        p[3] = 255;
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb, a: f32) {
        for yy in y.max(0)..(y + h).min(self.img.height as i32) {
            for xx in x.max(0)..(x + w).min(self.img.width as i32) {
                self.blend(xx, yy, c, a);
            }
        }
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb, thick: i32) {
        self.fill_rect(x, y, w, thick, c, 1.0);
        self.fill_rect(x, y + h - thick, w, thick, c, 1.0);
        self.fill_rect(x, y, thick, h, c, 1.0);
        self.fill_rect(x + w - thick, y, thick, h, c, 1.0);
    }

    /// Anti-aliased-ish thick line (Wu-style sampling along the segment).
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgb, width: f32) {
        let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        let steps = (len * 2.0).ceil().max(1.0) as i32;
        let r = width * 0.5;
        for s in 0..=steps {
            let t = s as f32 / steps as f32;
            let (x, y) = (x0 + (x1 - x0) * t, y0 + (y1 - y0) * t);
            let (ix0, iy0) = ((x - r - 1.0).floor() as i32, (y - r - 1.0).floor() as i32);
            let (ix1, iy1) = ((x + r + 1.0).ceil() as i32, (y + r + 1.0).ceil() as i32);
            for yy in iy0..=iy1 {
                for xx in ix0..=ix1 {
                    let d = ((xx as f32 + 0.5 - x).powi(2) + (yy as f32 + 0.5 - y).powi(2)).sqrt();
                    let a = (r + 0.5 - d).clamp(0.0, 1.0);
                    if a > 0.0 {
                        // Max-blend so overlapping samples do not darken.
                        let i = yy * self.img.width as i32 + xx;
                        if xx >= 0 && yy >= 0 && xx < self.img.width as i32 && yy < self.img.height as i32 {
                            let p = &self.img.pixels[(i * 4) as usize..(i * 4 + 3) as usize];
                            let cur = p.iter().zip(c.iter()).map(|(a, b)| (*a as i32 - *b as i32).abs()).sum::<i32>();
                            if cur > 6 {
                                self.blend(xx, yy, c, a);
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn dashed(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, c: Rgb, width: f32, dash: f32) {
        let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt().max(1e-3);
        let n = (len / dash).ceil() as i32;
        for k in (0..n).step_by(2) {
            let t0 = k as f32 * dash / len;
            let t1 = ((k + 1) as f32 * dash / len).min(1.0);
            self.line(x0 + (x1 - x0) * t0, y0 + (y1 - y0) * t0, x0 + (x1 - x0) * t1, y0 + (y1 - y0) * t1, c, width);
        }
    }

    pub fn disc(&mut self, cx: f32, cy: f32, r: f32, c: Rgb) {
        for yy in (cy - r - 1.0) as i32..=(cy + r + 1.0) as i32 {
            for xx in (cx - r - 1.0) as i32..=(cx + r + 1.0) as i32 {
                let d = ((xx as f32 + 0.5 - cx).powi(2) + (yy as f32 + 0.5 - cy).powi(2)).sqrt();
                self.blend(xx, yy, c, (r + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    /// Width and height of `text` at `px`.
    pub fn measure(text: &str, px: f32) -> (f32, f32) {
        let f = font().as_scaled(PxScale::from(px));
        let w: f32 = text.chars().map(|ch| f.h_advance(f.glyph_id(ch))).sum();
        (w, f.ascent() - f.descent())
    }

    /// Draw text with its top-left at (x, y).
    pub fn text(&mut self, x: f32, y: f32, text: &str, px: f32, c: Rgb) {
        let f = font().as_scaled(PxScale::from(px));
        let mut caret = x;
        let base = y + f.ascent();
        for ch in text.chars() {
            let id = f.glyph_id(ch);
            let g = id.with_scale_and_position(PxScale::from(px), point(caret, base));
            caret += f.h_advance(id);
            if let Some(o) = font().outline_glyph(g) {
                let b = o.px_bounds();
                o.draw(|gx, gy, cov| {
                    self.blend(b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, c, cov);
                });
            }
        }
    }

    /// Text on a filled rounded-ish box (a label chip). Returns the chip rectangle.
    pub fn chip(&mut self, x: f32, y: f32, text: &str, px: f32, fg: Rgb, bg: Rgb) -> (i32, i32, i32, i32) {
        let (w, h) = Self::measure(text, px);
        let (pw, ph) = (4.0, 2.0);
        let r = (x as i32, y as i32, (w + 2.0 * pw).ceil() as i32, (h + 2.0 * ph).ceil() as i32);
        self.fill_rect(r.0, r.1, r.2, r.3, bg, 0.92);
        self.text(x + pw, y + ph, text, px, fg);
        r
    }

    fn cells(&self, r: (i32, i32, i32, i32)) -> Vec<usize> {
        let cols = self.img.width.div_ceil(CELL) as i32;
        let rows = self.img.height.div_ceil(CELL) as i32;
        let mut out = Vec::new();
        for cy in (r.1.max(0) / CELL as i32)..=((r.1 + r.3 - 1).max(0) / CELL as i32).min(rows - 1) {
            for cx in (r.0.max(0) / CELL as i32)..=((r.0 + r.2 - 1).max(0) / CELL as i32).min(cols - 1) {
                out.push((cy * cols + cx) as usize);
            }
        }
        out
    }
    fn free(&self, r: (i32, i32, i32, i32)) -> bool {
        r.0 >= 0
            && r.1 >= 0
            && r.0 + r.2 <= self.img.width as i32
            && r.1 + r.3 <= self.img.height as i32
            && self.cells(r).iter().all(|&i| !self.taken[i])
    }
    pub fn reserve(&mut self, r: (i32, i32, i32, i32)) {
        for i in self.cells(r) {
            self.taken[i] = true;
        }
    }

    /// A label chip anchored at (ax, ay): placed on the anchor if free, otherwise at the nearest
    /// free spot on a spiral, joined to the anchor by a leader line. Returns the chip centre.
    pub fn label(&mut self, ax: f32, ay: f32, text: &str, px: f32, fg: Rgb, bg: Rgb) -> (f32, f32) {
        let (w, h) = Self::measure(text, px);
        let (cw, ch) = ((w + 8.0).ceil() as i32, (h + 4.0).ceil() as i32);
        let mut best = None;
        'search: for ring in 0..24 {
            let rad = ring as f32 * 10.0;
            let n = if ring == 0 { 1 } else { 8 + ring * 2 };
            for k in 0..n {
                let a = k as f32 / n as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
                let (cx, cy) = (ax + rad * a.cos(), ay + rad * a.sin());
                let r = ((cx - cw as f32 / 2.0) as i32, (cy - ch as f32 / 2.0) as i32, cw, ch);
                if self.free(r) {
                    best = Some(r);
                    break 'search;
                }
            }
        }
        let r = best.unwrap_or((
            ((ax - cw as f32 / 2.0) as i32).clamp(0, (self.img.width as i32 - cw).max(0)),
            ((ay - ch as f32 / 2.0) as i32).clamp(0, (self.img.height as i32 - ch).max(0)),
            cw,
            ch,
        ));
        let centre = (r.0 as f32 + r.2 as f32 / 2.0, r.1 as f32 + r.3 as f32 / 2.0);
        if (centre.0 - ax).abs() > (cw as f32) / 2.0 || (centre.1 - ay).abs() > (ch as f32) / 2.0 {
            self.line(ax, ay, centre.0, centre.1, bg, 2.0);
            self.disc(ax, ay, 3.0, bg);
        }
        self.reserve(r);
        self.chip(r.0 as f32, r.1 as f32, text, px, fg, bg);
        centre
    }

    /// Colour the pixels of `mask` that border non-mask pixels (an outline from an id mask).
    pub fn outline(&mut self, mask: &[bool], c: Rgb, thick: i32) {
        let (w, h) = (self.img.width as i32, self.img.height as i32);
        if mask.len() != (w * h) as usize {
            return;
        }
        let mut edge = vec![false; mask.len()];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) as usize;
                if !mask[i] {
                    continue;
                }
                let border = [(-1, 0), (1, 0), (0, -1), (0, 1)].iter().any(|(dx, dy)| {
                    let (nx, ny) = (x + dx, y + dy);
                    nx < 0 || ny < 0 || nx >= w || ny >= h || !mask[(ny * w + nx) as usize]
                });
                edge[i] = border;
            }
        }
        for y in 0..h {
            for x in 0..w {
                if edge[(y * w + x) as usize] {
                    self.fill_rect(x - thick / 2, y - thick / 2, thick, thick, c, 1.0);
                }
            }
        }
    }

    /// A caption strip at the top-left of a panel rectangle.
    pub fn caption(&mut self, x: i32, y: i32, text: &str) {
        let r = self.chip(x as f32 + 4.0, y as f32 + 4.0, text, 13.0, WHITE, [40, 44, 52]);
        self.reserve(r);
    }

    /// A row of colour swatches with names, bottom-left of a panel.
    pub fn legend(&mut self, x: i32, y_bottom: i32, items: &[(Rgb, String)]) {
        let mut cx = x as f32 + 6.0;
        let y = y_bottom as f32 - 22.0;
        for (c, name) in items {
            let (w, _) = Self::measure(name, 12.0);
            if cx + w + 22.0 > self.img.width as f32 {
                break;
            }
            self.fill_rect(cx as i32 - 2, y as i32 - 2, (w + 24.0) as i32, 20, WHITE, 0.85);
            self.fill_rect(cx as i32, y as i32 + 2, 12, 12, *c, 1.0);
            self.text(cx + 16.0, y + 1.0, name, 12.0, BLACK);
            self.reserve((cx as i32 - 2, y as i32 - 2, (w + 24.0) as i32, 20));
            cx += w + 30.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: u32, h: u32) -> RgbaImage {
        RgbaImage { width: w, height: h, pixels: vec![255; (w * h * 4) as usize] }
    }

    #[test]
    fn text_draws_ink_and_labels_avoid_each_other() {
        let mut img = blank(200, 100);
        let mut c = Canvas::new(&mut img);
        c.text(4.0, 4.0, "c1 #812", 14.0, BLACK);
        let a = c.label(100.0, 50.0, "1", 14.0, WHITE, BLUE);
        let b = c.label(100.0, 50.0, "2", 14.0, WHITE, RED);
        assert!(a != b, "second label must move off the first");
        let ink = img.pixels.chunks(4).filter(|p| p[0] < 128).count();
        assert!(ink > 50);
    }

    #[test]
    fn outline_marks_only_the_border() {
        let mut img = blank(10, 10);
        let mut mask = vec![false; 100];
        for y in 2..8 {
            for x in 2..8 {
                mask[y * 10 + x] = true;
            }
        }
        Canvas::new(&mut img).outline(&mask, RED, 1);
        assert_eq!(img.pixel(2, 2)[1], RED[1]);
        assert_eq!(img.pixel(5, 5), [255, 255, 255, 255]);
    }
}
