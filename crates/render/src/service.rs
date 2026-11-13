//! A warm, reusable headless renderer for agents and tools that need many offscreen images.
//!
//! [`RenderService`] is made once per process. It keeps the GPU device, compiled pipelines, a
//! growing mesh atlas ([`SceneBuilder`] + its GPU mirror), a uniform buffer with one 256-byte slot
//! per camera, and a small pool of render targets with their readback buffers. A call syncs the
//! atlas (packing only meshes it has not seen), draws every [`Panel`] into one composite target in a
//! single render pass, resolves once and reads back once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use glam::{DMat3, DVec3, Vec3};
use ldraw::{Color, FlatPart, Library};

use crate::atlas::{GpuAtlas, GrowBuffer, SceneBuilder, SceneFrame};
use crate::gpu::{
    BACKGROUND, COLOR_FORMAT, DEPTH_FORMAT, DrawList, GLOBALS_STRIDE, Gpu, PICK_FORMAT, Pipelines, draw_list,
    globals_layout, view_globals,
};
use crate::scene::{Camera, SceneOptions, SceneStats};

/// A pixel rectangle (top-left origin).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    /// The whole of a `size` image.
    pub fn full(size: (u32, u32)) -> Rect {
        Rect { x: 0, y: 0, w: size.0, h: size.1 }
    }

    pub fn aspect(&self) -> f32 {
        self.w as f32 / self.h.max(1) as f32
    }

    /// `cols` × `rows` cells covering `size`, row by row, separated by `gap` pixels.
    pub fn grid(size: (u32, u32), cols: u32, rows: u32, gap: u32) -> Vec<Rect> {
        let (cols, rows) = (cols.max(1), rows.max(1));
        let w = size.0.saturating_sub(gap * (cols - 1)) / cols;
        let h = size.1.saturating_sub(gap * (rows - 1)) / rows;
        let mut out = Vec::with_capacity((cols * rows) as usize);
        for r in 0..rows {
            for c in 0..cols {
                out.push(Rect { x: c * (w + gap), y: r * (h + gap), w, h });
            }
        }
        out
    }

    fn clamp(self, size: (u32, u32)) -> Rect {
        let x = self.x.min(size.0);
        let y = self.y.min(size.1);
        Rect { x, y, w: self.w.min(size.0 - x), h: self.h.min(size.1 - y) }
    }
}

/// One camera drawn into one rectangle of the composite image. Panels must not overlap (they share
/// one depth buffer).
#[derive(Clone, Copy, Debug)]
pub struct Panel {
    pub camera: Camera,
    pub rect: Rect,
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub scene: SceneOptions,
    /// MSAA sample count (1 or 4).
    pub samples: u32,
    pub background: wgpu::Color,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { scene: SceneOptions::default(), samples: 4, background: BACKGROUND }
    }
}

/// Tightly packed 8-bit sRGB RGBA pixels, top row first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        self.pixels[i..i + 4].try_into().unwrap()
    }

    /// A copy of one rectangle (clamped to the image).
    pub fn crop(&self, r: Rect) -> RgbaImage {
        let r = r.clamp((self.width, self.height));
        let mut pixels = Vec::with_capacity((r.w * r.h * 4) as usize);
        for y in r.y..r.y + r.h {
            let i = ((y * self.width + r.x) * 4) as usize;
            pixels.extend_from_slice(&self.pixels[i..i + (r.w * 4) as usize]);
        }
        RgbaImage { width: r.w, height: r.h, pixels }
    }

    /// Pixels without alpha.
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() / 4 * 3);
        for p in self.pixels.chunks_exact(4) {
            out.extend_from_slice(&p[..3]);
        }
        out
    }
}

/// A PNG of `img` tuned for speed: RGB (renders are opaque), fast deflate.
pub fn encode_png_fast(img: &RgbaImage) -> Vec<u8> {
    let rgb = img.to_rgb();
    let mut out = Vec::with_capacity(rgb.len() / 4);
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut w = enc.write_header().expect("png header into a Vec");
        w.write_image_data(&rgb).expect("png data into a Vec");
    }
    out
}

