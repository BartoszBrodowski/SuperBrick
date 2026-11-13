//! Full-connector tools. Analysis is asynchronous; accepted edits use the shared engine commands.
use crate::app::Tool;
use eframe::egui::{self, Color32, Key, Pos2, Rect, Stroke};
use engine::{
    Cmd, Snapshot,
    connectors::{self, Analysis, Endpoint},
    joints::Articulation,
};
use glam::{DVec3, Vec3};
use render::Camera;
use std::collections::BTreeSet;
use std::sync::mpsc;

#[derive(Default)]
pub struct Mechanical {
    stamp: Option<(u64, usize)>,
    analysis_mode: Option<bool>,
    rx: Option<mpsc::Receiver<Result<Analysis, String>>>,
    analysis: Option<Analysis>,
    error: String,
    feedback: String,
    clicked: Option<u64>,
    joint: Option<u64>,
    axis: usize,
    source: Option<Endpoint>,
    target: Option<Endpoint>,
    pose: usize,
    confirm: bool,
    dragging: Option<(usize, f64)>,
    angle: f64,
    visibility_key: String,
    visible: std::collections::BTreeSet<(u64, usize, usize)>,
    legal_key: String,
    legal: Option<std::collections::BTreeSet<(u64, usize, usize)>>,
    legal_rx: Option<mpsc::Receiver<Result<Vec<Endpoint>, String>>>,
}
impl Mechanical {
    pub fn graph(&self) -> Option<&engine::joints::JointGraph> {
        self.analysis.as_ref().map(|a| &a.graph)
    }
    pub fn prepare(
        &mut self,
        ui: &mut egui::Ui,
        tool: Tool,
        snapshot: &Snapshot,
        file: usize,
        keyboard: bool,
    ) -> Option<Cmd> {
        if !matches!(tool, Tool::Hinge | Tool::Connect | Tool::Lasso) {
            self.dragging = None;
            return None;
        }
        let stamp = (snapshot.revision, file);
        let full = tool != Tool::Connect;
        if self.stamp != Some(stamp) || self.analysis_mode.is_some_and(|mode| mode != full) {
            self.analysis_mode = Some(full);
            self.stamp = Some(stamp);
            self.analysis = None;
            self.error.clear();
            self.feedback.clear();
            self.dragging = None;
            self.confirm = false;
            self.source = None;
            self.target = None;
            self.legal = None;
            self.legal_rx = None;
            self.legal_key.clear();
            self.visibility_key.clear();
            let (tx, rx) = mpsc::channel();
            self.rx = Some(rx);
            let model = snapshot.model.clone();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let result = if full {
                    connectors::analyze(&model, file)
                } else {
                    connectors::request(&model, file, serde_json::json!({"action":"endpoints"}))
                        .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
                };
                let _ = tx.send(result);
                ctx.request_repaint();
            });
        }
        if let Some(rx) = &self.rx {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(a) => self.analysis = Some(a),
                    Err(e) => self.error = e,
                }
                self.rx = None;
            }
        }
        if self.rx.is_some() {
            ui.label(if full {
                "Reading connector graph…"
            } else {
                "Reading part connectors (no whole-build solve)…"
            });
            return None;
        }
        if !self.error.is_empty() {
            ui.colored_label(Color32::LIGHT_RED, &self.error);
            if ui.button("Retry connector setup").clicked() {
                self.stamp = None;
            }
            return None;
        }
        let ids = Self::ids(snapshot, file);
        if !self.feedback.is_empty() {
            ui.colored_label(Color32::LIGHT_RED, &self.feedback);
        }
        if tool == Tool::Connect {
            if let Some(rx) = &self.legal_rx {
                if let Ok(result) = rx.try_recv() {
                    match result {
                        Ok(v) => self.legal = Some(v.iter().filter_map(endpoint_key).collect()),
                        Err(e) => self.feedback = e,
                    }
                    self.legal_rx = None;
                }
            }
            if let (Some(source), Some(a)) = (&self.source, &self.analysis) {
                let key = format!("{stamp:?}:{source:?}");
                if self.legal_key != key {
                    self.legal_key = key;
                    self.legal = None;
                    let source = source.clone();
                    let targets: Vec<_> = a.connectors.iter().map(|c| c.endpoint()).collect();
                    let model = snapshot.model.clone();
                    let ctx = ui.ctx().clone();
                    let (tx, rx) = mpsc::channel();
                    self.legal_rx = Some(rx);
                    std::thread::spawn(move || {
                        let r=connectors::request(&model,file,serde_json::json!({"action":"compatible","source":source,"targets":targets})).and_then(|v|serde_json::from_value(v["destinations"].clone()).map_err(|e|e.to_string()));
                        let _ = tx.send(r);
                        ctx.request_repaint();
                    });
                }
            }
            let mut command = None;
            ui.horizontal_wrapped(|ui| {
                ui.label(if self.source.is_none() {
                    "1 · Pick the piece/connector to move"
                } else if self.target.is_none() {
                    "2 · Pick a visible compatible destination"
                } else {
                    "3 · Connect (Enter), or choose another destination"
                });
                if ui.button("Reset").clicked() {
                    self.source = None;
                    self.target = None;
                    self.legal = None;
                    self.legal_key.clear();
                }
            });
            if self.legal_rx.is_some() {
                ui.label("Filtering connector families…");
            } else if self.source.is_some() && self.legal.as_ref().is_some_and(|v| v.is_empty()) {
                ui.label("No compatible destinations. Choose Auto on the moving piece or Reset.");
            }
            ui.small("Hidden/occluded connectors are suppressed. The chosen mate is collision-checked when you Connect.");
            if let Some(a) = &self.analysis {
                ui.horizontal_wrapped(|ui| {
                    Self::endpoint_ui(
                        ui,
                        "Moving",
                        &mut self.source,
                        a,
                        if self.visibility_key.is_empty() {
                            None
                        } else {
                            Some(&self.visible)
                        },
                    );
                    let legal_visible: BTreeSet<_> = self
                        .legal
                        .iter()
                        .flat_map(|v| v.iter())
                        .filter(|k| self.visibility_key.is_empty() || self.visible.contains(k))
                        .copied()
                        .collect();
                    Self::endpoint_ui(ui, "Destination", &mut self.target, a, Some(&legal_visible));
                    ui.label("Pose");
                    ui.add(egui::DragValue::new(&mut self.pose).range(0..=100));
                    let ready = self.source.is_some()
                        && self
                            .target
                            .as_ref()
                            .and_then(endpoint_key)
                            .is_some_and(|k| legal_visible.contains(&k));
                    if ui
                        .add_enabled(ready, egui::Button::new("Connect · Enter"))
                        .clicked()
                        || (ready
                            && keyboard
                            && !ui.ctx().egui_wants_keyboard_input()
                            && ui.input(|i| i.key_pressed(Key::Enter)))
                    {
                        command = Some(Cmd::Connect {
                            ids: ids.clone(),
                            file,
                            source: self.source.clone().unwrap(),
                            target: self.target.clone().unwrap(),
                            pose: self.pose,
                        });
                    }
                });
            }
            return command;
        }
        if tool == Tool::Lasso {
            return None;
        }
        let Some(clicked) = self.clicked else {
            ui.label("H: click a rigid chunk, choose its joint, then drag a ring or use arrows.");
            return None;
        };
        let Some(analysis) = &self.analysis else {
            return None;
        };
        let mut connected = BTreeSet::from([clicked]);
        loop {
            let old = connected.len();
            for j in &analysis.graph.joints {
                if connected.contains(&j.a) || connected.contains(&j.b) {
                    connected.insert(j.a);
                    connected.insert(j.b);
                }
            }
            if connected.len() == old {
                break;
            }
        }
        for near in &analysis.near_joints {
            let source_in = connected.contains(&near.source.part);
            let target_in = connected.contains(&near.target.part);
            if source_in == target_in {
                continue;
            }
            ui.colored_label(
                Color32::YELLOW,
                format!(
                    "Nearby {}: #{} ↔ #{}; centres {:.2} LDU apart. Not yet an engaged hinge.",
                    near.label, near.source.part, near.target.part, near.distance
                ),
            );
            if ui
                .button(format!(
                    "Seat ball/socket #{} ↔ #{} (collision checked)",
                    near.source.part, near.target.part
                ))
                .clicked()
            {
                let (source, target) = if source_in {
                    (near.source.clone(), near.target.clone())
                } else {
                    (near.target.clone(), near.source.clone())
                };
                return Some(Cmd::Connect {
                    ids: ids.clone(),
                    file,
                    source,
                    target,
                    pose: 0,
                });
            }
        }
        let pivots = analysis.graph.pivots(clicked).unwrap_or_default();
        let joints = analysis.graph.joints.clone();
        if !pivots.contains(&self.joint.unwrap_or(0)) {
            self.joint = if pivots.len() == 1 {
                pivots.first().copied()
            } else {
                None
            };
            self.axis = 0;
        }
        if self.joint.is_none() && !pivots.is_empty() {
            ui.colored_label(Color32::YELLOW,format!("{} joints available: choose a numbered joint on the build or from the menu. Rotation is disabled until you choose.",pivots.len()));
        }
        let name = |id| {
            snapshot
                .model
                .parts
                .get(&id)
                .map(|p| p.name.as_str())
                .unwrap_or("unknown")
        };
        let label = |j: &engine::joints::Joint| {
            format!(
                "{} · #{} {} ↔ #{} {}",
                j.label,
                j.a,
                name(j.a),
                j.b,
                name(j.b)
            )
        };
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("hinge-pivot")
                .selected_text(
                    self.joint
                        .and_then(|id| joints.iter().find(|j| j.id == id))
                        .map(&label)
                        .unwrap_or("Choose rotation joint…".into()),
                )
                .show_ui(ui, |ui| {
                    for id in &pivots {
                        let j = joints.iter().find(|j| j.id == *id).unwrap();
                        if ui
                            .selectable_value(&mut self.joint, Some(*id), label(j))
                            .changed()
                        {
                            self.confirm = false;
                            self.axis = 0;
                        }
                    }
                });
            if let Some(plan) = self.plan().ok() {
                for i in 0..plan.axes.len() {
                    ui.selectable_value(&mut self.axis, i, format!("Axis {}", i + 1));
                }
                if !plan.detach.is_empty() {
                    ui.checkbox(
                        &mut self.confirm,
                        format!("Allow opposite joint(s) {:?} to detach", plan.detach),
                    );
                }
            }
        });
        let Ok(plan) = self.plan() else {
            if pivots.is_empty() {
                ui.label("This chunk has no available rotation joint.");
            }
            return None;
        };
        let mut angle = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "{} moving pieces · arrows rotate 15° · Shift+arrows move",
                plan.moving.len()
            ));
            if ui.button("−15°").clicked() {
                angle = Some(-15.);
            }
            if ui.button("+15°").clicked() {
                angle = Some(15.);
            }
        });
        if keyboard && !ui.ctx().egui_wants_keyboard_input() {
            for (key, sign) in [
                (Key::ArrowLeft, -1.),
                (Key::ArrowRight, 1.),
                (Key::ArrowUp, 1.),
                (Key::ArrowDown, -1.),
            ] {
                if ui.input(|i| i.key_pressed(key)) {
                    if ui.input(|i| i.modifiers.shift) {
                        return Some(Cmd::Move {
                            ids: plan.moving.iter().copied().collect(),
                            delta: if matches!(key, Key::ArrowLeft | Key::ArrowRight) {
                                DVec3::X * sign * 4.
                            } else {
                                DVec3::NEG_Y * sign * 4.
                            },
                            free: false,
                        });
                    }
                    angle = Some(sign * 15.);
                }
            }
        }
        angle.and_then(|angle| self.rotation_command(snapshot, file, angle))
    }
    fn ids(s: &Snapshot, file: usize) -> Vec<u64> {
        s.model
            .parts
            .values()
            .filter(|p| p.file == file)
            .map(|p| p.id)
            .collect()
    }
    fn plan(&self) -> Result<Articulation, String> {
        self.analysis
            .as_ref()
            .ok_or("Graph not loaded")?
            .graph
            .articulate(
                self.clicked.ok_or("Pick a chunk")?,
                self.joint.ok_or("Pick a joint")?,
            )
    }
    fn rotation(&self, s: &Snapshot, file: usize, angle: f64) -> Result<Cmd, String> {
        let plan = self.plan()?;
        let detach = if self.confirm {
            plan.detach.clone()
        } else {
            vec![]
        };
        plan.rotation(self.axis, angle, &detach)?;
        Ok(Cmd::Articulate {
            ids: Self::ids(s, file),
            file,
            clicked: self.clicked.unwrap(),
            joint: self.joint.unwrap(),
            axis: self.axis,
            angle,
            detach,
        })
    }
    fn rotation_command(&mut self, s: &Snapshot, file: usize, angle: f64) -> Option<Cmd> {
        match self.rotation(s, file, angle) {
            Ok(command) => {
                self.feedback.clear();
                Some(command)
            }
            Err(reason) => {
                self.feedback = reason;
                None
            }
        }
    }
    fn endpoint_ui(
        ui: &mut egui::Ui,
        label: &str,
        endpoint: &mut Option<Endpoint>,
        a: &Analysis,
        allowed: Option<&BTreeSet<(u64, usize, usize)>>,
    ) {
        let Some(e) = endpoint else {
            ui.label(format!("{label}: click piece"));
            return;
        };
        egui::ComboBox::from_id_salt(label)
            .selected_text(format!(
                "{label} #{}: {}",
                e.part,
                e.index
                    .map(|i| format!("connector {i}"))
                    .unwrap_or("Auto".into())
            ))
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(e.index.is_none(), "Auto · best compatible connector")
                    .clicked()
                {
                    e.index = None;
                    e.leaf = None;
                }
                let part = e.part;
                for c in a.connectors.iter().filter(|c| {
                    c.part == part && allowed.is_none_or(|v| v.contains(&(c.part, c.leaf, c.index)))
                }) {
                    if ui
                        .selectable_label(
                            e.index == Some(c.index) && e.leaf == Some(c.leaf),
                            format!("{} · {}:{}", c.label, c.leaf, c.index),
                        )
                        .clicked()
                    {
                        *e = c.endpoint();
                    }
                }
            });
    }
    pub fn picked(
        &mut self,
        tool: Tool,
        id: u64,
        pointer: Pos2,
        cam: &Camera,
        rect: Rect,
        snapshot: &Snapshot,
    ) -> Option<Cmd> {
        let a = self.analysis.as_ref()?;
        if tool == Tool::Connect {
            let closest = a
                .connectors
                .iter()
                .filter(|c| c.part == id)
                .filter(|c| {
                    self.visibility_key.is_empty()
                        || self.visible.contains(&(c.part, c.leaf, c.index))
                })
                .filter(|c| {
                    self.source.as_ref().is_none_or(|s| s.part == id)
                        || self
                            .legal
                            .as_ref()
                            .is_some_and(|v| v.contains(&(c.part, c.leaf, c.index)))
                })
                .filter_map(|c| project(cam, rect, c.position).map(|p| (c, p.distance(pointer))))
                .filter(|(_, d)| *d < 7. || self.source.as_ref().is_some_and(|s| s.part != id))
                .min_by(|a, b| a.1.total_cmp(&b.1));
            let e = closest.map(|(c, _)| c.endpoint()).unwrap_or(Endpoint {
                part: id,
                index: None,
                leaf: None,
            });
            if self.source.is_none() {
                self.source = Some(e);
                self.target = None;
            } else if self.source.as_ref().unwrap().part != id {
                let allowed = self.legal.as_ref().is_some_and(|legal| {
                    endpoint_key(&e).map_or_else(
                        || legal.iter().any(|c| c.0 == id),
                        |key| legal.contains(&key),
                    )
                });
                if !allowed {
                    self.feedback="Choose a highlighted destination; hidden or incompatible connectors cannot be selected.".into();
                    return None;
                }
                self.target = Some(e);
            } else {
                self.source = Some(e);
            }
            return Some(Cmd::Select { ids: vec![id] });
        }
        if tool == Tool::Hinge {
            self.feedback.clear();
            self.clicked = Some(id);
            self.confirm = false;
            self.axis = 0;
            let pivots = a.graph.pivots(id).unwrap_or_default();
            self.joint = if pivots.len() == 1 {
                pivots.first().copied()
            } else {
                None
            };
            let ids = self
                .plan()
                .map(|p| p.moving.into_iter().collect())
                .unwrap_or_else(|_| {
                    a.graph
                        .rigid_chunk(id)
                        .unwrap_or_default()
                        .into_iter()
                        .collect()
                });
            return Some(Cmd::Select { ids });
        }
        let _ = snapshot;
        None
    }
    pub fn overlay(
        &mut self,
        ui: &egui::Ui,
        tool: Tool,
        cam: &Camera,
        rect: Rect,
        resp: &egui::Response,
        snapshot: &Snapshot,
        file: usize,
        lib: &ldraw::Library,
    ) -> (bool, Option<Cmd>) {
        let painter = ui.painter_at(rect);
        if tool == Tool::Connect {
            let visibility_key = format!("{}:{file}:{cam:?}", snapshot.revision);
            if self.visibility_key != visibility_key {
                self.visibility_key = visibility_key;
                let occlusion = crate::visibility::Occlusion::new(snapshot, file, lib);
                self.visible = self
                    .analysis
                    .iter()
                    .flat_map(|a| &a.connectors)
                    .filter(|c| {
                        let hidden = snapshot.model.parts.get(&c.part).is_some_and(|p| p.hidden);
                        !hidden
                            && [0.0, 4.0, -4.0].iter().any(|d| {
                                occlusion.visible(cam.eye().as_dvec3(), c.position + c.axis * *d)
                            })
                    })
                    .map(|c| (c.part, c.leaf, c.index))
                    .collect();
            }
            let mut clicked = None;
            if let Some(a) = &self.analysis {
                for c in &a.connectors {
                    let key = (c.part, c.leaf, c.index);
                    if !self.visible.contains(&key) {
                        continue;
                    }
                    if self.source.as_ref().is_some_and(|s| s.part != c.part)
                        && !self.legal.as_ref().is_some_and(|v| v.contains(&key))
                    {
                        continue;
                    }
                    if let Some(p) = project(cam, rect, c.position) {
                        let selected = self.source.as_ref().is_some_and(|e| e.part == c.part)
                            || self.target.as_ref().is_some_and(|e| e.part == c.part);
                        painter.circle_stroke(
                            p,
                            if selected { 4. } else { 2.5 },
                            Stroke::new(
                                1.,
                                if selected {
                                    Color32::YELLOW
                                } else {
                                    Color32::LIGHT_BLUE
                                },
                            ),
                        );
                        if resp.clicked()
                            && ui
                                .input(|i| i.pointer.interact_pos())
                                .is_some_and(|pos| pos.distance(p) < 7.)
                        {
                            clicked = Some((c.endpoint(), p));
                        }
                    }
                }
            }
            if let Some((endpoint, _)) = clicked {
                let id = endpoint.part;
                if self.source.as_ref().is_none_or(|s| s.part == id) {
                    self.source = Some(endpoint);
                    self.target = None;
                } else {
                    self.target = Some(endpoint);
                }
                return (true, Some(Cmd::Select { ids: vec![id] }));
            }
            return (false, None);
        }
        if tool != Tool::Hinge {
            return (false, None);
        }
        if self.joint.is_none() {
            if let (Some(clicked), Some(a)) = (self.clicked, self.analysis.as_ref()) {
                let pivots = a.graph.pivots(clicked).unwrap_or_default();
                let cursor = ui.input(|i| i.pointer.interact_pos());
                let mut nearest = None;
                for (index, id) in pivots.iter().enumerate() {
                    if let Some(j) = a.graph.joints.iter().find(|j| j.id == *id) {
                        if let Some(p) = project(cam, rect, j.point) {
                            painter.circle_filled(p, 11., Color32::from_rgb(50, 90, 150));
                            painter.text(
                                p,
                                egui::Align2::CENTER_CENTER,
                                (index + 1).to_string(),
                                egui::FontId::proportional(14.),
                                Color32::WHITE,
                            );
                            if let Some(cursor) = cursor {
                                let distance = cursor.distance(p);
                                if distance < 15. && nearest.is_none_or(|(_, d)| distance < d) {
                                    nearest = Some((*id, distance));
                                }
                            }
                        }
                    }
                }
                if resp.clicked() {
                    if let Some((id, _)) = nearest {
                        self.joint = Some(id);
                        self.axis = 0;
                        self.confirm = false;
                        return (
                            true,
                            self.plan().ok().map(|p| Cmd::Select {
                                ids: p.moving.into_iter().collect(),
                            }),
                        );
                    }
                }
            }
        }
        let Ok(plan) = self.plan() else {
            return (false, None);
        };
        let radius =
            (cam.dist as f64 * (cam.fov_y as f64 / 2.).tan() * 2. / rect.height() as f64) * 72.;
        let pos = ui.input(|i| i.pointer.interact_pos());
        let mut nearest = None;
        for (i, axis) in plan.axes.iter().enumerate() {
            let (u, v) = basis(*axis);
            let mut points = Vec::new();
            for n in 0..=96 {
                let t = n as f64 / 96. * std::f64::consts::TAU;
                if let Some(p) =
                    project(cam, rect, plan.pivot + (u * t.cos() + v * t.sin()) * radius)
                {
                    points.push(p);
                }
            }
            if let Some(p) = pos {
                let d = points
                    .iter()
                    .map(|q| q.distance(p))
                    .fold(f32::INFINITY, f32::min);
                if d < 10. && nearest.is_none_or(|(_, best)| d < best) {
                    nearest = Some((i, d));
                }
            }
            let color = [
                Color32::LIGHT_RED,
                Color32::LIGHT_GREEN,
                Color32::LIGHT_BLUE,
            ][i % 3];
            painter.add(egui::Shape::line(
                points,
                Stroke::new(if i == self.axis { 3. } else { 1.5 }, color),
            ));
        }
        let mut consumed = false;
        let mut command = None;
        if resp.clicked() {
            if let Some((axis, _)) = nearest {
                self.axis = axis;
                consumed = true;
            }
        }
        if resp.drag_started() {
            if let (Some((axis, _)), Some(pos)) = (nearest, pos) {
                if let Some(start) = angle_at(cam, rect, pos, plan.pivot, plan.axes[axis]) {
                    self.axis = axis;
                    self.dragging = Some((axis, start));
                    self.angle = 0.;
                }
            }
        }
        if let Some((axis, start)) = self.dragging {
            consumed = true;
            if let Some(pos) = pos {
                if let Some(current) = angle_at(cam, rect, pos, plan.pivot, plan.axes[axis]) {
                    let diff = (current - start)
                        .sin()
                        .atan2((current - start).cos())
                        .to_degrees();
                    let step = if ui.input(|i| i.modifiers.shift) {
                        1.
                    } else {
                        15.
                    };
                    self.angle = (diff / step).round() * step;
                }
            }
            if let Some(center) = project(cam, rect, plan.pivot) {
                painter.text(
                    center + egui::vec2(10., -20.),
                    egui::Align2::LEFT_BOTTOM,
                    format!("{:+.0}° · release to apply", self.angle),
                    egui::FontId::proportional(15.),
                    Color32::WHITE,
                );
            }
            // Lightweight wire preview; the full connector/solid check happens atomically on release.
            let rotation = glam::DMat3::from_axis_angle(plan.axes[axis], self.angle.to_radians());
            for part in snapshot.model.flat(file) {
                let Some(id) = snapshot.model.id_at(file, part.root_line) else {
                    continue;
                };
                if !plan.moving.contains(&id) {
                    continue;
                }
                if let Ok(mesh) = lib.mesh(&part.name, ldraw::Lod::Low) {
                    for edge in mesh.lines.iter().step_by((mesh.lines.len() / 160).max(1)) {
                        let points = edge.map(|p| {
                            project(
                                cam,
                                rect,
                                plan.pivot
                                    + rotation * (part.pos + part.rot * p.as_dvec3() - plan.pivot),
                            )
                        });
                        if let [Some(a), Some(b)] = points {
                            painter.line_segment([a, b], Stroke::new(1., Color32::YELLOW));
                        }
                    }
                }
            }
            if resp.drag_stopped() {
                command = self.rotation_command(snapshot, file, self.angle);
                self.dragging = None;
            }
        }
        (consumed, command)
    }
}

