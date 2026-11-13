//! Voxel physics for placed parts: the core of `bench-next/physics.py`, ported for speed.
//!
//! * [`Shape`]: a part's body as the intersection of three axis-aligned depth hulls (per-pixel
//!   min/max depth of its low-LOD triangles seen along x, y and z, 1 LDU pixels), male studs as
//!   r=6 h=4 cylinders, and anti-studs found on the underside (`find_antistuds`).
//! * [`Body`]: a shape placed with rotation `r` (local -> world) and translation `t`.
//! * [`relation`]: overlap volume, contact, and stud links between two bodies, sampled on a 1-LDU
//!   grid at half-integer coordinates, with engaged stud volumes removed and the "pressed home" test.
//!
//! Not ported yet (they live in `snaps.py` and the rest of `physics.py`): LDCad shadow connectors
//! (pins, axles, bars, clips, hinges...), clip edges, gears, part compliance margins and the
//! narrow phase for odd angles. Links here are geometric stud <-> anti-stud pairs, which is what
//! `snaps.py` reports for plain bricks and plates.

use std::sync::Arc;

use glam::{DMat3, DVec3};
use ldraw::{Library, Lod};

pub const EPS: f64 = 0.3;
pub const STUD_R: f64 = 6.0;
pub const STUD_H: f64 = 4.0;
pub const CONNECT_TOL: f64 = 1.6;
pub const COLLIDE_MIN: f64 = 6.0;
/// Margin added around an engaged connector when subtracting it from the overlap.
pub const KEEPOUT: f64 = 0.6;