/// Where the last call's time went, in milliseconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderTimings {
    /// Atlas sync and instance layout on the CPU.
    pub sync_ms: f64,
    /// Buffer uploads, pipeline/target lookup and command recording.
    pub upload_ms: f64,
    /// Submit until the GPU finished drawing and copying into the readback buffer.
    pub render_ms: f64,
    /// Mapping the readback buffer and unpadding rows.
    pub readback_ms: f64,
    pub total_ms: f64,
}

/// A pooled composite target and its readback buffer.
struct Target {
    size: (u32, u32),
    samples: u32,
    msaa: Option<wgpu::TextureView>,
    resolve: wgpu::Texture,
    resolve_view: wgpu::TextureView,
    depth: wgpu::TextureView,
    readback: wgpu::Buffer,
    padded_row: u32,
    used: u64,
}

impl Target {
    fn new(gpu: &Gpu, size: (u32, u32), samples: u32) -> Target {
        let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
        let tex = |label: &str, format, samples, usage| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let ra = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let resolve = tex("service color", COLOR_FORMAT, 1, ra | wgpu::TextureUsages::COPY_SRC);
        let msaa = (samples > 1)
            .then(|| tex("service msaa", COLOR_FORMAT, samples, ra).create_view(&Default::default()));
        let depth = tex("service depth", DEPTH_FORMAT, samples, ra).create_view(&Default::default());
        let padded_row = (size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("service readback"),
            size: padded_row as u64 * size.1 as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Target {
            size,
            samples,
            msaa,
            resolve_view: resolve.create_view(&Default::default()),
            resolve,
            depth,
            readback,
            padded_row,
            used: 0,
        }
    }
}

/// Targets kept warm; the least recently used beyond this is dropped.
const TARGET_POOL: usize = 4;

struct Inner {
    layout: wgpu::BindGroupLayout,
    pipes: HashMap<(wgpu::TextureFormat, u32), Pipelines>,
    builder: SceneBuilder,
    atlas: GpuAtlas,
    globals: GrowBuffer,
    /// Bind group over `globals` and the palette, with the atlas generation it was made for.
    bind: Option<(wgpu::BindGroup, u64)>,
    targets: Vec<Target>,
    clock: u64,
    timings: RenderTimings,
    stats: SceneStats,
    /// Id-buffer target (`R32Uint` + depth + readback), kept for the last size asked.
    ids: Option<IdTarget>,
}

/// The id-buffer target of [`RenderService::render_ids`].
struct IdTarget {
    size: (u32, u32),
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    depth: wgpu::TextureView,
    readback: wgpu::Buffer,
    padded_row: u32,
}

impl IdTarget {
    fn new(gpu: &Gpu, size: (u32, u32)) -> IdTarget {
        let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
        let ra = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("service ids"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PICK_FORMAT,
            usage: ra | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let depth = gpu
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("service ids depth"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: ra,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let padded_row = (size.0 * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("service ids readback"),
            size: padded_row as u64 * size.1 as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        IdTarget { size, view: tex.create_view(&Default::default()), tex, depth, readback, padded_row }
    }
}

/// Per-pixel part ids from [`RenderService::render_ids`]: 0 is background, otherwise the index of
/// the flattened part plus one.
#[derive(Clone, Debug, Default)]
pub struct IdImage {
    pub width: u32,
    pub height: u32,
    pub ids: Vec<u32>,
}

impl IdImage {
    pub fn at(&self, x: u32, y: u32) -> u32 {
        self.ids[(y * self.width + x) as usize]
    }

    /// Visible pixel count per flattened part index inside `r` (index = id - 1).
    pub fn counts(&self, r: Rect, parts: usize) -> Vec<u32> {
        let mut out = vec![0u32; parts];
        let r = r.clamp((self.width, self.height));
        for y in r.y..r.y + r.h {
            let row = &self.ids[(y * self.width) as usize..][r.x as usize..(r.x + r.w) as usize];
            for &id in row {
                if id > 0 {
                    if let Some(c) = out.get_mut(id as usize - 1) {
                        *c += 1;
                    }
                }
            }
        }
        out
    }
}

/// Warm headless renderer; see the module docs. `Send + Sync`: calls are serialised internally.
pub struct RenderService {
    gpu: Arc<Gpu>,
    inner: Mutex<Inner>,
}

const _: fn() = || {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<RenderService>();
};

impl RenderService {
    /// Open a headless device and set up the service.
    pub fn new() -> Result<RenderService, String> {
        Ok(RenderService::with_gpu(Arc::new(Gpu::headless()?)))
    }

    /// A service on an existing device (shared with, say, a window).
    pub fn with_gpu(gpu: Arc<Gpu>) -> RenderService {
        let layout = globals_layout(&gpu.device);
        let inner = Inner {
            layout,
            pipes: HashMap::new(),
            builder: SceneBuilder::new(),
            atlas: GpuAtlas::new(&gpu),
            globals: GrowBuffer::new(&gpu, "service globals", wgpu::BufferUsages::UNIFORM, GLOBALS_STRIDE * 16),
            bind: None,
            targets: Vec::new(),
            clock: 0,
            timings: RenderTimings::default(),
            stats: SceneStats::default(),
            ids: None,
        };
        RenderService { gpu, inner: Mutex::new(inner) }
    }

    pub fn gpu(&self) -> &Arc<Gpu> {
        &self.gpu
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic mid-render leaves nothing half-written that a later call depends on.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Compile the default pipelines and draw one small frame (a 2x4 brick), so the first real
    /// call does not pay for shader compilation and first-use driver work.
    pub fn warm_up(&self, lib: &Library) {
        let _ = self.thumbnail(lib, "3001.dat", Color(4), 64);
    }

    /// Timings of the last render.
    pub fn last_timings(&self) -> RenderTimings {
        self.lock().timings
    }

    /// Scene statistics of the last render.
    pub fn last_stats(&self) -> SceneStats {
        self.lock().stats.clone()
    }

    /// Model bounds (min, max) as the renderer sees them (part meshes, not just origins).
    pub fn bounds(&self, lib: &Library, flat: &[FlatPart], opts: &SceneOptions) -> (Vec3, Vec3) {
        self.lock().builder.sync(lib, flat, opts).bounds
    }

    /// Draw `flat` once per panel into a `size` image. `flags` sets per-part flag bits
    /// (`FLAG_*`, by flattened part index) for this frame only.
    pub fn render_panels(
        &self,
        lib: &Library,
        flat: &[FlatPart],
        flags: &[(u32, u32)],
        panels: &[Panel],
        size: (u32, u32),
    ) -> Result<RgbaImage, String> {
        self.render_panels_with(lib, flat, flags, panels, size, &RenderOptions::default())
    }

    pub fn render_panels_with(
        &self,
        lib: &Library,
        flat: &[FlatPart],
        flags: &[(u32, u32)],
        panels: &[Panel],
        size: (u32, u32),
        opts: &RenderOptions,
    ) -> Result<RgbaImage, String> {
        self.render_framed(lib, flat, flags, size, opts, |_| panels.to_vec())
    }

    /// Like [`render_panels_with`](Self::render_panels_with), with panels chosen after the sync,
    /// from the frame (for example to frame cameras on [`SceneFrame::bounds`]).
    pub fn render_framed(
        &self,
        lib: &Library,
        flat: &[FlatPart],
        flags: &[(u32, u32)],
        size: (u32, u32),
        opts: &RenderOptions,
        panels: impl FnOnce(&SceneFrame) -> Vec<Panel>,
    ) -> Result<RgbaImage, String> {
        if size.0 == 0 || size.1 == 0 {
            return Err(format!("empty image size {}x{}", size.0, size.1));
        }
        let limit = self.gpu.device.limits().max_texture_dimension_2d;
        if size.0 > limit || size.1 > limit {
            return Err(format!("image size {}x{} exceeds the GPU limit {limit}", size.0, size.1));
        }
        let gpu = &*self.gpu;
        let t0 = Instant::now();
        let mut guard = self.lock();
        let inner = &mut *guard;

        let mut frame = inner.builder.sync(lib, flat, &opts.scene);
        frame.apply_flags(flags);
        let panels = panels(&frame);
        let t_sync = Instant::now();

        // Uploads: new atlas vertices, palette, instances, one globals slot per panel.
        inner.atlas.upload(gpu, &mut inner.builder, &frame);
        let mut globals = vec![0u8; (panels.len().max(1) as u64 * GLOBALS_STRIDE) as usize];
        for (i, p) in panels.iter().enumerate() {
            let g = view_globals(&p.camera, (p.rect.w, p.rect.h));
            let at = i * GLOBALS_STRIDE as usize;
            globals[at..at + size_of_val(&g)].copy_from_slice(bytemuck::bytes_of(&g));
        }
        let globals_grew = inner.globals.write(gpu, 0, &globals);
        let samples = opts.samples.max(1);
        let pipes = inner
            .pipes
            .entry((COLOR_FORMAT, samples))
            .or_insert_with(|| Pipelines::with_layout(gpu, &inner.layout, COLOR_FORMAT, samples))
            .clone();
        let generation = inner.atlas.generation;
        if globals_grew || inner.bind.as_ref().is_none_or(|(_, g)| *g != generation) {
            let bind = pipes.bind_group(&gpu.device, &inner.globals.buf, &inner.atlas.palette.buf);
            inner.bind = Some((bind, generation));
        }
        let bind = &inner.bind.as_ref().unwrap().0;

        // Target from the pool.
        inner.clock += 1;
        let ti = match inner.targets.iter().position(|t| t.size == size && t.samples == samples) {
            Some(i) => i,
            None => {
                if inner.targets.len() >= TARGET_POOL {
                    let lru = (0..inner.targets.len()).min_by_key(|&i| inner.targets[i].used).unwrap();
                    inner.targets.swap_remove(lru);
                }
                inner.targets.push(Target::new(gpu, size, samples));
                inner.targets.len() - 1
            }
        };
        let target = &mut inner.targets[ti];
        target.used = inner.clock;

        let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("service") });
        {
            let (view, resolve) = match &target.msaa {
                Some(m) => (m, Some(&target.resolve_view)),
                None => (&target.resolve_view, None),
            };
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("service panels"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: resolve,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(opts.background),
                        store: if resolve.is_some() { wgpu::StoreOp::Discard } else { wgpu::StoreOp::Store },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let list = DrawList {
                tri: &inner.atlas.tri.buf,
                line: &inner.atlas.line.buf,
                cond: &inner.atlas.cond.buf,
                inst: &inner.atlas.inst.buf,
                meshes: inner.builder.meshes(),
                batches: &frame.batches,
                edges: frame.edges,
            };
            if !frame.instances.is_empty() {
                for (i, p) in panels.iter().enumerate() {
                    let r = p.rect.clamp(size);
                    if r.w == 0 || r.h == 0 {
                        continue;
                    }
                    pass.set_viewport(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 0.0, 1.0);
                    pass.set_scissor_rect(r.x, r.y, r.w, r.h);
                    pass.set_bind_group(0, bind, &[(i as u64 * GLOBALS_STRIDE) as u32]);
                    draw_list(&mut pass, &pipes, &list);
                }
            }
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.resolve,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &target.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(target.padded_row),
                    rows_per_image: Some(size.1),
                },
            },
            wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        );
        let t_upload = Instant::now();
        gpu.queue.submit([enc.finish()]);
        gpu.wait();
        let t_render = Instant::now();

        let slice = target.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        gpu.wait();
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("readback map failed: {e}"))?;
        let row = (size.0 * 4) as usize;
        let mut pixels = Vec::with_capacity(row * size.1 as usize);
        {
            let data = slice.get_mapped_range().map_err(|e| format!("readback not mapped: {e:?}"))?;
            for r in data.chunks(target.padded_row as usize).take(size.1 as usize) {
                pixels.extend_from_slice(&r[..row]);
            }
        }
        target.readback.unmap();
        let t_end = Instant::now();

        let ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1e3;
        inner.timings = RenderTimings {
            sync_ms: ms(t0, t_sync),
            upload_ms: ms(t_sync, t_upload),
            render_ms: ms(t_upload, t_render),
            readback_ms: ms(t_render, t_end),
            total_ms: ms(t0, t_end),
        };
        inner.stats = frame.stats;
        Ok(RgbaImage { width: size.0, height: size.1, pixels })
    }

