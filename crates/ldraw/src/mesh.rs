//! Flattened part geometry (`Geom` and `Library._flatten` in ldraw.py).
//!
//! Positions are f32 and transforms run in f32, as in the Python bench, so Shape depth maps built from
//! them later compare closely. Triangles of male stud primitives are flagged (`tstud`) and each stud is
//! also kept as an instance (`studs`) so the renderer can draw studs instanced and cull covered ones.

use std::sync::Arc;

use glam::{DMat3, DVec3, Mat3, Vec3};

use crate::{
    HOLLOW_STUDS, MALE_STUDS,
    color::{Color, EDGE_BASE},
    library::{Library, Lod, Result, Error, norm},
    num::{floats, split_lines},
};

/// One male stud of a part, in the part's frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Stud {
    /// Base centre of the stud.
    pub pos: Vec3,
    /// Unit direction the stud points (LDraw -Y of the primitive).
    pub dir: Vec3,
    pub hollow: bool,
    /// Full transform of the stud primitive (may include scale or mirroring).
    pub mat: Mat3,
    /// Index into [`MALE_STUDS`].
    pub prim: u8,
    /// Colour the stud is drawn in (after recolouring).
    pub color: Color,
    /// False for a stud primitive nested inside another stud primitive (draw only outer ones).
    pub outer: bool,
    /// The stud's triangles: `tris[tri_start .. tri_start + tri_count]`; likewise its edge lines and
    /// conditional lines. Each range is the stud primitive's whole subtree.
    pub tri_start: u32,
    pub tri_count: u32,
    pub line_start: u32,
    pub line_count: u32,
    pub cond_start: u32,
    pub cond_count: u32,
}

/// Flattened geometry of one LDraw file in its own frame.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub tris: Vec<[Vec3; 3]>,
    pub tcol: Vec<Color>,
    /// Triangle belongs to a male stud primitive.
    pub tstud: Vec<bool>,
    pub lines: Vec<[Vec3; 2]>,
    pub lcol: Vec<Color>,
    /// Conditional lines: p1 p2 c1 c2.
    pub cond: Vec<[Vec3; 4]>,
    pub ccol: Vec<Color>,
    pub studs: Vec<Stud>,
}

/// Recolour a code for a sub-file placed with colour `color` (`Geom.transformed.recol`).
pub fn recol(c: Color, color: Color) -> Color {
    if color == Color::MAIN {
        c
    } else if c == Color::EDGE {
        if color.0 >= 0 { Color(EDGE_BASE + color.0) } else { Color::EDGE }
    } else if c == Color::MAIN {
        color
    } else {
        c
    }
}

/// LDraw row-major `a b c d e f g h i` to a matrix (columns are glam's storage).
pub fn mat_from_rows(v: &[f64]) -> DMat3 {
    DMat3::from_cols(
        DVec3::new(v[0], v[3], v[6]),
        DVec3::new(v[1], v[4], v[7]),
        DVec3::new(v[2], v[5], v[8]),
    )
}

/// A matrix back to LDraw row-major order.
pub fn mat_to_rows(m: &DMat3) -> [f64; 9] {
    m.transpose().to_cols_array()
}

fn v3(a: &[f64]) -> Vec3 {
    // Parse as f64, then round to f32, exactly as numpy's float32 arrays do.
    Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32)
}

impl Mesh {
    pub fn is_empty(&self) -> bool {
        self.tris.is_empty() && self.lines.is_empty() && self.cond.is_empty()
    }

