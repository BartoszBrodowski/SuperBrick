//! Small, deterministic reseating after an ordinary rotation. Articulated joints
//! use their own constraints and never pass through this search.
use crate::{Engine, Part, Result};
use glam::DVec3;
use physics::Body;

#[derive(Clone)]
pub(crate) struct Record {
    revision: u64,
    before: Vec<Part>,
    after: Vec<Part>,
    axis: DVec3,
    angle: f64,
}

impl Engine {
    pub(crate) fn fit_rotation(
        &mut self,
        before: Vec<Part>,
        axis: DVec3,
        angle: f64,
    ) -> Result<()> {
        if before.is_empty() {
            return Ok(());
        }
        let inverse = self
            .last_rotation
            .as_ref()
            .filter(|last| {
                last.revision == self.revision
                    && (last.axis * last.angle + axis * angle).length() < 1e-8
                    && last.after.len() == before.len()
                    && before.iter().all(|p| {
                        last.after
                            .iter()
                            .any(|q| p.id == q.id && p.pos == q.pos && p.rot == q.rot)
                    })
            })
            .map(|last| last.before.clone());
        if let Some(restore) = inverse {
            for p in restore {
                let current = self.model.parts.get_mut(&p.id).unwrap();
                current.pos = p.pos;
                current.rot = p.rot;
                self.model.write_part(p.id);
            }
            self.remember_rotation(before, axis, angle);
            return Ok(());
        }
        let file = before[0].file;
        if before.iter().any(|p| p.file != file) {
            return Ok(());
        }
        let lines = before
            .iter()
            .map(|p| p.line)
            .collect::<std::collections::HashSet<_>>();
        let original_flat = {
            let mut model = self.model.clone();
            for p in &before {
                model.parts.insert(p.id, p.clone());
                model.write_part(p.id);
            }
            model.flat(file)
        };
        for p in &before {
            self.model.write_part(p.id);
        }
        let flat = self.model.flat(file);
        let mut moving = Vec::new();
        let mut fixed = Vec::new();
        let mut original = Vec::new();
        for p in &flat {
            let shape = self
                .validator
                .shape(&self.lib, &p.name)
                .ok_or_else(|| format!("Cannot check rotation geometry for {}", p.name))?;
            let body = Body::new(shape, p.rot, p.pos);
            if lines.contains(&p.root_line) {
                moving.push(body);
            } else {
                fixed.push(body);
            }
        }
        for p in original_flat
            .iter()
            .filter(|p| lines.contains(&p.root_line))
        {
            original.push(Body::new(
                self.validator
                    .shape(&self.lib, &p.name)
                    .ok_or_else(|| format!("Cannot check rotation geometry for {}", p.name))?,
                p.rot,
                p.pos,
            ));
        }
        fixed.retain(|b| {
            moving.iter().any(|a| {
                (a.fhi + DVec3::splat(21.0)).cmpge(b.flo).all()
                    && (b.fhi + DVec3::splat(21.0)).cmpge(a.flo).all()
            })
        });
        // One stud of local reseating, on a fine 2-LDU grid. Add exact box-face
        // offsets as well, so ordinary flush arrangements need not land on it.
        let mut axes = [Vec::new(), Vec::new(), Vec::new()];
        for a in &mut axes {
            a.extend((-10..=10).map(|v| v as f64 * 2.0));
        }
        for a in &moving {
            for b in &fixed {
                for k in 0..3 {
                    for v in [b.fhi[k] - a.flo[k], b.flo[k] - a.fhi[k]] {
                        if v.abs() <= 20.0 {
                            axes[k].push(v);
                        }
                    }
                }
            }
        }
        for a in &mut axes {
            a.sort_by(f64::total_cmp);
            a.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        }
        let mut offsets = Vec::new();
        for &x in &axes[0] {
            for &y in &axes[1] {
                for &z in &axes[2] {
                    let d = DVec3::new(x, y, z);
                    if d.length_squared() <= 400.000001 {
                        offsets.push(d);
                    }
                }
            }
        }
        offsets.sort_by(|a, b| {
            a.length_squared()
                .total_cmp(&b.length_squared())
                .then(a.x.total_cmp(&b.x))
                .then(a.y.total_cmp(&b.y))
                .then(a.z.total_cmp(&b.z))
        });
        let solution = offsets
            .into_iter()
            .find(|delta| {
                let bodies = moving
                    .iter()
                    .map(|a| Body::new(a.shape.clone(), a.r, a.t + *delta))
                    .collect::<Vec<_>>();
                if bodies
                    .iter()
                    .any(|a| fixed.iter().any(|b| !physics::fast_legal(a, b)))
                {
                    return false;
                }
                // Exact shape occupancy/contact, not merely overlapping bounding boxes.
                bodies.iter().any(|a| {
                    original.iter().any(|b| {
                        physics::relation(a, b, true)
                            .is_some_and(|r| r.contact || r.raw_overlap > 0.0)
                    })
                })
            })
            .ok_or("No collision-free rotation touching the original placement within one stud")?;
        for p in &before {
            self.model.parts.get_mut(&p.id).unwrap().pos += solution;
            self.model.write_part(p.id);
        }
        self.remember_rotation(before, axis, angle);
        Ok(())
    }
    fn remember_rotation(&mut self, before: Vec<Part>, axis: DVec3, angle: f64) {
        let after = before
            .iter()
            .map(|p| self.model.parts[&p.id].clone())
            .collect();
        self.last_rotation = Some(Record {
            revision: self.revision + 1,
            before,
            after,
            axis,
            angle,
        });
    }
}
