//! CPU side of the renderer. A flattened model becomes one vertex atlas (each unique part body once,
//! plus each stud primitive once) and per-instance transforms grouped into draw batches.
//!
//! Stud modes:
//! - `Baked`: every part mesh keeps its own stud triangles (the simple way, the baseline).
//! - `Instanced`: stud subtrees are cut out of part bodies and drawn as instances of the primitive.
//! - `Culled`: compatibility name for `Instanced`. The old bounding-box coverage
//!   approximation is disabled: a panel/hinge's bounding box contains empty space
//!   and must not erase exposed studs. Real connector-proven culling is future work.

use std::{collections::HashMap, ops::Range, sync::Arc, time::Instant};

use glam::{Mat3, Mat4, Vec3};
use ldraw::{Color, Colors, FlatPart, Library, Lod, MALE_STUDS, Mesh, canonical_name, mesh::recol};
use rayon::prelude::*;

/// Vertex colour codes: the instance's colour, its edge colour, or (high bit) a palette slot's edge colour.
pub const COL_MAIN: u32 = 0xFFFF_FFFF;
pub const COL_EDGE: u32 = 0xFFFF_FFFE;
pub const COL_EDGE_OF: u32 = 0x8000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudMode {
    Baked,
    Instanced,
    Culled,
}

impl StudMode {
    pub const ALL: [StudMode; 3] = [StudMode::Baked, StudMode::Instanced, StudMode::Culled];

    pub fn name(self) -> &'static str {
        match self {
            StudMode::Baked => "baked",
            StudMode::Instanced => "instanced",
            StudMode::Culled => "culled",
        }
    }

    pub fn parse(s: &str) -> Option<StudMode> {
        StudMode::ALL.into_iter().find(|m| m.name() == s)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SceneOptions {
    pub lod: Lod,
    pub stud_lod: Lod,
    pub studs: StudMode,
    pub edges: bool,
    /// Colour for parts placed in colour 16 at the top level.
    pub default_color: Color,
}

impl Default for SceneOptions {
    fn default() -> Self {
        SceneOptions { lod: Lod::Std, stud_lod: Lod::Std, studs: StudMode::Culled, edges: true, default_color: Color(71) }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub col: u32,
}

/// One end of a conditional line, with the other end and both control points.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CondVertex {
    pub pos: [f32; 3],
    pub col: u32,
    pub other: [f32; 3],
    pub c1: [f32; 3],
    pub c2: [f32; 3],
}

/// Per-instance data: the 3x3 columns, translation, palette slot and pick id (64 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Instance {
    pub c0: [f32; 3],
    pub c1: [f32; 3],
    pub c2: [f32; 3],
    pub t: [f32; 3],
    pub color: u32,
    pub id: u32,
    /// Bit 0: selected (tinted). Bit 1: hidden.
    pub flags: u32,
    pub _pad: u32,
}

impl Instance {
    pub fn new(m: Mat3, t: Vec3, color: u32, id: u32) -> Instance {
        Instance {
            c0: m.x_axis.to_array(),
            c1: m.y_axis.to_array(),
            c2: m.z_axis.to_array(),
            t: t.to_array(),
            color,
            id,
            flags: 0,
            _pad: 0,
        }
    }
}

/// Linear RGBA for a colour and its edge colour.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PaletteEntry {
    pub main: [f32; 4],
    pub edge: [f32; 4],
}

/// Where one mesh's vertices sit in the three atlases.
#[derive(Clone, Debug, Default)]
pub struct MeshRange {
    pub tris: Range<u32>,
    pub lines: Range<u32>,
    pub cond: Range<u32>,
}

