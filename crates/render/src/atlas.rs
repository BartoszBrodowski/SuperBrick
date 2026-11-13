//! A mesh atlas that only grows, for long-lived renderers.
//!
//! [`Scene::build`](crate::Scene::build) re-packs every mesh of a model into fresh vertex buffers.
//! [`SceneBuilder`] keeps the packed meshes between calls instead (keyed by canonical part name,
//! [`Lod`] and stud cut), so a sync after an edit only appends meshes it has not seen and rebuilds
//! the (cheap) per-part instance list. [`GpuAtlas`] mirrors it on the GPU in buffers that grow by
//! doubling, copying old contents on the device.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use glam::Vec3;
use ldraw::{Color, FlatPart, Library, Lod, MALE_STUDS, Mesh, canonical_name, mesh::recol};
use rayon::prelude::*;

use crate::gpu::Gpu;
use crate::scene::{
    body_keep, world_aabb, Atlas, Batch, CondVertex, Instance, MeshRange, Palette, PaletteEntry,
    PaletteTable, SceneOptions, SceneStats, StudMode, Vertex,
};

/// Which part of a mesh an atlas entry holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Cut {
    /// The whole mesh (studs baked in).
    Full,
    /// The mesh without its outer studs, which are drawn as instanced primitives.
    Body,
}

struct Entry {
    range: MeshRange,
    /// `None` when the library could not mesh the part.
    mesh: Option<Arc<Mesh>>,
    bbox: (Vec3, Vec3),
}

/// One synced frame: instances and draw batches over the builder's atlas.
#[derive(Clone, Debug, Default)]
pub struct SceneFrame {
    pub instances: Vec<Instance>,
    pub batches: Vec<Batch>,
    pub bounds: (Vec3, Vec3),
    pub edges: bool,
    pub stats: SceneStats,
}

impl SceneFrame {
    /// Set the flag bits of every instance (bodies and studs) of each listed flattened part.
    pub fn apply_flags(&mut self, flags: &[(u32, u32)]) {
        if flags.is_empty() {
            return;
        }
        let n = flags.iter().map(|&(p, _)| p as usize + 1).max().unwrap_or(0);
        let mut by_part = vec![0u32; n];
        for &(p, f) in flags {
            by_part[p as usize] = f;
        }
        for i in &mut self.instances {
            if let Some(&f) = by_part.get(i.id as usize) {
                i.flags = f;
            }
        }
    }
}

/// Vertices appended since the last upload, starting at `base` (tri, line, cond vertex counts).
pub struct AtlasDelta {
    pub base: [u32; 3],
    pub tri: Vec<Vertex>,
    pub line: Vec<Vertex>,
    pub cond: Vec<CondVertex>,
}

/// Vertex bytes past which [`SceneBuilder::sync`] drops the atlas and repacks only the current model.
pub const ATLAS_RESET_BYTES: u64 = 512 << 20;

/// CPU side of a growing atlas: which meshes are packed where, plus the shared colour palette.
#[derive(Default)]
pub struct SceneBuilder {
    /// Library the atlas was built from; a different library starts over.
    fingerprint: Option<u64>,
    parts: HashMap<(Lod, Cut), HashMap<String, u32>>,
    prims: HashMap<(u8, Lod), u32>,
    entries: Vec<Entry>,
    ranges: Vec<MeshRange>,
    palette: PaletteTable,
    /// Vertices not yet handed to the GPU.
    pending: Atlas,
}

impl SceneBuilder {
    pub fn new() -> SceneBuilder {
        SceneBuilder::default()
    }

    /// Forget every packed mesh (the next sync re-packs from offset 0).
    pub fn clear(&mut self) {
        *self = SceneBuilder::default();
    }

    /// Mesh ranges by atlas mesh index (what [`Batch::mesh`] indexes).
    pub fn meshes(&self) -> &[MeshRange] {
        &self.ranges
    }

    pub fn palette(&self) -> &[PaletteEntry] {
        &self.palette.entries
    }

    /// Total packed vertex counts (tri, line, cond), uploaded or not.
    pub fn vertex_counts(&self) -> [u32; 3] {
        self.pending.end()
    }

    /// Bytes of packed vertex data.
    pub fn atlas_bytes(&self) -> usize {
        let [t, l, c] = self.vertex_counts();
        (t as usize + l as usize) * size_of::<Vertex>() + c as usize * size_of::<CondVertex>()
    }

