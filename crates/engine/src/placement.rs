//! Geometric-stud placement and surface-relative grid nudging. Other connector families
//! remain explicitly outside the current native validator's completeness guarantee.
use super::{Cmd, Engine, Id, PartSpec, Result};
use glam::{DMat3, DVec3};
use std::collections::{BTreeMap, BTreeSet};

/// Opaque, revision-stamped result of an isolated placement solve.
pub struct PreparedPlacement {
    revision: u64,
    document: Vec<u8>,
    model: super::Model,
    ids: Vec<Id>,
}
pub struct PlacementSolution {
    pub prepared: PreparedPlacement,
    pub rotation: DMat3,
    pub translation: DVec3,
}
impl PreparedPlacement {
    pub(crate) fn rebase(&mut self, snapshot: &super::Snapshot) {
        self.revision = snapshot.revision;
        self.document = snapshot.model.doc.to_bytes();
    }
}
#[derive(Default)]
pub struct NativePlacementCache {
    validator: super::validation::Validator,
}
fn ids_or_part_empty(moving: &[physics::Body]) -> bool {
    moving.is_empty()
}
impl super::Snapshot {
    /// Diagnostic only: actual collision checks still include all hidden geometry.
    pub fn hidden_placement_blockers(
        &self,
        lib: std::sync::Arc<ldraw::Library>,
        file: usize,
        ids: &[Id],
        part: &PartSpec,
        near: DVec3,
        rotation: DMat3,
        ground: bool,
    ) -> Vec<Id> {
        let mut e = self.fork(lib);
        let flat = self.model.flat(file);
        let index: std::collections::HashMap<_, _> = self
            .model
            .parts
            .values()
            .map(|p| ((0, p.file, p.line), p))
            .collect();
        let is_moving = |p: &ldraw::FlatPart| {
            index
                .get(&(0, file, p.root_line))
                .is_some_and(|v| ids.contains(&v.id))
        };
        let mut moving: Vec<_> = if ids.is_empty() {
            e.validator
                .shape(&e.lib, &part.name)
                .map(|s| vec![physics::Body::new(s, rotation * part.rot, near)])
                .unwrap_or_default()
        } else {
            flat.iter()
                .filter(|p| is_moving(p))
                .filter_map(|p| {
                    e.validator.shape(&e.lib, &p.name).map(|s| {
                        physics::Body::new(
                            s,
                            rotation * p.rot,
                            rotation * (p.pos - part.pos) + near,
                        )
                    })
                })
                .collect()
        };
        if ground && !moving.is_empty() {
            let bottom = moving
                .iter()
                .map(|b| b.fhi.y)
                .fold(f64::NEG_INFINITY, f64::max);
            let dy = e.floor_height(file) - bottom;
            moving = moving
                .into_iter()
                .map(|b| physics::Body::new(b.shape, b.r, b.t + DVec3::Y * dy))
                .collect();
        }
        let mut blockers = self.hidden_overlapping(&mut e, file, &moving, ids);
        if blockers.is_empty() && !ids_or_part_empty(&moving) {
            // The cursor pose itself may overlap nothing hidden while the pose the solver *would*
            // have chosen is blocked by a hidden piece: re-solve with hidden pieces removed and
            // report the ones that overlap that pose.
            blockers = self
                .hidden_blockers_of_resolved_pose(
                    e.lib.clone(), file, ids, part, near, rotation, ground,
                )
                .unwrap_or_default();
        }
        blockers
    }
    fn hidden_overlapping(
        &self,
        e: &mut Engine,
        file: usize,
        moving: &[physics::Body],
        ids: &[Id],
    ) -> Vec<Id> {
        let flat = self.model.flat(file);
        let index: std::collections::HashMap<_, _> = self
            .model
            .parts
            .values()
            .map(|p| ((0, p.file, p.line), p))
            .collect();
        let mut blockers = BTreeSet::new();
        for p in &flat {
            if index
                .get(&(0, file, p.root_line))
                .is_some_and(|v| ids.contains(&v.id))
                || !p
                    .ancestors
                    .iter()
                    .chain(std::iter::once(&p.src))
                    .any(|s| index.get(s).is_some_and(|v| v.hidden))
            {
                continue;
            }
            if let Some(shape) = e.validator.shape(&e.lib, &p.name) {
                let body = physics::Body::new(shape, p.rot, p.pos);
                if moving.iter().any(|m| !physics::fast_legal(m, &body)) {
                    if let Some(id) = index.get(&(0, file, p.root_line)).map(|v| v.id) {
                        blockers.insert(id);
                    }
                }
            }
        }
        blockers.into_iter().collect()
    }
    /// Hidden pieces that stop the solver's preferred pose (found by solving without them).
    fn hidden_blockers_of_resolved_pose(
        &self,
        lib: std::sync::Arc<ldraw::Library>,
        file: usize,
        ids: &[Id],
        part: &PartSpec,
        near: DVec3,
        rotation: DMat3,
        ground: bool,
    ) -> Result<Vec<Id>> {
        let hidden: Vec<Id> = self
            .model
            .parts
            .values()
            .filter(|p| p.file == file && p.hidden && !ids.contains(&p.id))
            .map(|p| p.id)
            .collect();
        if hidden.is_empty() {
            return Ok(vec![]);
        }
        let mut e = self.fork(lib);
        e.apply_for_preview(Cmd::Remove { ids: hidden })?;
        let solved = self.prepare_with_engine(
            &mut e,
            file,
            ids.to_vec(),
            part.clone(),
            near,
            DVec3::ZERO,
            ground,
            false,
            rotation,
            false,
        )?;
        let flat = solved.prepared.model.flat(file);
        let moving: Vec<_> = flat
            .iter()
            .filter(|p| {
                solved
                    .prepared
                    .model
                    .parts
                    .values()
                    .any(|v| solved.prepared.ids.contains(&v.id) && v.file == file && v.line == p.root_line)
            })
            .filter_map(|p| {
                e.validator
                    .shape(&e.lib, &p.name)
                    .map(|s| physics::Body::new(s, p.rot, p.pos))
            })
            .collect();
        Ok(self.hidden_overlapping(&mut e, file, &moving, &solved.prepared.ids))
    }
    pub fn prepare_placement(
        &self,
        lib: std::sync::Arc<ldraw::Library>,
        file: usize,
        ids: Vec<Id>,
        part: PartSpec,
        near: DVec3,
        facing: DVec3,
        ground: bool,
        free: bool,
        rotation: DMat3,
    ) -> Result<PlacementSolution> {
        let mut e = self.fork(lib);
        self.prepare_with_engine(
            &mut e, file, ids, part, near, facing, ground, free, rotation, true,
        )
    }
    /// Immediate geometric/grid solve: never invokes the Python connector service.
    pub fn prepare_native_placement(
        &self,
        lib: std::sync::Arc<ldraw::Library>,
        cache: &mut NativePlacementCache,
        file: usize,
        ids: Vec<Id>,
        part: PartSpec,
        near: DVec3,
        facing: DVec3,
        ground: bool,
        free: bool,
        rotation: DMat3,
    ) -> Result<PlacementSolution> {
        let mut e = self.fork(lib);
        std::mem::swap(&mut e.validator, &mut cache.validator);
        let result = self.prepare_with_engine(
            &mut e, file, ids, part, near, facing, ground, free, rotation, false,
        );
        std::mem::swap(&mut e.validator, &mut cache.validator);
        result
    }
    fn prepare_with_engine(
        &self,
        e: &mut Engine,
        file: usize,
        ids: Vec<Id>,
        mut part: PartSpec,
        near: DVec3,
        facing: DVec3,
        ground: bool,
        free: bool,
        rotation: DMat3,
        full_connectors: bool,
    ) -> Result<PlacementSolution> {
        super::check_pose(DVec3::ZERO, rotation)?;
        let pivot = part.pos;
        if rotation != DMat3::IDENTITY {
            if !ids.is_empty() {
                for id in &ids {
                    let p = e.model.parts.get_mut(id).ok_or("unknown moving piece")?;
                    if p.locked {
                        return Err("piece is locked".into());
                    }
                    p.pos = rotation * (p.pos - pivot) + pivot;
                    p.rot = rotation * p.rot;
                    p.revision = self.revision + 1;
                }
                for id in &ids {
                    e.model.write_part(*id);
                }
            }
            part.rot = rotation * part.rot;
        }
        let full = if full_connectors && !free && (!ground || e.needs_connector_drag(&part.name)) {
            e.full_drag_preview(file, &ids, part.clone(), near).ok()
        } else {
            None
        };
        let (command, r, t) = if let Some(full) = full {
            full
        } else if !ids.is_empty() {
            let command =
                e.move_preview(file, ids.clone(), part.clone(), near, facing, ground, free)?;
            let t = match &command {
                Cmd::Move { delta, .. } => *delta,
                _ => DVec3::ZERO,
            };
            (command, DMat3::IDENTITY, t)
        } else {
            let part = if free {
                PartSpec { pos: near, ..part }
            } else {
                e.place_preview(file, part, near, facing, ground, &[])?
            };
            let t = part.pos;
            (
                Cmd::Add {
                    file,
                    parts: vec![part],
                    free,
                },
                DMat3::IDENTITY,
                t,
            )
        };
        // Native preview already checked candidate collisions with fast_legal.
        // Do not repeat the expensive full-model pass or change the accepted pose.
        let command = if !full_connectors {
            match command {
                Cmd::Add { file, parts, .. } => Cmd::Add {
                    file,
                    parts,
                    free: true,
                },
                Cmd::Move { ids, delta, .. } => Cmd::Move {
                    ids,
                    delta,
                    free: true,
                },
                other => other,
            }
        } else {
            command
        };
        let result = e.apply_for_preview(command)?;
        let selected: Vec<Id> = if ids.is_empty() {
            result
                .get("ids")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_u64())
                .collect()
        } else {
            ids
        };
        let translation = if selected
            .first()
            .is_some_and(|id| self.model.parts.contains_key(id))
        {
            r * (pivot - rotation * pivot) + t
        } else {
            t
        };
        Ok(PlacementSolution {
            prepared: PreparedPlacement {
                revision: self.revision,
                document: self.model.doc.to_bytes(),
                model: e.model.clone(),
                ids: selected,
            },
            rotation: r * rotation,
            translation,
        })
    }
}
impl Engine {
    pub fn commit_placement(
        &mut self,
        mut prepared: PreparedPlacement,
    ) -> Result<serde_json::Value> {
        if self.revision != prepared.revision || self.model.doc.to_bytes() != prepared.document {
            return Err("The build changed during placement; try again".into());
        }
        for p in prepared.model.parts.values_mut() {
            if self
                .model
                .parts
                .get(&p.id)
                .is_none_or(|old| old.revision != p.revision)
            {
                p.revision = self.revision + 1;
            }
        }
        // The preview ran in a fork without this engine's id reserver, so ids it gave new parts
        // were never reserved: other processes (or this engine's next block) can hand them out
        // again. Give every new part an id from this engine's own allocator instead.
        let old = std::mem::take(&mut prepared.model.parts);
        let mut renamed = BTreeMap::new();
        for (id, mut p) in old {
            if !self.model.parts.contains_key(&id) {
                let fresh = self.allocate();
                renamed.insert(id, fresh);
                p.id = fresh;
            }
            prepared.model.parts.insert(p.id, p);
        }
        for id in &mut prepared.ids {
            if let Some(fresh) = renamed.get(id) {
                *id = *fresh;
            }
        }
        self.undo.push(self.model.clone());
        self.redo.clear();
        self.revision += 1;
        prepared.model.selection = prepared.ids.iter().copied().collect();
        self.model = prepared.model;
        self.refresh_drafts();
        Ok(serde_json::json!({"ids":prepared.ids,"revision":self.revision}))
    }
}