/// Per-pixel (min, max) depth over triangles given as image-space (x, y, depth) vertices, NaN
/// where empty. Same rules as `raster.rasterize`: pixel centres at +0.5, pixel box
/// `ceil(min - 0.5) ..= floor(max - 0.5)`, barycentric weights `>= -1e-7`, slivers skipped.
pub fn minmax_depth(tris: &[[DVec3; 3]], w: usize, h: usize) -> (Vec<f64>, Vec<f64>) {
    let mut lo = vec![f64::INFINITY; w * h];
    let mut hi = vec![f64::NEG_INFINITY; w * h];
    for t in tris {
        let [a, b, q] = *t;
        let area = (b.x - a.x) * (q.y - a.y) - (q.x - a.x) * (b.y - a.y);
        let x0 = ((a.x.min(b.x).min(q.x) - 0.5).ceil() as i64).max(0);
        let x1 = ((a.x.max(b.x).max(q.x) - 0.5).floor() as i64).min(w as i64 - 1);
        let y0 = ((a.y.min(b.y).min(q.y) - 0.5).ceil() as i64).max(0);
        let y1 = ((a.y.max(b.y).max(q.y) - 0.5).floor() as i64).min(h as i64 - 1);
        if x1 < x0 || y1 < y0 || area.abs() <= 1e-12 {
            continue;
        }
        for py in y0..=y1 {
            let cy = py as f64 + 0.5;
            for px in x0..=x1 {
                let cx = px as f64 + 0.5;
                let w0 = ((b.x - cx) * (q.y - cy) - (q.x - cx) * (b.y - cy)) / area;
                let w1 = ((q.x - cx) * (a.y - cy) - (a.x - cx) * (q.y - cy)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 >= -1e-7 && w1 >= -1e-7 && w2 >= -1e-7 {
                    let z = w0 * a.z + w1 * b.z + w2 * q.z;
                    let i = py as usize * w + px as usize;
                    lo[i] = lo[i].min(z);
                    hi[i] = hi[i].max(z);
                }
            }
        }
    }
    for v in lo.iter_mut().chain(hi.iter_mut()) {
        if v.is_infinite() {
            *v = f64::NAN;
        }
    }
    (lo, hi)
}

/// A part type's solid model, in its local LDraw frame.
#[derive(Debug, Clone)]
pub struct Shape {
    pub name: String,
    /// Body grid corners (whole LDU) and size `hi - lo`.
    pub lo: [i64; 3],
    pub hi: [i64; 3],
    pub n: [usize; 3],
    /// y hull: min/max y per (z, x) pixel, index `z * nx + x`.
    pub hy: (Vec<f64>, Vec<f64>),
    /// x hull: min/max x per (y, z) pixel, index `y * nz + z`.
    pub hx: (Vec<f64>, Vec<f64>),
    /// z hull: min/max z per (y, x) pixel, index `y * nx + x`.
    pub hz: (Vec<f64>, Vec<f64>),
    pub studs: Vec<DVec3>,
    pub sdir: Vec<DVec3>,
    pub shollow: Vec<bool>,
    /// Anti-stud receptacles (local), all opening along local +y.
    pub anti: Vec<DVec3>,
    pub full_lo: DVec3,
    pub full_hi: DVec3,
    /// Studs bucketed on a 20-LDU grid over their cylinders' boxes, so a point tests only nearby studs.
    stud_grid: StudGrid,
}

#[derive(Debug, Clone, Default)]
struct StudGrid {
    lo: DVec3,
    n: [usize; 3],
    cells: Vec<Vec<u32>>,
}

const STUD_CELL: f64 = 20.0;

impl StudGrid {
    fn build(studs: &[DVec3], sdir: &[DVec3]) -> StudGrid {
        if studs.is_empty() {
            return StudGrid::default();
        }
        let boxes: Vec<(DVec3, DVec3)> = studs
            .iter()
            .zip(sdir)
            .map(|(&s, &d)| {
                let e = s + d * STUD_H;
                (
                    s.min(e) - DVec3::splat(STUD_R),
                    s.max(e) + DVec3::splat(STUD_R),
                )
            })
            .collect();
        let lo = boxes
            .iter()
            .fold(DVec3::splat(f64::INFINITY), |m, b| m.min(b.0));
        let hi = boxes
            .iter()
            .fold(DVec3::splat(f64::NEG_INFINITY), |m, b| m.max(b.1));
        let n = ((hi - lo) / STUD_CELL).ceil().max(DVec3::ONE);
        let n = [n.x as usize, n.y as usize, n.z as usize];
        let mut cells = vec![Vec::new(); n[0] * n[1] * n[2]];
        for (i, (a, b)) in boxes.iter().enumerate() {
            let c0 = ((*a - lo) / STUD_CELL).floor();
            let c1 = ((*b - lo) / STUD_CELL).floor();
            for x in c0.x as usize..=(c1.x as usize).min(n[0] - 1) {
                for y in c0.y as usize..=(c1.y as usize).min(n[1] - 1) {
                    for z in c0.z as usize..=(c1.z as usize).min(n[2] - 1) {
                        cells[(x * n[1] + y) * n[2] + z].push(i as u32);
                    }
                }
            }
        }
        StudGrid { lo, n, cells }
    }

    fn near(&self, p: DVec3) -> &[u32] {
        if self.cells.is_empty() {
            return &[];
        }
        let c = (p - self.lo) / STUD_CELL;
        if c.x < 0.0 || c.y < 0.0 || c.z < 0.0 {
            return &[];
        }
        let (x, y, z) = (c.x as usize, c.y as usize, c.z as usize);
        if x >= self.n[0] || y >= self.n[1] || z >= self.n[2] {
            return &[];
        }
        &self.cells[(x * self.n[1] + y) * self.n[2] + z]
    }
}

fn round_half_even(v: f64) -> f64 {
    v.round_ties_even()
}

impl Shape {
    /// Build from the part's low-LOD mesh, as `physics.build_shape` does.
    pub fn build(lib: &Library, name: &str) -> ldraw::Result<Shape> {
        let m = lib.mesh(name, Lod::Low)?;
        let to64 = |t: &[glam::Vec3; 3]| [t[0].as_dvec3(), t[1].as_dvec3(), t[2].as_dvec3()];
        let any_stud = m.tstud.iter().any(|&s| s);
        let mut body: Vec<[DVec3; 3]> = m
            .tris
            .iter()
            .zip(&m.tstud)
            .filter(|(_, s)| !any_stud || !**s)
            .map(|(t, _)| to64(t))
            .collect();
        if body.is_empty() {
            body = m.tris.iter().map(to64).collect();
        }
        if body.is_empty() {
            body.push([DVec3::ZERO; 3]);
        }
        let (mut pmin, mut pmax) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        for t in &body {
            for v in t {
                pmin = pmin.min(*v);
                pmax = pmax.max(*v);
            }
        }
        let lo = (pmin + 1e-6).floor();
        let hi = (pmax - 1e-6).ceil().max(lo + 1.0);
        let lo_i = [lo.x as i64, lo.y as i64, lo.z as i64];
        let hi_i = [hi.x as i64, hi.y as i64, hi.z as i64];
        let n = [
            (hi_i[0] - lo_i[0]) as usize,
            (hi_i[1] - lo_i[1]) as usize,
            (hi_i[2] - lo_i[2]) as usize,
        ];
        let view = |f: &dyn Fn(DVec3) -> DVec3| -> Vec<[DVec3; 3]> {
            body.iter().map(|t| [f(t[0]), f(t[1]), f(t[2])]).collect()
        };
        let hy = minmax_depth(
            &view(&|v| DVec3::new(v.x - lo.x, v.z - lo.z, v.y)),
            n[0],
            n[2],
        );
        let hx = minmax_depth(
            &view(&|v| DVec3::new(v.z - lo.z, v.y - lo.y, v.x)),
            n[2],
            n[1],
        );
        let hz = minmax_depth(
            &view(&|v| DVec3::new(v.x - lo.x, v.y - lo.y, v.z)),
            n[0],
            n[1],
        );

        // De-duplicate studs (some parts reference overlapping primitives); first one of each key wins.
        let (mut studs, mut sdir, mut shollow) = (Vec::new(), Vec::new(), Vec::new());
        let mut seen = std::collections::HashSet::new();
        for s in &m.studs {
            let (p, d) = (s.pos.as_dvec3(), s.dir.as_dvec3());
            let key: [i64; 6] = [p.x, p.y, p.z, d.x * 10.0, d.y * 10.0, d.z * 10.0]
                .map(|v| round_half_even(v) as i64);
            if seen.insert(key) {
                studs.push(p);
                sdir.push(d);
                shollow.push(s.hollow);
            }
        }
        let (mut flo, mut fhi) = (pmin, pmax);
        for (s, d) in studs.iter().zip(&sdir) {
            for p in [*s, *s + *d * STUD_H, *s - STUD_R, *s + STUD_R] {
                flo = flo.min(p);
                fhi = fhi.max(p);
            }
        }
        for t in &m.tris {
            for v in t {
                flo = flo.min(v.as_dvec3());
                fhi = fhi.max(v.as_dvec3());
            }
        }
        let stud_grid = StudGrid::build(&studs, &sdir);
        let mut sh = Shape {
            name: ldraw::canonical_name(name),
            lo: lo_i,
            hi: hi_i,
            n,
            hy,
            hx,
            hz,
            studs,
            sdir,
            shollow,
            anti: Vec::new(),
            full_lo: flo,
            full_hi: fhi,
            stud_grid,
        };
        sh.anti = find_antistuds(&sh);
        Ok(sh)
    }

    /// Is a local point inside the part? `shrink` erodes the body hulls by extra LDU.
    #[inline]
    pub fn inside(&self, p: DVec3, studs: bool, shrink: f64) -> bool {
        let eps = EPS + shrink;
        let ix = (p.x - self.lo[0] as f64).floor();
        let iy = (p.y - self.lo[1] as f64).floor();
        let iz = (p.z - self.lo[2] as f64).floor();
        let [nx, ny, nz] = self.n;
        if ix >= 0.0
            && iy >= 0.0
            && iz >= 0.0
            && (ix as usize) < nx
            && (iy as usize) < ny
            && (iz as usize) < nz
        {
            let (a, b, c) = (ix as usize, iy as usize, iz as usize);
            let (i, j, k) = (c * nx + a, b * nz + c, b * nx + a);
            // NaN (empty pixel) fails every comparison, so it reads as outside.
            if self.hy.0[i] + eps < p.y
                && p.y < self.hy.1[i] - eps
                && self.hx.0[j] + eps < p.x
                && p.x < self.hx.1[j] - eps
                && self.hz.0[k] + eps < p.z
                && p.z < self.hz.1[k] - eps
            {
                return true;
            }
        }
        studs && self.in_stud(p)
    }

    #[inline]
    fn in_stud(&self, p: DVec3) -> bool {
        const R2: f64 = (STUD_R - EPS) * (STUD_R - EPS);
        self.stud_grid.near(p).iter().any(|&i| {
            let (s, d) = (self.studs[i as usize], self.sdir[i as usize]);
            let q = p - s;
            let t = q.dot(d);
            t > EPS && t < STUD_H - EPS && q.length_squared() - t * t < R2
        })
    }
}

/// Down-facing receptacles (opening along local +y) on a 10-LDU lattice: where a stud cylinder
/// fits into empty space under a ceiling and is gripped by material (walls, tubes) around it.
/// A line-by-line port of `physics.find_antistuds`.
pub fn find_antistuds(sh: &Shape) -> Vec<DVec3> {
    let (lo, hi) = (sh.lo, sh.hi);
    let ymax = &sh.hy.1; // (nz, nx): lowest surface in each column (LDraw +y is down)
    let (nx, nz) = (sh.n[0] as i64, sh.n[2] as i64);
    let arange = |a: f64, b: f64, s: f64| -> Vec<f64> {
        let mut v = Vec::new();
        let mut i = 0;
        loop {
            let x = a + i as f64 * s;
            if x >= b {
                break v;
            }
            v.push(x);
            i += 1;
        }
    };
    let xs = arange(
        ((lo[0] as f64 + 5.0) / 10.0).ceil() * 10.0,
        hi[0] as f64 - 5.0 + 1e-6,
        10.0,
    );
    let zs = arange(
        ((lo[2] as f64 + 5.0) / 10.0).ceil() * 10.0,
        hi[2] as f64 - 5.0 + 1e-6,
        10.0,
    );
    if xs.is_empty() || zs.is_empty() {
        return Vec::new();
    }
    // Candidates in meshgrid "xy" ravel order: z outer, x inner.
    let cands: Vec<(f64, f64)> = zs
        .iter()
        .flat_map(|&z| xs.iter().map(move |&x| (x, z)))
        .collect();
    let gather = |c: (f64, f64), dx: f64, dz: f64| -> f64 {
        let px = (c.0 + dx - lo[0] as f64).floor() as i64;
        let pz = (c.1 + dz - lo[2] as f64).floor() as i64;
        if px >= 0 && px < nx && pz >= 0 && pz < nz {
            ymax[(pz * nx + px) as usize]
        } else {
            f64::NAN
        }
    };
    let g = arange(-10.5, 11.0, 1.0);
    let (mut disk_off, mut ring_off) = (Vec::new(), Vec::new());
    for &gz in &g {
        for &gx in &g {
            let r = gx.hypot(gz);
            if r <= 4.6 {
                disk_off.push((gx, gz));
            }
            if (6.4..=10.6).contains(&r) {
                ring_off.push((gx, gz));
            }
        }
    }
    let probes: Vec<Vec<(f64, f64)>> = (0..8)
        .map(|k| {
            let th = k as f64 * std::f64::consts::PI / 4.0;
            arange(6.5, 10.6, 1.0)
                .iter()
                .map(|rr| ((rr * th.cos()).floor() + 0.5, (rr * th.sin()).floor() + 0.5))
                .collect()
        })
        .collect();
    struct Cand {
        ring: Vec<f64>,
        disk_low: f64,
        has_ceiling: bool,
        probes: Vec<Vec<f64>>,
    }
    let info: Vec<Cand> = cands
        .iter()
        .map(|&c| {
            let disk: Vec<f64> = disk_off.iter().map(|&(dx, dz)| gather(c, dx, dz)).collect();
            let valid = disk.iter().filter(|v| !v.is_nan()).count();
            Cand {
                ring: ring_off.iter().map(|&(dx, dz)| gather(c, dx, dz)).collect(),
                disk_low: disk
                    .iter()
                    .filter(|v| !v.is_nan())
                    .fold(f64::NEG_INFINITY, |m, &v| m.max(v)),
                has_ceiling: valid as f64 / disk.len() as f64 > 0.5,
                probes: probes
                    .iter()
                    .map(|p| p.iter().map(|&(dx, dz)| gather(c, dx, dz)).collect())
                    .collect(),
            }
        })
        .collect();
    // Planes: rounded surface depths that occur in at least 6 pixels, lowest (largest y) first.
    let mut counts = std::collections::BTreeMap::<i64, usize>::new();
    for v in ymax.iter().filter(|v| !v.is_nan()) {
        *counts.entry(round_half_even(*v) as i64).or_default() += 1;
    }
    let mut out = Vec::new();
    let mut taken = vec![false; cands.len()];
    for (&y, _) in counts.iter().rev().filter(|(_, c)| **c >= 6) {
        let y = y as f64;
        for (i, c) in info.iter().enumerate() {
            if taken[i] {
                continue;
            }
            let at_plane = c.ring.iter().filter(|v| (**v - y).abs() < 0.75).count() >= 3;
            let fits = c.disk_low <= y - (STUD_H - 0.7);
            let cnt = c
                .probes
                .iter()
                .filter(|p| p.iter().any(|v| *v >= y - 1.5))
                .count();
            if at_plane && fits && cnt >= 4 && (c.has_ceiling || cnt >= 6) {
                taken[i] = true;
                out.push(DVec3::new(cands[i].0, y, cands[i].1));
            }
        }
    }
    out
}

/// A placed shape: world = `r * local + t`.
#[derive(Debug, Clone)]
pub struct Body {
    pub shape: Arc<Shape>,
    pub r: DMat3,
    pub t: DVec3,
    rt: DMat3,
    /// World box of the full shape (studs included).
    pub flo: DVec3,
    pub fhi: DVec3,
    pub studs: Vec<DVec3>,
    pub sdir: Vec<DVec3>,
    pub anti: Vec<DVec3>,
    pub adir: Vec<DVec3>,
}

fn box_corners(lo: DVec3, hi: DVec3) -> [DVec3; 8] {
    std::array::from_fn(|i| {
        DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        )
    })
}