    /// Take the vertices appended since the last call, for upload.
    pub fn take_pending(&mut self) -> AtlasDelta {
        let end = self.pending.end();
        let p = std::mem::take(&mut self.pending);
        self.pending.base = end;
        AtlasDelta {
            base: p.base,
            tri: p.tri,
            line: p.line,
            cond: p.cond,
        }
    }

    fn reset_for(&mut self, lib: &Library) {
        let fp = lib.fingerprint();
        // The atlas only grows; a process that has seen very many models starts over.
        if self.fingerprint != Some(fp) || self.atlas_bytes() as u64 > ATLAS_RESET_BYTES {
            self.clear();
            self.fingerprint = Some(fp);
        }
    }

    fn add(&mut self, entry: Entry) -> u32 {
        self.ranges.push(entry.range.clone());
        self.entries.push(entry);
        self.entries.len() as u32 - 1
    }

    /// Atlas indices for `names` (canonical) at `lod`/`cut`, meshing and packing any not seen yet.
    fn ensure_parts(&mut self, lib: &Library, names: &[String], lod: Lod, cut: Cut) -> Vec<u32> {
        let known = self.parts.entry((lod, cut)).or_default();
        let fresh: Vec<&String> = names.iter().filter(|n| !known.contains_key(n.as_str())).collect();
        if !fresh.is_empty() {
            let meshes: Vec<Option<Arc<Mesh>>> =
                fresh.par_iter().map(|n| lib.mesh(n, lod).ok()).collect();
            for (n, m) in fresh.into_iter().zip(meshes) {
                let idx = self.pack(lib, m, cut);
                self.parts.get_mut(&(lod, cut)).unwrap().insert(n.clone(), idx);
            }
        }
        let known = &self.parts[&(lod, cut)];
        names.iter().map(|n| known[n.as_str()]).collect()
    }

    fn pack(&mut self, lib: &Library, m: Option<Arc<Mesh>>, cut: Cut) -> u32 {
        let mut pal = Palette { colors: &lib.colors, t: &mut self.palette };
        let (range, bbox) = match &m {
            Some(mesh) => {
                let keep = (cut == Cut::Body).then(|| body_keep(mesh));
                (self.pending.push(mesh, keep.as_ref(), &mut pal, true), mesh.bbox(false))
            }
            None => (MeshRange::default(), (Vec3::ZERO, Vec3::ZERO)),
        };
        self.add(Entry { range, mesh: m, bbox })
    }

    fn ensure_prim(&mut self, lib: &Library, prim: u8, lod: Lod) -> u32 {
        if let Some(&i) = self.prims.get(&(prim, lod)) {
            return i;
        }
        let m = lib.mesh(MALE_STUDS[prim as usize], lod).ok();
        let i = self.pack(lib, m, Cut::Full);
        self.prims.insert((prim, lod), i);
        i
    }

