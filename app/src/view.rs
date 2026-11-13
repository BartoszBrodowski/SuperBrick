use crate::app::State;
use eframe::{egui, egui_wgpu};
use egui::{Color32, Rect, Sense, Vec2, pos2};
use render::{COLOR_FORMAT, Camera, DEPTH_FORMAT, Gpu, Picker, Renderer, Scene, SceneOptions};
use std::{path::Path, sync::mpsc, time::Instant};
const SAMPLES: u32 = 4;
#[derive(Clone, Debug)]
pub struct LibraryDrag {
    pub name: String,
    pub color: i32,
}
pub struct Target {
    size: (u32, u32),
    color: wgpu::TextureView,
    resolve: wgpu::TextureView,
    depth: wgpu::TextureView,
    tex: egui::TextureId,
}
pub struct Loaded {
    pub floor_y: f32,
    pub renderer: Renderer,
    pub cam: Camera,
    pub ids: Vec<Option<u64>>,
    pub flat: Vec<ldraw::FlatPart>,
    pub base_flags: Vec<u32>,
    pub base_count: usize,
    pub lo: glam::Vec3,
    pub hi: glam::Vec3,
}
pub struct View {
    pub file: usize,
    pub loaded: Option<Loaded>,
    pub dirty: bool,
    pub open_submodel: Option<usize>,
    revision: u64,
    flags_revision: u64,
    flags_mode: u8,
    selection: Vec<u64>,
    rx: Option<mpsc::Receiver<(u64, Scene, Vec<ldraw::FlatPart>, Vec<Option<u64>>, Vec<u32>)>>,
    target: Option<Target>,
    picker: Option<Picker>,
    edges: bool,
    fast: bool,
    error: String,
    encode_ms: f64,
    interaction: crate::interaction::Interaction,
    mechanical: crate::mechanical::Mechanical,
    lasso: crate::lasso::Lasso,
    issue_seen: Option<u64>,
}
impl View {
    pub fn is_placing(&self) -> bool {
        self.interaction.is_placing()
    }
    pub fn new(file: usize) -> Self {
        Self {
            file,
            loaded: None,
            dirty: true,
            open_submodel: None,
            revision: u64::MAX,
            flags_revision: u64::MAX,
            flags_mode: 255,
            selection: vec![],
            rx: None,
            target: None,
            picker: None,
            edges: true,
            fast: false,
            error: String::new(),
            encode_ms: 0.0,
            interaction: Default::default(),
            mechanical: Default::default(),
            lasso: Default::default(),
            issue_seen: None,
        }
    }
    pub fn screenshot(&self, gpu: &Gpu, path: &Path) {
        if let Some(l) = &self.loaded {
            let target = render::Offscreen::new(gpu, SAMPLES, 1200, 800);
            target.render(gpu, &l.renderer, &l.cam);
            let _ = render::save_png(path, 1200, 800, &target.read_rgba(gpu));
        }
    }
}
fn make_target(
    gpu: &Gpu,
    rs: &egui_wgpu::RenderState,
    size: (u32, u32),
    old: Option<egui::TextureId>,
) -> Target {
    let ext = wgpu::Extent3d {
        width: size.0,
        height: size.1,
        depth_or_array_layers: 1,
    };
    let tex = |format, samples| {
        gpu.device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("viewport"),
                size: ext,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
            .create_view(&Default::default())
    };
    let color = tex(COLOR_FORMAT, SAMPLES);
    let depth = tex(DEPTH_FORMAT, SAMPLES);
    let resolve_tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewport resolve"),
        size: ext,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
    });
    let resolve = resolve_tex.create_view(&Default::default());
    let gamma = resolve_tex.create_view(&wgpu::TextureViewDescriptor {
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    let mut r = rs.renderer.write();
    let tex = if let Some(id) = old {
        r.update_egui_texture_from_wgpu_texture(&gpu.device, &gamma, wgpu::FilterMode::Linear, id);
        id
    } else {
        r.register_native_texture(&gpu.device, &gamma, wgpu::FilterMode::Linear)
    };
    Target {
        size,
        color,
        resolve,
        depth,
        tex,
    }
}
pub fn ui(st: &mut State, i: usize, ui: &mut egui::Ui) {
    let revision = st.snapshot.presentation_revision();
    let v = &mut st.views[i];
    if v.revision != revision && v.rx.is_none() {
        let snapshot = st.snapshot.clone();
        let file = v.file;
        let lib = st.lib.clone();
        let ctx = ui.ctx().clone();
        let (tx, rx) = mpsc::channel();
        v.rx = Some(rx);
        let fast = v.fast;
        std::thread::spawn(move || {
            let mut flat = snapshot.model.flat(file);
            let index: std::collections::HashMap<_, _> = snapshot
                .model
                .parts
                .values()
                .map(|p| ((p.file, p.line), p.id))
                .collect();
            let mut ids: Vec<_> = flat
                .iter()
                .map(|p| index.get(&(file, p.root_line)).copied())
                .collect();
            let mut flags = vec![0; flat.len()];
            for g in snapshot.ghosts(lib.clone(), file) {
                flags.push(match g.kind {
                    engine::presentation::GhostKind::Added => render::FLAG_GHOST,
                    engine::presentation::GhostKind::Removed => render::FLAG_DELETE,
                    engine::presentation::GhostKind::Stale => render::FLAG_STALE,
                });
                flat.push(g.part);
                ids.push(None);
            }
            let opts = SceneOptions {
                lod: if fast {
                    ldraw::Lod::Low
                } else {
                    ldraw::Lod::Std
                },
                stud_lod: ldraw::Lod::Low,
                // Ghosts and hidden parts must not remove visible base studs via box culling.
                studs: if flags.iter().any(|&f| f != 0)
                    || snapshot.model.parts.values().any(|p| p.hidden)
                {
                    render::StudMode::Instanced
                } else {
                    render::StudMode::Culled
                },
                edges: true,
                ..Default::default()
            };
            let scene = Scene::build(&lib, &flat, &opts);
            let _ = tx.send((revision, scene, flat, ids, flags));
            ctx.request_repaint();
        });
    }
    if let Some(rx) = &v.rx {
        if let Ok((rev, scene, flat, ids, base_flags)) = rx.try_recv() {
            let view_bounds = if flat.is_empty() {
                (
                    glam::vec3(-160.0, 0.0, -160.0),
                    glam::vec3(160.0, 0.0, 160.0),
                )
            } else {
                scene.bounds
            };
            let cam = v
                .loaded
                .as_ref()
                .map(|l| l.cam)
                .unwrap_or_else(|| Camera::framing(view_bounds.0, view_bounds.1, 1.5));
            let mut renderer = Renderer::new(&st.gpu, COLOR_FORMAT, SAMPLES, &scene);
            renderer.enable_ground(&st.gpu, COLOR_FORMAT, view_bounds);
            let file = v.file;
            let phase = st
                .handle
                .call(move |e| Ok(e.ground_phase(file)))
                .unwrap_or_default();
            renderer.set_ground_phase(glam::vec2(phase.x as f32, phase.z as f32));
            v.loaded = Some(Loaded {
                floor_y: view_bounds.1.y,
                renderer,
                cam,
                base_count: ids.iter().filter(|id| id.is_some()).count(),
                ids,
                flat,
                base_flags,
                lo: view_bounds.0,
                hi: view_bounds.1,
            });
            v.revision = rev;
            v.rx = None;
            v.selection.clear();
            v.flags_revision = u64::MAX;
            v.dirty = true;
        }
    }
    let mut view_action = 0;
    ui.horizontal(|ui| {
        if ui.small_button("Fit").clicked() {
            view_action = 1;
        }
        for (label, action) in [("Front", 2), ("Top", 3), ("Right", 4), ("3D", 5)] {
            if ui.small_button(label).clicked() {
                view_action = action;
            }
        }
        if let Some(l) = v.loaded.as_mut() {
            if ui.checkbox(&mut l.cam.ortho, "Ortho").changed() {
                v.dirty = true;
            }
        }
        if ui.checkbox(&mut v.edges, "Edges").changed() {
            v.dirty = true;
        }
        if ui.checkbox(&mut v.fast, "Low LOD").changed() {
            v.revision = u64::MAX;
        }
    });
    let mut mechanical_command =
        v.mechanical
            .prepare(ui, st.tool, &st.snapshot, v.file, st.active == i);
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let Some(l) = v.loaded.as_mut() else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Loading scene…",
            egui::FontId::proportional(15.0),
            Color32::GRAY,
        );
        return;
    };
    if let Some((file, indices, revision, serial)) = &st.issue {
        if *file == v.file && *revision == st.snapshot.revision && v.issue_seen != Some(*serial) {
            if let Some(p) = indices.first().and_then(|i| l.flat.get(*i)) {
                let (center, radius) = st
                    .lib
                    .mesh(&p.name, ldraw::Lod::Low)
                    .map(|m| {
                        let (lo, hi) = m.bbox(true);
                        ((lo + hi).as_dvec3() * 0.5, (hi - lo).length() * 0.5)
                    })
                    .unwrap_or((glam::DVec3::ZERO, 20.));
                l.cam.target = (p.pos + p.rot * center).as_vec3();
                l.cam.dist = (radius * 6.).max(80.);
            }
            v.issue_seen = Some(*serial);
            v.dirty = true;
        }
    }
    let background = if ui.visuals().dark_mode {
        wgpu::Color {
            r: 0.018,
            g: 0.023,
            b: 0.030,
            a: 1.0,
        }
    } else {
        wgpu::Color {
            r: 0.80,
            g: 0.82,
            b: 0.85,
            a: 1.0,
        }
    };
    if l.renderer.background != background {
        l.renderer.background = background;
        v.dirty = true;
    }
    if view_action > 0 {
        match view_action {
            1 => l.cam = Camera::framing(l.lo, l.hi, rect.aspect_ratio()),
            2 => {
                l.cam.yaw = 0.0;
                l.cam.pitch = 0.0;
                l.cam.ortho = true;
            }
            3 => {
                l.cam.yaw = 0.0;
                l.cam.pitch = 1.5706;
                l.cam.ortho = true;
            }
            4 => {
                l.cam.yaw = std::f32::consts::FRAC_PI_2;
                l.cam.pitch = 0.0;
                l.cam.ortho = true;
            }
            _ => {
                l.cam.yaw = 0.61;
                l.cam.pitch = 0.49;
                l.cam.ortho = false;
            }
        }
        v.dirty = true;
    }
    let d = resp.drag_delta();
    if (resp.dragged_by(egui::PointerButton::Secondary)
        || resp.dragged_by(egui::PointerButton::Middle))
        && d != Vec2::ZERO
    {
        let fwd = (l.cam.target - l.cam.eye()).normalize();
        let right = fwd.cross(glam::Vec3::NEG_Y).normalize();
        let up = right.cross(fwd);
        let k = l.cam.dist * (l.cam.fov_y * 0.5).tan() * 2.0 / rect.height().max(1.0);
        l.cam.target += (-right * d.x + up * d.y) * k;
        v.dirty = true;
    }
    if resp.hovered() {
        let (scroll, pinch, pointer) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.zoom_delta(),
                i.pointer.hover_pos(),
            )
        });
        if zoom_camera(
            &mut l.cam,
            rect,
            pointer.unwrap_or(rect.center()),
            scroll,
            pinch,
        ) {
            st.active = i;
            v.dirty = true;
        }
    }
    // Keep the expanded guide inside the clip volume without zooming the model out to fit it.
    l.cam.radius = l.cam.radius.max(l.renderer.ground_radius(l.cam.target));
    let selected: Vec<_> = st.snapshot.model.selection.iter().copied().collect();
    let flags_mode = st.show_hidden as u8 | ((st.tool == crate::app::Tool::Hide) as u8) << 1;
    if selected != v.selection || v.flags_revision != v.revision || v.flags_mode != flags_mode {
        let index: std::collections::HashMap<_, _> = st
            .snapshot
            .model
            .parts
            .values()
            .map(|p| ((p.file, p.line), p))
            .collect();
        let changes = l.flat.iter().enumerate().map(|(n, flat)| {
            let mut flags = l.base_flags[n];
            for source in flat.ancestors.iter().chain(std::iter::once(&flat.src)) {
                if source.0 == 0 {
                    if let Some(part) = index.get(&(source.1, source.2)) {
                        if part.hidden {
                            flags |= crate::interaction::hidden_flags(
                                st.show_hidden,
                                st.tool == crate::app::Tool::Hide,
                            );
                        }
                        if st.snapshot.model.selection.contains(&part.id) {
                            flags |= render::FLAG_SELECTED;
                        }
                    }
                }
            }
            (n as u32, flags)
        });
        l.renderer.set_part_flags(&st.gpu, changes);
        v.selection = selected;
        v.flags_revision = v.revision;
        v.flags_mode = flags_mode;
        v.dirty = true;
    }
    let ppp = ui.ctx().pixels_per_point();
    let size = (
        (rect.width() * ppp).round().max(1.0) as u32,
        (rect.height() * ppp).round().max(1.0) as u32,
    );
    if v.target.as_ref().is_none_or(|t| t.size != size) {
        v.target = Some(make_target(
            &st.gpu,
            &st.rs,
            size,
            v.target.as_ref().map(|t| t.tex),
        ));
        v.dirty = true;
    }
    if resp.contains_pointer() {
        st.active = i;
    }
    let interaction = v.interaction.update(
        crate::interaction::Input {
            gpu: &st.gpu,
            lib: &st.lib,
            snapshot: &st.snapshot,
            file: v.file,
            tool: st.tool,
            place: st.place.as_deref(),
            place_rot: st.place_rot,
            assembly: &st.place_assembly,
            color: st.color,
            show_hidden: st.show_hidden,
        },
        l,
        &mut v.picker,
        rect,
        &resp,
        ui,
    );
    v.dirty |= interaction.dirty;
    if let Some(file) = interaction.open_submodel {
        v.open_submodel = Some(file);
    }
    if let Some(message) = interaction.message {
        st.message = message;
    }
    let t = v.target.as_ref().unwrap();
    if v.dirty {
        let started = Instant::now();
        let mut enc = st.gpu.device.create_command_encoder(&Default::default());
        l.renderer.show_edges = v.edges;
        l.renderer.encode(
            &st.gpu,
            &mut enc,
            &t.color,
            if v.interaction.has_preview() {
                None
            } else {
                Some(&t.resolve)
            },
            &t.depth,
            &l.cam,
            size,
        );
        v.interaction.render(
            &st.gpu, &mut enc, &t.color, &t.resolve, &t.depth, &l.cam, size,
        );
        st.gpu.queue.submit([enc.finish()]);
        v.encode_ms = started.elapsed().as_secs_f64() * 1000.0;
        v.dirty = false;
    }
    painter.image(
        t.tex,
        rect,
        Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
        Color32::WHITE,
    );
    if let Some((file, indices, revision, _)) = &st.issue {
        if *file == v.file && *revision == st.snapshot.revision {
            let contact_review = st.report.as_ref().is_some_and(|r| {
                r.reviewed
                    .iter()
                    .any(|p| p.warning && indices.contains(&p.a) && indices.contains(&p.b))
            });
            let cleared = indices.first().is_some_and(|i| {
                st.report.as_ref().is_some_and(|r| {
                    r.rows.get(*i).is_some_and(|row| row.collides.is_empty())
                        && r.reviewed
                            .iter()
                            .any(|p| !p.collision && (p.a == *i || p.b == *i))
                })
            });
            for (n, index) in indices.iter().enumerate() {
                if let Some(p) = l.flat.get(*index) {
                    if let Ok(mesh) = st.lib.mesh(&p.name, ldraw::Lod::Low) {
                        let color = if n == 0 {
                            if contact_review {
                                egui::Color32::YELLOW
                            } else if cleared {
                                egui::Color32::LIGHT_BLUE
                            } else {
                                egui::Color32::LIGHT_RED
                            }
                        } else {
                            egui::Color32::from_rgb(255, 190, 40)
                        };
                        for edge in &mesh.lines {
                            let points = edge.map(|q| {
                                crate::mechanical::project(
                                    &l.cam,
                                    rect,
                                    p.pos + p.rot * q.as_dvec3(),
                                )
                            });
                            if let [Some(a), Some(b)] = points {
                                ui.painter_at(rect)
                                    .line_segment([a, b], egui::Stroke::new(2., color));
                            }
                        }
                    }
                }
            }
            let diagnosis = st.report.as_ref().and_then(|r| {
                r.reviewed
                    .iter()
                    .find(|p| indices.contains(&p.a) && indices.contains(&p.b))
            });
            if let Some(p) = diagnosis {
                for point in &p.points {
                    if let Some(q) =
                        crate::mechanical::project(&l.cam, rect, glam::DVec3::from_array(*point))
                    {
                        let paint = ui.painter_at(rect);
                        let stroke = egui::Stroke::new(
                            2.5,
                            if contact_review {
                                Color32::YELLOW
                            } else {
                                Color32::LIGHT_RED
                            },
                        );
                        paint.circle_stroke(q, 9., stroke);
                        paint.line_segment(
                            [q - egui::vec2(13., 0.), q + egui::vec2(13., 0.)],
                            stroke,
                        );
                        paint.line_segment(
                            [q - egui::vec2(0., 13.), q + egui::vec2(0., 13.)],
                            stroke,
                        );
                    }
                }
            }
            ui.painter_at(rect).text(
                rect.left_top() + egui::vec2(12., 12.),
                egui::Align2::LEFT_TOP,
                if contact_review {
                    "Checker X-ray · yellow contact review (not a draft blocker)"
                } else if cleared {
                    "Checker X-ray · blue reviewed connection (cleared)"
                } else {
                    "Checker X-ray · red issue / amber neighbour"
                },
                egui::FontId::proportional(14.),
                if contact_review {
                    egui::Color32::YELLOW
                } else if cleared {
                    egui::Color32::LIGHT_BLUE
                } else {
                    egui::Color32::LIGHT_RED
                },
            );
            if let Some(p) = diagnosis {
                if !p.reasons.is_empty() {
                    ui.painter_at(rect).text(
                        rect.left_top() + egui::vec2(12., 34.),
                        egui::Align2::LEFT_TOP,
                        format!("#{} ↔ #{}: {}", p.a + 1, p.b + 1, p.reasons.join("; ")),
                        egui::FontId::proportional(13.),
                        if contact_review {
                            Color32::YELLOW
                        } else {
                            Color32::LIGHT_RED
                        },
                    );
                }
            }
        }
    }
    let (gizmo_active, gizmo_command) = v.mechanical.overlay(
        ui,
        st.tool,
        &l.cam,
        rect,
        &resp,
        &st.snapshot,
        v.file,
        &st.lib,
    );
    if st.tool == crate::app::Tool::Lasso {
        if let Some(c) = v.lasso.update(
            ui,
            &resp,
            &l.cam,
            rect,
            l,
            &st.snapshot,
            v.mechanical.graph(),
        ) {
            mechanical_command = Some(c);
        }
    }
    if let Some(c) = gizmo_command {
        mechanical_command = Some(c);
    }
    if !gizmo_active {
        if let Some(id) = interaction.picked {
            if let Some(pointer) = ui.input(|i| i.pointer.interact_pos()) {
                mechanical_command =
                    v.mechanical
                        .picked(st.tool, id, pointer, &l.cam, rect, &st.snapshot);
            }
        }
    }
    painter.text(
        rect.right_bottom() - egui::vec2(12.0, 12.0),
        egui::Align2::RIGHT_BOTTOM,
        format!(
            "{} pieces{} · encode {:.2} ms",
            l.base_count,
            if l.base_count < l.flat.len() {
                format!(" + {} ghosts", l.flat.len() - l.base_count)
            } else {
                String::new()
            },
            v.encode_ms
        ),
        egui::FontId::proportional(13.0),
        ui.visuals().text_color(),
    );
    if !v.error.is_empty() {
        ui.label(&v.error);
    }
    if let Some(prepared) = interaction.prepared {
        let result = st.handle.call(move |e| e.commit_placement(prepared));
        st.value_result(result);
    } else if let Some((c, revision)) = mechanical_command
        .map(|c| (c, st.snapshot.revision))
        .or(interaction.command)
    {
        let result = st.handle.call(move |e| {
            if e.revision != revision {
                return Err("The build changed during placement; try again".into());
            }
            crate::interaction::commit(e, c)
        });
        st.value_result(result);
    }
}