    /// Draw `panels` into an id buffer instead of colours: each pixel holds the index of the
    /// flattened part in front plus one (0 = background). Bodies only, no edges, no MSAA. Parts
    /// flagged hidden, ghost, delete or stale (and hidden-ghost unless also `FLAG_PICK_HIDDEN`) are
    /// left out, so they neither show nor occlude. Used to measure what each view really shows.
    pub fn render_ids(
        &self,
        lib: &Library,
        flat: &[FlatPart],
        flags: &[(u32, u32)],
        panels: &[Panel],
        size: (u32, u32),
        scene: &SceneOptions,
    ) -> Result<IdImage, String> {
        if size.0 == 0 || size.1 == 0 {
            return Err(format!("empty image size {}x{}", size.0, size.1));
        }
        let gpu = &*self.gpu;
        let mut guard = self.lock();
        let inner = &mut *guard;
        let mut frame = inner.builder.sync(lib, flat, scene);
        frame.apply_flags(flags);
        inner.atlas.upload(gpu, &mut inner.builder, &frame);
        let mut globals = vec![0u8; (panels.len().max(1) as u64 * GLOBALS_STRIDE) as usize];
        for (i, p) in panels.iter().enumerate() {
            let g = view_globals(&p.camera, (p.rect.w, p.rect.h));
            let at = i * GLOBALS_STRIDE as usize;
            globals[at..at + size_of_val(&g)].copy_from_slice(bytemuck::bytes_of(&g));
        }
        let globals_grew = inner.globals.write(gpu, 0, &globals);
        let pipes = inner
            .pipes
            .entry((COLOR_FORMAT, 1))
            .or_insert_with(|| Pipelines::with_layout(gpu, &inner.layout, COLOR_FORMAT, 1))
            .clone();
        let generation = inner.atlas.generation;
        if globals_grew || inner.bind.as_ref().is_none_or(|(_, g)| *g != generation) {
            let bind = pipes.bind_group(&gpu.device, &inner.globals.buf, &inner.atlas.palette.buf);
            inner.bind = Some((bind, generation));
        }
        if inner.ids.as_ref().is_none_or(|t| t.size != size) {
            inner.ids = Some(IdTarget::new(gpu, size));
        }
        let bind = &inner.bind.as_ref().unwrap().0;
        let target = inner.ids.as_ref().unwrap();
        let meshes = inner.builder.meshes();
        let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("ids") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("service ids"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !frame.instances.is_empty() {
                pass.set_pipeline(&pipes.pick);
                pass.set_vertex_buffer(1, inner.atlas.inst.buf.slice(..));
                pass.set_vertex_buffer(0, inner.atlas.tri.buf.slice(..));
                for (i, p) in panels.iter().enumerate() {
                    let r = p.rect.clamp(size);
                    if r.w == 0 || r.h == 0 {
                        continue;
                    }
                    pass.set_viewport(r.x as f32, r.y as f32, r.w as f32, r.h as f32, 0.0, 1.0);
                    pass.set_scissor_rect(r.x, r.y, r.w, r.h);
                    pass.set_bind_group(0, bind, &[(i as u64 * GLOBALS_STRIDE) as u32]);
                    for b in &frame.batches {
                        let tris = meshes[b.mesh as usize].tris.clone();
                        if !tris.is_empty() && !b.instances.is_empty() {
                            pass.draw(tris, b.instances.clone());
                        }
                    }
                }
            }
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &target.readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(target.padded_row),
                    rows_per_image: Some(size.1),
                },
            },
            wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 },
        );
        gpu.queue.submit([enc.finish()]);
        let slice = target.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        gpu.wait();
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("id readback map failed: {e}"))?;
        let mut ids = Vec::with_capacity((size.0 * size.1) as usize);
        {
            let data = slice.get_mapped_range().map_err(|e| format!("id readback not mapped: {e:?}"))?;
            for r in data.chunks(target.padded_row as usize).take(size.1 as usize) {
                ids.extend(r[..(size.0 * 4) as usize].chunks_exact(4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])));
            }
        }
        target.readback.unmap();
        Ok(IdImage { width: size.0, height: size.1, ids })
    }

    /// A `px`-square picture of one part in `color`, three-quarter view (the parts-panel recipe).
    pub fn thumbnail(&self, lib: &Library, part: &str, color: Color, px: u32) -> Result<RgbaImage, String> {
        let p = FlatPart {
            name: part.to_string(),
            color,
            pos: DVec3::ZERO,
            rot: DMat3::IDENTITY,
            step: 1,
            path: String::new(),
            src: (0, 0, 0),
            root_line: 0,
            ancestors: Vec::new(),
        };
        let size = (px, px);
        self.render_framed(lib, &[p], &[], size, &RenderOptions::default(), |f| {
            vec![Panel { camera: Camera::framing(f.bounds.0, f.bounds.1, 1.0), rect: Rect::full(size) }]
        })
    }
}