    /// Bring the atlas up to date for `flat` and lay out its instances and batches. Matches
    /// [`Scene::build`](crate::Scene::build) except that mesh indices are atlas-wide and edge
    /// geometry is always packed (`opts.edges` only sets [`SceneFrame::edges`]).
    pub fn sync(&mut self, lib: &Library, flat: &[FlatPart], opts: &SceneOptions) -> SceneFrame {
        let t_all = Instant::now();
        self.reset_for(lib);
        let mut stats = SceneStats { parts: flat.len(), ..Default::default() };
        let instanced = opts.studs != StudMode::Baked;
        let cut = if instanced { Cut::Body } else { Cut::Full };

        // Unique names in first-seen order, then their atlas entries.
        let mut index: HashMap<String, u32> = HashMap::new();
        let mut names: Vec<String> = Vec::new();
        let mut raw: HashMap<&str, u32> = HashMap::new();
        let part_unique: Vec<u32> = flat
            .iter()
            .map(|p| {
                if let Some(&u) = raw.get(p.name.as_str()) {
                    return u;
                }
                let n = canonical_name(&p.name);
                let u = *index.entry(n.clone()).or_insert_with(|| {
                    names.push(n);
                    names.len() as u32 - 1
                });
                raw.insert(p.name.as_str(), u);
                u
            })
            .collect();
        let t = Instant::now();
        let mesh_of_unique = self.ensure_parts(lib, &names, opts.lod, cut);
        stats.unique_parts = names.len();
        stats.missing = names
            .iter()
            .zip(&mesh_of_unique)
            .filter(|(_, m)| self.entries[**m as usize].mesh.is_none())
            .map(|(n, _)| n.clone())
            .collect();

        // Stud primitives used by any part here.
        let mut prim_mesh = [u32::MAX; 64];
        if instanced {
            for &m in &mesh_of_unique {
                let Some(mesh) = self.entries[m as usize].mesh.clone() else {
                    continue;
                };
                for s in mesh.studs.iter().filter(|s| s.outer) {
                    if prim_mesh[s.prim as usize] == u32::MAX {
                        let i = self.ensure_prim(lib, s.prim, opts.stud_lod);
                        prim_mesh[s.prim as usize] = if self.entries[i as usize].mesh.is_some() { i } else { u32::MAX - 1 };
                    }
                }
            }
        }
        stats.mesh_ms = t.elapsed().as_secs_f64() * 1e3;

        // Part instances, then stud instances, bucketed by (mesh, transparent).
        let mut pal = Palette { colors: &lib.colors, t: &mut self.palette };
        let mut buckets: HashMap<(u32, bool), Vec<Instance>> = HashMap::new();
        let mut studs: Vec<(u32, bool, Instance)> = Vec::new();
        let mut lo = Vec3::splat(f32::INFINITY);
        let mut hi = Vec3::splat(f32::NEG_INFINITY);
        for (i, p) in flat.iter().enumerate() {
            let u = mesh_of_unique[part_unique[i] as usize];
            let entry = &self.entries[u as usize];
            let Some(mesh) = &entry.mesh else {
                continue;
            };
            stats.drawn_parts += 1;
            let color = if p.color == Color::MAIN { opts.default_color } else { p.color };
            let slot = pal.slot(color);
            let transparent = pal.transparent(slot);
            let (m, t) = (p.rot.as_mat3(), p.pos.as_vec3());
            buckets.entry((u, transparent)).or_default().push(Instance::new(m, t, slot, i as u32));
            let (wa, wb) = world_aabb(m, t, entry.bbox.0, entry.bbox.1);
            lo = lo.min(wa);
            hi = hi.max(wb);
            if instanced {
                for s in mesh.studs.iter().filter(|s| s.outer) {
                    let pm = prim_mesh[s.prim as usize];
                    if pm >= u32::MAX - 1 {
                        continue;
                    }
                    let sslot = pal.slot(recol(s.color, color));
                    let tr = pal.transparent(sslot);
                    studs.push((pm, tr, Instance::new(m * s.mat, m * s.pos + t, sslot, i as u32)));
                }
            }
        }
        stats.stud_instances = studs.len();
        for (pm, tr, inst) in studs {
            buckets.entry((pm, tr)).or_default().push(inst);
        }

        // Batches: opaque first, then transparent, each sorted by mesh.
        let mut keys: Vec<(u32, bool)> = buckets.keys().copied().collect();
        keys.sort_by_key(|&(m, tr)| (tr, m));
        let total: usize = buckets.values().map(Vec::len).sum();
        let mut instances = Vec::with_capacity(total);
        let mut batches = Vec::with_capacity(keys.len());
        for k in keys {
            let start = instances.len() as u32;
            instances.extend_from_slice(&buckets[&k]);
            batches.push(Batch { mesh: k.0, instances: start..instances.len() as u32, transparent: k.1 });
        }
        for b in &batches {
            let r = &self.ranges[b.mesh as usize];
            let n = (b.instances.end - b.instances.start) as u64;
            stats.tris += n * (r.tris.len() as u64 / 3);
            if opts.edges {
                stats.lines += n * (r.lines.len() as u64 / 2);
                stats.cond += n * (r.cond.len() as u64 / 2);
            }
            stats.draw_calls += [!r.tris.is_empty(), opts.edges && !r.lines.is_empty(), opts.edges && !r.cond.is_empty()]
                .iter()
                .filter(|&&x| x)
                .count();
        }
        stats.atlas_bytes = self.atlas_bytes();
        stats.instance_bytes = instances.len() * size_of::<Instance>();
        stats.build_ms = t_all.elapsed().as_secs_f64() * 1e3;
        if lo.x > hi.x {
            (lo, hi) = (Vec3::splat(-10.0), Vec3::splat(10.0));
        }
        SceneFrame { instances, batches, bounds: (lo, hi), edges: opts.edges, stats }
    }
}