fn endpoint_key(e: &Endpoint) -> Option<(u64, usize, usize)> {
    Some((e.part, e.leaf?, e.index?))
}
fn basis(axis: DVec3) -> (DVec3, DVec3) {
    let u = axis
        .cross(if axis.x.abs() < 0.8 {
            DVec3::X
        } else {
            DVec3::Y
        })
        .normalize();
    (u, axis.cross(u).normalize())
}
pub(crate) fn project(cam: &Camera, rect: Rect, p: DVec3) -> Option<Pos2> {
    let v = cam.view_proj(rect.aspect_ratio()) * p.as_vec3().extend(1.);
    if v.w <= 0. {
        return None;
    }
    let q = v.truncate() / v.w;
    Some(egui::pos2(
        rect.center().x + q.x * rect.width() / 2.,
        rect.center().y - q.y * rect.height() / 2.,
    ))
}
fn angle_at(cam: &Camera, rect: Rect, pos: Pos2, pivot: DVec3, axis: DVec3) -> Option<f64> {
    let inv = cam.view_proj(rect.aspect_ratio()).inverse();
    let x = (pos.x - rect.left()) / rect.width() * 2. - 1.;
    let y = 1. - (pos.y - rect.top()) / rect.height() * 2.;
    let p = inv.project_point3(Vec3::new(x, y, 0.)).as_dvec3();
    let d = (inv.project_point3(Vec3::new(x, y, 1.)).as_dvec3() - p).normalize();
    let den = d.dot(axis);
    if den.abs() < 1e-5 {
        return None;
    }
    let hit = p + d * ((pivot - p).dot(axis) / den) - pivot;
    let (u, v) = basis(axis);
    Some(hit.dot(v).atan2(hit.dot(u)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_hinge_click_selects_chunk_and_plain_arrows_articulate() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/lock_centered.ldr");
        let mut e = engine::Engine::new(
            std::sync::Arc::new(ldraw::Library::open_default().unwrap()),
            ldraw::Document::load(&path).unwrap(),
            None,
        );
        let snapshot = e.snapshot();
        let analysis = connectors::analyze(&snapshot.model, 0).unwrap();
        let mut tool = Mechanical {
            stamp: Some((snapshot.revision, 0)),
            analysis: Some(analysis),
            ..Default::default()
        };
        let cam = Camera::framing(Vec3::splat(-100.), Vec3::splat(100.), 1.);
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(600., 600.));
        let selection = tool
            .picked(Tool::Hinge, 2, rect.center(), &cam, rect, &snapshot)
            .unwrap();
        assert!(matches!(selection,Cmd::Select{ids} if ids==vec![2]));
        let ctx = egui::Context::default();
        let mut command = None;
        let raw = egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ui| {
            command = tool.prepare(ui, Tool::Hinge, &snapshot, 0, true);
        });
        output.textures_delta.clear();
        assert!(matches!(command, Some(Cmd::Articulate { angle: 15., .. })));
        e.execute(command.unwrap()).unwrap();
    }
    #[test]
    fn paired_hinge_ring_release_commits_the_preview_without_detach_confirmation() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/parallel_hinges.ldr");
        let lib = std::sync::Arc::new(ldraw::Library::open_default().unwrap());
        let mut e = engine::Engine::new(lib.clone(), ldraw::Document::load(&path).unwrap(), None);
        let snapshot = e.snapshot();
        let before = e.model.doc.to_bytes();
        let mut tool = Mechanical {
            stamp: Some((snapshot.revision, 0)),
            analysis: Some(connectors::analyze(&snapshot.model, 0).unwrap()),
            ..Default::default()
        };
        let cam = Camera::framing(Vec3::splat(-100.), Vec3::splat(100.), 1.);
        let rect = Rect::from_min_size(egui::pos2(10., 10.), egui::vec2(600., 600.));
        tool.picked(Tool::Hinge, 6, rect.center(), &cam, rect, &snapshot);
        assert!(
            tool.joint.is_none(),
            "multiple pivots must require explicit choice"
        );
        assert!(tool.rotation(&snapshot, 0, 15.).is_err());
        assert_eq!(e.model.doc.to_bytes(), before);
        tool.joint = tool
            .analysis
            .as_ref()
            .unwrap()
            .graph
            .pivots(6)
            .unwrap()
            .first()
            .copied();
        let plan = tool.plan().unwrap();
        assert!(plan.detach.is_empty());
        let radius =
            (cam.dist as f64 * (cam.fov_y as f64 / 2.).tan() * 2. / rect.height() as f64) * 72.;
        let (u, v) = basis(plan.axes[0]);
        let point = |angle: f64| {
            let t = angle.to_radians();
            project(
                &cam,
                rect,
                plan.pivot + (u * t.cos() + v * t.sin()) * radius,
            )
            .unwrap()
        };
        let ctx = egui::Context::default();
        let mut frame = |events: Vec<egui::Event>| {
            let mut command = None;
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800., 800.))),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                command = tool
                    .overlay(ui, Tool::Hinge, &cam, rect, &resp, &snapshot, 0, &lib)
                    .1;
            });
            output.textures_delta.clear();
            command
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        assert!(frame(vec![egui::Event::PointerMoved(point(40.))]).is_none());
        assert!(frame(vec![button(point(40.), true)]).is_none());
        assert!(frame(vec![egui::Event::PointerMoved(point(60.))]).is_none());
        assert!(frame(vec![egui::Event::PointerMoved(point(90.))]).is_none());
        assert_eq!(
            e.model.doc.to_bytes(),
            before,
            "preview must not mutate the document"
        );
        let command = frame(vec![button(point(90.), false)])
            .expect("releasing a coaxial hinge preview must produce a transaction");
        assert!(matches!(&command, Cmd::Articulate { angle, detach, .. }
            if angle.abs() >= 15. && detach.is_empty()));
        e.execute(command).unwrap();
        assert_ne!(e.model.doc.to_bytes(), before);
        assert_eq!(
            connectors::analyze(&e.model, 0)
                .unwrap()
                .graph
                .joints
                .iter()
                .filter(|j| j.label.contains("hinge"))
                .count(),
            2
        );
        e.undo(false).unwrap();
        assert_eq!(e.model.doc.to_bytes(), before);
    }
    #[test]
    fn connect_tool_two_piece_selection_and_enter_seats_a_technic_pin() {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../bench-next/tests/connections/pin_in_brick_hole.ldr");
        let mut e = engine::Engine::new(
            std::sync::Arc::new(ldraw::Library::open_default().unwrap()),
            ldraw::Document::load(&path).unwrap(),
            None,
        );
        e.execute(Cmd::Move {
            ids: vec![2],
            delta: DVec3::X * 140.,
            free: true,
        })
        .unwrap();
        let snapshot = e.snapshot();
        let mut tool = Mechanical {
            stamp: Some((snapshot.revision, 0)),
            analysis: Some(connectors::analyze(&snapshot.model, 0).unwrap()),
            ..Default::default()
        };
        let cam = Camera::framing(Vec3::splat(-100.), Vec3::splat(100.), 1.);
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(600., 600.));
        for id in [2, 1] {
            if id == 1 {
                let source = tool.source.clone().unwrap();
                let targets: Vec<_> = tool
                    .analysis
                    .as_ref()
                    .unwrap()
                    .connectors
                    .iter()
                    .map(|c| c.endpoint())
                    .collect();
                let result = connectors::request(
                    &snapshot.model,
                    0,
                    serde_json::json!({"action":"destinations","source":source,"targets":targets}),
                )
                .unwrap();
                let endpoints: Vec<Endpoint> =
                    serde_json::from_value(result["destinations"].clone()).unwrap();
                tool.legal = Some(endpoints.iter().filter_map(endpoint_key).collect());
                tool.legal_key = format!("{:?}:{source:?}", (snapshot.revision, 0usize));
            }
            tool.picked(
                Tool::Connect,
                id,
                egui::pos2(-1000., -1000.),
                &cam,
                rect,
                &snapshot,
            );
        }
        let ctx = egui::Context::default();
        let mut command = None;
        let raw = egui::RawInput {
            events: vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ui| {
            command = tool.prepare(ui, Tool::Connect, &snapshot, 0, true);
        });
        output.textures_delta.clear();
        assert!(matches!(command, Some(Cmd::Connect { .. })));
        e.execute(command.unwrap()).unwrap();
        assert!(
            connectors::analyze(&e.model, 0)
                .unwrap()
                .graph
                .joints
                .iter()
                .any(|j| j.label == "pin-hole")
        );
    }
}