/// One draw: a mesh for a contiguous run of instances.
#[derive(Clone, Debug)]
pub struct Batch {
    pub mesh: u32,
    pub instances: Range<u32>,
    pub transparent: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SceneStats {
    pub parts: usize,
    pub drawn_parts: usize,
    pub unique_parts: usize,
    pub missing: Vec<String>,
    pub stud_instances: usize,
    pub studs_culled: usize,
    pub tris: u64,
    pub lines: u64,
    pub cond: u64,
    pub draw_calls: usize,
    pub atlas_bytes: usize,
    pub instance_bytes: usize,
    pub mesh_ms: f64,
    pub cull_ms: f64,
    pub build_ms: f64,
}

pub struct Scene {
    pub tri_verts: Vec<Vertex>,
    pub line_verts: Vec<Vertex>,
    pub cond_verts: Vec<CondVertex>,
    pub meshes: Vec<MeshRange>,
    pub instances: Vec<Instance>,
    pub batches: Vec<Batch>,
    pub palette: Vec<PaletteEntry>,
    pub bounds: (Vec3, Vec3),
    pub edges: bool,
    pub stats: SceneStats,
}

fn srgb_to_linear(v: u8) -> f32 {
    let c = v as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Palette slots handed out so far. Only grows, so slot numbers baked into atlas vertices stay valid.
#[derive(Clone, Default)]
pub(crate) struct PaletteTable {
    slots: HashMap<i32, u32>,
    pub(crate) entries: Vec<PaletteEntry>,
}

/// A [`PaletteTable`] plus the colour definitions that fill new slots.
pub(crate) struct Palette<'a> {
    pub(crate) colors: &'a Colors,
    pub(crate) t: &'a mut PaletteTable,
}

impl Palette<'_> {
    pub(crate) fn slot(&mut self, c: Color) -> u32 {
        if let Some(&s) = self.t.slots.get(&c.0) {
            return s;
        }
        let rgba = self.colors.display_rgba(c);
        let edge = self.colors.edge_rgb(c.0);
        let e = PaletteEntry {
            main: [srgb_to_linear(rgba[0]), srgb_to_linear(rgba[1]), srgb_to_linear(rgba[2]), rgba[3] as f32 / 255.0],
            edge: [srgb_to_linear(edge[0]), srgb_to_linear(edge[1]), srgb_to_linear(edge[2]), 1.0],
        };
        let s = self.t.entries.len() as u32;
        self.t.entries.push(e);
        self.t.slots.insert(c.0, s);
        s
    }

    pub(crate) fn code(&mut self, c: Color) -> u32 {
        if c == Color::MAIN {
            COL_MAIN
        } else if c == Color::EDGE {
            COL_EDGE
        } else if let Some(src) = c.edge_source() {
            self.slot(src) | COL_EDGE_OF
        } else {
            self.slot(c)
        }
    }

    pub(crate) fn transparent(&self, slot: u32) -> bool {
        self.t.entries[slot as usize].main[3] < 0.999
    }
}

/// Vertex data for a run of meshes. `base` counts vertices that precede these (already uploaded by a
/// growing atlas), so ranges are absolute.
#[derive(Default)]
pub(crate) struct Atlas {
    pub(crate) base: [u32; 3],
    pub(crate) tri: Vec<Vertex>,
    pub(crate) line: Vec<Vertex>,
    pub(crate) cond: Vec<CondVertex>,
}

impl Atlas {
    /// Total vertex counts (tri, line, cond) including `base`.
    pub(crate) fn end(&self) -> [u32; 3] {
        [
            self.base[0] + self.tri.len() as u32,
            self.base[1] + self.line.len() as u32,
            self.base[2] + self.cond.len() as u32,
        ]
    }

    /// Append a mesh, skipping triangles/lines whose `keep` flag is false.
    pub(crate) fn push(&mut self, m: &Mesh, keep: Option<&Keep>, pal: &mut Palette, edges: bool) -> MeshRange {
        let t0 = self.end()[0];
        for (i, tri) in m.tris.iter().enumerate() {
            if keep.is_some_and(|k| !k.tris[i]) {
                continue;
            }
            let col = pal.code(m.tcol[i]);
            self.tri.extend(tri.iter().map(|v| Vertex { pos: v.to_array(), col }));
        }
        let [_, l0, c0] = self.end();
        if edges {
            for (i, l) in m.lines.iter().enumerate() {
                if keep.is_some_and(|k| !k.lines[i]) {
                    continue;
                }
                let col = pal.code(m.lcol[i]);
                self.line.extend(l.iter().map(|v| Vertex { pos: v.to_array(), col }));
            }
            for (i, c) in m.cond.iter().enumerate() {
                if keep.is_some_and(|k| !k.cond[i]) {
                    continue;
                }
                let col = pal.code(m.ccol[i]);
                let [p1, p2, a, b] = c.map(|v| v.to_array());
                self.cond.push(CondVertex { pos: p1, col, other: p2, c1: a, c2: b });
                self.cond.push(CondVertex { pos: p2, col, other: p1, c1: a, c2: b });
            }
        }
        let [t1, l1, c1] = self.end();
        MeshRange { tris: t0..t1, lines: l0..l1, cond: c0..c1 }
    }
}