impl Body {
    pub fn new(shape: Arc<Shape>, r: DMat3, t: DVec3) -> Body {
        let (mut flo, mut fhi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
        for c in box_corners(shape.full_lo, shape.full_hi) {
            let w = r * c + t;
            flo = flo.min(w);
            fhi = fhi.max(w);
        }
        Body {
            studs: shape.studs.iter().map(|s| r * *s + t).collect(),
            sdir: shape.sdir.iter().map(|d| r * *d).collect(),
            anti: shape.anti.iter().map(|a| r * *a + t).collect(),
            adir: shape.anti.iter().map(|_| r * DVec3::Y).collect(),
            rt: r.transpose(),
            shape,
            r,
            t,
            flo,
            fhi,
        }
    }

    #[inline]
    pub fn occ(&self, p: DVec3, studs: bool, shrink: f64) -> bool {
        self.shape.inside(self.rt * (p - self.t), studs, shrink)
    }
}

/// An engaged stud: `male_a` says which body carries the stud.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub male_a: bool,
    pub stud: usize,
    pub anti: usize,
    pub base: DVec3,
    pub dir: DVec3,
}

impl Link {
    /// The engaged stud's volume (plus margin), removed from the overlap.
    #[inline]
    fn keeps_out(&self, p: DVec3) -> bool {
        let d = p - self.base;
        let s = d.dot(self.dir);
        let r = STUD_R + KEEPOUT;
        s >= -KEEPOUT && s <= STUD_H + KEEPOUT && d.length_squared() - s * s <= r * r
    }
}