impl Engine {
    pub fn needs_connector_drag(&mut self, name: &str) -> bool {
        self.validator
            .shape(&self.lib, name)
            .is_some_and(|s| s.studs.is_empty())
    }
    /// Full-connector drag preview. Recomputed on commit; previews never mutate the document.
    pub fn full_drag_preview(
        &self,
        file: usize,
        ids: &[Id],
        part: PartSpec,
        near: DVec3,
    ) -> Result<(Cmd, DMat3, DVec3)> {
        if file != self.model.doc.main_index().unwrap_or(0) {
            return Err("Full connector dragging currently requires the main build".into());
        }
        let mut at = part.clone();
        at.pos = near;
        let request = if ids.is_empty() {
            serde_json::json!({"action":"drag","moving":ids,"part":at,"near":near})
        } else {
            serde_json::json!({"action":"drag","moving":ids,"source":{"part":ids[0]},"near":near})
        };
        let result = crate::connectors::request(&self.model, file, request)?;
        let (rotation, translation) = if ids.is_empty() {
            let pose: crate::connectors::Pose =
                serde_json::from_value(result["poses"][0].clone()).map_err(|e| e.to_string())?;
            let r = pose.rot * part.rot.transpose();
            (r, pose.pos - r * part.pos)
        } else {
            (
                serde_json::from_value(result["rotation"].clone()).map_err(|e| e.to_string())?,
                serde_json::from_value(result["translation"].clone()).map_err(|e| e.to_string())?,
            )
        };
        Ok((
            Cmd::Snap {
                ids: ids.to_vec(),
                file,
                part: at,
                near,
            },
            rotation,
            translation,
        ))
    }
    /// LDraw +Y points down. The virtual floor follows the bottom of the build.
    pub fn floor_height(&mut self, file: usize) -> f64 {
        self.model
            .flat(file)
            .iter()
            .filter_map(|p| {
                self.validator
                    .shape(&self.lib, &p.name)
                    .map(|s| physics::Body::new(s, p.rot, p.pos).fhi.y)
            })
            .reduce(f64::max)
            .unwrap_or(0.0)
    }
    /// Match an existing grounded build's lattice; new builds use world-origin studs.
    pub fn ground_phase(&mut self, file: usize) -> DVec3 {
        let floor = self.floor_height(file);
        for p in self.model.flat(file) {
            let Some(shape) = self.validator.shape(&self.lib, &p.name) else {
                continue;
            };
            let body = physics::Body::new(shape, p.rot, p.pos);
            if (body.fhi.y - floor).abs() > 0.6 {
                continue;
            }
            if let Some(anti) = body.anti.iter().find(|a| (a.y - floor).abs() < 0.6) {
                return DVec3::new(anti.x.rem_euclid(20.0), 0.0, anti.z.rem_euclid(20.0));
            }
        }
        DVec3::ZERO
    }
    pub fn snap(
        &mut self,
        file: usize,
        part: PartSpec,
        near: DVec3,
        facing: DVec3,
    ) -> Result<PartSpec> {
        self.place_preview(file, part, near, facing, false, &[])
    }