/// Which triangles, lines and conditional lines of a part mesh are body (not inside an outer stud).
pub(crate) struct Keep {
    tris: Vec<bool>,
    lines: Vec<bool>,
    cond: Vec<bool>,
}

pub(crate) fn body_keep(m: &Mesh) -> Keep {
    let mut k = Keep { tris: vec![true; m.tris.len()], lines: vec![true; m.lines.len()], cond: vec![true; m.cond.len()] };
    for s in m.studs.iter().filter(|s| s.outer) {
        let r = |start: u32, count: u32| start as usize..(start + count) as usize;
        k.tris[r(s.tri_start, s.tri_count)].fill(false);
        k.lines[r(s.line_start, s.line_count)].fill(false);
        k.cond[r(s.cond_start, s.cond_count)].fill(false);
    }
    k
}

pub(crate) fn world_aabb(m: Mat3, t: Vec3, lo: Vec3, hi: Vec3) -> (Vec3, Vec3) {
    let mut a = Vec3::splat(f32::INFINITY);
    let mut b = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8 {
        let c = Vec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let w = m * c + t;
        a = a.min(w);
        b = b.max(w);
    }
    (a, b)
}

struct StudCand {
    mesh: u32,
    m: Mat3,
    t: Vec3,
    slot: u32,
    part: u32,
}

