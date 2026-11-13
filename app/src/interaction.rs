//! Pointer gestures and transient placement. No document edits until a click/drop commits.
use crate::{
    app::Tool,
    view::{LibraryDrag, Loaded},
};
use eframe::egui;
#[cfg(test)]
use engine::Handle;
use engine::{Cmd, PartSpec, Snapshot};
use glam::{DMat3, DVec3};
use ldraw::{FlatPart, Library};
use render::{COLOR_FORMAT, Camera, Gpu, Picker, Renderer, Scene, SceneOptions};

#[derive(Default)]
pub struct Interaction {
    press_hit: Option<usize>,
    moving: Option<MoveDrag>,
    preview: Option<Preview>,
    drag_rotation: DMat3,
    shown_blockers: Vec<u64>,
}
/// UI placement keeps the manipulated pieces selected without adding a second undo step.
pub fn commit(engine: &mut engine::Engine, command: Cmd) -> engine::Result<serde_json::Value> {
    let selection = match &command {
        Cmd::Move { ids, .. } | Cmd::Snap { ids, .. } if !ids.is_empty() => Some(ids.clone()),
        Cmd::Add { .. } | Cmd::Snap { .. } => Some(vec![]),
        _ => None,
    };
    let result = engine.execute(command)?;
    if let Some(mut ids) = selection {
        if ids.is_empty() {
            ids = result
                .get("ids")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_u64())
                .collect();
        }
        if !ids.is_empty() {
            engine.execute(Cmd::Select { ids })?;
        }
    }
    Ok(result)
}
struct MoveDrag {
    ids: Vec<u64>,
    anchor: PartSpec,
    ground_offset: DVec3,
    surface_offset: DVec3,
    /// Height of the grabbed point above the piece's lowest point.
    grab_height: f64,
    revision: u64,
}
struct Preview {
    key: String,
    renderer: Renderer,
    delta: DVec3,
    rotation: DMat3,
    prepared: Option<engine::PreparedPlacement>,
    worker: crate::placement_worker::Worker,
    native_cache: engine::placement::NativePlacementCache,
    parts: usize,
    bounds: (glam::Vec3, glam::Vec3),
    blockers: Vec<u64>,
    query: String,
    error: String,
}
impl Preview {
    fn accept(&mut self, gpu: &Gpu, solution: engine::PlacementSolution) {
        let relative = solution.rotation * self.rotation.transpose();
        self.renderer.transform(
            gpu,
            relative.as_mat3(),
            (solution.translation - relative * self.delta).as_vec3(),
        );
        self.delta = solution.translation;
        self.rotation = solution.rotation;
        self.prepared = Some(solution.prepared);
        self.error.clear();
        self.blockers.clear();
        self.renderer
            .set_part_flags(gpu, (0..self.parts).map(|n| (n as u32, render::FLAG_GHOST)));
    }
    fn poll(&mut self, gpu: &Gpu) -> bool {
        let Some(done) = self.worker.poll() else {
            return false;
        };
        if done.query != self.query {
            return false;
        }
        self.blockers = done.blockers;
        match done.result {
            Ok(solution) => {
                self.accept(gpu, solution);
            }
            Err(e) => {
                if self.prepared.is_some() {
                    return false;
                }
                self.error = e;
                self.prepared = None;
                self.renderer.set_part_flags(
                    gpu,
                    (0..self.parts).map(|n| (n as u32, render::FLAG_DELETE)),
                );
            }
        }
        true
    }
}
pub struct Input<'a> {
    pub gpu: &'a Gpu,
    pub lib: &'a std::sync::Arc<Library>,
    pub snapshot: &'a std::sync::Arc<Snapshot>,
    pub file: usize,
    pub tool: Tool,
    pub place: Option<&'a str>,
    pub place_rot: DMat3,
    pub assembly: &'a [PartSpec],
    pub color: i32,
    pub show_hidden: bool,
}
#[derive(Default)]
pub struct Outcome {
    pub picked: Option<u64>,
    pub command: Option<(Cmd, u64)>,
    pub prepared: Option<engine::PreparedPlacement>,
    pub open_submodel: Option<usize>,
    pub message: Option<String>,
    pub dirty: bool,
}
impl Interaction {
    pub fn is_placing(&self) -> bool {
        self.preview.is_some() || self.moving.is_some()
    }
    pub fn has_preview(&self) -> bool {
        self.preview.is_some()
    }
    pub fn render(
        &self,
        gpu: &Gpu,
        enc: &mut wgpu::CommandEncoder,
        color: &wgpu::TextureView,
        resolve: &wgpu::TextureView,
        depth: &wgpu::TextureView,
        cam: &Camera,
        size: (u32, u32),
    ) {
        if let Some(p) = &self.preview {
            p.renderer
                .encode_overlay(gpu, enc, color, Some(resolve), depth, cam, size);
        }
    }
    pub fn update(
        &mut self,
        input: Input<'_>,
        l: &mut Loaded,
        picker: &mut Option<Picker>,
        rect: egui::Rect,
        resp: &egui::Response,
        ui: &egui::Ui,
    ) -> Outcome {
        let mut out = Outcome::default();
        let pos = ui.input(|i| i.pointer.hover_pos());
        let inside = pos.is_some_and(|p| rect.contains(p)) && ui.rect_contains_pointer(rect);
        let released = ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary));
        let free = ui.input(|i| i.modifiers.alt);
        let library = egui::DragAndDrop::payload::<LibraryDrag>(ui.ctx());
        let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape))
            || self
                .moving
                .as_ref()
                .is_some_and(|d| d.revision != input.snapshot.revision);
        if cancelled {
            self.shown_blockers.clear();
            self.drag_rotation = DMat3::IDENTITY;
            self.moving = None;
            self.press_hit = None;
            self.preview = None;
            egui::DragAndDrop::clear_payload(ui.ctx());
            restore_flags(l, &input, &[]);
            out.dirty = true;
            return out;
        }
        if let Some(p) = self.preview.as_mut() {
            out.dirty |= p.poll(input.gpu);
        }
        if inside && ui.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
            self.press_hit = pick(l, picker, &input, ui, rect, pos.unwrap());
        }
        if resp.drag_started_by(egui::PointerButton::Primary)
            && library.is_none()
            && !matches!(
                input.tool,
                Tool::Hinge | Tool::Connect | Tool::Lasso | Tool::Hide
            )
        {
            if let Some(n) = self.press_hit {
                if let Some(id) = l
                    .ids
                    .get(n)
                    .copied()
                    .flatten()
                    .filter(|id| input.snapshot.model.parts.contains_key(id))
                {
                    let ids = if input.snapshot.model.selection.contains(&id) {
                        input.snapshot.model.selection.iter().copied().collect()
                    } else {
                        vec![id]
                    };
                    if ids.iter().any(|id| input.snapshot.model.parts[id].locked) {
                        out.message = Some("Unlock the piece before dragging it".into());
                    } else {
                        let flat = &l.flat[n];
                        let origin = ui
                            .input(|i| i.pointer.press_origin())
                            .unwrap_or(pos.unwrap_or(rect.center()));
                        let ray = screen_ray(&l.cam, rect, origin);
                        // The point grabbed on the piece itself: it stays under the
                        // cursor (in plan) wherever the piece is dragged.
                        let grab = mesh_hit(input.lib, flat, ray)
                            .or_else(|| floor_hit(ray, l.floor_y as f64))
                            .unwrap_or(flat.pos);
                        let ground_offset = grab - flat.pos;
                        let surface_offset = ground_offset;
                        let bottom = lib_bottom(input.lib, flat).unwrap_or(grab.y);
                        let grab_height = (bottom - grab.y).max(0.0);
                        self.moving = Some(MoveDrag {
                            ids,
                            anchor: spec(flat),
                            ground_offset,
                            surface_offset,
                            grab_height,
                            revision: input.snapshot.revision,
                        });
                    }
                }
            }
        }
        if library.is_none()
            && self.moving.is_none()
            && self.press_hit.is_none()
            && (!matches!(input.tool, Tool::Hinge | Tool::Lasso)
                || (input.tool == Tool::Hinge && ui.input(|i| i.modifiers.alt)))
            && resp.dragged_by(egui::PointerButton::Primary)
        {
            let delta = resp.drag_delta();
            l.cam.yaw -= delta.x * 0.008;
            l.cam.pitch = (l.cam.pitch + delta.y * 0.008).clamp(-1.5706, 1.5706);
            out.dirty = true;
        }
        let hidden = self
            .moving
            .as_ref()
            .map(|d| d.ids.as_slice())
            .unwrap_or(&[]);
        if self.moving.is_some() {
            restore_flags(l, &input, hidden);
        }
        let new_part = library
            .as_ref()
            .map(|p| (p.name.clone(), p.color, DMat3::IDENTITY))
            .or_else(|| {
                (input.tool == Tool::Place)
                    .then(|| {
                        input
                            .place
                            .map(|p| (p.to_string(), input.color, input.place_rot))
                    })
                    .flatten()
            });
        if inside && (self.moving.is_some() || new_part.is_some()) {
            let pos = pos.unwrap();
            let ray = screen_ray(&l.cam, rect, pos);
            let hit = pick(l, picker, &input, ui, rect, pos)
                .and_then(|n| l.flat.get(n))
                .and_then(|p| mesh_hit(input.lib, p, ray));
            let ground = hit.is_none();
            let plane = l.floor_y as f64 - self.moving.as_ref().map_or(0.0, |d| d.grab_height);
            if let Some(mut near) = hit.or_else(|| floor_hit(ray, plane)) {
                let facing = (l.cam.eye() - l.cam.target).as_dvec3().normalize();
                let (key, base, geometry) = if let Some(d) = &self.moving {
                    let offset = if ground {
                        d.ground_offset
                    } else {
                        d.surface_offset
                    };
                    near.x -= offset.x;
                    near.z -= offset.z;
                    let geometry = l
                        .flat
                        .iter()
                        .zip(&l.ids)
                        .filter(|(_, id)| id.is_some_and(|id| d.ids.contains(&id)))
                        .map(|(p, _)| p.clone())
                        .collect();
                    (
                        format!("move:{:?}:{}", d.ids, d.revision),
                        d.anchor.clone(),
                        geometry,
                    )
                } else {
                    let (name, color, rot) = new_part.as_ref().unwrap();
                    if library.is_none() && !input.assembly.is_empty() {
                        let base = input.assembly[0].clone();
                        (
                            format!(
                                "assembly:{}",
                                serde_json::to_string(input.assembly).unwrap()
                            ),
                            base,
                            input.assembly.iter().map(flat_spec).collect(),
                        )
                    } else {
                        let base = PartSpec {
                            name: name.clone(),
                            color: *color,
                            pos: DVec3::ZERO,
                            rot: *rot,
                            id: None,
                        };
                        (
                            format!("new:{name}:{color}:{rot:?}"),
                            base.clone(),
                            vec![flat_spec(&base)],
                        )
                    }
                };
                if self.preview.as_ref().is_none_or(|p| p.key != key) {
                    self.drag_rotation = DMat3::IDENTITY;
                    let scene = Scene::build(
                        input.lib,
                        &geometry,
                        &SceneOptions {
                            studs: render::StudMode::Instanced,
                            ..Default::default()
                        },
                    );
                    self.preview = Some(Preview {
                        key,
                        renderer: Renderer::new(
                            input.gpu,
                            COLOR_FORMAT,
                            l.renderer.samples,
                            &scene,
                        ),
                        delta: DVec3::ZERO,
                        rotation: DMat3::IDENTITY,
                        prepared: None,
                        worker: crate::placement_worker::Worker::new(ui.ctx().clone()),
                        native_cache: Default::default(),
                        parts: geometry.len(),
                        bounds: scene.bounds,
                        blockers: vec![],
                        query: String::new(),
                        error: String::new(),
                    });
                }
                let p = self.preview.as_mut().unwrap();
                if !ui.ctx().egui_wants_keyboard_input() {
                    for key in [
                        egui::Key::ArrowLeft,
                        egui::Key::ArrowRight,
                        egui::Key::ArrowUp,
                        egui::Key::ArrowDown,
                    ] {
                        if ui.input(|i| i.key_pressed(key)) {
                            let forward = (l.cam.target - l.cam.eye()).normalize();
                            let right = forward.cross(glam::Vec3::NEG_Y).normalize();
                            let horizontal = if right.x.abs() >= right.z.abs() {
                                DVec3::X * right.x.signum() as f64
                            } else {
                                DVec3::Z * right.z.signum() as f64
                            };
                            let axis =
                                if matches!(key, egui::Key::ArrowLeft | egui::Key::ArrowRight) {
                                    DVec3::NEG_Y
                                } else {
                                    horizontal
                                };
                            let angle = if matches!(key, egui::Key::ArrowLeft | egui::Key::ArrowUp)
                            {
                                -std::f64::consts::FRAC_PI_2
                            } else {
                                std::f64::consts::FRAC_PI_2
                            };
                            self.drag_rotation =
                                DMat3::from_axis_angle(axis, angle) * self.drag_rotation;
                        }
                    }
                }
                let query = format!(
                    "{:?}:{ground}:{free}:{}:{:?}",
                    near.to_array().map(|v| (v * 10.0).round() as i64),
                    input.snapshot.revision,
                    self.drag_rotation
                );
                if p.query != query {
                    let job = crate::placement_worker::Job {
                        query: query.clone(),
                        snapshot: input.snapshot.clone(),
                        lib: input.lib.clone(),
                        file: input.file,
                        ids: self
                            .moving
                            .as_ref()
                            .map(|d| d.ids.clone())
                            .unwrap_or_default(),
                        part: base.clone(),
                        assembly: if library.is_none() && self.moving.is_none() {
                            input.assembly.to_vec()
                        } else {
                            vec![]
                        },
                        near,
                        facing,
                        ground,
                        free,
                        rotation: self.drag_rotation,
                    };
                    let immediate = if !job.assembly.is_empty() {
                        input.snapshot.prepare_assembly(
                            input.lib.clone(),
                            Some(&mut p.native_cache),
                            input.file,
                            job.assembly.clone(),
                            near,
                            facing,
                            ground,
                            free,
                            self.drag_rotation,
                        )
                    } else {
                        input.snapshot.prepare_native_placement(
                            input.lib.clone(),
                            &mut p.native_cache,
                            input.file,
                            job.ids.clone(),
                            base.clone(),
                            near,
                            facing,
                            ground,
                            free,
                            self.drag_rotation,
                        )
                    };
                    let rotation = self.drag_rotation;
                    let mut delta = near - rotation * base.pos;
                    if ground {
                        let mut bottom = f64::NEG_INFINITY;
                        for x in [p.bounds.0.x, p.bounds.1.x] {
                            for y in [p.bounds.0.y, p.bounds.1.y] {
                                for z in [p.bounds.0.z, p.bounds.1.z] {
                                    bottom = bottom.max(
                                        (rotation * DVec3::new(x as f64, y as f64, z as f64)).y,
                                    );
                                }
                            }
                        }
                        delta.y = l.floor_y as f64 - bottom;
                    }
                    let relative = rotation * p.rotation.transpose();
                    p.renderer.transform(
                        input.gpu,
                        relative.as_mat3(),
                        (delta - relative * p.delta).as_vec3(),
                    );
                    p.delta = delta;
                    p.rotation = rotation;
                    p.error = "Solving placement…".into();
                    let flags = render::FLAG_DELETE;
                    p.renderer
                        .set_part_flags(input.gpu, (0..geometry.len()).map(|n| (n as u32, flags)));
                    p.prepared = None;
                    p.blockers.clear();
                    p.query = query;
                    match immediate {
                        Ok(solution) => {
                            p.accept(input.gpu, solution);
                            // A hole can be visually empty: keep the immediate
                            // ground pose usable while nearby pin/axle mating
                            // refines it. Never replace a legal pose by failure.
                            if !free
                                && input
                                    .lib
                                    .mesh(&base.name, ldraw::Lod::Low)
                                    .is_ok_and(|m| m.studs.is_empty())
                            {
                                p.worker.submit(job);
                            }
                        }
                        Err(_) => p.worker.submit(job),
                    }
                    out.dirty = true;
                }
                let commit = if library.is_some() || self.moving.is_some() {
                    released
                } else {
                    resp.clicked()
                };
                if commit {
                    if let Some(prepared) = p.prepared.take() {
                        out.prepared = Some(prepared);
                    } else if p.error == "Solving placement…" {
                        out.message = Some("No confirmed snap yet—nothing placed. Wait for a blue preview or use Connect.".into());
                        p.query.clear();
                        p.error = "No confirmed snap—nothing placed".into();
                    } else {
                        out.message = Some(p.error.clone());
                    }
                }
                ui.ctx().set_cursor_icon(if p.prepared.is_some() {
                    egui::CursorIcon::Grabbing
                } else {
                    egui::CursorIcon::NotAllowed
                });
            } else if self.preview.take().is_some() {
                out.dirty = true;
            }
        } else if self.preview.take().is_some() {
            out.dirty = true;
        }
        if resp.clicked()
            && !matches!(input.tool, Tool::Place | Tool::Lasso)
            && library.is_none()
            && self.moving.is_none()
        {
            let hit = pos.and_then(|pos| pick(l, picker, &input, ui, rect, pos));
            let id = hit
                .and_then(|n| l.ids.get(n).copied().flatten())
                .filter(|id| input.snapshot.model.parts.contains_key(id));
            if let Some(id) = id {
                if matches!(input.tool, Tool::Hinge | Tool::Connect) {
                    out.picked = Some(id);
                    return out;
                }
                let part = &input.snapshot.model.parts[&id];
                if resp.double_clicked() {
                    out.open_submodel = input.snapshot.model.doc.file_index(&part.name);
                }
                let command = match input.tool {
                    Tool::Hide => Cmd::Hide {
                        ids: vec![id],
                        value: !part.hidden,
                    },
                    Tool::Delete => Cmd::Remove { ids: vec![id] },
                    Tool::Paint => Cmd::Paint {
                        ids: vec![id],
                        color: input.color,
                    },
                    _ => {
                        let mut ids = if ui.input(|i| i.modifiers.shift || i.modifiers.command) {
                            input.snapshot.model.selection.clone()
                        } else {
                            Default::default()
                        };
                        if !ids.insert(id) {
                            ids.remove(&id);
                        }
                        Cmd::Select {
                            ids: ids.into_iter().collect(),
                        }
                    }
                };
                out.command = Some((command, input.snapshot.revision));
            } else if input.tool == Tool::Select {
                out.command = Some((Cmd::Select { ids: vec![] }, input.snapshot.revision));
            }
        }
        if released {
            if self.moving.take().is_some() {
                restore_flags(l, &input, &[]);
                out.dirty = true;
            }
            if library.is_some() {
                egui::DragAndDrop::clear_payload(ui.ctx());
            }
            self.press_hit = None;
            if (out.command.is_some() || out.prepared.is_some()) && self.preview.take().is_some() {
                out.dirty = true;
            }
        }
        let blockers = self
            .preview
            .as_ref()
            .map(|p| p.blockers.clone())
            .unwrap_or_default();
        if blockers != self.shown_blockers || (self.moving.is_some() && !blockers.is_empty()) {
            restore_flags(
                l,
                &input,
                self.moving
                    .as_ref()
                    .map(|d| d.ids.as_slice())
                    .unwrap_or(&[]),
            );
            l.renderer.set_part_flags(
                input.gpu,
                l.ids
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| id.is_some_and(|id| blockers.contains(&id)))
                    .map(|(n, _)| (n as u32, render::FLAG_DELETE)),
            );
            self.shown_blockers = blockers;
            out.dirty = true;
        }
        out
    }
}