    /// Append `sub` placed by `m`, `t` with colour `color`.
    pub fn append_transformed(&mut self, sub: &Mesh, m: Mat3, t: Vec3, color: Color) {
        let base = self.tris.len() as u32;
        let (lbase, cbase) = (self.lines.len() as u32, self.cond.len() as u32);
        self.tris.extend(sub.tris.iter().map(|tri| tri.map(|v| m * v + t)));
        self.tcol.extend(sub.tcol.iter().map(|&c| recol(c, color)));
        self.tstud.extend_from_slice(&sub.tstud);
        self.lines.extend(sub.lines.iter().map(|l| l.map(|v| m * v + t)));
        self.lcol.extend(sub.lcol.iter().map(|&c| recol(c, color)));
        self.cond.extend(sub.cond.iter().map(|l| l.map(|v| m * v + t)));
        self.ccol.extend(sub.ccol.iter().map(|&c| recol(c, color)));
        self.studs.extend(sub.studs.iter().map(|s| {
            let d = m * s.dir;
            Stud {
                pos: m * s.pos + t,
                dir: d / d.length().max(1e-9),
                mat: m * s.mat,
                color: recol(s.color, color),
                tri_start: s.tri_start + base,
                line_start: s.line_start + lbase,
                cond_start: s.cond_start + cbase,
                ..s.clone()
            }
        }));
    }

    /// Axis-aligned bounds, ignoring stud triangles when asked (`Geom.bbox`).
    pub fn bbox(&self, exclude_studs: bool) -> (Vec3, Vec3) {
        let use_mask = exclude_studs && self.tstud.iter().any(|&s| s) && self.tstud.iter().any(|&s| !s);
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for (i, tri) in self.tris.iter().enumerate() {
            if use_mask && self.tstud[i] {
                continue;
            }
            for v in tri {
                lo = lo.min(*v);
                hi = hi.max(*v);
            }
        }
        if lo.x > hi.x { (Vec3::ZERO, Vec3::ZERO) } else { (lo, hi) }
    }
}

struct SubRef {
    color: Color,
    pos: DVec3,
    m: DMat3,
    name: String,
}

impl Library {
    /// Flattened geometry of a part or other library file (`Library.geom`).
    pub fn mesh(&self, name: &str, lod: Lod) -> Result<Arc<Mesh>> {
        let key = self.resolve(name, lod).ok_or_else(|| Error::NotFound(name.to_string()))?;
        let mut stack = Vec::new();
        Ok(self.flatten_key(&key, lod, &mut stack, 0).0)
    }