#[derive(Debug, Clone, Default)]
pub struct Relation {
    /// LDU³ of solid overlap with engaged stud volumes removed.
    pub overlap: f64,
    /// LDU³ before removing them.
    pub raw_overlap: f64,
    pub contact: bool,
    pub studs_ab: usize,
    pub studs_ba: usize,
    pub links: Vec<Link>,
    /// Studs lined up but the parts do not touch outside them (a gap): the links were dropped.
    pub not_home: bool,
    /// Grid points sampled (cost measure).
    pub points: usize,
}

impl Relation {
    pub fn collides(&self) -> bool {
        self.overlap >= COLLIDE_MIN
    }
}

/// Stud <-> anti-stud pairs: every stud of `m` within CONNECT_TOL of an opposite-facing anti-stud of `f`.
fn stud_links(m: &Body, f: &Body, male_a: bool, out: &mut Vec<Link>) {
    for (i, (s, d)) in m.studs.iter().zip(&m.sdir).enumerate() {
        if let Some(j) = f
            .anti
            .iter()
            .zip(&f.adir)
            .position(|(a, ad)| s.distance(*a) < CONNECT_TOL && d.dot(*ad) < -0.9)
        {
            out.push(Link {
                male_a,
                stud: i,
                anti: j,
                base: *s,
                dir: *d,
            });
        }
    }
}