/// A device buffer that grows by doubling.
pub(crate) struct GrowBuffer {
    pub buf: wgpu::Buffer,
    label: &'static str,
    usage: wgpu::BufferUsages,
}

impl GrowBuffer {
    pub fn new(gpu: &Gpu, label: &'static str, usage: wgpu::BufferUsages, size: u64) -> GrowBuffer {
        let usage = usage | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC;
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: size.max(256).next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT),
            usage,
            mapped_at_creation: false,
        });
        GrowBuffer { buf, label, usage }
    }

    /// Make room for `need` bytes, keeping the first `keep` bytes. Returns true if the buffer was
    /// replaced (bind groups over it must be remade).
    pub fn reserve(&mut self, gpu: &Gpu, need: u64, keep: u64) -> bool {
        if need <= self.buf.size() {
            return false;
        }
        let max = gpu.device.limits().max_buffer_size;
        let size = need.max((self.buf.size() * 2).min(max)).next_multiple_of(256);
        let buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(self.label),
            size,
            usage: self.usage,
            mapped_at_creation: false,
        });
        let keep = keep.min(self.buf.size()) / wgpu::COPY_BUFFER_ALIGNMENT * wgpu::COPY_BUFFER_ALIGNMENT;
        if keep > 0 {
            let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("atlas grow") });
            enc.copy_buffer_to_buffer(&self.buf, 0, &buf, 0, keep);
            gpu.queue.submit([enc.finish()]);
        }
        self.buf = buf;
        true
    }

    /// Write `data` at byte `offset`, growing (and keeping everything before `offset`) if needed.
    pub fn write<T: bytemuck::Pod>(&mut self, gpu: &Gpu, offset: u64, data: &[T]) -> bool {
        let bytes: &[u8] = bytemuck::cast_slice(data);
        let grew = self.reserve(gpu, offset + bytes.len() as u64, offset);
        if !bytes.is_empty() {
            gpu.queue.write_buffer(&self.buf, offset, bytes);
        }
        grew
    }
}

/// GPU mirror of a [`SceneBuilder`] atlas, plus the per-frame instance and palette buffers.
pub(crate) struct GpuAtlas {
    pub tri: GrowBuffer,
    pub line: GrowBuffer,
    pub cond: GrowBuffer,
    pub inst: GrowBuffer,
    pub palette: GrowBuffer,
    /// Bumped whenever a buffer a bind group refers to (the palette) is replaced.
    pub generation: u64,
}

impl GpuAtlas {
    pub fn new(gpu: &Gpu) -> GpuAtlas {
        let vb = wgpu::BufferUsages::VERTEX;
        GpuAtlas {
            tri: GrowBuffer::new(gpu, "atlas tris", vb, 1 << 20),
            line: GrowBuffer::new(gpu, "atlas lines", vb, 1 << 20),
            cond: GrowBuffer::new(gpu, "atlas cond", vb, 1 << 20),
            inst: GrowBuffer::new(gpu, "instances", vb, 1 << 16),
            palette: GrowBuffer::new(gpu, "palette", wgpu::BufferUsages::STORAGE, 4096),
            generation: 0,
        }
    }

    /// Upload new atlas vertices, the palette and `frame`'s instances.
    pub fn upload(&mut self, gpu: &Gpu, builder: &mut SceneBuilder, frame: &SceneFrame) {
        let d = builder.take_pending();
        let vs = size_of::<Vertex>() as u64;
        let cs = size_of::<CondVertex>() as u64;
        self.tri.write(gpu, d.base[0] as u64 * vs, &d.tri);
        self.line.write(gpu, d.base[1] as u64 * vs, &d.line);
        self.cond.write(gpu, d.base[2] as u64 * cs, &d.cond);
        let pal = builder.palette();
        let zero = [<PaletteEntry as bytemuck::Zeroable>::zeroed()];
        let pal: &[PaletteEntry] = if pal.is_empty() { &zero } else { pal };
        if self.palette.write(gpu, 0, pal) {
            self.generation += 1;
        }
        self.inst.reserve(gpu, (frame.instances.len() * size_of::<Instance>()) as u64, 0);
        if !frame.instances.is_empty() {
            gpu.queue.write_buffer(&self.inst.buf, 0, bytemuck::cast_slice(&frame.instances));
        }
    }
}