impl Scene {
    pub fn build(lib: &Library, flat: &[FlatPart], opts: &SceneOptions) -> Scene {
        let t_all = Instant::now();
        let mut stats = SceneStats { parts: flat.len(), ..Default::default() };

        // Unique parts, meshed in parallel (the library caches meshes, so later builds are cheap).
        let mut index: HashMap<String, u32> = HashMap::new();
        let mut names: Vec<String> = Vec::new();
        let part_mesh: Vec<u32> = flat
            .iter()
            .map(|p| {
                let n = canonical_name(&p.name);
                *index.entry(n.clone()).or_insert_with(|| {
                    names.push(n);
                    names.len() as u32 - 1
                })
            })
            .collect();
        let t = Instant::now();
        let meshes: Vec<Option<Arc<Mesh>>> = names.par_iter().map(|n| lib.mesh(n, opts.lod).ok()).collect();
        let instanced = opts.studs != StudMode::Baked;
        let mut prims: Vec<u8> = Vec::new();
        if instanced {
            for m in meshes.iter().flatten() {
                for s in m.studs.iter().filter(|s| s.outer) {
                    if !prims.contains(&s.prim) {
                        prims.push(s.prim);
                    }
                }
            }
        }
        let prim_meshes: Vec<Option<Arc<Mesh>>> =
            prims.par_iter().map(|&p| lib.mesh(MALE_STUDS[p as usize], opts.stud_lod).ok()).collect();
        stats.mesh_ms = t.elapsed().as_secs_f64() * 1e3;
        stats.unique_parts = names.len();
        stats.missing = names.iter().zip(&meshes).filter(|(_, m)| m.is_none()).map(|(n, _)| n.clone()).collect();

        // Atlas: part bodies, then stud primitives.
        let mut table = PaletteTable::default();
        let mut pal = Palette { colors: &lib.colors, t: &mut table };
        let mut atlas = Atlas::default();
        let mut ranges: Vec<MeshRange> = Vec::new();
        for m in &meshes {
            ranges.push(match m {
                Some(m) if instanced => atlas.push(m, Some(&body_keep(m)), &mut pal, opts.edges),
                Some(m) => atlas.push(m, None, &mut pal, opts.edges),
                None => MeshRange::default(),
            });
        }
        let prim_base = ranges.len() as u32;
        let mut prim_slot = [u32::MAX; 64];
        for (i, (p, m)) in prims.iter().zip(&prim_meshes).enumerate() {
            if let Some(m) = m {
                prim_slot[*p as usize] = prim_base + i as u32;
                ranges.push(atlas.push(m, None, &mut pal, opts.edges));
            } else {
                ranges.push(MeshRange::default());
            }
        }

        // Part instances, and stud candidates.
        let mut buckets: HashMap<(u32, bool), Vec<Instance>> = HashMap::new();
        let mut cands: Vec<StudCand> = Vec::new();
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for (i, p) in flat.iter().enumerate() {
            let u = part_mesh[i];
            let Some(mesh) = &meshes[u as usize] else {
                continue;
            };
            stats.drawn_parts += 1;
            let color = if p.color == Color::MAIN { opts.default_color } else { p.color };
            let slot = pal.slot(color);
            let transparent = pal.transparent(slot);
            let (m, t) = (p.rot.as_mat3(), p.pos.as_vec3());
            buckets.entry((u, transparent)).or_default().push(Instance::new(m, t, slot, i as u32));
            let (a, b) = mesh.bbox(false);
            let (wa, wb) = world_aabb(m, t, a, b);
            lo = lo.min(wa);
            hi = hi.max(wb);
            if instanced {
                for s in mesh.studs.iter().filter(|s| s.outer) {
                    let pm = prim_slot[s.prim as usize];
                    if pm == u32::MAX {
                        continue;
                    }
                    let (sm, st) = (m * s.mat, m * s.pos + t);
                    cands.push(StudCand { mesh: pm, m: sm, t: st, slot: pal.slot(recol(s.color, color)), part: i as u32 });
                }
            }
        }

        // Do not infer stud visibility from a point inside another part's box.
        // Keep geometry and let depth testing hide genuinely occluded studs.
        for c in &cands {
                let transparent = pal.transparent(c.slot);
                buckets.entry((c.mesh, transparent)).or_default().push(Instance::new(c.m, c.t, c.slot, c.part));
                stats.stud_instances += 1;
        }

        // Batches: opaque first, then transparent, each sorted by mesh.
        let mut keys: Vec<(u32, bool)> = buckets.keys().copied().collect();
        keys.sort_by_key(|&(m, tr)| (tr, m));
        let mut instances = Vec::new();
        let mut batches = Vec::new();
        for k in keys {
            let list = &buckets[&k];
            let start = instances.len() as u32;
            instances.extend_from_slice(list);
            batches.push(Batch { mesh: k.0, instances: start..instances.len() as u32, transparent: k.1 });
        }
        for b in &batches {
            let r = &ranges[b.mesh as usize];
            let n = (b.instances.end - b.instances.start) as u64;
            stats.tris += n * (r.tris.len() as u64 / 3);
            stats.lines += n * (r.lines.len() as u64 / 2);
            stats.cond += n * (r.cond.len() as u64 / 2);
            stats.draw_calls += [!r.tris.is_empty(), !r.lines.is_empty(), !r.cond.is_empty()].iter().filter(|&&x| x).count();
        }
        stats.atlas_bytes = atlas.tri.len() * size_of::<Vertex>()
            + atlas.line.len() * size_of::<Vertex>()
            + atlas.cond.len() * size_of::<CondVertex>();
        stats.instance_bytes = instances.len() * size_of::<Instance>();
        stats.build_ms = t_all.elapsed().as_secs_f64() * 1e3;
        if lo.x > hi.x {
            (lo, hi) = (Vec3::splat(-10.0), Vec3::splat(10.0));
        }
        Scene {
            tri_verts: atlas.tri,
            line_verts: atlas.line,
            cond_verts: atlas.cond,
            meshes: ranges,
            instances,
            batches,
            palette: table.entries,
            bounds: (lo, hi),
            edges: opts.edges,
            stats,
        }
    }
}

/// Orbit camera around a target. LDraw is -Y up; yaw 0 looks at the model's front (from -Z).
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub dist: f32,
    pub fov_y: f32,
    /// Radius of the scene, for near/far planes.
    pub radius: f32,
    /// Orthographic projection; `dist` then sets the zoom (half-height = dist·tan(fov/2)).
    pub ortho: bool,
}

impl Camera {
    /// A three-quarter view that fits the box.
    pub fn framing(lo: Vec3, hi: Vec3, aspect: f32) -> Camera {
        let target = (lo + hi) * 0.5;
        let radius = ((hi - lo).length() * 0.5).max(1.0);
        let fov_y = 30f32.to_radians();
        let fov_x = 2.0 * ((fov_y * 0.5).tan() * aspect).atan();
        let dist = radius / (fov_y.min(fov_x) * 0.5).sin() * 1.02;
        Camera { target, yaw: 35f32.to_radians(), pitch: 28f32.to_radians(), dist, fov_y, radius, ortho: false }
    }