pub(crate) fn hidden_flags(show: bool, pick: bool) -> u32 {
    if show {
        render::FLAG_HIDDEN_GHOST | if pick { render::FLAG_PICK_HIDDEN } else { 0 }
    } else {
        render::FLAG_HIDDEN
    }
}
fn restore_flags(l: &mut Loaded, input: &Input<'_>, hidden: &[u64]) {
    let index: std::collections::HashMap<_, _> = input
        .snapshot
        .model
        .parts
        .values()
        .map(|p| ((p.file, p.line), p))
        .collect();
    l.renderer.set_part_flags(
        input.gpu,
        l.flat.iter().enumerate().map(|(n, flat)| {
            let mut flags = l.base_flags[n];
            if l.ids[n].is_some_and(|id| hidden.contains(&id)) {
                flags |= render::FLAG_HIDDEN;
            }
            for source in flat.ancestors.iter().chain(std::iter::once(&flat.src)) {
                if source.0 == 0 {
                    if let Some(p) = index.get(&(source.1, source.2)) {
                        if p.hidden {
                            flags |= hidden_flags(input.show_hidden, input.tool == Tool::Hide);
                        }
                        if input.snapshot.model.selection.contains(&p.id) {
                            flags |= render::FLAG_SELECTED;
                        }
                    }
                }
            }
            (n as u32, flags)
        }),
    );
}
fn pick(
    l: &Loaded,
    picker: &mut Option<Picker>,
    input: &Input<'_>,
    ui: &egui::Ui,
    rect: egui::Rect,
    pos: egui::Pos2,
) -> Option<usize> {
    let ppp = ui.ctx().pixels_per_point();
    let size = (
        (rect.width() * ppp).round().max(1.0) as u32,
        (rect.height() * ppp).round().max(1.0) as u32,
    );
    l.renderer
        .pick(
            input.gpu,
            picker.get_or_insert_with(|| Picker::new(input.gpu, size)),
            &l.cam,
            size,
            (
                ((pos.x - rect.left()) * ppp) as u32,
                ((pos.y - rect.top()) * ppp) as u32,
            ),
        )
        .map(|n| n as usize)
}
pub fn screen_ray(cam: &Camera, rect: egui::Rect, pos: egui::Pos2) -> (DVec3, DVec3) {
    let inverse = cam.view_proj(rect.aspect_ratio()).inverse();
    let x = (pos.x - rect.left()) / rect.width() * 2.0 - 1.0;
    let y = 1.0 - (pos.y - rect.top()) / rect.height() * 2.0;
    let a = inverse.project_point3(glam::vec3(x, y, 0.0)).as_dvec3();
    let b = inverse.project_point3(glam::vec3(x, y, 1.0)).as_dvec3();
    (a, (b - a).normalize())
}
#[cfg(test)]
fn ground_hit((origin, direction): (DVec3, DVec3)) -> Option<DVec3> {
    floor_hit((origin, direction), 0.0)
}
fn floor_hit((origin, direction): (DVec3, DVec3), floor: f64) -> Option<DVec3> {
    if direction.y.abs() < 1e-8 {
        return None;
    }
    let t = (floor - origin.y) / direction.y;
    (t >= 0.0).then_some(origin + direction * t)
}
fn lib_bottom(lib: &Library, flat: &FlatPart) -> Option<f64> {
    let mesh = lib.mesh(&flat.name, ldraw::Lod::Low).ok()?;
    mesh.tris
        .iter()
        .flatten()
        .map(|v| (flat.rot * v.as_dvec3() + flat.pos).y)
        .max_by(f64::total_cmp)
}
fn mesh_hit(lib: &Library, flat: &FlatPart, (origin, direction): (DVec3, DVec3)) -> Option<DVec3> {
    let mesh = lib.mesh(&flat.name, ldraw::Lod::Low).ok()?;
    let inverse = flat.rot.inverse();
    let local_origin = inverse * (origin - flat.pos);
    let local_direction = inverse * direction;
    let t = mesh
        .tris
        .iter()
        .filter_map(|tri| {
            let [a, b, c] = tri.map(|v| v.as_dvec3());
            let ab = b - a;
            let ac = c - a;
            let h = local_direction.cross(ac);
            let det = ab.dot(h);
            if det.abs() < 1e-9 {
                return None;
            }
            let s = local_origin - a;
            let u = s.dot(h) / det;
            let q = s.cross(ab);
            let v = local_direction.dot(q) / det;
            let t = ac.dot(q) / det;
            (u >= -1e-6 && v >= -1e-6 && u + v <= 1.0 + 1e-6 && t >= 0.0).then_some(t)
        })
        .min_by(f64::total_cmp)?;
    Some(origin + direction * t)
}
fn spec(p: &FlatPart) -> PartSpec {
    PartSpec {
        name: p.name.clone(),
        color: p.color.0,
        pos: p.pos,
        rot: p.rot,
        id: None,
    }
}
fn flat_spec(p: &PartSpec) -> FlatPart {
    FlatPart {
        name: p.name.clone(),
        color: ldraw::Color(p.color),
        pos: p.pos,
        rot: p.rot,
        step: 1,
        path: String::new(),
        src: (0, 0, 0),
        root_line: 0,
        ancestors: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    struct Harness {
        context: egui::Context,
        gpu: Gpu,
        lib: Arc<Library>,
        handle: Handle,
        interaction: Interaction,
        loaded: Loaded,
        picker: Option<Picker>,
        tool: Tool,
        place: Option<String>,
        place_rot: DMat3,
        assembly: Vec<PartSpec>,
        color: i32,
        show_hidden: bool,
        wait_for_solver: bool,
        time: f64,
    }
    fn rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(220., 20.), egui::vec2(800., 600.))
    }
    impl Harness {
        fn new() -> Self {
            let gpu = Gpu::headless().unwrap();
            let lib = Arc::new(Library::open_default().unwrap());
            let handle = Handle::spawn(engine::Engine::new(
                lib.clone(),
                ldraw::Document::new("main.ldr", "test"),
                None,
            ));
            let flat = vec![];
            let scene = Scene::build(&lib, &flat, &SceneOptions::default());
            let loaded = Loaded {
                floor_y: 0.0,
                renderer: Renderer::new(&gpu, COLOR_FORMAT, 4, &scene),
                cam: Camera::framing(glam::Vec3::splat(-160.), glam::Vec3::splat(160.), 4. / 3.),
                ids: vec![],
                flat,
                base_flags: vec![],
                base_count: 0,
                lo: scene.bounds.0,
                hi: scene.bounds.1,
            };
            Self {
                context: egui::Context::default(),
                gpu,
                lib,
                handle,
                interaction: Default::default(),
                loaded,
                picker: None,
                tool: Tool::Select,
                place: None,
                place_rot: DMat3::IDENTITY,
                assembly: vec![],
                color: 4,
                show_hidden: false,
                wait_for_solver: true,
                time: 0.,
            }
        }
        fn screen(&self, p: DVec3) -> egui::Pos2 {
            let p = self
                .loaded
                .cam
                .view_proj(rect().aspect_ratio())
                .project_point3(p.as_vec3());
            egui::pos2(
                rect().center().x + p.x * rect().width() * 0.5,
                rect().center().y - p.y * rect().height() * 0.5,
            )
        }
        fn frame(&mut self, events: Vec<egui::Event>) -> bool {
            self.time += 0.05;
            let snapshot = self.handle.snapshot();
            let mut outcome = Outcome::default();
            let mut output = self.context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1040., 660.),
                    )),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = ui.put(
                            egui::Rect::from_min_size(egui::pos2(20., 20.), egui::vec2(160., 60.)),
                            egui::Button::new("Brick 1 x 2").sense(egui::Sense::click_and_drag()),
                        );
                        response.dnd_set_drag_payload(LibraryDrag {
                            name: "3004.dat".into(),
                            color: 4,
                        });
                        if response.clicked() {
                            self.tool = Tool::Place;
                            self.place = Some("3004.dat".into());
                            self.place_rot = DMat3::IDENTITY;
                        }
                        let response = ui.allocate_rect(rect(), egui::Sense::click_and_drag());
                        let input = Input {
                            gpu: &self.gpu,
                            lib: &self.lib,
                            snapshot: &snapshot,
                            file: 0,
                            tool: self.tool,
                            place: self.place.as_deref(),
                            place_rot: self.place_rot,
                            assembly: &self.assembly,
                            color: self.color,
                            show_hidden: self.show_hidden,
                        };
                        if self.interaction.preview.is_none() && self.interaction.moving.is_none() {
                            restore_flags(&mut self.loaded, &input, &[]);
                        }
                        outcome = self.interaction.update(
                            input,
                            &mut self.loaded,
                            &mut self.picker,
                            rect(),
                            &response,
                            ui,
                        );
                    });
                },
            );
            output.textures_delta.clear();
            if self.interaction.has_preview() {
                let texture = |format, samples| {
                    self.gpu
                        .device
                        .create_texture(&wgpu::TextureDescriptor {
                            label: None,
                            size: wgpu::Extent3d {
                                width: 800,
                                height: 600,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: samples,
                            dimension: wgpu::TextureDimension::D2,
                            format,
                            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                            view_formats: &[],
                        })
                        .create_view(&Default::default())
                };
                let color = texture(COLOR_FORMAT, 4);
                let resolve = texture(COLOR_FORMAT, 1);
                let depth = texture(render::DEPTH_FORMAT, 4);
                let mut encoder = self.gpu.device.create_command_encoder(&Default::default());
                // Exercise the real retained-depth/MSAA preview passes too.
                self.loaded.renderer.encode(
                    &self.gpu,
                    &mut encoder,
                    &color,
                    None,
                    &depth,
                    &self.loaded.cam,
                    (800, 600),
                );
                self.interaction.render(
                    &self.gpu,
                    &mut encoder,
                    &color,
                    &resolve,
                    &depth,
                    &self.loaded.cam,
                    (800, 600),
                );
                self.gpu.queue.submit([encoder.finish()]);
                self.gpu.wait();
            }
            let committed = if let Some(prepared) = outcome.prepared {
                self.handle
                    .call(move |e| e.commit_placement(prepared))
                    .unwrap();
                true
            } else if let Some((command, revision)) = outcome.command {
                self.handle
                    .call(move |e| {
                        assert_eq!(e.revision, revision);
                        commit(e, command)
                    })
                    .unwrap();
                true
            } else {
                false
            };
            if committed {
                let snapshot = self.handle.snapshot();
                let flat = snapshot.model.flat(0);
                let ids = flat
                    .iter()
                    .map(|p| snapshot.model.id_at(0, p.root_line))
                    .collect();
                let scene = Scene::build(&self.lib, &flat, &SceneOptions::default());
                self.loaded.renderer = Renderer::new(&self.gpu, COLOR_FORMAT, 4, &scene);
                self.loaded.base_flags = vec![0; flat.len()];
                self.loaded.ids = ids;
                self.loaded.flat = flat;
                return true;
            }
            if let Some(p) = self.interaction.preview.as_mut() {
                if self.wait_for_solver && p.error == "Solving placement…" {
                    p.worker.wait();
                    return self.frame(vec![]);
                }
            }
            false
        }
        fn pointer(&mut self, pos: egui::Pos2, pressed: Option<bool>) -> bool {
            let mut events = vec![egui::Event::PointerMoved(pos)];
            if let Some(pressed) = pressed {
                events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            self.frame(events)
        }
        fn click(&mut self, pos: egui::Pos2) {
            self.pointer(pos, Some(true));
            self.pointer(pos, Some(false));
        }
    }
    #[test]
    fn native_preview_is_ready_in_the_pointer_frame_and_late_results_never_drop() {
        let mut h = Harness::new();
        h.wait_for_solver = false;
        h.frame(vec![]);
        h.tool = Tool::Place;
        h.place = Some("3004.dat".into());
        let cursor = h.screen(DVec3::new(80., 0., 0.));
        h.pointer(cursor, None);
        assert!(
            h.interaction.preview.as_ref().unwrap().prepared.is_some(),
            "native snap must be confirmed in this frame, not after a worker result"
        );
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let mut worker =
            crate::placement_worker::Worker::with_solver(h.context.clone(), move |job| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                job.snapshot.prepare_native_placement(
                    job.lib.clone(),
                    &mut Default::default(),
                    job.file,
                    job.ids.clone(),
                    job.part.clone(),
                    job.near,
                    job.facing,
                    job.ground,
                    job.free,
                    job.rotation,
                )
            });
        let query = h.interaction.preview.as_ref().unwrap().query.clone();
        worker.submit(crate::placement_worker::Job {
            assembly: vec![],
            query,
            snapshot: h.handle.snapshot(),
            lib: h.lib.clone(),
            file: 0,
            ids: vec![],
            part: PartSpec {
                name: "3004.dat".into(),
                color: 4,
                pos: DVec3::ZERO,
                rot: DMat3::IDENTITY,
                id: None,
            },
            near: DVec3::new(80., 0., 0.),
            facing: DVec3::NEG_Y,
            ground: true,
            free: false,
            rotation: DMat3::IDENTITY,
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let p = h.interaction.preview.as_mut().unwrap();
        p.worker = worker;
        p.prepared = None;
        p.error = "Solving placement…".into();
        h.click(cursor);
        assert!(h.handle.snapshot().model.parts.is_empty());
        release_tx.send(()).unwrap();
        h.interaction.preview.as_mut().unwrap().worker.wait();
        h.frame(vec![]);
        assert!(
            h.handle.snapshot().model.parts.is_empty(),
            "finishing a solve after release must never place anything"
        );
    }

    #[test]
    fn hide_click_toggle_and_blocked_hover_reveal_do_not_remove_collision_geometry() {
        let mut h = Harness::new();
        h.frame(vec![]);
        h.tool = Tool::Place;
        h.place = Some("3003.dat".into());
        let ground = h.screen(DVec3::ZERO);
        h.click(ground);
        assert_eq!(h.handle.snapshot().model.parts.len(), 1);
        h.tool = Tool::Hide;
        h.place = None;
        let center = h.screen(DVec3::new(0., -12., 0.));
        h.click(center);
        assert!(h.handle.snapshot().model.parts[&1].hidden);
        h.show_hidden = true;
        h.frame(vec![]);
        h.click(center);
        assert!(!h.handle.snapshot().model.parts[&1].hidden);
        h.click(center);
        assert!(h.handle.snapshot().model.parts[&1].hidden);
        h.show_hidden = false;
        h.tool = Tool::Place;
        h.place = Some("3005.dat".into());
        h.pointer(ground, None);
        assert_eq!(h.interaction.shown_blockers, vec![1]);
        h.click(ground);
        assert_eq!(h.handle.snapshot().model.parts.len(), 1);
        assert!(h.handle.snapshot().model.parts[&1].hidden);
        let outside = h.screen(DVec3::new(120., 0., 0.));
        h.pointer(outside, None);
        assert!(h.interaction.shown_blockers.is_empty());
        assert!(h.handle.snapshot().model.parts[&1].hidden);
    }

    #[test]
    fn connect_mode_keeps_empty_space_camera_orbit() {
        let mut h = Harness::new();
        h.frame(vec![]);
        h.tool = Tool::Connect;
        let pos = rect().center();
        let before = h.loaded.cam.yaw;
        h.pointer(pos, Some(true));
        h.pointer(pos + egui::vec2(80., 10.), None);
        h.pointer(pos + egui::vec2(80., 10.), Some(false));
        assert!((h.loaded.cam.yaw - before).abs() > 0.1);
        assert!(h.handle.snapshot().model.parts.is_empty());
    }
    #[test]
    fn arrows_rotate_transient_placement_without_editing_the_original() {
        let mut h = Harness::new();
        h.tool = Tool::Place;
        h.place = Some("3004.dat".into());
        let pos = egui::pos2(600., 300.);
        h.pointer(pos, None);
        let original = h.handle.snapshot().model.doc.to_bytes();
        h.frame(vec![egui::Event::Key {
            key: egui::Key::ArrowRight,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }]);
        assert_eq!(h.handle.snapshot().model.doc.to_bytes(), original);
        assert_ne!(h.interaction.drag_rotation, DMat3::IDENTITY);
        h.click(pos);
        let snapshot = h.handle.snapshot();
        assert_eq!(snapshot.model.parts.len(), 1);
        assert_ne!(snapshot.model.parts[&1].rot, DMat3::IDENTITY);
        h.handle.call(|e| e.undo(false)).unwrap();
        assert_eq!(h.handle.snapshot().model.doc.to_bytes(), original);
    }
    #[test]
    fn copied_piece_places_repeatedly_with_exact_colour_and_rotation() {
        let mut h = Harness::new();
        let rot = DMat3::from_rotation_z(std::f64::consts::FRAC_PI_2);
        h.handle
            .command(Cmd::Add {
                file: 0,
                parts: vec![PartSpec {
                    name: "3004.dat".into(),
                    color: 1,
                    rot,
                    pos: DVec3::new(-200., -40., 0.),
                    id: None,
                }],
                free: true,
            })
            .unwrap();
        h.handle.command(Cmd::Select { ids: vec![1] }).unwrap();
        let before = h.handle.snapshot().model.doc.to_bytes();
        let piece = crate::app::copied_piece(&h.handle.snapshot()).unwrap();
        h.place = Some(piece.name);
        h.place_rot = piece.rot;
        h.color = piece.color;
        h.tool = Tool::Place;
        h.frame(vec![]);
        let target = h.screen(DVec3::new(80., 0., 0.));
        h.pointer(target, None);
        assert!(h.interaction.has_preview());
        assert_eq!(h.handle.snapshot().model.doc.to_bytes(), before);
        h.click(target);
        h.click(h.screen(DVec3::new(240., 0., 0.)));
        let snapshot = h.handle.snapshot();
        assert_eq!(snapshot.model.parts.len(), 3);
        assert_eq!(
            snapshot.model.selection,
            [3].into_iter().collect(),
            "latest placed copy stays selected"
        );
        assert_eq!(snapshot.model.parts[&1].pos, DVec3::new(-200., -40., 0.));
        for p in snapshot.model.parts.values() {
            assert_eq!(p.name, "3004.dat");
            assert_eq!(p.color, 1);
            assert_eq!(p.rot, rot);
        }
        h.handle.call(|e| e.undo(false)).unwrap();
        h.handle.call(|e| e.undo(false)).unwrap();
        assert_eq!(h.handle.snapshot().model.doc.to_bytes(), before);
    }
    #[test]
    fn copied_assembly_has_a_whole_preview_and_atomic_repeat_placement() {
        let mut h = Harness::new();
        h.handle
            .command(Cmd::Add {
                file: 0,
                parts: (0..3)
                    .map(|i| PartSpec {
                        name: "3005.dat".into(),
                        color: i + 1,
                        pos: DVec3::new(-200., -24. * (i + 1) as f64, 0.),
                        rot: DMat3::IDENTITY,
                        id: None,
                    })
                    .collect(),
                free: true,
            })
            .unwrap();
        h.handle
            .command(Cmd::Select { ids: vec![1, 2, 3] })
            .unwrap();
        let before = h.handle.snapshot().model.doc.to_bytes();
        h.assembly = h.handle.snapshot().copied_assembly().unwrap();
        h.place = Some(h.assembly[0].name.clone());
        h.tool = Tool::Place;
        h.frame(vec![]);
        let target = h.screen(DVec3::new(80., 0., 0.));
        h.pointer(target, None);
        assert_eq!(h.interaction.preview.as_ref().unwrap().parts, 3);
        assert_eq!(h.handle.snapshot().model.doc.to_bytes(), before);
        h.click(target);
        let snap = h.handle.snapshot();
        assert_eq!(snap.model.parts.len(), 6);
        assert_eq!(snap.model.selection.len(), 3);
        assert_eq!(
            snap.model.parts[&6].pos - snap.model.parts[&4].pos,
            DVec3::new(0., -48., 0.)
        );
        h.handle.call(|e| e.undo(false)).unwrap();
        assert_eq!(h.handle.snapshot().model.doc.to_bytes(), before);
    }
    #[test]
    fn empty_space_orbit_can_go_under_floor_and_look_up() {
        let mut h = Harness::new();
        h.frame(vec![]);
        let start = rect().center();
        let end = egui::pos2(start.x, rect().top() + 2.0);
        h.pointer(start, Some(true));
        h.pointer(end, None);
        h.pointer(end, Some(false));
        assert!(
            h.loaded.cam.pitch < -1.4,
            "orbit should allow looking almost straight up"
        );
        assert!(
            h.loaded.cam.eye().y > h.loaded.floor_y,
            "camera should be allowed beneath the floor (+Y down)"
        );
        assert!(h.handle.snapshot().model.parts.is_empty());
    }

    #[test]
    fn library_drop_move_and_repeat_placement_are_distinct_gestures() {
        let mut h = Harness::new();
        h.frame(vec![]);
        let library = egui::pos2(80., 50.);
        let ground = h.screen(DVec3::ZERO);
        h.pointer(library, Some(true));
        h.pointer(ground, None);
        assert!(matches!(h.tool, Tool::Select));
        assert!(h.interaction.has_preview());
        assert!(
            h.handle.snapshot().model.parts.is_empty(),
            "drag preview must not mutate the document"
        );
        assert!(h.pointer(ground, Some(false)));
        assert!(
            matches!(h.tool, Tool::Select),
            "a library drop must not enable repeat placement"
        );
        assert_eq!(h.handle.snapshot().model.parts.len(), 1);
        let original = h.handle.snapshot().model.doc.to_bytes();
        let part = h.loaded.flat[0].clone();
        let piece = h.screen(part.pos + DVec3::new(0., 12., 0.));
        let camera = (h.loaded.cam.yaw, h.loaded.cam.pitch);
        h.pointer(piece, Some(true));
        let grab = mesh_hit(&h.lib, &part, screen_ray(&h.loaded.cam, rect(), piece)).unwrap();
        let destination = h.screen(grab + DVec3::new(120., 0., 80.));
        h.pointer(destination, None);
        assert!(
            h.interaction.moving.is_some(),
            "dragging a piece should move it, not orbit"
        );
        assert_eq!(camera, (h.loaded.cam.yaw, h.loaded.cam.pitch));
        assert_eq!(h.handle.snapshot().model.doc.to_bytes(), original);
        assert!(h.pointer(destination, Some(false)));
        assert!(
            (h.handle.snapshot().model.parts[&1].pos - part.pos - DVec3::new(120., 0., 80.))
                .length()
                < 0.01
        );
        h.handle.call(|e| e.undo(false)).unwrap();
        assert_eq!(
            h.handle.snapshot().model.doc.to_bytes(),
            original,
            "one undo reverses the whole drag"
        );
        h.click(library);
        assert!(matches!(h.tool, Tool::Place));
        let count = h.handle.snapshot().model.parts.len();
        h.click(h.screen(DVec3::new(-100., 0., 60.)));
        h.click(h.screen(DVec3::new(100., 0., -60.)));
        assert_eq!(
            h.handle.snapshot().model.parts.len(),
            count + 2,
            "repeat placement must work on separate empty ground cells"
        );
        assert!(matches!(h.tool, Tool::Place));
        // A source drag dropped outside the viewport does not place anything.
        let count = h.handle.snapshot().model.parts.len();
        h.pointer(library, Some(true));
        h.pointer(egui::pos2(190., 200.), None);
        h.pointer(egui::pos2(190., 200.), Some(false));
        assert_eq!(h.handle.snapshot().model.parts.len(), count);
    }
}