fn square(r: &DMat3) -> bool {
    r.to_cols_array()
        .iter()
        .all(|v| v.abs() < 1e-3 || v.abs() > 1.0 - 1e-3)
}

fn on_lattice(t: DVec3) -> bool {
    (t - t.round()).abs().max_element() < 1e-3
}

/// Sampling frame for the pair (rotation, origin), or None for the world grid (`physics.grid_frame`).
fn grid_frame(a: &Body, b: &Body) -> Option<(DMat3, DVec3)> {
    for x in [a, b] {
        if square(&x.r) {
            return if on_lattice(x.t) {
                None
            } else {
                let c = x.r.to_cols_array().map(f64::round);
                Some((DMat3::from_cols_array(&c), x.t))
            };
        }
    }
    Some((a.r, a.t))
}

fn frame_box(x: &Body, rf: &DMat3, of: DVec3) -> (DVec3, DVec3) {
    let rft = rf.transpose();
    let (mut lo, mut hi) = (DVec3::splat(f64::INFINITY), DVec3::splat(f64::NEG_INFINITY));
    for c in box_corners(x.shape.full_lo, x.shape.full_hi) {
        let p = rft * (x.r * c + x.t - of);
        lo = lo.min(p);
        hi = hi.max(p);
    }
    (lo, hi)
}