    /// Non-mutating placement. A ground hit is a ground placement, not a request to
    /// jump to an unrelated connector. Excluded source IDs are the rigid drag group.
    pub fn place_preview(
        &mut self,
        file: usize,
        mut part: PartSpec,
        near: DVec3,
        facing: DVec3,
        ground: bool,
        exclude: &[Id],
    ) -> Result<PartSpec> {
        if !near.is_finite() {
            return Err("invalid placement point".into());
        }
        let lines: BTreeSet<_> = exclude
            .iter()
            .filter_map(|id| self.model.parts.get(id))
            .filter(|p| p.file == file)
            .map(|p| p.line)
            .collect();
        let shape = self
            .validator
            .shape(&self.lib, &part.name)
            .ok_or("missing part geometry")?;
        let flat: Vec<_> = self
            .model
            .flat(file)
            .into_iter()
            .filter(|p| !lines.contains(&p.root_line))
            .collect();
        let bodies: Vec<_> = flat
            .iter()
            .filter_map(|p| {
                self.validator
                    .shape(&self.lib, &p.name)
                    .map(|s| physics::Body::new(s, p.rot, p.pos))
            })
            .collect();
        let base = physics::Body::new(shape.clone(), part.rot, DVec3::ZERO);
        // The virtual plate has stud centres at multiples of 20 LDU. Seat the
        // bottom antistud lattice on it, including odd/even footprint phases.
        let reference = shape
            .anti
            .iter()
            .map(|p| part.rot * *p)
            .max_by(|a, b| a.y.total_cmp(&b.y))
            .or_else(|| shape.studs.first().map(|p| part.rot * *p))
            .unwrap_or(DVec3::ZERO);
        let phase = self.ground_phase(file);
        let ground_pos = DVec3::new(
            ((near.x + reference.x - phase.x) / 20.0).round() * 20.0 + phase.x - reference.x,
            self.floor_height(file) - base.fhi.y,
            ((near.z + reference.z - phase.z) / 20.0).round() * 20.0 + phase.z - reference.z,
        );
        if ground || flat.is_empty() {
            let body = physics::Body::new(shape, part.rot, ground_pos);
            if !bodies.iter().all(|b| physics::fast_legal(&body, b)) {
                return Err("That ground position is occupied".into());
            }
            part.pos = ground_pos;
            return Ok(part);
        }
        let near_part = part.clone();
        self.stud_candidates(file, &near_part, near, facing, exclude, 80.0, 1)?
            .into_iter()
            .next()
            .ok_or_else(|| {
                "no supported collision-free stud snap near the cursor (Alt places freely)".into()
            })
    }