    /// Returns the mesh and whether it was cut short (depth limit or a reference cycle);
    /// cut-short results are not cached, so a file reached shallower elsewhere still gets all of it.
    fn flatten_key(&self, key: &str, lod: Lod, stack: &mut Vec<String>, depth: u32) -> (Arc<Mesh>, bool) {
        let ck = (key.to_string(), lod);
        if let Some(m) = self.meshes.read().get(&ck) {
            return (m.clone(), false);
        }
        if depth > 40 || stack.iter().any(|k| k == key) {
            return (Arc::new(Mesh::default()), true);
        }
        let Ok(text) = self.read_text(key) else {
            return (Arc::new(Mesh::default()), false);
        };
        let mut g = Mesh::default();
        let mut subs: Vec<SubRef> = Vec::new();
        for raw in split_lines(&text) {
            let s = raw.trim();
            if s.is_empty() || s.starts_with('0') {
                continue;
            }
            let t: Vec<&str> = s.split_whitespace().collect();
            let Ok(typ) = t[0].parse::<i64>() else { continue };
            let col = || t.get(1).and_then(|c| Color::parse(c));
            match typ {
                1 if t.len() >= 15 => {
                    let (Some(color), Some(v)) = (col(), floats::<12>(&t[2..14])) else { continue };
                    subs.push(SubRef {
                        color,
                        pos: DVec3::new(v[0], v[1], v[2]),
                        m: mat_from_rows(&v[3..12]),
                        name: t[14..].join(" "),
                    });
                }
                2 if t.len() >= 8 => {
                    let (Some(c), Some(v)) = (col(), floats::<6>(&t[2..8])) else { continue };
                    g.lines.push([v3(&v[0..3]), v3(&v[3..6])]);
                    g.lcol.push(c);
                }
                3 if t.len() >= 11 => {
                    let (Some(c), Some(v)) = (col(), floats::<9>(&t[2..11])) else { continue };
                    g.tris.push([v3(&v[0..3]), v3(&v[3..6]), v3(&v[6..9])]);
                    g.tcol.push(c);
                    g.tstud.push(false);
                }
                4 if t.len() >= 14 => {
                    let (Some(c), Some(v)) = (col(), floats::<12>(&t[2..14])) else { continue };
                    let (a, b, cc, d) = (v3(&v[0..3]), v3(&v[3..6]), v3(&v[6..9]), v3(&v[9..12]));
                    g.tris.push([a, b, cc]);
                    g.tris.push([a, cc, d]);
                    g.tcol.extend([c, c]);
                    g.tstud.extend([false, false]);
                }
                5 if t.len() >= 14 => {
                    let (Some(c), Some(v)) = (col(), floats::<12>(&t[2..14])) else { continue };
                    g.cond.push([v3(&v[0..3]), v3(&v[3..6]), v3(&v[6..9]), v3(&v[9..12])]);
                    g.ccol.push(c);
                }
                _ => {}
            }
        }
        let mut truncated = false;
        stack.push(key.to_string());
        for r in subs {
            let Some(skey) = self.resolve(&r.name, lod) else { continue };
            let n = norm(&r.name);
            let base = n.rsplit('/').next().unwrap_or(&n);
            let (sub, cut) = self.flatten_key(&skey, lod, stack, depth + 1);
            truncated |= cut;
            let (m32, p32) = (r.m.as_mat3(), r.pos.as_vec3());
            let (tri0, line0, cond0) = (g.tris.len(), g.lines.len(), g.cond.len());
            let stud0 = g.studs.len();
            g.append_transformed(&sub, m32, p32, r.color);
            if skey.starts_with("p/")
                && let Some(prim) = MALE_STUDS.iter().position(|&s| s == base)
            {
                g.tstud[tri0..].fill(true);
                for s in &mut g.studs[stud0..] {
                    s.outer = false;
                }
                let d = r.m * DVec3::new(0.0, -1.0, 0.0);
                let len = d.length();
                let d = if len > 0.0 { d / len } else { DVec3::NEG_Y };
                g.studs.push(Stud {
                    pos: p32,
                    dir: d.as_vec3(),
                    hollow: HOLLOW_STUDS.contains(&base),
                    mat: m32,
                    prim: prim as u8,
                    color: r.color,
                    outer: true,
                    tri_start: tri0 as u32,
                    tri_count: (g.tris.len() - tri0) as u32,
                    line_start: line0 as u32,
                    line_count: (g.lines.len() - line0) as u32,
                    cond_start: cond0 as u32,
                    cond_count: (g.cond.len() - cond0) as u32,
                });
            }
        }
        stack.pop();
        let g = Arc::new(g);
        if truncated {
            return (g, true);
        }
        let g = self.meshes.write().entry(ck).or_insert(g).clone();
        (g, false)
    }

    /// Drop all cached meshes (e.g. after the library changed on disk).
    pub fn clear_mesh_cache(&self) {
        self.meshes.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recolour_rules() {
        assert_eq!(recol(Color(16), Color(4)), Color(4));
        assert_eq!(recol(Color(24), Color(4)), Color(EDGE_BASE + 4));
        assert_eq!(recol(Color(24), Color::direct(0x112233)), Color(24));
        assert_eq!(recol(Color(2), Color(4)), Color(2));
        assert_eq!(recol(Color(16), Color(16)), Color(16));
    }

    #[test]
    fn row_major_round_trip() {
        let rows = [0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0];
        let m = mat_from_rows(&rows);
        assert_eq!(mat_to_rows(&m), rows);
        // x' = c*z: the first row maps local z onto world x.
        assert_eq!(m * DVec3::new(0.0, 0.0, 1.0), DVec3::new(1.0, 0.0, 0.0));
    }
}