/// Face-neighbouring set cells between two occupancy grids laid out x-major (`(x * ny + y) * nz + z`).
fn adjacent(a: &[bool], b: &[bool], n: [usize; 3]) -> bool {
    let [nx, ny, nz] = n;
    let strides = [ny * nz, nz, 1];
    for ax in 0..3 {
        if n[ax] < 2 {
            continue;
        }
        let st = strides[ax];
        for x in 0..nx {
            for y in 0..ny {
                for z in 0..nz {
                    let c = [x, y, z];
                    if c[ax] + 1 >= n[ax] {
                        continue;
                    }
                    let i = (x * ny + y) * nz + z;
                    if (a[i] && b[i + st]) || (a[i + st] && b[i]) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Overlap, contact and stud links between two bodies, or None when their boxes are apart.
pub fn relation(a: &Body, b: &Body, need_contact: bool) -> Option<Relation> {
    let mut lo = a.flo.max(b.flo) - 1.0;
    let mut hi = a.fhi.min(b.fhi) + 1.0;
    if (hi - lo).min_element() <= 0.0 {
        return None;
    }
    let mut links = Vec::new();
    stud_links(a, b, true, &mut links);
    stud_links(b, a, false, &mut links);
    let count = |l: &[Link]| {
        (
            l.iter().filter(|m| m.male_a).count(),
            l.iter().filter(|m| !m.male_a).count(),
        )
    };
    let (mut ab, mut ba) = count(&links);
    let frame = grid_frame(a, b);
    if let Some((rf, of)) = frame {
        let (l1, h1) = frame_box(a, &rf, of);
        let (l2, h2) = frame_box(b, &rf, of);
        lo = l1.max(l2) - 1.0;
        hi = h1.min(h2) + 1.0;
        if (hi - lo).min_element() <= 0.0 {
            return Some(Relation {
                contact: ab + ba > 0,
                studs_ab: ab,
                studs_ba: ba,
                links,
                ..Default::default()
            });
        }
    }
    let vol = (hi - lo).element_product();
    let step = if vol < 1.5e6 { 1.0 } else { 2.0 };
    let axis = |a: usize| -> (f64, usize) {
        let s0 = (lo[a] / step).floor() * step + step / 2.0;
        let n = if hi[a] > s0 {
            ((hi[a] - s0) / step).ceil() as usize
        } else {
            0
        };
        (s0, n)
    };
    let (ax, ay, az) = (axis(0), axis(1), axis(2));
    let n = [ax.1, ay.1, az.1];
    let total = n[0] * n[1] * n[2];
    if total == 0 {
        return Some(Relation {
            contact: ab + ba > 0,
            studs_ab: ab,
            studs_ba: ba,
            links,
            ..Default::default()
        });
    }
    let mut oa = vec![false; total];
    let mut ob = vec![false; total];
    let mut any_a = false;
    let point = |i: usize, j: usize, k: usize| -> DVec3 {
        let q = DVec3::new(
            ax.0 + i as f64 * step,
            ay.0 + j as f64 * step,
            az.0 + k as f64 * step,
        );
        match &frame {
            Some((rf, of)) => *rf * q + *of,
            None => q,
        }
    };
    let (mut raw, mut hard, mut hard_free) = (0usize, 0usize, 0usize);
    let mut idx = 0;
    for i in 0..n[0] {
        for j in 0..n[1] {
            for k in 0..n[2] {
                let p = point(i, j, k);
                let ia = a.occ(p, true, 0.0);
                let ib = b.occ(p, true, 0.0);
                oa[idx] = ia;
                ob[idx] = ib;
                any_a |= ia;
                if ia && ib {
                    raw += 1;
                    if !links.iter().any(|l| l.keeps_out(p)) {
                        hard += 1;
                        hard_free += 1;
                    }
                }
                idx += 1;
            }
        }
    }
    let vox = step * step * step;
    let mut rel = Relation {
        points: total,
        ..Default::default()
    };
    if !any_a {
        rel.contact = ab + ba > 0;
        rel.studs_ab = ab;
        rel.studs_ba = ba;
        rel.links = links;
        return Some(rel);
    }
    rel.raw_overlap = raw as f64 * vox;
    rel.overlap = hard as f64 * vox;
    rel.contact = ab + ba > 0 || raw > 0;
    if !rel.contact && need_contact {
        rel.contact = adjacent(&oa, &ob, n);
    }
    if !links.is_empty() && hard_free == 0 {
        // Studs only hold when pressed home: outside the stud volumes the bodies must touch.
        let mut idx = 0;
        for i in 0..n[0] {
            for j in 0..n[1] {
                for k in 0..n[2] {
                    if (oa[idx] || ob[idx]) && links.iter().any(|l| l.keeps_out(point(i, j, k))) {
                        oa[idx] = false;
                        ob[idx] = false;
                    }
                    idx += 1;
                }
            }
        }
        if !adjacent(&oa, &ob, n) {
            rel.not_home = true;
            links.clear();
            (ab, ba) = (0, 0);
        }
    }
    let _ = count;
    rel.studs_ab = ab;
    rel.studs_ba = ba;
    rel.links = links;
    Some(rel)
}

/// Early-exit collision predicate for geometric-stud placement previews. This shares the
/// voxel sampling frame and engaged-stud keep-outs with `relation`; it does NOT replace
/// shadow-connector matching or full relation on drop.
pub fn fast_legal(a: &Body, b: &Body) -> bool {
    if (a.flo.cmpgt(b.fhi + DVec3::ONE) | b.flo.cmpgt(a.fhi + DVec3::ONE)).any() {
        return true;
    }
    let mut links = Vec::new();
    stud_links(a, b, true, &mut links);
    stud_links(b, a, false, &mut links);
    let (r, t) = grid_frame(a, b).unwrap_or((DMat3::IDENTITY, DVec3::ZERO));
    let (alo, ahi) = frame_box(a, &r, t);
    let (blo, bhi) = frame_box(b, &r, t);
    let lo = alo.max(blo) - DVec3::ONE;
    let hi = ahi.min(bhi) + DVec3::ONE;
    if lo.cmpge(hi).any() {
        return true;
    }
    // Same 1/2-LDU voxel-centre convention as the full path for normal-sized parts.
    let step = if (hi - lo).element_product() < 1.5e6 {
        1.0
    } else {
        2.0
    };
    let start = (lo / step).floor() * step + DVec3::splat(step / 2.0);
    let mut hard = 0;
    let mut x = start.x;
    while x < hi.x {
        let mut y = start.y;
        while y < hi.y {
            let mut z = start.z;
            while z < hi.z {
                let p = r * DVec3::new(x, y, z) + t;
                if a.occ(p, true, 0.0)
                    && b.occ(p, true, 0.0)
                    && !links.iter().any(|l| l.keeps_out(p))
                {
                    hard += 1;
                    if hard as f64 * step * step * step >= COLLIDE_MIN {
                        return false;
                    }
                }
                z += step;
            }
            y += step;
        }
        x += step;
    }
    true
}