    /// Every distinct collision-free stud/anti-stud seat for `part` (at its given rotation) on
    /// visible pieces whose boxes come within `radius` LDU of `near`, nearest first, at most
    /// `limit`. A zero `facing` accepts connectors facing any way (agents have no camera).
    pub fn stud_candidates(
        &mut self,
        file: usize,
        part: &PartSpec,
        near: DVec3,
        facing: DVec3,
        exclude: &[Id],
        radius: f64,
        limit: usize,
    ) -> Result<Vec<PartSpec>> {
        if !near.is_finite() || !radius.is_finite() {
            return Err("invalid placement point".into());
        }
        let any_facing = facing.length_squared() < 1e-9;
        let lines: BTreeSet<_> = exclude
            .iter()
            .filter_map(|id| self.model.parts.get(id))
            .filter(|p| p.file == file)
            .map(|p| p.line)
            .collect();
        let shape = self
            .validator
            .shape(&self.lib, &part.name)
            .ok_or("missing part geometry")?;
        let flat: Vec<_> = self
            .model
            .flat(file)
            .into_iter()
            .filter(|p| !lines.contains(&p.root_line))
            .collect();
        let bodies: Vec<_> = flat
            .iter()
            .filter_map(|p| {
                self.validator
                    .shape(&self.lib, &p.name)
                    .map(|s| physics::Body::new(s, p.rot, p.pos))
            })
            .collect();
        let mut candidates = Vec::new();
        let hidden: std::collections::HashSet<_> = self
            .model
            .parts
            .values()
            .filter(|p| p.hidden)
            .map(|p| (0, p.file, p.line))
            .collect();
        for p in &flat {
            if p.ancestors
                .iter()
                .chain(std::iter::once(&p.src))
                .any(|s| hidden.contains(s))
            {
                continue;
            }
            let Some(target_shape) = self.validator.shape(&self.lib, &p.name) else {
                continue;
            };
            let target = physics::Body::new(target_shape, p.rot, p.pos);
            if (target.flo - near)
                .max(DVec3::ZERO)
                .max(near - target.fhi)
                .length()
                > radius
            {
                continue;
            }
            for (j, stud) in target.studs.iter().enumerate() {
                if !any_facing && target.sdir[j].dot(facing) < 0.05 {
                    continue;
                }
                if target.sdir[j].dot(part.rot * DVec3::Y) > -0.9 {
                    continue;
                }
                for anti in &shape.anti {
                    let pos = *stud - part.rot * *anti;
                    candidates.push((pos.distance_squared(near), pos));
                }
            }
            for (j, anti) in target.anti.iter().enumerate() {
                if !any_facing && target.adir[j].dot(facing) < 0.05 {
                    continue;
                }
                for (k, stud) in shape.studs.iter().enumerate() {
                    if target.adir[j].dot(part.rot * shape.sdir[k]) < -0.9 {
                        let pos = *anti - part.rot * *stud;
                        candidates.push((pos.distance_squared(near), pos));
                    }
                }
            }
        }
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut tried = BTreeSet::new();
        let mut out = Vec::new();
        // Interactive snapping only needs the nearest hit; enumeration may look further.
        let budget = if limit <= 1 { 256 } else { 4096.max(limit * 16) };
        for (_, pos) in candidates.into_iter() {
            let key = pos.to_array().map(|v| (v * 100.0).round() as i64);
            if !tried.insert(key) {
                continue;
            }
            if tried.len() > budget {
                break;
            }
            let body = physics::Body::new(shape.clone(), part.rot, pos);
            if bodies.iter().all(|b| physics::fast_legal(&body, b)) {
                let mut spec = part.clone();
                spec.pos = pos;
                out.push(spec);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    pub fn move_preview(
        &mut self,
        file: usize,
        ids: Vec<Id>,
        anchor: PartSpec,
        near: DVec3,
        facing: DVec3,
        ground: bool,
        free: bool,
    ) -> Result<Cmd> {
        if ids.is_empty()
            || ids.iter().any(|id| {
                self.model
                    .parts
                    .get(id)
                    .is_none_or(|p| p.file != file || p.locked)
            })
        {
            return Err("select unlocked pieces in this build".into());
        }
        let desired = if free {
            PartSpec {
                pos: near,
                ..anchor.clone()
            }
        } else {
            self.place_preview(file, anchor.clone(), near, facing, ground, &ids)?
        };
        let mut delta = desired.pos - anchor.pos;
        let lines: BTreeSet<_> = ids.iter().map(|id| self.model.parts[id].line).collect();
        if ground && !free {
            let bottom = self
                .model
                .flat(file)
                .iter()
                .filter(|p| lines.contains(&p.root_line))
                .filter_map(|p| {
                    self.validator
                        .shape(&self.lib, &p.name)
                        .map(|s| physics::Body::new(s, p.rot, p.pos).fhi.y)
                })
                .fold(f64::NEG_INFINITY, f64::max);
            delta.y = self.floor_height(file) - bottom;
        }
        if !free {
            let mut moving = Vec::new();
            let mut fixed = Vec::new();
            for p in self.model.flat(file) {
                let shape = self
                    .validator
                    .shape(&self.lib, &p.name)
                    .ok_or("missing part geometry")?;
                if lines.contains(&p.root_line) {
                    moving.push(physics::Body::new(shape, p.rot, p.pos + delta));
                } else {
                    fixed.push(physics::Body::new(shape, p.rot, p.pos));
                }
            }
            if moving
                .iter()
                .any(|a| fixed.iter().any(|b| !physics::fast_legal(a, b)))
            {
                return Err("The dragged selection would collide".into());
            }
        }
        Ok(Cmd::Move { ids, delta, free })
    }
    pub(crate) fn nudge(
        &mut self,
        ids: Vec<Id>,
        direction: DVec3,
        normal: bool,
        alt: Option<DVec3>,
    ) -> Result<serde_json::Value> {
        if ids.is_empty() || !direction.is_finite() || direction.length() < 1e-6 {
            return Err("select parts and a nudge direction".into());
        }
        let file = self.model.parts[&ids[0]].file;
        if ids.iter().any(|id| self.model.parts[id].file != file) {
            return Err("nudge within one model".into());
        }
        let original = self.model.clone();
        let flat = self.model.flat(file);
        let lines: BTreeSet<_> = ids.iter().map(|id| self.model.parts[id].line).collect();
        let report = self.validator.check(&self.lib, &flat);
        let mut normals = Vec::new();
        for (i, p) in flat
            .iter()
            .enumerate()
            .filter(|(_, p)| lines.contains(&p.root_line))
        {
            for &j in &report.rows[i].links {
                if lines.contains(&flat[j].root_line) {
                    continue;
                }
                let Some(a) = self.validator.shape(&self.lib, &p.name) else {
                    continue;
                };
                let q = &flat[j];
                let Some(b) = self.validator.shape(&self.lib, &q.name) else {
                    continue;
                };
                if let Some(r) = physics::relation(
                    &physics::Body::new(a, p.rot, p.pos),
                    &physics::Body::new(b, q.rot, q.pos),
                    false,
                ) {
                    for l in r.links {
                        normals.push(l.dir);
                    }
                }
            }
        }
        let anchored = !normals.is_empty();
        let surface = normals.first().copied().unwrap_or(DVec3::NEG_Y);
        let direction = match alt {
            Some(a) if !normal && a.is_finite() && surface.y.abs() < 0.7 => a,
            _ => direction,
        };
        let mut axis = direction - surface * surface.dot(direction);
        let pitch = if normal {
            axis = surface * direction.dot(DVec3::NEG_Y).signum();
            8.0
        } else {
            20.0
        };
        if axis.length() < 1e-6 {
            return Err("nudge direction is perpendicular to the engaged surface".into());
        }
        axis = axis.normalize();
        for k in 1..=64 {
            self.model = original.clone();
            let delta = axis * pitch * k as f64;
            if let Ok(value) = self.apply(Cmd::Move {
                ids: ids.clone(),
                delta,
                free: false,
            }) {
                let flat = self.model.flat(file);
                let r = self.validator.check(&self.lib, &flat);
                let linked = r.rows.iter().enumerate().any(|(i, row)| {
                    lines.contains(&flat[i].root_line)
                        && row
                            .links
                            .iter()
                            .any(|&j| !lines.contains(&flat[j].root_line))
                });
                if !anchored || linked {
                    return Ok(value);
                }
            }
        }
        self.model = original;
        Err("no legal supported grid position in the next 64 pitches".into())
    }
}