    /// Named view directions as (yaw, pitch) in radians: `front` looks from -Z, `right` from +X,
    /// `top` from above (-Y, with the model's front at the bottom of the image), `iso` is the
    /// three-quarter view of [`Camera::framing`].
    pub fn preset(name: &str) -> Option<(f32, f32)> {
        let (yaw, pitch): (f32, f32) = match name.to_ascii_lowercase().as_str() {
            "front" => (0.0, 0.0),
            "back" => (180.0, 0.0),
            "right" => (90.0, 0.0),
            "left" => (-90.0, 0.0),
            "top" => (0.0, 90.0),
            "bottom" => (0.0, -90.0),
            "iso" => (35.0, 28.0),
            _ => return None,
        };
        Some((yaw.to_radians(), pitch.to_radians()))
    }

    /// Names accepted by [`Camera::preset`].
    pub const PRESETS: [&'static str; 7] = ["front", "back", "left", "right", "top", "bottom", "iso"];

    /// A camera looking from `yaw`/`pitch` that tightly fits the box `lo..hi` (for example a few
    /// parts) while keeping near/far planes wide enough for a scene of radius `scene_radius`
    /// around it. The (perspective) camera backs off until every corner is inside the frustum; set
    /// `ortho` afterwards for a parallel view that still contains the box.
    pub fn framing_subset(lo: Vec3, hi: Vec3, aspect: f32, yaw: f32, pitch: f32, scene_radius: f32) -> Camera {
        let target = (lo + hi) * 0.5;
        let fov_y = 30f32.to_radians();
        let mut cam = Camera { target, yaw, pitch, dist: 1.0, fov_y, radius: 1.0, ortho: false };
        let (right, up, back) = cam.basis();
        let ty = (fov_y * 0.5).tan();
        let tx = ty * aspect.max(1e-3);
        let mut dist: f32 = 0.0;
        for i in 0..8 {
            let c = Vec3::new(
                if i & 1 == 0 { lo.x } else { hi.x },
                if i & 2 == 0 { lo.y } else { hi.y },
                if i & 4 == 0 { lo.z } else { hi.z },
            ) - target;
            let (x, y, z) = (c.dot(right).abs(), c.dot(up).abs(), c.dot(back));
            dist = dist.max(z + x / tx).max(z + y / ty);
        }
        let half = ((hi - lo).length() * 0.5).max(1.0);
        cam.dist = (dist * 1.06).max(half * 1.2).max(1.0);
        cam.radius = scene_radius.max(half).max(1.0);
        cam
    }

    /// Camera-space axes (right, up, towards the eye) in world coordinates.
    pub fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        let back = Vec3::new(cp * sy, -sp, -cp * cy);
        let up = self.up();
        let right = up.cross(back).normalize_or(Vec3::X);
        (right, back.cross(right), back)
    }

    /// Screen-up direction: -Y, except straight above or below the target, where it is the
    /// direction the view tilts towards (so top/bottom views stay defined).
    fn up(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        if cp.abs() > 1e-4 {
            return Vec3::NEG_Y;
        }
        let (sy, cy) = self.yaw.sin_cos();
        Vec3::new(-sp * sy, -cp, sp * cy).normalize_or(Vec3::Z)
    }

    pub fn eye(&self) -> Vec3 {
        let (sp, cp) = self.pitch.sin_cos();
        let (sy, cy) = self.yaw.sin_cos();
        self.target + self.dist * Vec3::new(cp * sy, -sp, -cp * cy)
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        if self.ortho {
            // Stand well back so the whole scene is in front of the near plane however far we zoom.
            let back = self.radius * 2.5 + self.dist + 10.0;
            let dir = (self.eye() - self.target).normalize_or(Vec3::NEG_Z);
            let view = glam::camera::rh::view::look_at_mat4(self.target + dir * back, self.target, self.up());
            let h = self.dist * (self.fov_y * 0.5).tan();
            let w = h * aspect;
            return glam::camera::rh::proj::directx::orthographic(-w, w, -h, h, 1.0, back + self.radius * 2.5 + 10.0) * view;
        }
        let view = glam::camera::rh::view::look_at_mat4(self.eye(), self.target, self.up());
        let near = (self.dist - self.radius * 1.2).max(self.dist * 0.02).max(0.5);
        let far = self.dist + self.radius * 1.2;
        glam::camera::rh::proj::directx::perspective(self.fov_y, aspect, near, far) * view
    }
}
