//! CPU ray visibility for connector markers. Holes stay selectable; opaque geometry
//! in front of a marker (including its own part) hides it from drawing and picking.
use glam::{DMat3, DVec3};
use std::sync::Arc;

struct Body {
    mesh: Arc<ldraw::Mesh>,
    inverse: DMat3,
    pos: DVec3,
    lo: DVec3,
    hi: DVec3,
}
pub struct Occlusion {
    bodies: Vec<Body>,
}
impl Occlusion {
    pub fn new(snapshot: &engine::Snapshot, file: usize, lib: &ldraw::Library) -> Self {
        let bodies = snapshot
            .model
            .flat(file)
            .into_iter()
            .filter_map(|p| {
                let hidden = p.ancestors.iter().chain(std::iter::once(&p.src)).any(|s| {
                    s.0 == 0
                        && snapshot
                            .model
                            .parts
                            .values()
                            .any(|q| q.file == s.1 && q.line == s.2 && q.hidden)
                });
                if hidden
                    || lib
                        .colors
                        .by_code
                        .get(&p.color.0)
                        .is_some_and(|c| c.alpha < 255)
                {
                    return None;
                }
                let mesh = lib.mesh(&p.name, ldraw::Lod::Std).ok()?;
                let (lo, hi) = mesh.bbox(false);
                Some(Body {
                    mesh,
                    inverse: p.rot.transpose(),
                    pos: p.pos,
                    lo: lo.as_dvec3(),
                    hi: hi.as_dvec3(),
                })
            })
            .collect();
        Self { bodies }
    }
    pub fn visible(&self, eye: DVec3, point: DVec3) -> bool {
        let length = eye.distance(point);
        if length < 1. {
            return true;
        }
        let direction = (point - eye) / length;
        !self.bodies.iter().any(|b| {
            let o = b.inverse * (eye - b.pos);
            let d = b.inverse * direction;
            if !box_hit(o, d, b.lo, b.hi, length - 0.8) {
                return false;
            }
            b.mesh.tris.iter().any(|t| {
                triangle_hit(o, d, t.map(|v| v.as_dvec3()))
                    .is_some_and(|t| t > 0.01 && t < length - 0.8)
            })
        })
    }
}
fn box_hit(o: DVec3, d: DVec3, lo: DVec3, hi: DVec3, max: f64) -> bool {
    let mut near = 0f64;
    let mut far = max;
    for i in 0..3 {
        if d[i].abs() < 1e-12 {
            if o[i] < lo[i] || o[i] > hi[i] {
                return false;
            }
        } else {
            let a = (lo[i] - o[i]) / d[i];
            let b = (hi[i] - o[i]) / d[i];
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
    }
    near <= far
}
fn triangle_hit(o: DVec3, d: DVec3, t: [DVec3; 3]) -> Option<f64> {
    let a = t[1] - t[0];
    let b = t[2] - t[0];
    let h = d.cross(b);
    let det = a.dot(h);
    if det.abs() < 1e-10 {
        return None;
    }
    let s = o - t[0];
    let u = s.dot(h) / det;
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = s.cross(a);
    let v = d.dot(q) / det;
    if v < 0. || u + v > 1. {
        return None;
    }
    Some(b.dot(q) / det)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opaque_brick_hides_markers_behind_but_not_in_front() {
        let lib = Arc::new(ldraw::Library::open_default().unwrap());
        let doc = ldraw::Document::parse(b"1 4 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n", "test.ldr");
        let e = engine::Engine::new(lib.clone(), doc, None);
        let oc = Occlusion::new(&e.snapshot(), 0, &lib);
        let eye = DVec3::new(0., 12., -200.);
        assert!(oc.visible(eye, DVec3::new(0., 12., -40.)));
        assert!(!oc.visible(eye, DVec3::new(0., 12., 40.)));
    }
}