/// Keep the point under the pointer on the camera-target plane fixed while zooming.
/// The same construction works for perspective and orthographic projections.
fn zoom_camera(
    cam: &mut Camera,
    rect: egui::Rect,
    pointer: egui::Pos2,
    scroll: f32,
    pinch: f32,
) -> bool {
    if !scroll.is_finite() || !pinch.is_finite() || pinch <= 0.0 || rect.height() <= 0.0 {
        return false;
    }
    let old = cam.dist;
    let new = (old * (-scroll * 0.004).clamp(-4.0, 4.0).exp() / pinch).clamp(1.0, 1e9);
    if (new - old).abs() <= f32::EPSILON * old {
        return false;
    }
    let forward = (cam.target - cam.eye()).normalize();
    let right = forward.cross(glam::Vec3::NEG_Y).normalize();
    let up = right.cross(forward);
    let scale = 2.0 * old * (cam.fov_y * 0.5).tan() / rect.height();
    let offset =
        (right * (pointer.x - rect.center().x) - up * (pointer.y - rect.center().y)) * scale;
    cam.target += offset * (1.0 - new / old);
    cam.dist = new;
    true
}

#[cfg(test)]
mod zoom_tests {
    use super::*;

    #[test]
    fn wheel_and_pinch_keep_pointer_anchor_in_both_projections() {
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 80.0), egui::vec2(800.0, 600.0));
        for ortho in [false, true] {
            for (scroll, pinch) in [(120.0, 1.0), (0.0, 1.5), (-90.0, 1.0), (0.0, 0.8)] {
                let mut cam = Camera::framing(
                    glam::Vec3::splat(-50.0),
                    glam::Vec3::splat(50.0),
                    rect.aspect_ratio(),
                );
                cam.ortho = ortho;
                let forward = (cam.target - cam.eye()).normalize();
                let right = forward.cross(glam::Vec3::NEG_Y).normalize();
                let up = right.cross(forward);
                let anchor = cam.target + right * 20.0 + up * 10.0;
                let before = cam.view_proj(rect.aspect_ratio()).project_point3(anchor);
                let pointer = egui::pos2(
                    rect.center().x + before.x * rect.width() * 0.5,
                    rect.center().y - before.y * rect.height() * 0.5,
                );
                let old = cam.dist;
                assert!(zoom_camera(&mut cam, rect, pointer, scroll, pinch));
                assert_eq!(cam.dist < old, scroll > 0.0 || pinch > 1.0);
                let after = cam.view_proj(rect.aspect_ratio()).project_point3(anchor);
                assert!((before.x - after.x).abs() < 1e-4);
                assert!((before.y - after.y).abs() < 1e-4);
            }
        }
    }

    #[test]
    fn no_input_and_invalid_pinch_do_not_move_camera() {
        let mut cam = Camera::framing(glam::Vec3::ZERO, glam::Vec3::splat(100.0), 1.0);
        let original = cam;
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(500.0, 500.0));
        for pinch in [1.0, 0.0, f32::NAN] {
            assert!(!zoom_camera(&mut cam, rect, rect.center(), 0.0, pinch));
            assert_eq!(cam.dist, original.dist);
            assert_eq!(cam.target, original.target);
        }
    }
}
